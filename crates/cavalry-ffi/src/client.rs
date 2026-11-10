//! The client: the attachment to one driver and its conductor thread, made from a
//! context, and the adds and commands it answers (`spec/c-abi.md`, "Functions",
//! client, adds and commands).

use std::ffi::{CString, c_char, c_int};

use cavalry_client::{Cavalry, Closed};

use crate::codes::*;
use crate::context::cavalry_context_t;
use crate::error;
use crate::handle;
use crate::publication::{cavalry_publication_t, publication_handle};
use crate::recording::{cavalry_recording_t, recording_handle};
use crate::replay::{cavalry_replay_t, replay_handle};
use crate::strings;
use crate::subscription::{cavalry_subscription_t, subscription_handle};

/// A client; made by `cavalry_init`, freed by `cavalry_close`.
pub struct cavalry_t {
    client: Cavalry,
    dir_c: CString,
}

/// Attaches to the driver at the context's directory and starts the client conductor,
/// in one call, as the Rust `Cavalry::connect` does. The client keeps its own copy of
/// the directory, so the context may be closed before or after it.
///
/// # Safety
///
/// `client` is null or a writable location; `context` is null or a live context
/// handle not in use by another call ("Threads").
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_init(
    client: *mut *mut cavalry_t,
    context: *mut cavalry_context_t,
) -> c_int {
    if client.is_null() {
        error::null("cavalry_init", "client");
        return -1;
    }
    if context.is_null() {
        error::null("cavalry_init", "context");
        return -1;
    }
    // SAFETY: a live context handle, read during this call, by the caller's convention.
    let context = unsafe { &*context };
    match Cavalry::connect_to(context.dir()) {
        Ok(connected) => {
            let dir_c = context.dir_c().clone();
            // SAFETY: `client` is non-null and writable by the caller's convention.
            unsafe {
                *client = handle::give(cavalry_t {
                    client: connected,
                    dir_c,
                })
            };
            0
        }
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Closes the client and frees the handle: its publications and subscriptions answer
/// `CLOSED` from then and still need their own close.
///
/// # Safety
///
/// `client` is null or a live client handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_close(client: *mut cavalry_t) -> c_int {
    if client.is_null() {
        error::null("cavalry_close", "client");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention ("Lifetimes").
    let handle = unsafe { handle::take_out(client) };
    match handle.client.close() {
        Ok(()) => 0,
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Whether the client is closed, by `cavalry_close` or by what its conductor found.
///
/// # Safety
///
/// `client` is null or a live client handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_is_closed(client: *const cavalry_t) -> bool {
    if client.is_null() {
        error::null("cavalry_is_closed", "client");
        return false;
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*client).client.is_closed() }
}

/// 0 while the client is open, else why it closed, `CAVALRY_CLOSED_*`.
///
/// # Safety
///
/// `client` is null or a live client handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_closed(client: *const cavalry_t) -> i32 {
    if client.is_null() {
        error::null("cavalry_closed", "client");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    match unsafe { (*client).client.closed() } {
        None => 0,
        Some(why) => closed_code(why),
    }
}

pub(crate) const fn closed_code(why: Closed) -> i32 {
    match why {
        Closed::ByUser => CAVALRY_CLOSED_BY_USER,
        Closed::DriverClosed => CAVALRY_CLOSED_DRIVER_CLOSED,
        Closed::DriverTimeout => CAVALRY_CLOSED_DRIVER_TIMEOUT,
        Closed::ClientTimeout => CAVALRY_CLOSED_CLIENT_TIMEOUT,
        Closed::Lapped => CAVALRY_CLOSED_LAPPED,
        Closed::Corrupt => CAVALRY_CLOSED_CORRUPT,
        Closed::Overrun => CAVALRY_CLOSED_OVERRUN,
    }
}

/// The client's id, the one it took from the command ring when it attached.
///
/// # Safety
///
/// `client` is null or a live client handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_client_id(client: *const cavalry_t) -> i64 {
    if client.is_null() {
        error::null("cavalry_client_id", "client");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*client).client.client_id() }
}

/// The driver directory the client attached to, valid for the client's life.
///
/// # Safety
///
/// `client` is null or a live client handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_dir(client: *const cavalry_t) -> *const c_char {
    if client.is_null() {
        error::null("cavalry_dir", "client");
        return std::ptr::null();
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*client).dir_c.as_ptr() }
}

/// The client a handle wraps, for a function of another handle that sends commands
/// through it: a committed log's open and close.
///
/// # Safety
///
/// `client` is a live client handle not in use by another call ("Threads"), so the
/// exclusive reference is the only one for the call it is made for.
pub(crate) unsafe fn cavalry_mut<'a>(client: *mut cavalry_t) -> &'a mut Cavalry {
    // SAFETY: the caller's convention above.
    unsafe { &mut (*client).client }
}

/// Reads the three arguments every add takes, or sets the error and returns `None`.
///
/// # Safety
///
/// As for the adds: `out` null or writable, `client` null or live, `channel` null or
/// NUL-terminated.
unsafe fn add_arguments<'a, T>(
    function: &str,
    out: *mut *mut T,
    client: *mut cavalry_t,
    channel: *const c_char,
) -> Option<(&'a mut Cavalry, &'a str)> {
    if out.is_null() {
        error::null(function, "the handle to fill");
        return None;
    }
    if client.is_null() {
        error::null(function, "client");
        return None;
    }
    if channel.is_null() {
        error::null(function, "channel");
        return None;
    }
    // SAFETY: `channel` is NUL-terminated by the caller's convention.
    let channel = unsafe { strings::channel(function, channel) }?;
    // SAFETY: `client` is non-null, checked above, and a live handle used from one
    // thread at a time ("Threads"), so the exclusive reference is the only one for
    // this call.
    let client = unsafe { &mut (*client).client };
    Some((client, channel))
}

/// Adds a publication on `channel` and `stream_id`, waiting for the driver's answer or
/// the 10 s response timeout.
///
/// # Safety
///
/// `publication` is null or writable; `client` is null or a live client handle not in
/// use by another call; `channel` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_add_publication(
    publication: *mut *mut cavalry_publication_t,
    client: *mut cavalry_t,
    channel: *const c_char,
    stream_id: i32,
) -> c_int {
    // SAFETY: the caller's conventions above.
    let Some((client, channel)) =
        (unsafe { add_arguments("cavalry_add_publication", publication, client, channel) })
    else {
        return -1;
    };
    match client.add_publication(channel, stream_id) {
        Ok(added) => {
            // SAFETY: `publication` is non-null and writable.
            unsafe { *publication = publication_handle(added) };
            0
        }
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Adds an exclusive publication: one publisher, so its claims keep the term id and
/// offset without a fetch-and-add.
///
/// # Safety
///
/// As for `cavalry_add_publication`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_add_exclusive_publication(
    publication: *mut *mut cavalry_publication_t,
    client: *mut cavalry_t,
    channel: *const c_char,
    stream_id: i32,
) -> c_int {
    // SAFETY: the caller's conventions above.
    let Some((client, channel)) = (unsafe {
        add_arguments(
            "cavalry_add_exclusive_publication",
            publication,
            client,
            channel,
        )
    }) else {
        return -1;
    };
    match client.add_exclusive_publication(channel, stream_id) {
        Ok(added) => {
            // SAFETY: `publication` is non-null and writable.
            unsafe { *publication = publication_handle(added) };
            0
        }
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Adds a subscription on `channel` and `stream_id`.
///
/// # Safety
///
/// As for `cavalry_add_publication`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_add_subscription(
    subscription: *mut *mut cavalry_subscription_t,
    client: *mut cavalry_t,
    channel: *const c_char,
    stream_id: i32,
) -> c_int {
    // SAFETY: the caller's conventions above.
    let Some((client, channel)) =
        (unsafe { add_arguments("cavalry_add_subscription", subscription, client, channel) })
    else {
        return -1;
    };
    match client.add_subscription(channel, stream_id) {
        Ok(added) => {
            // SAFETY: `subscription` is non-null and writable.
            unsafe { *subscription = subscription_handle(added) };
            0
        }
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Starts recording the stream of `channel` and `stream_id` into the durable log.
///
/// # Safety
///
/// As for `cavalry_add_publication`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_start_recording(
    recording: *mut *mut cavalry_recording_t,
    client: *mut cavalry_t,
    channel: *const c_char,
    stream_id: i32,
) -> c_int {
    // SAFETY: the caller's conventions above.
    let Some((client, channel)) =
        (unsafe { add_arguments("cavalry_start_recording", recording, client, channel) })
    else {
        return -1;
    };
    match client.start_recording(channel, stream_id) {
        Ok(started) => {
            // SAFETY: `recording` is non-null and writable.
            unsafe { *recording = recording_handle(started) };
            0
        }
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Runs a command on the client that takes ids alone.
///
/// # Safety
///
/// `client` is null or a live client handle not in use by another call.
unsafe fn command(
    function: &str,
    client: *mut cavalry_t,
    run: impl FnOnce(&mut Cavalry) -> Result<(), cavalry_client::ClientError>,
) -> c_int {
    if client.is_null() {
        error::null(function, "client");
        return -1;
    }
    // SAFETY: a live handle used from one thread at a time ("Threads").
    let client = unsafe { &mut (*client).client };
    match run(client) {
        Ok(()) => 0,
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Stops the recording `recording_id`.
///
/// # Safety
///
/// `client` is null or a live client handle not in use by another call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_stop_recording(
    client: *mut cavalry_t,
    recording_id: i64,
) -> c_int {
    // SAFETY: the caller's convention above.
    unsafe {
        command("cavalry_stop_recording", client, |c| {
            c.stop_recording(recording_id)
        })
    }
}

/// Adds the destination `channel` to `publication`'s control channel, waiting for the
/// driver's answer or the 10 s response timeout (`spec/client-protocol.md`,
/// "Commands", add destination; ADR 0053, ADR 0057). The publication's registration
/// id is read from the handle's copy, so the call is allowed during a claim. The
/// driver answers once it has queued the change for its sender, which takes requests
/// in queue order, at most 16 at the start of a duty cycle: until the cycle that takes
/// this one, nothing the endpoint sends reaches the destination.
///
/// # Safety
///
/// `client` is null or a live client handle not in use by another call;
/// `publication` is null or a live publication handle; `channel` is null or
/// NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_add_destination(
    client: *mut cavalry_t,
    publication: *const cavalry_publication_t,
    channel: *const c_char,
) -> c_int {
    // SAFETY: the caller's conventions above.
    unsafe {
        destination(
            "cavalry_add_destination",
            client,
            publication,
            channel,
            true,
        )
    }
}

/// Removes the destination `channel`, as it was added, from `publication`'s control
/// channel; one not there is removed by nothing and answered the same. The driver
/// answers once it has queued the change for its sender, which takes requests in queue
/// order, at most 16 at the start of a duty cycle: until the cycle that takes this
/// one, everything the endpoint sends still reaches the destination.
///
/// # Safety
///
/// As for `cavalry_add_destination`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_remove_destination(
    client: *mut cavalry_t,
    publication: *const cavalry_publication_t,
    channel: *const c_char,
) -> c_int {
    // SAFETY: the caller's conventions above.
    unsafe {
        destination(
            "cavalry_remove_destination",
            client,
            publication,
            channel,
            false,
        )
    }
}

/// The two destination commands: the handles and the string checked as the adds check
/// theirs, the publication's registration id read from the handle's copied constants,
/// which a claim does not borrow, then the client's command.
///
/// # Safety
///
/// As for `cavalry_add_destination`.
unsafe fn destination(
    function: &str,
    client: *mut cavalry_t,
    publication: *const cavalry_publication_t,
    channel: *const c_char,
    add: bool,
) -> c_int {
    if publication.is_null() {
        error::null(function, "publication");
        return -1;
    }
    // SAFETY: a live publication handle, per the caller's convention; its constants
    // are the handle's own copy, read by `cavalry_publication_constants` the same way.
    let registration_id = unsafe { crate::publication::registration_id_of(publication) };
    if channel.is_null() {
        error::null(function, "channel");
        return -1;
    }
    // SAFETY: non-null, checked above, and NUL-terminated by the caller's convention.
    let Some(channel) = (unsafe { strings::channel(function, channel) }) else {
        return -1;
    };
    // SAFETY: the caller's convention on `client`.
    unsafe {
        command(function, client, |c| {
            if add {
                c.add_destination(registration_id, channel)
            } else {
                c.remove_destination(registration_id, channel)
            }
        })
    }
}

/// Truncates the recording `recording_id` to `position`.
///
/// # Safety
///
/// As for `cavalry_stop_recording`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_truncate_recording(
    client: *mut cavalry_t,
    recording_id: i64,
    position: i64,
) -> c_int {
    // SAFETY: the caller's convention above.
    unsafe {
        command("cavalry_truncate_recording", client, |c| {
            c.truncate_recording(recording_id, position)
        })
    }
}

/// Purges the recording's segment files below `new_start_position`.
///
/// # Safety
///
/// As for `cavalry_stop_recording`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_purge_segments(
    client: *mut cavalry_t,
    recording_id: i64,
    new_start_position: i64,
) -> c_int {
    // SAFETY: the caller's convention above.
    unsafe {
        command("cavalry_purge_segments", client, |c| {
            c.purge_segments(recording_id, new_start_position)
        })
    }
}

/// Purges the whole recording `recording_id`.
///
/// # Safety
///
/// As for `cavalry_stop_recording`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_purge_recording(
    client: *mut cavalry_t,
    recording_id: i64,
) -> c_int {
    // SAFETY: the caller's convention above.
    unsafe {
        command("cavalry_purge_recording", client, |c| {
            c.purge_recording(recording_id)
        })
    }
}

/// Starts a replay of `recording_id` from `position` for `length` bytes, or
/// `CAVALRY_NULL_POSITION`, `CAVALRY_NULL_LENGTH` and `CAVALRY_FOLLOW_LENGTH` as the
/// constants say, onto `replay_stream_id` of `channel`.
///
/// # Safety
///
/// As for `cavalry_add_publication`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_start_replay(
    replay: *mut *mut cavalry_replay_t,
    client: *mut cavalry_t,
    recording_id: i64,
    position: i64,
    length: i64,
    replay_stream_id: i32,
    channel: *const c_char,
) -> c_int {
    // SAFETY: the caller's conventions above.
    let Some((client, channel)) =
        (unsafe { add_arguments("cavalry_start_replay", replay, client, channel) })
    else {
        return -1;
    };
    match client.start_replay(recording_id, position, length, replay_stream_id, channel) {
        Ok(started) => {
            // SAFETY: `replay` is non-null and writable.
            unsafe { *replay = replay_handle(started) };
            0
        }
        Err(error) => {
            error::set_client(&error);
            -1
        }
    }
}

/// Stops the replay `replay_session_id`.
///
/// # Safety
///
/// As for `cavalry_stop_recording`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_stop_replay(
    client: *mut cavalry_t,
    replay_session_id: i64,
) -> c_int {
    // SAFETY: the caller's convention above.
    unsafe {
        command("cavalry_stop_replay", client, |c| {
            c.stop_replay(replay_session_id)
        })
    }
}
