//! The atomics and the cell every shared layout is built from. Under `cfg(loom)`, set
//! by `just loom`, they are loom's, so the model checker sees every load, store and
//! plain access the publish protocols in `counters::cells` make; otherwise they are
//! `core`'s, with the same `with` and `with_mut` shape so the protocol code is one
//! copy. Only this module names either.

#[cfg(loom)]
pub(crate) use loom::cell::UnsafeCell;
#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering};

#[cfg(not(loom))]
pub(crate) use core::sync::atomic::{AtomicI32, AtomicI64, AtomicU64, Ordering};

/// Plain bytes that one party writes before a release store and another reads after
/// an acquire load. The callers of `with` and `with_mut` carry the `// SAFETY:`
/// comment naming the protocol that keeps the two apart; this type only hands out the
/// pointer, as loom's does.
#[cfg(not(loom))]
#[repr(transparent)]
pub(crate) struct UnsafeCell<T>(core::cell::UnsafeCell<T>);

#[cfg(not(loom))]
impl<T> UnsafeCell<T> {
    /// A cell holding `value`. Production cells are never constructed: they are views
    /// over a mapping (`counters::file`). Tests build them on plain memory.
    #[cfg(test)]
    pub(crate) const fn new(value: T) -> Self {
        Self(core::cell::UnsafeCell::new(value))
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(*const T) -> R) -> R {
        f(self.0.get())
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(*mut T) -> R) -> R {
        f(self.0.get())
    }
}
