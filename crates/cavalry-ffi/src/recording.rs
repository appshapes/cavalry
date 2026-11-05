//! A recording's handle: its constants and its two positions (`spec/c-abi.md`,
//! "Functions", recording and replay).

use std::ffi::{CString, c_char, c_int};

use cavalry_client::Recording;

use crate::error;
use crate::handle;
use crate::strings;

/// A recording's handle; made by `cavalry_start_recording`, freed by
/// `cavalry_recording_close`, which stops nothing.
pub struct cavalry_recording_t {
    channel: CString,
    recording: Recording,
}

/// A recording's constants, the strings valid for the handle's life.
#[repr(C)]
pub struct cavalry_recording_constants_t {
    /// The channel recorded.
    pub channel: *const c_char,
    /// The recording's id in the catalog.
    pub recording_id: i64,
    /// The recorded stream's session id.
    pub session_id: i32,
    /// The recorded stream's stream id.
    pub stream_id: i32,
}

pub(crate) fn recording_handle(recording: Recording) -> *mut cavalry_recording_t {
    handle::give(cavalry_recording_t {
        channel: strings::c_string(recording.channel()),
        recording,
    })
}

/// Fills `constants` from the handle.
///
/// # Safety
///
/// `recording` is null or a live recording handle; `constants` is null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_recording_constants(
    recording: *const cavalry_recording_t,
    constants: *mut cavalry_recording_constants_t,
) -> c_int {
    if recording.is_null() {
        error::null("cavalry_recording_constants", "recording");
        return -1;
    }
    if constants.is_null() {
        error::null("cavalry_recording_constants", "constants");
        return -1;
    }
    // SAFETY: a live handle and a writable struct, by the caller's conventions.
    unsafe {
        let handle = &*recording;
        *constants = cavalry_recording_constants_t {
            channel: handle.channel.as_ptr(),
            recording_id: handle.recording.recording_id(),
            session_id: handle.recording.session_id(),
            stream_id: handle.recording.stream_id(),
        };
    }
    0
}

/// How far the recorder has written the recording.
///
/// # Safety
///
/// `recording` is null or a live recording handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_recording_position(recording: *const cavalry_recording_t) -> i64 {
    if recording.is_null() {
        error::null("cavalry_recording_position", "recording");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*recording).recording.position() }
}

/// How far the recording is synced to disk.
///
/// # Safety
///
/// As for `cavalry_recording_position`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_recording_synced_position(
    recording: *const cavalry_recording_t,
) -> i64 {
    if recording.is_null() {
        error::null("cavalry_recording_synced_position", "recording");
        return -1;
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*recording).recording.synced_position() }
}

/// Frees the handle; the recording itself is stopped by `cavalry_stop_recording`.
///
/// # Safety
///
/// `recording` is null or a live recording handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_recording_close(recording: *mut cavalry_recording_t) -> c_int {
    if recording.is_null() {
        error::null("cavalry_recording_close", "recording");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention.
    unsafe { handle::take(recording) };
    0
}
