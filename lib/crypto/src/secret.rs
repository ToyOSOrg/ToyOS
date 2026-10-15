//! Owned secrets. Volatile stores erase this allocation, not prior moves.

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

/// Erases a whole array, 16 bytes per volatile store. A single volatile store
/// of a large array is lowered through a stack copy, a `memcpy` call.
pub(crate) fn erase<T: Zeroable, const N: usize>(array: &mut [T; N]) {
    let bytes = core::mem::size_of::<[T; N]>();
    let base = (array as *mut [T; N]).cast::<u8>();
    for i in 0..bytes / 16 {
        // The 16 bytes lie inside the live, exclusively borrowed array, and
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
