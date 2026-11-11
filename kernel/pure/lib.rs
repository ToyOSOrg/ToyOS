//! What the kernel decides without touching the machine. The kernel binary
//! links it; the host runs its tests, because none of it reads a register, a
//! clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod bootwrite;
pub mod clock;
pub mod cpu;
pub mod dmaguard;
pub mod event;
pub mod frames;
pub mod iova;
pub mod message;
pub mod microcode;
pub mod mtrr;
pub mod owners;
pub mod chunked;
pub mod claimspace;
pub mod paged;
pub mod pcid;
pub mod pipe;
pub mod pointers;
pub mod rmrr;
pub mod sched;
/// Measurement scout, never landed: the kernel binary's probe, reached from here.
pub mod scout {
    #[cfg(feature = "scout-seg")]
    unsafe extern "Rust" {
        fn __scout_at(id: u16);
    }
    #[inline(always)]
    pub fn at(_id: u16) {
        // SAFETY: the kernel binary defines it; it touches only its own tables.
        #[cfg(feature = "scout-seg")]
        unsafe {
            __scout_at(_id)
        }
    }
}
pub mod vm;
pub mod wx;
pub mod xsave;
