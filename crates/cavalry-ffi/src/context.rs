//! The context: the driver directory a client will attach to, and the version check
//! every program runs first (`spec/c-abi.md`, "Handles", "Versions and what a reader
//! rejects").

use std::ffi::{CString, c_char, c_int};
use std::path::{Path, PathBuf};

use cavalry_client::Cavalry;

use crate::codes::{CAVALRY_ABI_VERSION, CAVALRY_ERR_ABI_VERSION};
use crate::error::{self, DOC};
use crate::{handle, strings};

/// The directory a client will attach to; made by `cavalry_context_init`, freed by
/// `cavalry_context_close`.
pub struct cavalry_context_t {
    dir: PathBuf,
    dir_c: CString,
}

impl cavalry_context_t {
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn dir_c(&self) -> &CString {
        &self.dir_c
    }
}

/// Makes a context whose directory is `CAVALRY_DIR` when set and not empty, else the
/// driver's default, after checking `abi_version` against the library's
/// `CAVALRY_ABI_VERSION`; the header's `cavalry_context_init(context)` macro passes
/// the header's own. A mismatch is −1 with `CAVALRY_ERR_ABI_VERSION`.
///
/// # Safety
///
/// `context` is null, or points at a location this call may write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_context_init_v(
    context: *mut *mut cavalry_context_t,
    abi_version: c_int,
) -> c_int {
    if context.is_null() {
        error::null("cavalry_context_init", "context");
        return -1;
    }
    if abi_version != CAVALRY_ABI_VERSION {
        error::set(
            CAVALRY_ERR_ABI_VERSION,
            format_args!(
                "cavalry_context_init refused (abi-version): the program was built against ABI \
                 version {abi_version} and this library is version {CAVALRY_ABI_VERSION}; \
                 rebuild the program against this library's header; see \
                 {DOC}#versions-and-what-a-reader-rejects"
            ),
        );
        return -1;
    }
    let dir = Cavalry::default_dir();
    let dir_c = strings::c_string_of_path(&dir);
    // SAFETY: `context` is non-null and writable by the caller's convention above.
    unsafe { *context = handle::give(cavalry_context_t { dir, dir_c }) };
    0
}

/// Replaces the context's directory with `dir`'s bytes; an empty string is refused
/// with `CAVALRY_ERR_EMPTY_DIR` and the context keeps the directory it had.
///
/// # Safety
///
/// `context` is null or a live context handle; `dir` is null or NUL-terminated.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_context_set_dir(
    context: *mut cavalry_context_t,
    dir: *const c_char,
) -> c_int {
    if context.is_null() {
        error::null("cavalry_context_set_dir", "context");
        return -1;
    }
    if dir.is_null() {
        error::null("cavalry_context_set_dir", "dir");
        return -1;
    }
    // SAFETY: `dir` is non-null and NUL-terminated by the caller's convention.
    let Some(path) = (unsafe { strings::dir("cavalry_context_set_dir", dir) }) else {
        return -1;
    };
    // SAFETY: `context` is a live handle used from one thread at a time ("Threads").
    let context = unsafe { &mut *context };
    context.dir_c = strings::c_string_of_path(&path);
    context.dir = path;
    0
}

/// The directory `cavalry_init` will use, valid until the next `set_dir` or the
/// context's close; null for a null context.
///
/// # Safety
///
/// `context` is null or a live context handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_context_get_dir(
    context: *const cavalry_context_t,
) -> *const c_char {
    if context.is_null() {
        error::null("cavalry_context_get_dir", "context");
        return std::ptr::null();
    }
    // SAFETY: a live handle, by the caller's convention.
    unsafe { (*context).dir_c.as_ptr() }
}

/// Frees the context. A client made from it keeps its own copy of the directory, so
/// the order of the two closes does not matter.
///
/// # Safety
///
/// `context` is null or a live context handle, closed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn cavalry_context_close(context: *mut cavalry_context_t) -> c_int {
    if context.is_null() {
        error::null("cavalry_context_close", "context");
        return -1;
    }
    // SAFETY: a live handle freed once, by the caller's convention ("Lifetimes").
    unsafe { handle::take(context) };
    0
}
