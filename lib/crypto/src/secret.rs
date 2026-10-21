//! Erasure of secret storage. The crate's erasure contract:
//!
//! - **Erased: owners.** A [`Secret`], and every type that owns a key, seed,
//!   expanded key, key schedule, scalar or shared secret, erases its own
//!   storage when it drops. So do the hash, HMAC, Keccak, SHA-1 and Poly1305
//!   states.
//! - **Erased: key-equivalent results.** Each key agreement erases its
//!   shared value before it becomes the returned bytes: X25519's x2 / z2, and
//!   P-256/P-384's product point with its affine coordinates. The bytes are
//!   then the caller's `SharedSecret`, an owner.
//! - **Erased: workspaces.** A secret operation's explicitly managed state is
//!   a [`Workspace`], dropped when the operation returns, on every path out of
//!   it, early returns and `?` included. The X25519 ladder's (x2, z2, x3, z3)
//!   is one. Its slot is what LLVM writes at the ladder's end; the copies the
//!   loop works on in registers and other slots are not erased.
//! - **Erased per value, for now.** Ed25519 scalars (`scalar25519::S`, until
//!   its reduction is rewritten) and ML-KEM polynomials (`mlkem::Poly`) still
//!   erase every temporary on drop.
//! - **How.** Volatile stores of zero over the value's own storage, then a
//!   sequentially consistent compiler fence. They erase that location and no
//!   other.
//! - **Not erased.** Other arithmetic intermediates: 2^255 - 19 field
//!   elements, NIST residues and both curves' points are `Copy` values.
//!   Registers; spill slots and every other copy the compiler makes, by-value
//!   moves and returns included; a moved value's earlier location; finished
//!   stack frames; the caller's buffers and public outputs. These remain
//!   until something overwrites them.
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

/// Types whose all-zero bytes are a valid value.
///
/// # Safety
/// Every bit pattern of zero bytes must be a valid `Self`: no references,
/// `NonZero` fields or enums without a zero discriminant.
pub(crate) unsafe trait Zeroable: Copy {}
// Integers and arrays of zeroable elements are valid at zero.
unsafe impl Zeroable for u8 {}
unsafe impl Zeroable for u32 {}
unsafe impl Zeroable for u64 {}
unsafe impl Zeroable for u128 {}
unsafe impl<T: Zeroable, const N: usize> Zeroable for [T; N] {}

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
pub(crate) struct Workspace<T: Zeroable>(pub(crate) T);

impl<T: Zeroable> Drop for Workspace<T> {
    fn drop(&mut self) {
        erase(&mut self.0);
    }
}
