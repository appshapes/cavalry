//! A committed log's handle: its open, its poll with the C fragment handler, an
//! entry's position, its reads and its close (`spec/c-abi.md`, "Functions", committed
//! log, "Calls during a claim or a poll", "A frame's position"; ADR 0070). The Rust
//! core is `cavalry_client::CommittedLog` (`spec/cluster.md`, "The service"; ADR 0066).

use std::ffi::{c_int, c_void};
use std::ptr;

use cavalry_client::CommittedLog;

use crate::client::{cavalry_mut, cavalry_t};
use crate::codes::*;
use crate::error::{self, DOC};
use crate::handle;
use crate::subscription::{
    NO_FAULT, cavalry_fault_t, cavalry_fragment_handler_t, cavalry_header_t, fault_fields,
    fault_struct,
};

/// Whether a poll runs: the state word every function of the handle reads first.
// not a state machine: the word records whether the caller's poll is running, a borrow the page replaces by convention, and no counter counts a caller's polls
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Polling,
}

/// A committed log; made by `cavalry_committed_log`, freed by
/// `cavalry_committed_log_close`.
pub struct cavalry_committed_log_t {
    state: State,
    fault: cavalry_fault_t,
    /// Copied out of the log when the handle is made, so that
    /// `cavalry_committed_log_constants` answers from inside the handler without
    /// reaching the log the running poll borrows.
    constants: cavalry_committed_log_constants_t,
    log: CommittedLog,
}

/// A committed log's constants.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct cavalry_committed_log_constants_t {
    /// The id of the member's recording of the log that the handle replays.
    pub recording_id: i64,
    /// The id of the member whose log this is.
    pub member_id: i32,
}

/// Reads the state word through the handle's pointer, without a reference to the
/// wrapped log.
///
/// # Safety
///
/// `log` is a live handle.
unsafe fn state(log: *const cavalry_committed_log_t) -> State {
    // SAFETY: a live handle; the read touches the state field alone, which a running
    // poll does not borrow.
    unsafe { ptr::addr_of!((*log).state).read() }
}

fn polling(function: &str) {
    error::set(
        CAVALRY_ERR_POLLING,
        format_args!(
            "{function} refused (polling): the call came from inside this committed log's \
             own fragment handler, while its poll runs; make it after the poll returns; see \
             {DOC}#calls-during-a-claim-or-a-poll"
        ),
    );
}

/// Opens the committed log of the cluster member on `client`'s driver at `position`,
/// as the Rust `Cavalry::committed_log` does: 0 with `*log` the handle; 0 with `*log`
/// null and no error set while the driver hosts no member that has a recording of its
/// log; −1 with the error and `*log` null when the driver refuses or the client fails.
///
/// # Safety
///
/// `log` is null or writable; `client` is null or a live client handle not in use by
/// another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log(
    log: *mut *mut cavalry_committed_log_t,
    client: *mut cavalry_t,
    position: i64,
) -> c_int {
    if log.is_null() {
        error::null("cavalry_committed_log", "the handle to fill");
        return -1;
    }
    // SAFETY: `log` is non-null, checked above, and writable, by the caller's
    // convention. Written first, so that every return below leaves a null handle
    // unless it gives one.
    unsafe { *log = ptr::null_mut() };
    if client.is_null() {
        error::null("cavalry_committed_log", "client");
        return -1;
    }
    // SAFETY: `client` is non-null, checked above, and a live handle used from one
    // thread at a time ("Threads").
    let client = unsafe { cavalry_mut(client) };
    match client.committed_log(position) {
        Ok(Some(opened)) => {
            let constants = cavalry_committed_log_constants_t {
                recording_id: opened.recording_id(),
                member_id: opened.member_id(),
            };
            // SAFETY: `log` is non-null and writable.
            unsafe {
                *log = handle::give(cavalry_committed_log_t {
                    state: State::Idle,
                    fault: NO_FAULT,
                    constants,
                    log: opened,
                });
            }
            0
        }
        Ok(None) => 0,
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Hands `handler` up to `fragment_limit` entries that begin below the member's commit
/// position, each as its bytes and its frame's header, with `clientd`; returns the
/// entries delivered, or −1 with the fault as the error and in
/// `cavalry_committed_log_fault`. The header carries the entry's position, which
/// `cavalry_committed_log_entry_position` returns from inside the handler.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call; `handler` is null or a
/// function that returns, without unwinding or a `longjmp`, and holds the rules of
/// "Calls during a claim or a poll".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_poll(
    log: *mut cavalry_committed_log_t,
    handler: cavalry_fragment_handler_t,
    clientd: *mut c_void,
    fragment_limit: usize,
) -> c_int {
    if log.is_null() {
        error::null("cavalry_committed_log_poll", "log");
        return -1;
    }
    let Some(handler) = handler else {
        error::null("cavalry_committed_log_poll", "handler");
        return -1;
    };
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(log) } == State::Polling {
        polling("cavalry_committed_log_poll");
        return -1;
    }
    let limit = fragment_limit.min(c_int::MAX as usize);
    // SAFETY: the state is Idle, so no poll borrows the log, and the handle is used
    // from one thread at a time: this is the one reference to it. The state word is
    // written through its own field so that a re-entrant call from the handler reads
    // it without touching the borrowed log.
    let result = unsafe {
        ptr::addr_of_mut!((*log).state).write(State::Polling);
        let inner = &mut *ptr::addr_of_mut!((*log).log);
        let result = inner.poll(
            |body, header, position| {
                let view = cavalry_header_t {
                    header: *header,
                    position,
                };
                // The caller's handler, by the convention above, inside the enclosing
                // unsafe block; the body and the header are valid for the call.
                handler(clientd, body.as_ptr(), body.len(), &view);
            },
            limit,
        );
        ptr::addr_of_mut!((*log).state).write(State::Idle);
        result
    };
    match result {
        Ok(delivered) => {
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*log).fault).write(NO_FAULT) };
            delivered as c_int
        }
        Err(fault) => {
            let fault = fault_struct(&fault);
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*log).fault).write(fault) };
            -1
        }
    }
}

/// The position in the log at which the entry begins whose header
/// `cavalry_committed_log_poll` handed its handler; −1 with `CAVALRY_ERR_NO_POSITION`
/// for a header from `cavalry_subscription_poll`, which carries none.
///
/// # Safety
///
/// `header` is null or the header a poll handed the handler, during that call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_entry_position(
    header: *const cavalry_header_t,
) -> i64 {
    if header.is_null() {
        error::null("cavalry_committed_log_entry_position", "header");
        return -1;
    }
    // SAFETY: the handler's header, valid for the call.
    let position = unsafe { (*header).position };
    if position == CAVALRY_NULL_POSITION {
        error::set(
            CAVALRY_ERR_NO_POSITION,
            format_args!(
                "cavalry_committed_log_entry_position refused (no-position): the header came \
                 from cavalry_subscription_poll, whose headers carry no position; compute a \
                 subscription's frame position from its image's constants; see \
                 {DOC}#a-frames-position"
            ),
        );
        return -1;
    }
    position
}

/// Fills `fault` with what the last poll stopped on.
///
/// # Safety
///
/// `log` is null or a live handle; `fault` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_fault(
    log: *const cavalry_committed_log_t,
    fault: *mut cavalry_fault_t,
) -> c_int {
    if log.is_null() {
        error::null("cavalry_committed_log_fault", "log");
        return -1;
    }
    if fault.is_null() {
        error::null("cavalry_committed_log_fault", "fault");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(log) } == State::Polling {
        polling("cavalry_committed_log_fault");
        return -1;
    }
    // SAFETY: the fault field alone, and a writable struct.
    unsafe { *fault = ptr::addr_of!((*log).fault).read() };
    0
}

/// Reads through the log when no poll runs, or sets the error.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call.
unsafe fn read<T>(
    function: &str,
    log: *const cavalry_committed_log_t,
    refused: T,
    run: impl FnOnce(&CommittedLog) -> T,
) -> T {
    if log.is_null() {
        error::null(function, "log");
        return refused;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(log) } == State::Polling {
        polling(function);
        return refused;
    }
    // SAFETY: no poll borrows the log.
    run(unsafe { &*ptr::addr_of!((*log).log) })
}

/// The position after the last entry delivered, or the position the log was opened at
/// before the first.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_position(
    log: *const cavalry_committed_log_t,
) -> i64 {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_committed_log_position",
            log,
            -1,
            CommittedLog::position,
        )
    }
}

/// The bound of the last poll: the highest commit position taken from the member's
/// counter, 0 before the first poll.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_commit_position(
    log: *const cavalry_committed_log_t,
) -> i64 {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_committed_log_commit_position",
            log,
            -1,
            CommittedLog::commit_position,
        )
    }
}

/// Whether the handle has come to its end: no poll will deliver another entry. What
/// an ended handle has delivered is said by `cavalry_committed_log_failed_at`,
/// `cavalry_committed_log_withdrawn_at` and `cavalry_committed_log_faulted`, read
/// after this returns `true`: it has delivered every entry it was owed only when all
/// three return `false`. In every case what it delivered ends at
/// `cavalry_committed_log_position`.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_is_ended(
    log: *const cavalry_committed_log_t,
) -> bool {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_committed_log_is_ended",
            log,
            false,
            CommittedLog::is_ended,
        )
    }
}

/// Whether the driver's replay of the member's recording failed; when it did, the
/// position after the last frame it played is written through `position`, and no
/// entry from that position on is delivered. The entries below it may still be on
/// their way when this first returns `true`: read it once
/// `cavalry_committed_log_is_ended` does. A null `position` is refused as any null
/// required pointer is, with `CAVALRY_ERR_NULL` and `false`.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call; `position` is null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_failed_at(
    log: *const cavalry_committed_log_t,
    position: *mut i64,
) -> bool {
    if log.is_null() {
        error::null("cavalry_committed_log_failed_at", "log");
        return false;
    }
    if position.is_null() {
        error::null("cavalry_committed_log_failed_at", "position");
        return false;
    }
    // SAFETY: the caller's convention above.
    let failed = unsafe {
        read(
            "cavalry_committed_log_failed_at",
            log,
            None,
            CommittedLog::failed_at,
        )
    };
    let Some(at) = failed else {
        return false;
    };
    // SAFETY: `position` is non-null, checked above, and writable, by the caller's
    // convention.
    unsafe { *position = at };
    true
}

/// Whether the driver withdrew the replay's image before the polls had read it to
/// its end; when it did, the position at which the entries the handle was not given
/// begin is written through `position`. The driver keeps a replay's image for the
/// publication linger, 5 s, after the replay ends, and then withdraws it, read or
/// not: a service that did not poll for that long after its member closed or left
/// the log ends so. The entries from that position on were played and not
/// delivered. A replay that failed is withdrawn so too, and both functions then
/// return `true`, with this position the lower. Read it once
/// `cavalry_committed_log_is_ended` returns `true`. A null `position` is refused as
/// any null required pointer is, with `CAVALRY_ERR_NULL` and `false`.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call; `position` is null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_withdrawn_at(
    log: *const cavalry_committed_log_t,
    position: *mut i64,
) -> bool {
    if log.is_null() {
        error::null("cavalry_committed_log_withdrawn_at", "log");
        return false;
    }
    if position.is_null() {
        error::null("cavalry_committed_log_withdrawn_at", "position");
        return false;
    }
    // SAFETY: the caller's convention above.
    let withdrawn = unsafe {
        read(
            "cavalry_committed_log_withdrawn_at",
            log,
            None,
            CommittedLog::withdrawn_at,
        )
    };
    let Some(at) = withdrawn else {
        return false;
    };
    // SAFETY: `position` is non-null, checked above, and writable, by the caller's
    // convention.
    unsafe { *position = at };
    true
}

/// Whether the replay's image, the log's own, faulted; when it did, the fault is
/// written through `fault` as `cavalry_committed_log_fault` wrote it for the poll that
/// met it: the code, `CAVALRY_ERR_FAULT_IMAGE` or `CAVALRY_ERR_FAULT_IMAGES_FULL`, the
/// image's session id, and the reader's position when the check failed, or
/// `CAVALRY_NULL_POSITION` where the fault has none. The poll that meets the fault
/// returns −1 once and later polls return 0; this answers from that poll on, for the
/// life of the handle, so a service that did not keep the poll's answer still tells
/// this end from a whole one (ADR 0080). `false`, with nothing written, for a log
/// whose image has not faulted. Read it once `cavalry_committed_log_is_ended` returns
/// `true`. A null `fault` is refused as any null required pointer is, with
/// `CAVALRY_ERR_NULL` and `false`. An answer sets no error.
///
/// # Safety
///
/// `log` is null or a live handle not in use by another call; `fault` is null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_faulted(
    log: *const cavalry_committed_log_t,
    fault: *mut cavalry_fault_t,
) -> bool {
    if log.is_null() {
        error::null("cavalry_committed_log_faulted", "log");
        return false;
    }
    if fault.is_null() {
        error::null("cavalry_committed_log_faulted", "fault");
        return false;
    }
    // SAFETY: the caller's convention above.
    let faulted = unsafe {
        read(
            "cavalry_committed_log_faulted",
            log,
            None,
            CommittedLog::faulted,
        )
    };
    let Some(kept) = faulted else {
        return false;
    };
    // SAFETY: `fault` is non-null, checked above, and writable, by the caller's
    // convention.
    unsafe { *fault = fault_fields(&kept) };
    true
}

/// Fills `constants` from the handle's copy.
///
/// # Safety
///
/// `log` is null or a live handle; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_constants(
    log: *const cavalry_committed_log_t,
    constants: *mut cavalry_committed_log_constants_t,
) -> c_int {
    if log.is_null() {
        error::null("cavalry_committed_log_constants", "log");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_committed_log_constants", "constants");
        return -1;
    }
    // SAFETY: a live handle and a writable struct, by the caller's conventions; the
    // read touches the constants field alone, which a running poll does not borrow.
    unsafe { *constants = ptr::addr_of!((*log).constants).read() };
    0
}

/// Stops the replay if it has not ended, closes the subscription and frees the
/// handle. With a null `client` it closes the subscription and frees the handle
/// alone, and returns 0: the driver ends the replay by itself once the replay connect
/// timeout has passed with no subscriber, which is how a log is closed after its
/// client's `cavalry_close`. The handle is freed whatever the function returns, with
/// one exception: a call from inside the log's own handler is refused with
/// `CAVALRY_ERR_POLLING` and frees nothing.
///
/// # Safety
///
/// `log` is null or a live handle, closed once; `client` is null or a live client
/// handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_committed_log_close(
    log: *mut cavalry_committed_log_t,
    client: *mut cavalry_t,
) -> c_int {
    if log.is_null() {
        error::null("cavalry_committed_log_close", "log");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(log) } == State::Polling {
        polling("cavalry_committed_log_close");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention; the log is moved
    // out, so its own close below consumes it.
    let closed = unsafe { handle::take_out(log) };
    if client.is_null() {
        // Dropping the log closes its subscription; the driver ends the replay once
        // the replay connect timeout has passed with no subscriber.
        drop(closed);
        return 0;
    }
    // SAFETY: `client` is non-null, checked above, and a live handle used from one
    // thread at a time ("Threads").
    let client = unsafe { cavalry_mut(client) };
    match closed.log.close(client) {
        Ok(()) => 0,
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}
