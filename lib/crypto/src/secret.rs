//! Erasure of secret storage. The crate's erasure contract:
//!
//! - **Erased: owners.** A [`Secret`], and every type that owns a key, seed,
//!   expanded key, key schedule, scalar or shared secret, erases its own
//!   storage when it drops. So do the hash, HMAC, Keccak, SHA-1 and Poly1305
//!   states.
//! - **Erased: key-agreement values.** Each key agreement writes its output
//!   bytes into a `Secret` it owns, then erases the values they came from:
//!   X25519's x2 / z2, and P-256/P-384's product point and affine
//!   coordinates, each held in a [`Workspace`]. The output `Secret` is moved
//!   into the returned `SharedSecret`, an owner. Not erased: the copies of
//!   the output bytes that move leaves in the agreement's finished frame, and
//!   the computation's arithmetic intermediates (below).
//! - **Erased: workspaces.** A secret operation's explicitly managed state is
//!   a [`Workspace`], dropped when the operation returns, on every path out of
//!   it, early returns and `?` included. The X25519 ladder's (x2, z2, x3, z3)
//!   and the Ed25519 comb's signed digits are workspaces. A workspace erases
//!   its own storage; the copies the computation works on in registers and
//!   other slots are not erased. scrypt's workspace is the caller's, and
//!   `scrypt::derive` erases it on every path out.
//! - **Erased per value.** Ed25519's secret scalars (`scalar25519::S`) are
//!   owners, and their reduction erases its workspaces; ML-KEM polynomials
//!   (`mlkem::Poly`) erase every temporary on drop.
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

/// Erases `bytes` with volatile stores, 16 bytes to a store: one volatile
/// store of a large array is lowered through a stack copy, a `memcpy` call.
pub(crate) fn wipe(bytes: &mut [u8]) {
    let (chunks, tail) = bytes.as_chunks_mut::<16>();
    for chunk in chunks {
        // SAFETY: the address is a live, exclusively borrowed chunk.
        unsafe { core::ptr::write_volatile(chunk, [0; 16]) };
    }
    for byte in tail {
        // SAFETY: the address is a live, exclusively borrowed byte.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

pub(crate) fn wipe32(words: &mut [u32]) {
    for word in words {
        // SAFETY: the address is a live, exclusively borrowed word.
        unsafe { core::ptr::write_volatile(word, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

pub(crate) fn wipe64(words: &mut [u64]) {
    for word in words {
        // SAFETY: the address is a live, exclusively borrowed word.
        unsafe { core::ptr::write_volatile(word, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

/// Types whose all-zero bytes are a valid value.
///
/// # Safety
/// The all-zero bit pattern must be a valid `Self`: no references,
/// `NonZero` fields or enums without a zero discriminant. `Self` has no
/// padding, so its bytes are all initialised.
pub unsafe trait Zeroable: Copy {}
// SAFETY: an integer is valid at zero.
unsafe impl Zeroable for u8 {}
// SAFETY: as `u8`.
unsafe impl Zeroable for i8 {}
// SAFETY: as `u8`.
unsafe impl Zeroable for u32 {}
// SAFETY: as `u8`.
unsafe impl Zeroable for u64 {}
// SAFETY: as `u8`.
unsafe impl Zeroable for u128 {}
// SAFETY: an array of elements valid at zero is valid at zero.
unsafe impl<T: Zeroable, const N: usize> Zeroable for [T; N] {}

/// Erases a whole value, as [`wipe`] erases its bytes.
pub(crate) fn erase<T: Zeroable>(value: &mut T) {
    // SAFETY: the value is live and exclusively borrowed for the slice's life,
    // its bytes are initialised (`Zeroable` has no padding), and zero bytes
    // are a valid `T`.
    wipe(unsafe { core::slice::from_raw_parts_mut((value as *mut T).cast::<u8>(), core::mem::size_of::<T>()) });
}

/// An owned array of `N` words, erased on drop: its own storage is overwritten with zeros by
/// volatile stores, under the module's contract. A copy made before it was owned, such as the
/// array [`Secret::new`] moves in from, is the caller's to erase.
pub struct Secret<const N: usize, T: Zeroable = u8>(pub(crate) [T; N]);

impl<const N: usize, T: Zeroable> Secret<N, T> {
    pub const fn new(value: [T; N]) -> Self {
        Self(value)
    }

    pub const fn expose(&self) -> &[T; N] {
        &self.0
    }

    pub fn expose_mut(&mut self) -> &mut [T; N] {
        &mut self.0
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::ManuallyDrop;

    fn wiped<T: Zeroable, const N: usize>(value: [T; N]) {
        let mut array = value;
        erase(&mut array);
        // The array's bytes are initialised and live for this borrow.
        let bytes = unsafe { core::slice::from_raw_parts(array.as_ptr().cast::<u8>(), core::mem::size_of_val(&array)) };
        assert!(bytes.iter().all(|&b| b == 0), "{} bytes", bytes.len());
    }

    /// `erase` over sizes around its 16-byte stores and alignments below
    /// theirs, including nested arrays and none at all.
    #[test]
    fn erase_zeroes_every_byte_of_every_shape() {
        wiped::<u8, 0>([]);
        wiped([0xffu8; 1]);
        wiped([0xffu8; 15]);
        wiped([0xffu8; 16]);
        wiped([0xffu8; 17]);
        wiped([0xffu8; 255]);
        wiped([u32::MAX; 3]);
        wiped([u64::MAX; 5]);
        wiped([u128::MAX; 2]);
        wiped([[u32::MAX; 16]; 4]);
    }

    #[test]
    fn wipes_zero_their_slices() {
        let mut bytes = [0xa5u8; 37];
        wipe(&mut bytes[3..]);
        assert_eq!(bytes[..3], [0xa5; 3]);
        assert!(bytes[3..].iter().all(|&b| b == 0));
        let mut words = [u64::MAX; 7];
        wipe64(&mut words);
        assert_eq!(words, [0; 7]);
        let mut words = [u32::MAX; 5];
        wipe32(&mut words);
        assert_eq!(words, [0; 5]);
    }

    /// The drop of a secret owner, or of an operation's workspace, leaves its
    /// storage zero.
    #[test]
    fn a_dropped_secret_leaves_zeros() {
        let mut secret = ManuallyDrop::new(Secret([0x3cu8; 61]));
        // The storage stays allocated, so it can be read after the drop;
        // `u8` has no invalid bit patterns.
        unsafe { ManuallyDrop::drop(&mut secret) };
        assert_eq!(secret.0, [0; 61]);
        let mut words = ManuallyDrop::new(Secret([[u32::MAX; 16]; 4]));
        unsafe { ManuallyDrop::drop(&mut words) };
        assert_eq!(words.0, [[0; 16]; 4]);
        let mut workspace = ManuallyDrop::new(Workspace([[u64::MAX; 5]; 3]));
        unsafe { ManuallyDrop::drop(&mut workspace) };
        assert_eq!(workspace.0, [[0; 5]; 3]);
    }
}
