//! The kernel ABI: struct layouts, syscall numbers, and the typed wrappers over
//! them. Completely unstable — userland reaches it through `toyos`, and the
//! kernel is the only other side.
//!
//! **A doc in this crate states what the ABI itself owns, and cites the
//! component by path for everything else.** A sentence asserting how the kernel
//! dispatches a call, or what another component keeps internally, is a claim
//! nothing here checks and somebody else's landing falsifies. A citation that
//! goes stale is a dead pointer — visible, and greppable; an assertion that
//! goes stale is a lie that still reads authoritative.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod audio;
pub mod boot;
pub mod clock;
pub mod handle;
pub mod hda;
pub mod inbox;
pub mod input;
pub mod inventory;
pub mod log;
pub mod part;
pub mod pci;
pub mod ring;
pub mod syscall;
pub mod virtio_sound;

pub use handle::{RawHandle, Rights, HANDLE_INVALID};

/// A process ID. Identifies a process — owns address space, handles, vruntime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(pub u32);

impl Pid {
    pub const MAX: Self = Pid(u32::MAX);
    pub fn raw(self) -> u32 { self.0 }
    pub fn from_raw(v: u32) -> Self { Pid(v) }
}

impl core::fmt::Display for Pid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl core::ops::Add for Pid {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { Pid(self.0 + rhs.0) }
}

/// A thread ID. Identifies a schedulable entity — goes in run queues.
/// Every process has at least one thread (the main thread).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tid(pub u32);

impl Tid {
    pub const MAX: Self = Tid(u32::MAX);
    pub fn raw(self) -> u32 { self.0 }
    pub fn from_raw(v: u32) -> Self { Tid(v) }
}

impl core::fmt::Display for Tid {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl core::ops::Add for Tid {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { Tid(self.0 + rhs.0) }
}

/// GPU framebuffer info passed between kernel and userland.
/// Shared definition so both sides agree on the layout.
///
/// **The three handles are installed by the read that answers this.** A
/// description is a set of buffers, and the process being told about them is
/// the one that must be able to map them — which is never the process that
/// minted the claim, because `/bin/init` mints every claim and holds none.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct FramebufferInfo {
    pub scanout: [RawHandle; 2],
    pub cursor: RawHandle,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: u32,
    pub flags: u32,
}

/// Every byte belongs to a field: this crosses the boundary through
/// `as_bytes`, so a gap would publish whatever the kernel stack held. Every
/// field here is a `u32` or a `repr(transparent)` wrapper over one, so the
/// `repr(C)` layout has no padding without needing a separate size check.
impl FramebufferInfo {
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `self` is a valid `&Self` (non-null, aligned, readable for
        // `size_of::<Self>()` bytes) and, per the doc comment above, every
        // byte the slice exposes is an initialized field, not a padding gap.
        unsafe {
            core::slice::from_raw_parts(self as *const Self as *const u8, core::mem::size_of::<Self>())
        }
    }
}

// SAFETY: FramebufferInfo is #[repr(C)] and every field is a u32 or a
// `repr(transparent)` wrapper over one — no padding, no pointers.
unsafe impl Sync for FramebufferInfo {}
// SAFETY: see the `Sync` impl immediately above — the same reasoning (no
// pointers, no interior mutability) covers `Send`.
unsafe impl Send for FramebufferInfo {}

/// Where a thread finds its own [`Tid`]: the kernel writes it into the thread
/// control block at `TP + TCB_TID` before the thread's first instruction.
/// `TP + 0` is the psABI's self-pointer and `TP + 8` the DTV pointer; the word
/// is the thread's own memory, so what it says is the thread's word about
/// itself and nothing more.
pub const TCB_TID: usize = 16;

/// The calling thread's id, read off its control block without a syscall.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn current_tid() -> Tid {
    let tid: u32;
    // SAFETY: `fs` is this thread's TP, set by the kernel at the thread's
    // start, and `TP + TCB_TID` lies inside the 64-byte TCB the kernel
    // reserves there; a 4-byte load of it touches nothing else.
    unsafe {
        core::arch::asm!(
            "mov {tid:e}, dword ptr fs:[{at}]",
            tid = out(reg) tid,
            at = const TCB_TID,
            options(nostack, readonly, preserves_flags),
        );
    }
    Tid(tid)
}

/// The calling thread's id, read off its control block without a syscall.
#[cfg(target_arch = "aarch64")]
#[inline]
pub fn current_tid() -> Tid {
    let tp: u64;
    // SAFETY: a read of `TPIDR_EL0`, the thread pointer, into a register.
    unsafe { core::arch::asm!("mrs {tp}, tpidr_el0", tp = out(reg) tp, options(nomem, nostack)) };
    // SAFETY: `TP + TCB_TID` lies inside the TCB the kernel reserves at TP.
    Tid(unsafe { core::ptr::read_volatile((tp as usize + TCB_TID) as *const u32) })
}
