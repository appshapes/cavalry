//! Boxing a handle for the caller and freeing it at its close, with the poison a debug
//! build writes before the free so that a use after the close is loud (`spec/c-abi.md`,
//! "Lifetimes"; `CLAUDE.md`, Unsafe).

use std::alloc::{Layout, dealloc};
use std::ptr;

/// Gives the caller a handle that owns `value`.
pub(crate) fn give<T>(value: T) -> *mut T {
    Box::into_raw(Box::new(value))
}

/// Takes the handle back and frees it: drops the value, fills its memory with `0xDD`
/// in a debug build, and releases it.
///
/// # Safety
///
/// `handle` came from [`give`] and is freed once, which "Lifetimes" makes the caller's
/// convention: a handle closed twice, or used after its close, is undefined.
pub(crate) unsafe fn take<T>(handle: *mut T) {
    // SAFETY: `handle` was made by `Box::into_raw` in `give`, so it is a valid,
    // uniquely owned `T` with the global allocator's layout for `T`, and this is the
    // one free of it, by the caller's convention above.
    unsafe {
        ptr::drop_in_place(handle);
        #[cfg(debug_assertions)]
        ptr::write_bytes(handle.cast::<u8>(), 0xDD, std::mem::size_of::<T>());
        if std::mem::size_of::<T>() != 0 {
            dealloc(handle.cast::<u8>(), Layout::new::<T>());
        }
    }
}

/// Takes the handle back and moves its value out, for a close that consumes the Rust
/// value: fills the memory with `0xDD` in a debug build and releases it.
///
/// # Safety
///
/// As for [`take`].
pub(crate) unsafe fn take_out<T>(handle: *mut T) -> T {
    // SAFETY: as in `take`; the value is moved out before the memory is poisoned and
    // released, so nothing is dropped twice.
    unsafe {
        let value = ptr::read(handle);
        #[cfg(debug_assertions)]
        ptr::write_bytes(handle.cast::<u8>(), 0xDD, std::mem::size_of::<T>());
        if std::mem::size_of::<T>() != 0 {
            dealloc(handle.cast::<u8>(), Layout::new::<T>());
        }
        value
    }
}
