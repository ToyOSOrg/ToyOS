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
pub mod dirnodes;
pub mod microcode;
pub mod mtrr;
pub mod pcid;
pub mod pipe;
pub mod pointers;
pub mod proclife;
pub mod sched;
pub mod xsave;
