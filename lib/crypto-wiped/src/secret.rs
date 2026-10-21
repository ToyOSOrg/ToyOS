//! Erasure of secret storage. The crate's erasure contract:
//!
//! - **Erased: owners.** A [`Secret`], and every type that owns a key, seed,
//!   expanded key, key schedule, scalar or shared secret, erases its own
//!   storage when it drops.
//! - **Erased: workspaces.** A secret operation's explicitly managed state,
//!   such as the X25519 ladder's coordinates or a scalar multiplication's
//!   accumulator, is a [`Workspace`]. It drops when the operation returns, on
//!   every path out of it, early returns and `?` included.
//! - **How.** Volatile stores of zero over the value's own storage, then a
//!   sequentially consistent compiler fence. They erase that location and no
//!   other.
//! - **Not erased.** Arithmetic intermediates (field elements and residues
//!   are `Copy` values); registers; spill slots and every other copy the
//!   compiler makes, by-value moves and returns included; a moved value's
//!   earlier location; finished stack frames; the caller's buffers and public
//!   outputs. These remain until something overwrites them.
//! - **Panics.** An unwinding panic drops, and so erases, every owner and
//!   workspace on the unwound frames. Under `panic = "abort"` nothing runs
//!   after the panic.

use core::sync::atomic::{Ordering, compiler_fence};

pub(crate) fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // The address is a live, exclusively borrowed byte.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(feature = "wpa2")]
pub(crate) fn wipe32(words: &mut [u32]) {
    for word in words {
        // The address is a live, exclusively borrowed word.
        unsafe { core::ptr::write_volatile(word, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

pub(crate) fn wipe64(words: &mut [u64]) {
    for word in words {
        // The address is a live, exclusively borrowed word.
        unsafe { core::ptr::write_volatile(word, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Element types whose all-zero bytes are a valid value.
pub(crate) trait Zeroable: Copy {}
impl Zeroable for u8 {}
impl Zeroable for u32 {}
impl Zeroable for u64 {}
impl Zeroable for u128 {}
impl<T: Zeroable, const N: usize> Zeroable for [T; N] {}

/// Erases a whole value, 16 bytes per volatile store. A single volatile store
/// of a large array is lowered through a stack copy, a `memcpy` call.
pub(crate) fn erase<T: Zeroable>(value: &mut T) {
    let bytes = core::mem::size_of::<T>();
    let base = (value as *mut T).cast::<u8>();
    for i in 0..bytes / 16 {
        // The 16 bytes lie inside the live, exclusively borrowed value, and
        // zero bytes are a valid `T`.
        unsafe { core::ptr::write_volatile(base.add(16 * i).cast::<[u8; 16]>(), [0; 16]) };
    }
    for i in bytes / 16 * 16..bytes {
        // As above, one byte.
        unsafe { core::ptr::write_volatile(base.add(i), 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// An owned array of `N` words, erased on drop.
pub(crate) struct Secret<const N: usize, T: Zeroable = u8>(pub(crate) [T; N]);

impl<const N: usize, T: Zeroable> Drop for Secret<N, T> {
    fn drop(&mut self) {
        erase(&mut self.0);
    }
}

/// An operation's secret working state, erased when the operation ends, on
/// every return path.
#[allow(dead_code)]
pub(crate) struct Workspace<T: Zeroable>(pub(crate) T);

impl<T: Zeroable> Drop for Workspace<T> {
    fn drop(&mut self) {
        erase(&mut self.0);
    }
}
