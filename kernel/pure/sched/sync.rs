//! Atomics and `Arc`, resolved to whichever world compiles these sources:
//! `core`/`alloc` for the kernel and the simulator, loom's instrumented
//! atomics for the `kernel-loom` package.
//!
//! A feature and a second package rather than the usual `--cfg loom`, because
//! a `[target.'cfg(loom)'.dependencies]` entry lands in every lockfile that
//! resolves the kernel — including `kernel/Cargo.lock`, which would gain loom
//! and its 30 transitive host crates. The kernel declares `loom` purely so `cfg`
//! checking knows the name, and never enables it.

#[cfg(not(feature = "loom"))]
pub use alloc::sync::Arc;
#[cfg(not(feature = "loom"))]
pub use core::sync::atomic::{fence, AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};

#[cfg(feature = "loom")]
pub use loom::sync::atomic::{fence, AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
#[cfg(feature = "loom")]
pub use loom::sync::Arc;

/// Interior mutability for a small shared cell, supplied by the environment:
/// a watch's waiter list and the per-process fair share. The kernel's
/// implementor is never held across a pass or a switch; the simulator and the
/// loom models supply their own.
///
/// It lives here rather than in one of its users because the scheduler core may
/// not implement a lock itself — that would need `unsafe`, which only
/// `mailbox.rs` is allowed to write.
pub trait CellLock<T>: Sync {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}
