//! A bounds-checked volatile window over memory something other than this
//! thread also reads or writes: a register aperture, a ring or queue a device
//! walks, or a region a peer and a device share with this process.
//!
//! **Every driver in userland reaches such memory through this and nothing
//! else**: a second window with a weaker bound is the one reached on the boot
//! nobody watched.
//!
//! Volatile because every one of them races this process: a plain read of a
//! completion's phase bit can be hoisted out of the loop that waits on it, a
//! plain write of a doorbell elided, and a plain copy out of a block the device
//! is still filling is a data race the compiler may assume away. Every bound is
//! an `assert!`: the build that ships is the one that checks.

use core::mem::{align_of, size_of};

/// A window of `len` bytes from `base`.
#[derive(Clone, Copy)]
pub struct Window {
    base: *mut u8,
    len: usize,
}

// SAFETY: a window is an address and a length; what makes an access through
// it sound is the mapping its constructor's contract names, not the thread
// using it.
unsafe impl Send for Window {}

impl Window {
    /// # Safety
    /// `base` must name at least `len` bytes of a live mapping, for as long as
    /// this window or any window derived from it is used.
    pub unsafe fn new(base: *mut u8, len: usize) -> Self {
        Self { base, len }
    }

    pub fn bytes(self) -> usize {
        self.len
    }

    /// The first byte, for a caller handing the range on as a slice.
    pub fn as_ptr(self) -> *mut u8 {
        self.base
    }

    /// `len` bytes of this window from `offset`.
    pub fn sub(self, offset: usize, len: usize) -> Self {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "window: {len} bytes at {offset:#x} run past {:#x}",
            self.len
        );
        // SAFETY: the assertion kept `offset + len` inside `self`, which its
        // constructor's contract says is mapped.
        Self { base: unsafe { self.base.add(offset) }, len }
    }

    fn at<T>(self, offset: usize) -> *mut T {
        assert!(
            offset.checked_add(size_of::<T>()).is_some_and(|end| end <= self.len),
            "window: a {}-byte access at {offset:#x} runs past {:#x}",
            size_of::<T>(),
            self.len
        );
        assert!(
            (self.base as usize + offset) % align_of::<T>() == 0,
            "window: a {}-byte access at {offset:#x} is not aligned for it",
            size_of::<T>()
        );
        // SAFETY: bounded and aligned by the two assertions above.
        unsafe { self.base.add(offset) as *mut T }
    }

    pub fn read<T: Copy>(self, offset: usize) -> T {
        // SAFETY: `at` bounded and aligned the pointer; volatile because the
        // other side may write the same bytes concurrently.
        unsafe { self.at::<T>(offset).read_volatile() }
    }

    pub fn write<T: Copy>(self, offset: usize, value: T) {
        // SAFETY: `at` bounded and aligned the pointer; volatile because the
        // other side may read the same bytes concurrently.
        unsafe { self.at::<T>(offset).write_volatile(value) }
    }

    /// Copy `out.len()` bytes from `offset` out, a word at a time, the whole
    /// range bounded once.
    ///
    /// # Panics
    /// Unless both `offset` and `out.len()` are multiples of eight and the
    /// window's base is word aligned.
    pub fn copy_out(self, offset: usize, out: &mut [u8]) {
        let from = self.words(offset, out.len());
        for (i, chunk) in out.chunks_exact_mut(8).enumerate() {
            // SAFETY: `words` bounded `from .. from + out.len()` inside the
            // window and aligned it; volatile because the other side may write
            // the same bytes concurrently.
            let word = unsafe { from.add(i).read_volatile() };
            chunk.copy_from_slice(&word.to_ne_bytes());
        }
    }

    /// Copy `data` in at `offset`, a word at a time; the same rule as
    /// [`Self::copy_out`].
    pub fn copy_in(self, offset: usize, data: &[u8]) {
        let to = self.words(offset, data.len());
        for (i, chunk) in data.chunks_exact(8).enumerate() {
            let word = u64::from_ne_bytes(chunk.try_into().expect("chunks of eight"));
            // SAFETY: as in `copy_out`.
            unsafe { to.add(i).write_volatile(word) };
        }
    }

    /// `len` bytes from `offset` as words, bounded and aligned once.
    fn words(self, offset: usize, len: usize) -> *mut u64 {
        assert!(offset % 8 == 0 && len % 8 == 0, "window: a copy of part of a word");
        let whole = self.sub(offset, len);
        assert!(whole.base as usize % align_of::<u64>() == 0, "window: a copy from an unaligned window");
        whole.base as *mut u64
    }

    /// Zero the whole window: only before the other side is told of it.
    pub fn zero(self) {
        // SAFETY: `self.len` bytes from `self.base`, the whole of what this
        // window covers.
        unsafe { core::ptr::write_bytes(self.base, 0, self.len) }
    }
}
