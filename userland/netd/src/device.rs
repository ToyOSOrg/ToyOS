//! What both NIC drivers need from the substrate beside the SDK's
//! `toyos::volatile::Window`: the kernel's word for a call a bring-up cannot go
//! on without, and the latch a diagnostic is printed on.

use std::cell::Cell;

use toyos_abi::syscall::SyscallError;

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
