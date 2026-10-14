//! Owned byte secrets. Volatile stores erase this allocation, not prior moves.

use core::sync::atomic::{Ordering, compiler_fence};

pub(crate) fn wipe(bytes: &mut [u8]) {
    for byte in bytes {
        // The address is a live, exclusively borrowed byte.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

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

pub(crate) struct Secret<const N: usize>(pub(crate) [u8; N]);

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        wipe(&mut self.0);
    }
}

/// Owned word workspace; passing a borrow avoids an unwiped Copy argument.
pub(crate) struct SecretWords32<const N: usize>(pub(crate) [u32; N]);
impl<const N: usize> Drop for SecretWords32<N> {
    fn drop(&mut self) {
        wipe32(&mut self.0);
    }
}
