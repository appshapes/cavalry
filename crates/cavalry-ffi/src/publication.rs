//! A publication's handle, its constants, the one claim it holds at a time and the
//! struct that names it (`spec/c-abi.md`, "Structs", "Functions", publication,
//! "Calls during a claim or a poll", "Which claim a struct names").

use std::ffi::{CString, c_char, c_int};
use std::ptr;

use cavalry_client::{BufferClaim, Publication};

use crate::codes::*;
use crate::error::{self, DOC};
use crate::handle;
use crate::strings;

/// Whether a claim is outstanding on the publication: the state word every function
/// of the handle reads first, through the handle's pointer and without a reference to
/// the wrapped publication, so a refusal never touches the borrowed value.
// not a state machine: the word records whether the caller holds a claim, a borrow the page replaces by convention, and no counter counts a caller's claims
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Claimed { position_after: i64 },
}

/// The publication's constants, copied out of it when the handle is made, so that
/// `cavalry_publication_constants` answers during a claim without reaching the
/// publication the claim borrows.
#[derive(Clone, Copy)]
struct Constants {
    registration_id: i64,
    session_id: i32,
    stream_id: i32,
    max_payload_length: usize,
    max_message_length: usize,
    is_exclusive: bool,
}

/// A publication, concurrent or exclusive; made by `cavalry_add_publication` or
/// `cavalry_add_exclusive_publication`, freed by `cavalry_publication_close`.
pub struct cavalry_publication_t {
    state: State,
    channel: CString,
    constants: Constants,
    /// The claim outstanding, if any, borrowed from `publication` below; dropped first,
    /// which aborts it, since fields drop in declaration order.
    claim: Option<BufferClaim<'static>>,
    publication: Publication,
}

/// The struct `cavalry_publication_try_claim` fills: the body to write, then
/// `cavalry_buffer_claim_commit` or `_abort`.
#[repr(C)]
pub struct cavalry_buffer_claim_t {
    /// The frame's body in the term; write the message here.
    pub data: *mut u8,
    /// The length claimed.
    pub length: usize,
    /// The position after the frame, which the claim's commit returns.
    pub position_after: i64,
    /// The publication the claim belongs to.
    pub publication: *mut cavalry_publication_t,
}

/// A publication's constants, the strings valid for the handle's life.
#[repr(C)]
pub struct cavalry_publication_constants_t {
    /// The channel.
    pub channel: *const c_char,
    /// The id under which the driver knows the publication.
    pub registration_id: i64,
    /// The stream's session id.
    pub session_id: i32,
    /// The stream id.
    pub stream_id: i32,
    /// The longest body a claim may ask for.
    pub max_payload_length: usize,
    /// The longest message an offer may ask for, fragmented.
    pub max_message_length: usize,
    /// Whether the publication is exclusive.
    pub is_exclusive: bool,
}

pub(crate) fn publication_handle(publication: Publication) -> *mut cavalry_publication_t {
    handle::give(cavalry_publication_t {
        state: State::Idle,
        channel: strings::c_string(publication.channel()),
        constants: Constants {
            registration_id: publication.registration_id(),
            session_id: publication.session_id(),
            stream_id: publication.stream_id(),
            max_payload_length: publication.max_payload_length(),
            max_message_length: publication.max_message_length(),
            is_exclusive: publication.is_exclusive(),
        },
        claim: None,
        publication,
    })
}

/// Reads the state word through the handle's pointer, without a reference to the
/// wrapped publication.
///
/// # Safety
///
/// `publication` is a live handle.
unsafe fn state(publication: *const cavalry_publication_t) -> State {
    // SAFETY: a live handle; the read touches the state field alone, which no claim
    // borrows.
    unsafe { ptr::addr_of!((*publication).state).read() }
}

fn claim_outstanding(function: &str) {
    error::set(
        CAVALRY_ERR_CLAIM_OUTSTANDING,
        format_args!(
            "{function} refused (claim-outstanding): a claim is outstanding on this publication; \
             commit or abort it with its struct first; see {DOC}#calls-during-a-claim-or-a-poll"
        ),
    );
}

fn no_claim(function: &str) {
    error::set(
        CAVALRY_ERR_NO_CLAIM,
        format_args!(
            "{function} refused (no-claim): the struct names no claim outstanding on its \
             publication; a claim is committed or aborted once, with the struct its try_claim \
             filled; see {DOC}#which-claim-a-struct-names"
        ),
    );
}

/// Writes the fields a refused `try_claim` or a released claim leaves.
///
/// # Safety
///
/// `claim` is non-null and writable.
unsafe fn clear(claim: *mut cavalry_buffer_claim_t) {
    // SAFETY: non-null and writable, by the caller's convention.
    unsafe {
        *claim = cavalry_buffer_claim_t {
            data: ptr::null_mut(),
            length: 0,
            position_after: CAVALRY_NULL_POSITION,
            publication: ptr::null_mut(),
        };
    }
}

/// Claims `length` bytes in place: fills `claim` and returns the position after the
/// frame, or a result code, `CAVALRY_PUBLICATION_*`. One claim is outstanding on a
/// publication at a time; a second before the first's commit or abort is
/// `CAVALRY_PUBLICATION_ERROR` with `CAVALRY_ERR_CLAIM_OUTSTANDING`.
///
/// # Safety
///
/// `publication` is null or a live handle not in use by another call; `claim` is null
/// or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_try_claim(
    publication: *mut cavalry_publication_t,
    length: usize,
    claim: *mut cavalry_buffer_claim_t,
) -> i64 {
    if publication.is_null() {
        error::null("cavalry_publication_try_claim", "publication");
        return CAVALRY_PUBLICATION_ERROR;
    }
    if claim.is_null() {
        error::null("cavalry_publication_try_claim", "claim");
        return CAVALRY_PUBLICATION_ERROR;
    }
    // SAFETY: a live handle, by the caller's convention.
    if let State::Claimed { position_after } = unsafe { state(publication) } {
        claim_outstanding("cavalry_publication_try_claim");
        // SAFETY: `claim` is non-null and writable.
        let names_it = unsafe {
            (*claim).publication == publication && (*claim).position_after == position_after
        };
        if !names_it {
            // SAFETY: non-null and writable.
            unsafe { clear(claim) };
        }
        return CAVALRY_PUBLICATION_ERROR;
    }
    // SAFETY: the state is Idle, so no claim borrows the publication, and the handle is
    // used from one thread at a time ("Threads"): this is the one reference to it.
    let inner = unsafe { &mut *ptr::addr_of_mut!((*publication).publication) };
    match inner.try_claim(length) {
        Ok(mut made) => {
            let position_after = made.position_after();
            let body = made.body();
            let data = body.as_mut_ptr();
            let body_length = body.len();
            // SAFETY: the claim borrows `inner`, a field of the handle it is stored in,
            // which outlives it: the claim is dropped at its commit or abort, or before
            // the publication at the handle's close (field order), and while it is
            // stored the state word refuses every other use of the publication.
            let kept: BufferClaim<'static> = unsafe { std::mem::transmute(made) };
            // SAFETY: the state and claim fields alone, which nothing borrows.
            unsafe {
                ptr::addr_of_mut!((*publication).claim).write(Some(kept));
                ptr::addr_of_mut!((*publication).state).write(State::Claimed { position_after });
                *claim = cavalry_buffer_claim_t {
                    data,
                    length: body_length,
                    position_after,
                    publication,
                };
            }
            position_after
        }
        Err(offer) => {
            // SAFETY: non-null and writable.
            unsafe { clear(claim) };
            error::offer_code(&offer)
        }
    }
}

/// The outstanding claim the struct names, or `None` with the error set.
///
/// # Safety
///
/// `claim` is non-null and readable; its `publication`, when not null, is a live
/// handle not in use by another call.
unsafe fn named<'a>(
    function: &str,
    claim: *mut cavalry_buffer_claim_t,
) -> Option<&'a mut BufferClaim<'static>> {
    // SAFETY: non-null and readable, by the caller's convention.
    let (publication, position_after) = unsafe { ((*claim).publication, (*claim).position_after) };
    if publication.is_null() {
        no_claim(function);
        return None;
    }
    // SAFETY: a live handle, by the caller's convention.
    match unsafe { state(publication) } {
        State::Claimed {
            position_after: outstanding,
        } if outstanding == position_after => {
            // SAFETY: the state says a claim is stored, and the claim field is the one
            // this call touches.
            unsafe { (*ptr::addr_of_mut!((*publication).claim)).as_mut() }
        }
        _ => {
            no_claim(function);
            None
        }
    }
}

/// Releases the handle's claim after its commit or abort.
///
/// # Safety
///
/// `publication` is the live handle whose claim was just committed or aborted.
unsafe fn release(publication: *mut cavalry_publication_t, claim: *mut cavalry_buffer_claim_t) {
    // SAFETY: the state and claim fields alone; the claim is taken out and dropped
    // after its commit or abort consumed it, so nothing is dropped twice.
    unsafe {
        ptr::addr_of_mut!((*publication).state).write(State::Idle);
        #[cfg(debug_assertions)]
        clear(claim);
        #[cfg(not(debug_assertions))]
        let _ = claim;
    }
}

/// Sets the reserved value the frame's header carries to the subscriber.
///
/// # Safety
///
/// `claim` is null or a struct `cavalry_publication_try_claim` filled, whose
/// publication is a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_buffer_claim_set_reserved_value(
    claim: *mut cavalry_buffer_claim_t,
    value: i64,
) -> c_int {
    if claim.is_null() {
        error::null("cavalry_buffer_claim_set_reserved_value", "claim");
        return -1;
    }
    // SAFETY: the caller's conventions above.
    let Some(kept) = (unsafe { named("cavalry_buffer_claim_set_reserved_value", claim) }) else {
        return -1;
    };
    kept.set_reserved_value(value);
    0
}

/// Commits the claim: publishes the frame, returns the position after it, and
/// releases the struct.
///
/// # Safety
///
/// As for `cavalry_buffer_claim_set_reserved_value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_buffer_claim_commit(claim: *mut cavalry_buffer_claim_t) -> i64 {
    if claim.is_null() {
        error::null("cavalry_buffer_claim_commit", "claim");
        return -1;
    }
    // SAFETY: the caller's conventions above.
    if unsafe { named("cavalry_buffer_claim_commit", claim) }.is_none() {
        return -1;
    }
    // SAFETY: `named` found the struct's publication live with this claim stored.
    unsafe {
        let publication = (*claim).publication;
        let kept = (*ptr::addr_of_mut!((*publication).claim)).take();
        let position = kept.map_or(-1, BufferClaim::commit);
        release(publication, claim);
        position
    }
}

/// Aborts the claim: the frame becomes padding a reader skips, and the struct is
/// released.
///
/// # Safety
///
/// As for `cavalry_buffer_claim_set_reserved_value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_buffer_claim_abort(claim: *mut cavalry_buffer_claim_t) -> i64 {
    if claim.is_null() {
        error::null("cavalry_buffer_claim_abort", "claim");
        return -1;
    }
    // SAFETY: the caller's conventions above.
    if unsafe { named("cavalry_buffer_claim_abort", claim) }.is_none() {
        return -1;
    }
    // SAFETY: as in `cavalry_buffer_claim_commit`.
    unsafe {
        let publication = (*claim).publication;
        let kept = (*ptr::addr_of_mut!((*publication).claim)).take();
        let position = kept.map_or(-1, BufferClaim::abort);
        release(publication, claim);
        position
    }
}

/// Offers `length` bytes from `buffer`, copied into the log and fragmented when
/// longer than the maximum payload; returns the position after the message or a
/// result code.
///
/// # Safety
///
/// `publication` is null or a live handle not in use by another call; `buffer` is
/// null or readable for `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_offer(
    publication: *mut cavalry_publication_t,
    buffer: *const u8,
    length: usize,
) -> i64 {
    if publication.is_null() {
        error::null("cavalry_publication_offer", "publication");
        return CAVALRY_PUBLICATION_ERROR;
    }
    if buffer.is_null() && length > 0 {
        error::null("cavalry_publication_offer", "buffer");
        return CAVALRY_PUBLICATION_ERROR;
    }
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(publication) } != State::Idle {
        claim_outstanding("cavalry_publication_offer");
        return CAVALRY_PUBLICATION_ERROR;
    }
    let message: &[u8] = if length == 0 {
        &[]
    } else {
        // SAFETY: readable for `length` bytes, by the caller's convention, during the
        // call.
        unsafe { std::slice::from_raw_parts(buffer, length) }
    };
    // SAFETY: the state is Idle, so no claim borrows the publication.
    let inner = unsafe { &mut *ptr::addr_of_mut!((*publication).publication) };
    match inner.offer(message) {
        Ok(position) => position,
        Err(offer) => error::offer_code(&offer),
    }
}

/// Reads through the publication when no claim is outstanding, or sets the error.
///
/// # Safety
///
/// `publication` is a live handle not in use by another call.
unsafe fn read<T>(
    function: &str,
    publication: *const cavalry_publication_t,
    refused: T,
    run: impl FnOnce(&Publication) -> T,
) -> T {
    // SAFETY: a live handle, by the caller's convention.
    if unsafe { state(publication) } != State::Idle {
        claim_outstanding(function);
        return refused;
    }
    // SAFETY: the state is Idle, so no claim borrows the publication.
    run(unsafe { &*ptr::addr_of!((*publication).publication) })
}

/// The publication's position, the position after its last committed frame, or −1.
///
/// # Safety
///
/// `publication` is null or a live handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_position(
    publication: *const cavalry_publication_t,
) -> i64 {
    if publication.is_null() {
        error::null("cavalry_publication_position", "publication");
        return -1;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_publication_position",
            publication,
            -1,
            |inner| match inner.position() {
                Ok(position) => position,
                Err(error) => {
                    error::set_client(&error);
                    -1
                }
            },
        )
    }
}

/// The publisher limit the driver maintains.
///
/// # Safety
///
/// As for `cavalry_publication_position`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_position_limit(
    publication: *const cavalry_publication_t,
) -> i64 {
    if publication.is_null() {
        error::null("cavalry_publication_position_limit", "publication");
        return -1;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_publication_position_limit",
            publication,
            -1,
            Publication::limit,
        )
    }
}

/// Whether a subscriber has a position on the stream or a recording is live.
///
/// # Safety
///
/// As for `cavalry_publication_position`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_is_connected(
    publication: *const cavalry_publication_t,
) -> bool {
    if publication.is_null() {
        error::null("cavalry_publication_is_connected", "publication");
        return false;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_publication_is_connected",
            publication,
            false,
            Publication::is_connected,
        )
    }
}

/// Whether the publication's client is closed.
///
/// # Safety
///
/// As for `cavalry_publication_position`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_is_closed(
    publication: *const cavalry_publication_t,
) -> bool {
    if publication.is_null() {
        error::null("cavalry_publication_is_closed", "publication");
        return false;
    }
    // SAFETY: the caller's convention above.
    unsafe {
        read(
            "cavalry_publication_is_closed",
            publication,
            false,
            Publication::is_closed,
        )
    }
}

/// Fills `constants` from the copy the handle took when it was made; allowed while a
/// claim is outstanding.
///
/// # Safety
///
/// `publication` is null or a live handle; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_constants(
    publication: *const cavalry_publication_t,
    constants: *mut cavalry_publication_constants_t,
) -> c_int {
    if publication.is_null() {
        error::null("cavalry_publication_constants", "publication");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_publication_constants", "constants");
        return -1;
    }
    // SAFETY: a live handle and a writable struct, by the caller's conventions. The
    // reads go through the handle's pointer to its `channel` and `constants` fields
    // alone, the copies `publication_handle` took, and never form a reference to the
    // wrapped publication, which an outstanding claim borrows (`cavalry_publication_t`).
    unsafe {
        let channel = (*ptr::addr_of!((*publication).channel)).as_ptr();
        let copy = ptr::addr_of!((*publication).constants).read();
        *constants = cavalry_publication_constants_t {
            channel,
            registration_id: copy.registration_id,
            session_id: copy.session_id,
            stream_id: copy.stream_id,
            max_payload_length: copy.max_payload_length,
            max_message_length: copy.max_message_length,
            is_exclusive: copy.is_exclusive,
        };
    }
    0
}

/// Closes the publication and frees the handle; a claim still outstanding is aborted
/// first.
///
/// # Safety
///
/// `publication` is null or a live handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_publication_close(
    publication: *mut cavalry_publication_t,
) -> c_int {
    if publication.is_null() {
        error::null("cavalry_publication_close", "publication");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention; the claim is
    // dropped, which aborts it, before the publication is closed.
    let handle = unsafe { handle::take_out(publication) };
    let cavalry_publication_t {
        claim, publication, ..
    } = handle;
    drop(claim);
    match publication.close() {
        Ok(()) => 0,
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}
