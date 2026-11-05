//! A replay's handle: its constants, its position and its end (`spec/c-abi.md`,
//! "Functions", recording and replay).

use std::ffi::{CString, c_char, c_int};

use cavalry_client::Replay;
use cavalry_proto::client_protocol::ReplayStopReason;

use crate::codes::*;
use crate::error;
use crate::handle;
use crate::strings;

/// A replay's handle; made by `cavalry_start_replay`, freed by `cavalry_replay_close`,
/// which stops nothing.
pub struct cavalry_replay_t {
    channel: CString,
    replay: Replay,
}

/// A replay's constants, the strings valid for the handle's life.
#[repr(C)]
pub struct cavalry_replay_constants_t {
    /// The channel the replay publishes on.
    pub channel: *const c_char,
    /// The id under which the driver knows the replay.
    pub replay_session_id: i64,
    /// The recording replayed.
    pub recording_id: i64,
    /// The position the replay started from.
    pub start_position: i64,
    /// The replay publication's session id.
    pub session_id: i32,
    /// The replay's stream id.
    pub stream_id: i32,
}

pub(crate) fn replay_handle(replay: Replay) -> *mut cavalry_replay_t {
    handle::give(cavalry_replay_t {
        channel: strings::c_string(replay.channel()),
        replay,
    })
}

/// Fills `constants` from the handle.
///
/// # Safety
///
/// `replay` is null or a live replay handle; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_replay_constants(
    replay: *const cavalry_replay_t,
    constants: *mut cavalry_replay_constants_t,
) -> c_int {
    if replay.is_null() {
        error::null("cavalry_replay_constants", "replay");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_replay_constants", "constants");
        return -1;
    }
    // SAFETY: a live handle and a writable struct, by the caller's conventions.
    unsafe {
        let handle = &*replay;
        *constants = cavalry_replay_constants_t {
            channel: handle.channel.as_ptr(),
            replay_session_id: handle.replay.replay_session_id(),
            recording_id: handle.replay.recording_id(),
            start_position: handle.replay.start_position(),
            session_id: handle.replay.session_id(),
            stream_id: handle.replay.stream_id(),
        };
    }
    0
}

/// How far the replayer has written the replay.
///
/// # Safety
///
/// `replay` is null or a live replay handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_replay_position(replay: *const cavalry_replay_t) -> i64 {
    if replay.is_null() {
        error::null("cavalry_replay_position", "replay");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*replay).replay.position() }
}

/// Whether the replay has ended; when it has, the end position and the reason,
/// `CAVALRY_REPLAY_STOP_*` or −1 for none, are written through the non-null pointers.
///
/// # Safety
///
/// `replay` is null or a live replay handle; `position` and `reason` are null or
/// writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_replay_ended(
    replay: *const cavalry_replay_t,
    position: *mut i64,
    reason: *mut i32,
) -> bool {
    if replay.is_null() {
        error::null("cavalry_replay_ended", "replay");
        return false;
    }
    // SAFETY: a live handle, by the caller's convention.
    let Some((at, why)) = (unsafe { (*replay).replay.ended() }) else {
        return false;
    };
    let code = match why {
        Some(ReplayStopReason::Ended) => CAVALRY_REPLAY_STOP_ENDED,
        Some(ReplayStopReason::Stopped) => CAVALRY_REPLAY_STOP_STOPPED,
        Some(ReplayStopReason::NotConnected) => CAVALRY_REPLAY_STOP_NOT_CONNECTED,
        Some(ReplayStopReason::Failed) => CAVALRY_REPLAY_STOP_FAILED,
        None => -1,
    };
    // SAFETY: each pointer is null or writable, by the caller's convention.
    unsafe {
        if !position.is_null() {
            *position = at;
        }
        if !reason.is_null() {
            *reason = code;
        }
    }
    true
}

/// Frees the handle; the replay itself is stopped by `cavalry_stop_replay`.
///
/// # Safety
///
/// `replay` is null or a live replay handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_replay_close(replay: *mut cavalry_replay_t) -> c_int {
    if replay.is_null() {
        error::null("cavalry_replay_close", "replay");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention.
    unsafe { handle::take(replay) };
    0
}
