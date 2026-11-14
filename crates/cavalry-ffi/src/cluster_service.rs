//! A cluster service's handle: its poll with the C fragment handler, an entry's
//! values, the reply and the action its handler calls, its reads and its close
//! (`spec/c-abi.md`, "Functions", cluster service, "Result codes", "Calls during a
//! claim or a poll", "What a header carries"; ADR 0123). The Rust core is
//! `cavalry_client::ClusterService` (`spec/cluster.md`, "The service" and "Replies";
//! ADR 0118).
//!
//! The Rust handler is `FnMut(&Committed, &mut Replies) -> Action`. The C handler is
//! the one fragment handler, which takes no `Replies` and returns nothing, so the
//! handle carries both across the call: a pointer to the `Replies` of the entry being
//! handled, which `cavalry_cluster_service_reply` answers through, and the action
//! `cavalry_cluster_service_action` set, which the poll returns to the Rust core.

use std::ffi::{c_int, c_void};
use std::ptr;

use cavalry_client::{Action, ClusterError, ClusterService, Committed, Entry, Replies, Reply};

use crate::client::{self, cavalry_mut, cavalry_t};
use crate::codes::*;
use crate::error::{self, DOC};
use crate::handle;
use crate::subscription::{
    Carries, NO_FAULT, cavalry_fault_t, cavalry_fragment_handler_t, cavalry_header_t, fault_fields,
};

/// Whether a poll runs: the state word every function of the handle reads first.
// not a state machine: the word records whether the caller's poll is running, a borrow the page replaces by convention, and no counter counts a caller's polls
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Polling,
}

/// A cluster's service beside one member; made by `cavalry_cluster_service`, freed by
/// `cavalry_cluster_service_close`.
pub struct cavalry_cluster_service_t {
    state: State,
    fault: cavalry_fault_t,
    /// What the handler does with the entry it is handling, `CAVALRY_ACTION_*`:
    /// written before each call of the handler, by `cavalry_cluster_service_action`
    /// from inside it, and read when it returns.
    action: i32,
    /// The `Replies` of the entry being handled, for the run of the C handler alone:
    /// null whenever no handler of this service runs, which is how a reply or an
    /// action from outside the handler is told.
    replies: *mut Replies<'static>,
    service: ClusterService,
}

// SAFETY: the one raw pointer the handle holds, `replies`, is null whenever no call on
// the handle runs: the poll writes it before it calls the C handler and writes null
// when the handler returns, on the polling thread, and nothing else writes it. A handle
// moves between threads only between calls ("Threads"), so it never carries the
// pointer with it. Everything else in the handle is `Send` on its own: plain data, and
// `cavalry_client::ClusterService`, which `lib.rs` asserts `Send` at compile time, so
// that a change making it `!Send` fails the build rather than passing under this impl.
unsafe impl Send for cavalry_cluster_service_t {}

/// One committed entry as a service is handed it: what `cavalry_cluster_entry_values`
/// fills. A session message's payload is the handler's `buffer` and `length`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct cavalry_cluster_entry_values_t {
    /// The position in the log at which the entry begins.
    pub position: i64,
    /// The leadership term the entry was appended in.
    pub leadership_term_id: i64,
    /// The cluster's time when its leader appended the entry, milliseconds since
    /// 1970-01-01T00:00:00Z: the one clock a service reads, so that every member's
    /// service reaches the same state.
    pub timestamp: i64,
    /// The session of a session message, a session open event or a session close
    /// event; −1 for every other entry, and for a session message the leader's own
    /// process appended for no session.
    pub session_id: i64,
    /// What the entry is, `CAVALRY_CLUSTER_ENTRY_*`.
    pub r#type: i32,
    /// The member that leads the term a new leadership term event begins; −1 for
    /// every other entry.
    pub leader_member_id: i32,
    /// Whether the leader closed the session of a session close event for the
    /// session timeout, and not the client; `false` for every other entry.
    pub timed_out: bool,
}

/// Reads the state word through the handle's pointer, without a reference to the
/// wrapped service.
///
/// # Safety
///
/// `service` is a live handle.
unsafe fn state(service: *const cavalry_cluster_service_t) -> State {
    // SAFETY: a live handle; the read touches the state field alone, which a running
    // poll does not borrow.
    unsafe { ptr::addr_of!((*service).state).read() }
}

fn polling(function: &str) {
    error::set(
        CAVALRY_ERR_POLLING,
        format_args!(
            "{function} refused (polling): the call came from inside this cluster service's \
             own fragment handler, while its poll runs; make it after the poll returns; see \
             {DOC}#calls-during-a-claim-or-a-poll"
        ),
    );
}

fn no_entry(function: &str) {
    error::set(
        CAVALRY_ERR_CLUSTER_NO_ENTRY,
        format_args!(
            "{function} refused (cluster-no-entry): the call came from outside this cluster \
             service's fragment handler, where no entry is being handled; make it from \
             inside the handler cavalry_cluster_service_poll calls; see \
             {DOC}#calls-during-a-claim-or-a-poll"
        ),
    );
}

/// An entry's values, as the struct holds them.
fn values_of(committed: &Committed<'_>) -> cavalry_cluster_entry_values_t {
    let (r#type, session_id, leader_member_id, timed_out) = match committed.entry {
        Entry::SessionMessage { session_id, .. } => {
            (CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE, session_id, -1, false)
        }
        Entry::SessionOpen { session_id } => {
            (CAVALRY_CLUSTER_ENTRY_SESSION_OPEN, session_id, -1, false)
        }
        Entry::SessionClose {
            session_id,
            timed_out,
        } => (
            CAVALRY_CLUSTER_ENTRY_SESSION_CLOSE,
            session_id,
            -1,
            timed_out,
        ),
        Entry::NewLeadershipTerm { leader_member_id } => (
            CAVALRY_CLUSTER_ENTRY_NEW_LEADERSHIP_TERM,
            -1,
            leader_member_id,
            false,
        ),
        Entry::Other => (CAVALRY_CLUSTER_ENTRY_OTHER, -1, -1, false),
    };
    cavalry_cluster_entry_values_t {
        position: committed.position,
        leadership_term_id: committed.leadership_term_id,
        timestamp: committed.timestamp,
        session_id,
        r#type,
        leader_member_id,
        timed_out,
    }
}

/// What a poll that failed stopped on, as `cavalry_cluster_service_fault` gives it,
/// with the thread's error set: a fault's fields, the position of an entry that is
/// no entry, or the client's or the driver's code alone.
fn stopped_on(error: &ClusterError) -> cavalry_fault_t {
    error::set_cluster(error);
    match error {
        ClusterError::Log(fault) | ClusterError::Egress(fault) => fault_fields(fault),
        ClusterError::Entry { position } => cavalry_fault_t {
            code: CAVALRY_ERR_CLUSTER_ENTRY,
            session_id: 0,
            position: *position,
        },
        _ => cavalry_fault_t {
            code: error::cavalry_errcode(),
            session_id: 0,
            position: CAVALRY_NULL_POSITION,
        },
    }
}

/// Makes a cluster's service beside the member on `client`'s driver, as the Rust
/// `Cavalry::cluster_service` does. It sends nothing: the service's first poll opens
/// the member's log. 0 with `*service` the handle; −1 with `*service` null for a null
/// argument, and for a call from inside the handler of a service whose poll holds
/// `client` (`spec/c-abi.md`; ADR 0123).
///
/// # Safety
///
/// `service` is null or writable; `client` is null or a live client handle not in use
/// by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service(
    service: *mut *mut cavalry_cluster_service_t,
    client: *mut cavalry_t,
) -> c_int {
    if service.is_null() {
        error::null("cavalry_cluster_service", "the handle to fill");
        return -1;
    }
    // SAFETY: `service` is non-null, checked above, and writable, by the caller's
    // convention. Written first, so that every return below leaves a null handle
    // unless it gives one.
    unsafe { *service = ptr::null_mut() };
    if client.is_null() {
        error::null("cavalry_cluster_service", "client");
        return -1;
    }
    // SAFETY: `client` is non-null, checked above, and a live handle.
    if unsafe { client::refused("cavalry_cluster_service", client) } {
        return -1;
    }
    // SAFETY: `client` is non-null, checked above, a live handle used from one thread
    // at a time ("Threads"), and held by no poll, checked above.
    let made = unsafe { cavalry_mut(client) }.cluster_service();
    // SAFETY: `service` is non-null and writable.
    unsafe {
        *service = handle::give(cavalry_cluster_service_t {
            state: State::Idle,
            fault: NO_FAULT,
            action: CAVALRY_ACTION_CONTINUE,
            replies: ptr::null_mut(),
            service: made,
        });
    }
    0
}

/// Reads up to `fragment_limit` committed entries of the member's log and hands
/// `handler` each, with `clientd`; returns the entries the handler took, or −1 with
/// the error and in `cavalry_cluster_service_fault`; 0 while the driver hosts no
/// member that has a log yet. A session message's payload is the handler's `buffer`
/// and `length`, and every other entry has a `length` of 0. The header carries the
/// entry's values, which `cavalry_cluster_entry_values` fills. From inside the
/// handler `cavalry_cluster_service_reply` answers a session and
/// `cavalry_cluster_service_action` says what the handler does with the entry; a
/// handler that calls no action continues. The poll is the service's duty cycle: it
/// opens the member's log, follows the log's sessions, and holds a publication to
/// each open session while the service answers. Those are commands sent with
/// `client`, which the poll holds for its run: none of them is sent, and nothing is
/// allocated, for a session message.
///
/// # Safety
///
/// `service` is null or a live handle not in use by another call; `client` is null or
/// a live client handle not in use by another call; `handler` is null or a function
/// that returns, without unwinding or a `longjmp`, and holds the rules of "Calls
/// during a claim or a poll".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_poll(
    service: *mut cavalry_cluster_service_t,
    client: *mut cavalry_t,
    handler: cavalry_fragment_handler_t,
    clientd: *mut c_void,
    fragment_limit: usize,
) -> c_int {
    if service.is_null() {
        error::null("cavalry_cluster_service_poll", "service");
        return -1;
    }
    if client.is_null() {
        error::null("cavalry_cluster_service_poll", "client");
        return -1;
    }
    let Some(handler) = handler else {
        error::null("cavalry_cluster_service_poll", "handler");
        return -1;
    };
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(service) } == State::Polling {
        polling("cavalry_cluster_service_poll");
        return -1;
    }
    // SAFETY: `client` is non-null, checked above, and a live handle.
    if unsafe { client::refused("cavalry_cluster_service_poll", client) } {
        return -1;
    }
    let limit = fragment_limit.min(c_int::MAX as usize);
    // SAFETY: both state words are Idle, so no poll borrows the service or the client,
    // and each handle is used from one thread at a time: these are the one reference
    // to each. Each state word is written through its own field, so that a re-entrant
    // call from the handler reads it without touching what the poll borrows. The
    // action and the pointer to the entry's `Replies` are written through their own
    // fields too, which the borrowed service does not hold; the pointer is derived
    // from the exclusive reference the Rust poll hands this closure, is used by
    // `cavalry_cluster_service_reply` only while the C handler runs, during which the
    // closure does not touch that reference, and is null again before the closure
    // returns it to the poll.
    let result = unsafe {
        ptr::addr_of_mut!((*service).state).write(State::Polling);
        client::hold(client, true);
        let inner = &mut *ptr::addr_of_mut!((*service).service);
        let result = inner.poll(
            cavalry_mut(client),
            |committed, replies| {
                let view = cavalry_header_t {
                    carries: Carries::Committed(values_of(committed)),
                };
                let payload: &[u8] = match committed.entry {
                    Entry::SessionMessage { payload, .. } => payload,
                    _ => &[],
                };
                ptr::addr_of_mut!((*service).action).write(CAVALRY_ACTION_CONTINUE);
                ptr::addr_of_mut!((*service).replies)
                    .write(ptr::from_mut(replies).cast::<Replies<'static>>());
                // The caller's handler, by the convention above, inside the enclosing
                // unsafe block; the payload and the header are valid for the call.
                handler(clientd, payload.as_ptr(), payload.len(), &view);
                ptr::addr_of_mut!((*service).replies).write(ptr::null_mut());
                match ptr::addr_of!((*service).action).read() {
                    CAVALRY_ACTION_ABORT => Action::Abort,
                    CAVALRY_ACTION_BREAK => Action::Break,
                    _ => Action::Continue,
                }
            },
            limit,
        );
        client::hold(client, false);
        ptr::addr_of_mut!((*service).state).write(State::Idle);
        result
    };
    match result {
        Ok(taken) => {
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*service).fault).write(NO_FAULT) };
            taken as c_int
        }
        Err(error) => {
            let fault = stopped_on(&error);
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*service).fault).write(fault) };
            -1
        }
    }
}

/// Fills `values` with the values of the committed entry whose header
/// `cavalry_cluster_service_poll` handed its handler; −1 with
/// `CAVALRY_ERR_WRONG_HEADER` for a header from any other poll, which carries a
/// frame's header or a reply's values and no entry's.
///
/// # Safety
///
/// `header` is null or the header a poll handed the handler, during that call;
/// `values` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_entry_values(
    header: *const cavalry_header_t,
    values: *mut cavalry_cluster_entry_values_t,
) -> c_int {
    if header.is_null() {
        error::null("cavalry_cluster_entry_values", "header");
        return -1;
    }
    if values.is_null() {
        error::null("cavalry_cluster_entry_values", "values");
        return -1;
    }
    // SAFETY: the handler's header, valid for the call.
    let Carries::Committed(entry) = (unsafe { (*header).carries }) else {
        error::set(
            CAVALRY_ERR_WRONG_HEADER,
            format_args!(
                "cavalry_cluster_entry_values refused (wrong-header): the header came from \
                 another poll than cavalry_cluster_service_poll and carries no entry's \
                 values; a committed log's entry is its bytes, and its position is \
                 cavalry_committed_log_entry_position; see {DOC}#what-a-header-carries"
            ),
        );
        return -1;
    };
    // SAFETY: a writable struct, by the caller's convention.
    unsafe { *values = entry };
    0
}

/// From inside the service's handler: sends the `length` bytes at `buffer` to the
/// session with `session_id`, as a session message on the egress stream with the
/// leadership term id and the cluster's time of the entry being handled, as the Rust
/// `Replies::reply` does, and returns what became of the reply,
/// `CAVALRY_CLUSTER_REPLY_*`. `_TOO_LONG` sets the thread's error too,
/// `CAVALRY_ERR_TOO_LONG`, with the length and the limit in the message. −1 with
/// `CAVALRY_ERR_CLUSTER_NO_ENTRY` for a call from outside the handler. Nothing here
/// allocates, and nothing waits.
///
/// # Safety
///
/// `service` is null or a live handle, and the call is made by the thread that polls
/// it or by no poll's handler at all; `buffer` is null with a `length` of 0, or
/// readable for `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_reply(
    service: *mut cavalry_cluster_service_t,
    session_id: i64,
    buffer: *const u8,
    length: usize,
) -> c_int {
    if service.is_null() {
        error::null("cavalry_cluster_service_reply", "service");
        return -1;
    }
    if buffer.is_null() && length > 0 {
        error::null("cavalry_cluster_service_reply", "buffer");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention; the read touches the pointer
    // field alone, which a running poll does not borrow.
    let replies = unsafe { ptr::addr_of!((*service).replies).read() };
    if replies.is_null() {
        no_entry("cavalry_cluster_service_reply");
        return -1;
    }
    let payload: &[u8] = if length == 0 {
        &[]
    } else {
        // SAFETY: readable for `length` bytes, by the caller's convention, during the
        // call.
        unsafe { std::slice::from_raw_parts(buffer, length) }
    };
    // SAFETY: the pointer is not null, so this service's poll is inside its C handler
    // and this call came from it: the pointer names the `Replies` the Rust poll handed
    // the closure for the entry being handled, which the closure does not touch until
    // the handler returns, so this is the one reference to it.
    let reply = unsafe { (*replies).reply(session_id, payload) };
    match reply {
        Reply::Sent => CAVALRY_CLUSTER_REPLY_SENT,
        Reply::NotAnswering => CAVALRY_CLUSTER_REPLY_NOT_ANSWERING,
        Reply::Retry => CAVALRY_CLUSTER_REPLY_RETRY,
        Reply::Dropped => CAVALRY_CLUSTER_REPLY_DROPPED,
        Reply::NoSession => CAVALRY_CLUSTER_REPLY_NO_SESSION,
        Reply::TooLong { found, limit } => {
            error::set(
                CAVALRY_ERR_TOO_LONG,
                format_args!(
                    "cavalry_cluster_service_reply refused (too-long): the reply is {found} \
                     bytes and one frame carries {limit} after the session message header; \
                     send a reply of at most {limit} bytes, or several; see \
                     docs/reference/cluster.md#replies"
                ),
            );
            CAVALRY_CLUSTER_REPLY_TOO_LONG
        }
    }
}

/// From inside the service's handler: says what the handler does with the entry it
/// was handed, `CAVALRY_ACTION_ABORT`, `CAVALRY_ACTION_BREAK` or
/// `CAVALRY_ACTION_CONTINUE`, the `Action` the Rust handler returns. A handler that
/// does not call it continues, and the last call for an entry counts. −1 with
/// `CAVALRY_ERR_CLUSTER_ACTION` for any other value, which leaves the action as it
/// was, and with `CAVALRY_ERR_CLUSTER_NO_ENTRY` for a call from outside the handler.
///
/// # Safety
///
/// `service` is null or a live handle, and the call is made by the thread that polls
/// it or by no poll's handler at all.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_action(
    service: *mut cavalry_cluster_service_t,
    action: i32,
) -> c_int {
    if service.is_null() {
        error::null("cavalry_cluster_service_action", "service");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention; the read touches the pointer
    // field alone, which a running poll does not borrow.
    if unsafe { ptr::addr_of!((*service).replies).read() }.is_null() {
        no_entry("cavalry_cluster_service_action");
        return -1;
    }
    if !matches!(
        action,
        CAVALRY_ACTION_ABORT | CAVALRY_ACTION_BREAK | CAVALRY_ACTION_CONTINUE
    ) {
        error::set(
            CAVALRY_ERR_CLUSTER_ACTION,
            format_args!(
                "cavalry_cluster_service_action refused (cluster-action): {action} is no \
                 action; pass CAVALRY_ACTION_ABORT, 1, CAVALRY_ACTION_BREAK, 2, or \
                 CAVALRY_ACTION_CONTINUE, 4; see {DOC}#result-codes"
            ),
        );
        return -1;
    }
    // SAFETY: the action field alone, which a running poll does not borrow.
    unsafe { ptr::addr_of_mut!((*service).action).write(action) };
    0
}

/// Fills `fault` with what the last poll stopped on: a fault's fields, the position
/// of an entry that is no entry with `CAVALRY_ERR_CLUSTER_ENTRY`, or the client's or
/// the driver's code; a code of 0 when the last poll returned a count.
///
/// # Safety
///
/// `service` is null or a live handle; `fault` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_fault(
    service: *const cavalry_cluster_service_t,
    fault: *mut cavalry_fault_t,
) -> c_int {
    if service.is_null() {
        error::null("cavalry_cluster_service_fault", "service");
        return -1;
    }
    if fault.is_null() {
        error::null("cavalry_cluster_service_fault", "fault");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(service) } == State::Polling {
        polling("cavalry_cluster_service_fault");
        return -1;
    }
    // SAFETY: the fault field alone, and a writable struct.
    unsafe { *fault = ptr::addr_of!((*service).fault).read() };
    0
}

/// Reads through the service when no poll runs, or sets the error.
///
/// # Safety
///
/// `service` is null or a live handle not in use by another call.
unsafe fn read<T>(
    function: &str,
    service: *const cavalry_cluster_service_t,
    refused: T,
    run: impl FnOnce(&ClusterService) -> T,
) -> T {
    if service.is_null() {
        error::null(function, "service");
        return refused;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(service) } == State::Polling {
        polling(function);
        return refused;
    }
    // SAFETY: no poll borrows the service.
    run(unsafe { &*ptr::addr_of!((*service).service) })
}

/// The position after the last entry the handler took, 0 before the first: how far
/// the service has applied the log.
///
/// # Safety
///
/// `service` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_position(
    service: *const cavalry_cluster_service_t,
) -> i64 {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_cluster_service_position",
            service,
            -1,
            ClusterService::position,
        )
    }
}

/// Whether the service answered the last entry it read: its member led that entry's
/// term and, as the entry was read, led it still, and the entry was not history.
///
/// # Safety
///
/// `service` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_answers(
    service: *const cavalry_cluster_service_t,
) -> bool {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_cluster_service_answers",
            service,
            false,
            ClusterService::answers,
        )
    }
}

/// How many sessions the log holds open, as far as the service has read: 10 at most.
///
/// # Safety
///
/// `service` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_sessions(
    service: *const cavalry_cluster_service_t,
) -> c_int {
    // SAFETY: the caller's convention above.
    unsafe {
        read("cavalry_cluster_service_sessions", service, -1, |service| {
            service.sessions() as c_int
        })
    }
}

/// Closes the service's publications and its counter, stops its read of the log, and
/// frees the handle, as the Rust `ClusterService::close` does. With a null `client`
/// it is the Rust handle's drop: the publications, the counter and the log's
/// subscription are closed with no answer waited for, the replay is left to the
/// driver, and the function returns 0, which is how a service is closed after its
/// client's `cavalry_close`. The handle is freed whatever the function returns, with
/// one exception: a call from inside the service's own handler, or from inside the
/// handler of another service whose poll holds `client`, is refused with
/// `CAVALRY_ERR_POLLING` and frees nothing.
///
/// # Safety
///
/// `service` is null or a live handle, closed once; `client` is null or a live client
/// handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_service_close(
    service: *mut cavalry_cluster_service_t,
    client: *mut cavalry_t,
) -> c_int {
    if service.is_null() {
        error::null("cavalry_cluster_service_close", "service");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(service) } == State::Polling {
        polling("cavalry_cluster_service_close");
        return -1;
    }
    // SAFETY: when it is not null, `client` is a live handle, by the caller's
    // convention. Checked before anything is freed.
    if !client.is_null() && unsafe { client::refused("cavalry_cluster_service_close", client) } {
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention; the service is
    // moved out, so its own close below consumes it.
    let closed = unsafe { handle::take_out(service) };
    if client.is_null() {
        // Dropping the service closes its publications, its counter and the log's
        // subscription; the driver ends the replay once the replay connect timeout
        // has passed with no subscriber.
        drop(closed);
        return 0;
    }
    // SAFETY: `client` is non-null, checked above, a live handle used from one thread
    // at a time ("Threads"), and held by no poll, checked above.
    let client = unsafe { cavalry_mut(client) };
    match closed.service.close(client) {
        Ok(()) => 0,
        Err(error) => {
            error::set_cluster(&error);
            -1
        }
    }
}

#[cfg(test)]
mod tests {
    use cavalry_client::{ClientError, Fault};

    use super::*;

    /// What a failed poll stopped on, as `cavalry_cluster_service_fault` gives it
    /// (`spec/c-abi.md`, "Structs", the fault): an entry that is no entry on 144 with
    /// its position, a fault with the fault's own fields, and the client's code with
    /// no session and no position. The thread's error is the same code each time.
    #[test]
    fn a_failed_poll_is_told_by_its_code_and_its_position() {
        let fault = stopped_on(&ClusterError::Entry { position: 4_096 });
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (CAVALRY_ERR_CLUSTER_ENTRY, 0, 4_096)
        );
        assert_eq!(error::cavalry_errcode(), CAVALRY_ERR_CLUSTER_ENTRY);

        let fault = stopped_on(&ClusterError::Log(Fault::ImagesFull { session_id: 7 }));
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (CAVALRY_ERR_FAULT_IMAGES_FULL, 7, CAVALRY_NULL_POSITION)
        );
        assert_eq!(error::cavalry_errcode(), CAVALRY_ERR_FAULT_IMAGES_FULL);

        let fault = stopped_on(&ClusterError::Log(Fault::Closed));
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (CAVALRY_ERR_FAULT_CLOSED, 0, CAVALRY_NULL_POSITION)
        );

        let fault = stopped_on(&ClusterError::Client(ClientError::ResponseTimeout {
            command: "add exclusive publication",
        }));
        assert_eq!(
            (fault.code, fault.session_id, fault.position),
            (CAVALRY_ERR_RESPONSE_TIMEOUT, 0, CAVALRY_NULL_POSITION)
        );
        assert_eq!(error::cavalry_errcode(), CAVALRY_ERR_RESPONSE_TIMEOUT);
    }

    /// Every entry of the Rust `Entry` on its define, with the fields the others lack
    /// at −1 and `false` (`spec/c-abi.md`, "Structs", the entry values).
    #[test]
    fn an_entrys_values_are_the_rust_entrys() {
        let committed = |entry| Committed {
            entry,
            position: 96,
            leadership_term_id: 3,
            timestamp: 1_700_000_000_000,
        };
        let values = |entry| {
            let values = values_of(&committed(entry));
            assert_eq!(
                (values.position, values.leadership_term_id, values.timestamp),
                (96, 3, 1_700_000_000_000)
            );
            (
                values.r#type,
                values.session_id,
                values.leader_member_id,
                values.timed_out,
            )
        };
        assert_eq!(
            values(Entry::SessionMessage {
                session_id: 5,
                payload: b"x",
            }),
            (CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE, 5, -1, false)
        );
        assert_eq!(
            values(Entry::SessionMessage {
                session_id: -1,
                payload: b"",
            }),
            (CAVALRY_CLUSTER_ENTRY_SESSION_MESSAGE, -1, -1, false)
        );
        assert_eq!(
            values(Entry::SessionOpen { session_id: 5 }),
            (CAVALRY_CLUSTER_ENTRY_SESSION_OPEN, 5, -1, false)
        );
        assert_eq!(
            values(Entry::SessionClose {
                session_id: 5,
                timed_out: true,
            }),
            (CAVALRY_CLUSTER_ENTRY_SESSION_CLOSE, 5, -1, true)
        );
        assert_eq!(
            values(Entry::SessionClose {
                session_id: 5,
                timed_out: false,
            }),
            (CAVALRY_CLUSTER_ENTRY_SESSION_CLOSE, 5, -1, false)
        );
        assert_eq!(
            values(Entry::NewLeadershipTerm {
                leader_member_id: 2,
            }),
            (CAVALRY_CLUSTER_ENTRY_NEW_LEADERSHIP_TERM, -1, 2, false)
        );
        assert_eq!(
            values(Entry::Other),
            (CAVALRY_CLUSTER_ENTRY_OTHER, -1, -1, false)
        );
    }
}
