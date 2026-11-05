//! The library's version and the ABI's (`spec/c-abi.md`, "Functions", errors and
//! version).

use std::ffi::{c_char, c_int};

use crate::codes::CAVALRY_ABI_VERSION;

fn part(text: &str) -> c_int {
    text.parse().unwrap_or(0)
}

/// The crate's major version.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_version_major() -> c_int {
    part(env!("CARGO_PKG_VERSION_MAJOR"))
}

/// The crate's minor version.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_version_minor() -> c_int {
    part(env!("CARGO_PKG_VERSION_MINOR"))
}

/// The crate's patch version.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_version_patch() -> c_int {
    part(env!("CARGO_PKG_VERSION_PATCH"))
}

/// The crate's version as text, such as `0.1.0`, static for the program's life.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_version_full() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0")
        .as_ptr()
        .cast::<c_char>()
}

/// The ABI's version, this page's, beside the header's `CAVALRY_ABI_VERSION`.
#[unsafe(no_mangle)]
pub extern "C" fn cavalry_abi_version() -> c_int {
    CAVALRY_ABI_VERSION
}
