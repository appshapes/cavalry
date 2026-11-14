//! A cluster session's handle: its connect, its offer, its poll with the C fragment
//! handler, a reply's values, its reads and its close (`spec/c-abi.md`, "Functions",
//! cluster session, "Calls during a claim or a poll", "What a header carries"; ADR
//! 0122). The Rust core is `cavalry_client::ClusterSession` (`spec/cluster.md`,
//! "Sessions"; ADR 0117, ADR 0121).

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use cavalry_client::ClusterSession;

use crate::client::{cavalry_mut, cavalry_t};
use crate::codes::*;
use crate::error::{self, DOC};
use crate::handle;
use crate::strings;
use crate::subscription::{
    Carries, NO_FAULT, cavalry_fault_t, cavalry_fragment_handler_t, cavalry_header_t, fault_struct,
};

/// The longest reason a closed session gives: what `ClusterSession::closed` keeps of
/// the cluster's detail, 32 bytes.
const REASON_LENGTH: usize = 32;

/// Whether a poll runs: the state word every function of the handle reads first.
// not a state machine: the word records whether the caller's poll is running, a borrow the page replaces by convention, and no counter counts a caller's polls
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Polling,
}

/// A client's session with a cluster; made by `cavalry_connect_cluster`, freed by
/// `cavalry_cluster_session_close`.
pub struct cavalry_cluster_session_t {
    state: State,
    fault: cavalry_fault_t,
    /// Copied out of the session when the handle is made, so that
    /// `cavalry_cluster_session_constants` answers from inside the handler without
    /// reaching the session the running poll borrows.
    constants: cavalry_cluster_session_constants_t,
    /// Whether a poll has found the session closed. A poll is what closes a session,
    /// so the poll that does so writes the reason below, once, and nothing writes it
    /// again: the pointer `cavalry_cluster_session_closed` returns is valid for the
    /// handle's life.
    closed: bool,
    /// Why the session closed, NUL-terminated; empty while it is open.
    reason: [u8; REASON_LENGTH + 1],
    session: ClusterSession,
}

/// A cluster session's constants.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct cavalry_cluster_session_constants_t {
    /// The id the leader gave the session.
    pub session_id: i64,
    /// The longest message `cavalry_cluster_session_offer` takes: what one frame
    /// carries after the session message header.
    pub max_message_length: usize,
}

/// What a reply carries before its payload: what `cavalry_cluster_reply_values` fills.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct cavalry_cluster_reply_values_t {
    /// The leadership term id of the entry the reply answers. The terms of the
    /// replies a session's handler is given never fall.
    pub leadership_term_id: i64,
    /// The cluster's time when the leader appended that entry, milliseconds since
    /// 1970-01-01T00:00:00Z.
    pub timestamp: i64,
}

/// Reads the state word through the handle's pointer, without a reference to the
/// wrapped session.
///
/// # Safety
///
/// `session` is a live handle.
unsafe fn state(session: *const cavalry_cluster_session_t) -> State {
    // SAFETY: a live handle; the read touches the state field alone, which a running
    // poll does not borrow.
    unsafe { ptr::addr_of!((*session).state).read() }
}

fn polling(function: &str) {
    error::set(
        CAVALRY_ERR_POLLING,
        format_args!(
            "{function} refused (polling): the call came from inside this cluster session's \
             own fragment handler, while its poll runs; make it after the poll returns; see \
             {DOC}#calls-during-a-claim-or-a-poll"
        ),
    );
}

/// Opens a session with a cluster, as the Rust `Cavalry::connect_cluster` does: a
/// subscription on `response_channel`, an exclusive publication to the `ingress`
/// endpoint of every member in `members`, a session connect to one member after
/// another, a redirect followed, and the session's counter once the leader has
/// opened the session. It blocks until then, or for 20 s. 0 with `*session` the
/// handle; −1 with `*session` null and the error: `CAVALRY_ERR_CLUSTER_MEMBERS`,
/// `_REFUSED`, `_VERSION` or `_CONNECT_TIMEOUT`, a fault's code, or the client's or
/// the driver's code (`spec/c-abi.md`; ADR 0122).
///
/// # Safety
///
/// `session` is null or writable; `client` is null or a live client handle not in use
/// by another call; `members` and `response_channel` are each null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_connect_cluster(
    session: *mut *mut cavalry_cluster_session_t,
    client: *mut cavalry_t,
    members: *const c_char,
    response_channel: *const c_char,
) -> c_int {
    if session.is_null() {
        error::null("cavalry_connect_cluster", "the handle to fill");
        return -1;
    }
    // SAFETY: `session` is non-null, checked above, and writable, by the caller's
    // convention. Written first, so that every return below leaves a null handle
    // unless it gives one.
    unsafe { *session = ptr::null_mut() };
    if client.is_null() {
        error::null("cavalry_connect_cluster", "client");
        return -1;
    }
    if members.is_null() {
        error::null("cavalry_connect_cluster", "members");
        return -1;
    }
    if response_channel.is_null() {
        error::null("cavalry_connect_cluster", "response_channel");
        return -1;
    }
    // SAFETY: both are non-null, checked above, and NUL-terminated, by the caller's
    // convention.
    let (Some(members), Some(response_channel)) = (unsafe {
        (
            strings::utf8(
                "cavalry_connect_cluster",
                "the members string",
                "the string the cluster's members were started with",
                members,
            ),
            strings::utf8(
                "cavalry_connect_cluster",
                "the response channel",
                "cavalry:udp?endpoint=<host:port>, a port of this client's host",
                response_channel,
            ),
        )
    }) else {
        return -1;
    };
    // SAFETY: `client` is non-null, checked above, and a live handle used from one
    // thread at a time ("Threads").
    let client = unsafe { cavalry_mut(client) };
    match client.connect_cluster(members, response_channel) {
        Ok(opened) => {
            let constants = cavalry_cluster_session_constants_t {
                session_id: opened.session_id(),
                max_message_length: opened.max_message_length(),
            };
            // SAFETY: `session` is non-null and writable.
            unsafe {
                *session = handle::give(cavalry_cluster_session_t {
                    state: State::Idle,
                    fault: NO_FAULT,
                    constants,
                    closed: false,
                    reason: [0; REASON_LENGTH + 1],
                    session: opened,
                });
            }
            0
        }
        Err(error) => {
            error::set_cluster(&error);
            -1
        }
    }
}

/// Sends the leader the `length` bytes at `buffer` as a session message, and returns
/// the position after it on the session's publication to the leader, or a result
/// code, `CAVALRY_PUBLICATION_*`: `_ERROR` with `CAVALRY_ERR_TOO_LONG` for a message
/// longer than the constants' `max_message_length`, and `_CLOSED` for a session that
/// is closed. A message taken is on its way to the leader, not committed. Nothing
/// here allocates, and nothing reads a clock.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call; `buffer` is null
/// with a `length` of 0, or readable for `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_offer(
    session: *mut cavalry_cluster_session_t,
    buffer: *const u8,
    length: usize,
) -> i64 {
    if session.is_null() {
        error::null("cavalry_cluster_session_offer", "session");
        return CAVALRY_PUBLICATION_ERROR;
    }
    if buffer.is_null() && length > 0 {
        error::null("cavalry_cluster_session_offer", "buffer");
        return CAVALRY_PUBLICATION_ERROR;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(session) } == State::Polling {
        polling("cavalry_cluster_session_offer");
        return CAVALRY_PUBLICATION_ERROR;
    }
    let message: &[u8] = if length == 0 {
        &[]
    } else {
        // SAFETY: readable for `length` bytes, by the caller's convention, during the
        // call.
        unsafe { std::slice::from_raw_parts(buffer, length) }
    };
    // SAFETY: the state is Idle, so no poll borrows the session, and the handle is
    // used from one thread at a time: this is the one reference to it.
    let inner = unsafe { &mut *ptr::addr_of_mut!((*session).session) };
    match inner.offer(message) {
        Ok(position) => position,
        Err(offer) => error::offer_code(&offer),
    }
}

/// Reads up to `fragment_limit` fragments of the response channel and hands `handler`
/// each reply that is not stale, as its payload, with `clientd`; returns the replies
/// handed over, or −1 with the fault as the error and in
/// `cavalry_cluster_session_fault`. The header carries the reply's leadership term id
/// and the cluster's time, which `cavalry_cluster_reply_values` fills from inside the
/// handler. A stale reply is passed over and counted, and is not in the count
/// returned. The poll is the session's duty cycle too: call it at least every 500 ms.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call; `handler` is null or
/// a function that returns, without unwinding or a `longjmp`, and holds the rules of
/// "Calls during a claim or a poll".
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_poll(
    session: *mut cavalry_cluster_session_t,
    handler: cavalry_fragment_handler_t,
    clientd: *mut c_void,
    fragment_limit: usize,
) -> c_int {
    if session.is_null() {
        error::null("cavalry_cluster_session_poll", "session");
        return -1;
    }
    let Some(handler) = handler else {
        error::null("cavalry_cluster_session_poll", "handler");
        return -1;
    };
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(session) } == State::Polling {
        polling("cavalry_cluster_session_poll");
        return -1;
    }
    let limit = fragment_limit.min(c_int::MAX as usize);
    // SAFETY: the state is Idle, so no poll borrows the session, and the handle is used
    // from one thread at a time: this is the one reference to it. The state word is
    // written through its own field so that a re-entrant call from the handler reads
    // it without touching the borrowed session. The reason is written through its own
    // fields after the poll has returned, once, by the poll that found the session
    // closed; nothing reads it before `closed` is set.
    let result = unsafe {
        ptr::addr_of_mut!((*session).state).write(State::Polling);
        let inner = &mut *ptr::addr_of_mut!((*session).session);
        let result = inner.poll(
            |payload, reply| {
                let view = cavalry_header_t {
                    carries: Carries::Reply(*reply),
                };
                // The caller's handler, by the convention above, inside the enclosing
                // unsafe block; the payload and the header are valid for the call.
                handler(clientd, payload.as_ptr(), payload.len(), &view);
            },
            limit,
        );
        ptr::addr_of_mut!((*session).state).write(State::Idle);
        if !ptr::addr_of!((*session).closed).read()
            && let Some(reason) = inner.closed()
        {
            // A C string ends at its first NUL, so a detail that held one is cut
            // there. No allocation: the bytes go into the handle's own array, whose
            // last byte stays 0.
            let bytes = reason.as_bytes();
            let length = bytes
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(bytes.len())
                .min(REASON_LENGTH);
            let kept = &mut *ptr::addr_of_mut!((*session).reason);
            if let (Some(kept), Some(bytes)) = (kept.get_mut(..length), bytes.get(..length)) {
                kept.copy_from_slice(bytes);
            }
            ptr::addr_of_mut!((*session).closed).write(true);
        }
        result
    };
    match result {
        Ok(delivered) => {
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*session).fault).write(NO_FAULT) };
            delivered as c_int
        }
        Err(fault) => {
            let fault = fault_struct(&fault);
            // SAFETY: the fault field alone.
            unsafe { ptr::addr_of_mut!((*session).fault).write(fault) };
            -1
        }
    }
}

/// Fills `values` with the leadership term id and the cluster's time of the reply
/// whose header `cavalry_cluster_session_poll` handed its handler; −1 with
/// `CAVALRY_ERR_WRONG_HEADER` for a header from `cavalry_subscription_poll` or from
/// `cavalry_committed_log_poll`, which carries a frame's header and no reply's values.
///
/// # Safety
///
/// `header` is null or the header a poll handed the handler, during that call;
/// `values` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_reply_values(
    header: *const cavalry_header_t,
    values: *mut cavalry_cluster_reply_values_t,
) -> c_int {
    if header.is_null() {
        error::null("cavalry_cluster_reply_values", "header");
        return -1;
    }
    if values.is_null() {
        error::null("cavalry_cluster_reply_values", "values");
        return -1;
    }
    // SAFETY: the handler's header, valid for the call.
    let Carries::Reply(reply) = (unsafe { (*header).carries }) else {
        error::set(
            CAVALRY_ERR_WRONG_HEADER,
            format_args!(
                "cavalry_cluster_reply_values refused (wrong-header): the header came from \
                 another poll than cavalry_cluster_session_poll and carries no reply's \
                 values; read a frame's fields with cavalry_header_values; see \
                 {DOC}#what-a-header-carries"
            ),
        );
        return -1;
    };
    // SAFETY: a writable struct, by the caller's convention.
    unsafe {
        *values = cavalry_cluster_reply_values_t {
            leadership_term_id: reply.leadership_term_id,
            timestamp: reply.timestamp,
        };
    }
    0
}

/// Fills `fault` with what the last poll stopped on.
///
/// # Safety
///
/// `session` is null or a live handle; `fault` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_fault(
    session: *const cavalry_cluster_session_t,
    fault: *mut cavalry_fault_t,
) -> c_int {
    if session.is_null() {
        error::null("cavalry_cluster_session_fault", "session");
        return -1;
    }
    if fault.is_null() {
        error::null("cavalry_cluster_session_fault", "fault");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(session) } == State::Polling {
        polling("cavalry_cluster_session_fault");
        return -1;
    }
    // SAFETY: the fault field alone, and a writable struct.
    unsafe { *fault = ptr::addr_of!((*session).fault).read() };
    0
}

/// Reads through the session when no poll runs, or sets the error.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call.
unsafe fn read<T>(
    function: &str,
    session: *const cavalry_cluster_session_t,
    refused: T,
    run: impl FnOnce(&ClusterSession) -> T,
) -> T {
    if session.is_null() {
        error::null(function, "session");
        return refused;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(session) } == State::Polling {
        polling(function);
        return refused;
    }
    // SAFETY: no poll borrows the session.
    run(unsafe { &*ptr::addr_of!((*session).session) })
}

/// The leadership term the session sends with: the term of the leader that opened it,
/// or of the last new leader event.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_leadership_term_id(
    session: *const cavalry_cluster_session_t,
) -> i64 {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_cluster_session_leadership_term_id",
            session,
            -1,
            ClusterSession::leadership_term_id,
        )
    }
}

/// The id of the member the session sends to: the leader, as the cluster last said.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_leader_member_id(
    session: *const cavalry_cluster_session_t,
) -> i32 {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_cluster_session_leader_member_id",
            session,
            -1,
            ClusterSession::leader_member_id,
        )
    }
}

/// How many stale replies the session's polls have passed over: replies whose
/// leadership term id was below that of a reply the session had handed over. It is
/// the value of the session's `cluster.replies_stale`.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_stale_replies(
    session: *const cavalry_cluster_session_t,
) -> i64 {
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_cluster_session_stale_replies",
            session,
            -1,
            ClusterSession::stale_replies,
        )
    }
}

/// Why the session is closed, a NUL-terminated string valid for the handle's life:
/// `TIMEOUT` when the leader closed the session for hearing nothing from it for the
/// session timeout, `NO LEADER` when its leader was lost and no member took its
/// place, or the detail of the cluster's closed event, at most its first 32 bytes.
/// Null, with no error set, while the session is open: that is an answer. A poll is
/// what closes a session, so the answer changes only across one. Null with the
/// error set for a null handle, or for a call from inside the session's own handler.
///
/// # Safety
///
/// `session` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_closed(
    session: *const cavalry_cluster_session_t,
) -> *const c_char {
    if session.is_null() {
        error::null("cavalry_cluster_session_closed", "session");
        return ptr::null();
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(session) } == State::Polling {
        polling("cavalry_cluster_session_closed");
        return ptr::null();
    }
    // SAFETY: the closed field alone, of a live handle no poll runs on.
    if !unsafe { ptr::addr_of!((*session).closed).read() } {
        return ptr::null();
    }
    // SAFETY: the string is the handle's own array, written once before `closed` was
    // set and NUL-terminated, so the pointer is valid until the handle is freed.
    unsafe { ptr::addr_of!((*session).reason).cast::<c_char>() }
}

/// Fills `constants` from the handle's copy.
///
/// # Safety
///
/// `session` is null or a live handle; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_constants(
    session: *const cavalry_cluster_session_t,
    constants: *mut cavalry_cluster_session_constants_t,
) -> c_int {
    if session.is_null() {
        error::null("cavalry_cluster_session_constants", "session");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_cluster_session_constants", "constants");
        return -1;
    }
    // SAFETY: a live handle and a writable struct, by the caller's conventions; the
    // read touches the constants field alone, which a running poll does not borrow.
    unsafe { *constants = ptr::addr_of!((*session).constants).read() };
    0
}

/// Offers the leader a session close for at most 500 ms, unless the session is closed
/// already, then closes the session's publications, its subscription and its counter,
/// and frees the handle. The handle is freed whatever the function returns, with one
/// exception: a call from inside the session's own handler is refused with
/// `CAVALRY_ERR_POLLING`, sends nothing and frees nothing.
///
/// # Safety
///
/// `session` is null or a live handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_cluster_session_close(
    session: *mut cavalry_cluster_session_t,
) -> c_int {
    if session.is_null() {
        error::null("cavalry_cluster_session_close", "session");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(session) } == State::Polling {
        polling("cavalry_cluster_session_close");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention; the session is
    // moved out, so its own close below consumes it.
    let closed = unsafe { handle::take_out(session) };
    match closed.session.close() {
        Ok(()) => 0,
        Err(error) => {
            error::set_cluster(&error);
            -1
        }
    }
}
