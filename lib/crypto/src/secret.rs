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

/// Erases a whole array with one volatile store, which the compiler lowers to
/// as few wide stores as the array's size allows.
pub(crate) fn erase<T: Copy + Default, const N: usize>(array: &mut [T; N]) {
    // The array is live and exclusively borrowed.
    unsafe { core::ptr::write_volatile(array, [T::default(); N]) };
    compiler_fence(Ordering::SeqCst);
}

/// An owned array of `N` words, erased on drop.
pub(crate) struct Secret<const N: usize, T: Copy + Default = u8>(pub(crate) [T; N]);

impl<const N: usize, T: Copy + Default> Drop for Secret<N, T> {
    fn drop(&mut self) {
        erase(&mut self.0);
    }
}
