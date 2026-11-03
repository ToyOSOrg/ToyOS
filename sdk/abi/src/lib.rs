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

mod arch;
pub mod acpi;
pub mod audio;
pub mod boot;
pub mod clock;
pub mod counters;
pub mod devset;
pub mod end;
pub mod handle;
pub mod inbox;
pub mod input;
pub mod inventory;
pub mod log;
pub mod pci;
pub mod quit;
pub mod syscall;
pub mod trace;
pub mod usersafe;

pub use handle::{RawHandle, Rights, HANDLE_INVALID};
pub use usersafe::UserSafe;

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

crate::user_safe! {
    /// The firmware's framebuffer, as its claim describes it.
    ///
    /// **The scanout handle is installed by the read that answers this**, in
    /// the process that reads it: the supervisor mints every claim and holds
    /// none.
    #[derive(Clone, Copy)]
    pub struct FramebufferInfo {
        pub scanout: RawHandle,
        pub width: u32,
        pub height: u32,
        pub stride: u32,
        pub pixel_format: u32,
        /// The panel's EDID base block as the loader handed it over, unparsed,
        /// and how many of these bytes it is: [`boot::EDID_BLOCK`] or zero.
        pub edid: [u8; boot::EDID_BLOCK],
        pub edid_len: u32,
    }
}

// SAFETY: FramebufferInfo is #[repr(C)] and every field is a u32, bytes, or a
// `repr(transparent)` wrapper over a u32 — no padding, no pointers.
unsafe impl Sync for FramebufferInfo {}
// SAFETY: see the `Sync` impl immediately above — the same reasoning (no
// pointers, no interior mutability) covers `Send`.
unsafe impl Send for FramebufferInfo {}

/// Where each architecture's thread control block keeps the thread's [`Tid`],
/// as an offset from TP, inside the TCB its psABI puts there.
pub mod tcb {
    /// x86-64, TLS variant II: the TCB is at and above TP, `TP + 0` the
    /// psABI's self-pointer and `TP + 8` the DTV pointer, inside the
    /// [`X86_64_BYTES`] the kernel reserves; TLS data is below TP.
    pub const X86_64_TID: usize = 16;
    pub const X86_64_BYTES: usize = 64;
    /// AArch64, TLS variant I: the TCB is the [`AARCH64_BYTES`] at TP, `TP + 0`
    /// the DTV pointer and `TP + 8` the word the psABI leaves to the
    /// implementation, which is this; the first TLS block starts at
    /// `TP + AARCH64_BYTES`, so nothing of the TCB may lie there.
    pub const AARCH64_TID: usize = 8;
    pub const AARCH64_BYTES: usize = 16;

    /// Each tid word lies inside its TCB and on none of the words its psABI
    /// already names: past x86-64's self-pointer and DTV pointer, and past
    /// AArch64's DTV pointer and wholly below its first TLS block.
    const TID: usize = core::mem::size_of::<u32>();
    const _: () = assert!(X86_64_TID >= 16 && X86_64_TID + TID <= X86_64_BYTES);
    const _: () = assert!(AARCH64_TID >= 8 && AARCH64_TID + TID <= AARCH64_BYTES);
    const _: () = assert!(AARCH64_BYTES == 16, "AArch64's psABI TCB is two words");
}

pub use arch::{current_tid, ioport, processor_id, TCB_TID};

/// An x86 processor's CPUID identity (Intel SDM Vol. 2A, "CPUID"; AMD APM
/// Vol. 3, CPUID Fn0000_0000 and Fn0000_0001_EAX).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ProcessorId {
    /// Leaf 0's vendor string: its EBX, EDX and ECX bytes, in that order.
    pub vendor: [u8; 12],
    /// Leaf 1's EAX: the family, model and stepping signature.
    pub signature: u32,
}
