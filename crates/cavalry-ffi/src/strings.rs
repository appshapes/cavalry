//! The strings the ABI takes and gives (`spec/c-abi.md`, "Strings"): a channel must
//! be UTF-8, a directory is bytes, and every string the library gives back is a
//! NUL-terminated copy the handle owns.

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};

use crate::codes::{CAVALRY_ERR_EMPTY_DIR, CAVALRY_ERR_NOT_UTF8};
use crate::error::{self, DOC};

/// Reads a channel: a NUL-terminated string that must be UTF-8.
///
/// # Safety
///
/// `channel` is non-null and NUL-terminated, the caller's convention for every
/// `const char *` the ABI takes.
pub(crate) unsafe fn channel<'a>(function: &str, channel: *const c_char) -> Option<&'a str> {
    // SAFETY: non-null and NUL-terminated by the caller's convention above, and read
    // during the call only.
    let bytes = unsafe { CStr::from_ptr(channel) };
    match bytes.to_str() {
        Ok(text) => Some(text),
        Err(_) => {
            error::set(
                CAVALRY_ERR_NOT_UTF8,
                format_args!(
                    "{function} refused (not-utf8): the channel is not UTF-8; pass the channel \
                     as the page spells it, such as cavalry:ipc; see {DOC}#strings"
                ),
            );
            None
        }
    }
}

/// Reads a string that is not a channel and must be UTF-8 all the same, since it is a
/// `&str` in the Rust API: `parameter` of `function`, with `pass` as what to pass.
///
/// # Safety
///
/// As for [`channel`].
pub(crate) unsafe fn utf8<'a>(
    function: &str,
    parameter: &str,
    pass: &str,
    text: *const c_char,
) -> Option<&'a str> {
    // SAFETY: non-null and NUL-terminated by the caller's convention, and read during
    // the call only.
    let bytes = unsafe { CStr::from_ptr(text) };
    match bytes.to_str() {
        Ok(text) => Some(text),
        Err(_) => {
            error::set(
                CAVALRY_ERR_NOT_UTF8,
                format_args!(
                    "{function} refused (not-utf8): {parameter} is not UTF-8; pass {pass}; see \
                     {DOC}#strings"
                ),
            );
            None
        }
    }
}

/// Reads a directory: NUL-terminated bytes, a path as the operating system holds it,
/// refused when empty.
///
/// # Safety
///
/// As for [`channel`].
pub(crate) unsafe fn dir(function: &str, dir: *const c_char) -> Option<PathBuf> {
    // SAFETY: non-null and NUL-terminated by the caller's convention.
    let bytes = unsafe { CStr::from_ptr(dir) }.to_bytes();
    if bytes.is_empty() {
        error::set(
            CAVALRY_ERR_EMPTY_DIR,
            format_args!(
                "{function} refused (empty-dir): an empty string names no directory; pass the \
                 driver directory, or \".\" for the working directory, or leave the context's \
                 default; see {DOC}#strings"
            ),
        );
        return None;
    }
    Some(path_of(bytes))
}

#[cfg(unix)]
fn path_of(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// A path as a NUL-terminated C string the handle keeps. A path from the operating
/// system or from a C string holds no NUL, so the conversion cannot fail; if it did,
/// the handle would give back the empty string rather than lie about the path.
pub(crate) fn c_string_of_path(path: &Path) -> CString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        CString::new(path.as_os_str().as_bytes()).unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        CString::new(path.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// A Rust string as a NUL-terminated C string the handle keeps; a channel came from a
/// C string or the driver, neither of which holds a NUL.
pub(crate) fn c_string(text: &str) -> CString {
    CString::new(text).unwrap_or_default()
}
