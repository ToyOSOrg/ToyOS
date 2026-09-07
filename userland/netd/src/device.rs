//! What both NIC drivers need from the substrate, in one place: a
//! bounds-checked volatile window over memory a device also touches, the
//! kernel's word for a call a bring-up cannot go on without, and the latch a
//! diagnostic is printed on.
//!
//! Neither driver may grow its own: a second window with a weaker bound is the
//! one that is reached on the boot nobody watched.

use std::cell::Cell;

use toyos_abi::syscall::SyscallError;

/// A bounds-checked volatile window over memory something other than this
/// thread also reads or writes — a register aperture, or a ring the device
/// walks.
///
/// Volatile because both race this process: a plain read of a used-ring index
/// can be hoisted out of the loop that waits on it, and a plain write to a
/// doorbell can be elided entirely. Every bound is an `assert!` and not a
/// `debug_assert!`: the build that ships is the release one.
#[derive(Clone, Copy)]
pub struct Window {
    base: *mut u8,
    len: usize,
}

impl Window {
    /// # Safety
    /// `base` must name at least `len` bytes of a live mapping, for as long as
    /// this window or any window derived from it is used.
    pub unsafe fn new(base: *mut u8, len: usize) -> Self {
        Self { base, len }
    }

    pub fn len(self) -> usize {
        self.len
    }

    /// The first byte, for a caller handing the range on as a slice.
    pub fn as_ptr(self) -> *mut u8 {
        self.base
    }

    pub fn sub(self, offset: usize, len: usize) -> Self {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "netd: a {len}-byte window at {offset:#x} runs past {:#x}",
            self.len
        );
        // SAFETY: the assertion above kept `offset + len` inside `self`, which
        // its own constructor's contract says is mapped.
        Self { base: unsafe { self.base.add(offset) }, len }
    }

    pub fn at<T>(self, offset: usize) -> *mut T {
        assert!(
            offset.checked_add(size_of::<T>()).is_some_and(|end| end <= self.len),
            "netd: a {}-byte access at {offset:#x} runs past {:#x}",
            size_of::<T>(),
            self.len
        );
        assert!(
            (self.base as usize + offset) % align_of::<T>() == 0,
            "netd: a {}-byte access at {offset:#x} is not aligned for it",
            size_of::<T>(),
        );
        // SAFETY: bounded and aligned by the two assertions above.
        unsafe { self.base.add(offset) as *mut T }
    }

    pub fn read<T: Copy>(self, offset: usize) -> T {
        // SAFETY: `at` bounded and aligned the pointer; volatile because the
        // device may write the same bytes concurrently.
        unsafe { self.at::<T>(offset).read_volatile() }
    }

    pub fn write<T: Copy>(self, offset: usize, value: T) {
        // SAFETY: `at` bounded and aligned the pointer; volatile because the
        // device may read the same bytes concurrently.
        unsafe { self.at::<T>(offset).write_volatile(value) }
    }

    pub fn zero(self) {
        // SAFETY: `self.len` bytes from `self.base`, which is the whole of what
        // this window covers.
        unsafe { std::ptr::write_bytes(self.base, 0, self.len) }
    }
}

/// The kernel refused a call a bring-up cannot go on without, and the word is
/// the kernel's own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KernelRefused {
    pub call: &'static str,
    pub why: SyscallError,
}

impl KernelRefused {
    pub fn on(call: &'static str) -> impl Fn(SyscallError) -> Self {
        move |why| Self { call, why }
    }
}

impl std::fmt::Display for KernelRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the kernel refused {}: {:?}", self.call, self.why)
    }
}

/// What a diagnostic last said, so it says it again only on a change.
///
/// **On change, not per element**: a device flooding a ring with descriptors a
/// driver will not act on costs one line, not one per descriptor, which is the
/// difference between a diagnostic and a way to drown the console from the
/// other side of the boundary.
pub struct Latch<T: Copy + PartialEq>(Cell<T>);

impl<T: Copy + PartialEq + Default> Default for Latch<T> {
    fn default() -> Self {
        Self(Cell::new(T::default()))
    }
}

impl<T: Copy + PartialEq> Latch<T> {
    /// The previous value if `now` is not it, and `None` if nothing has moved.
    pub fn moved(&self, now: T) -> Option<T> {
        let was = self.0.replace(now);
        (was != now).then_some(was)
    }
}
