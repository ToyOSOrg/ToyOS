//! What the kernel decides without touching the machine: the scheduler core
//! ([`sched`]), the process and thread lifecycle ([`proclife`]), which PCID an
//! address space is handed ([`pcid`]) and what type the range registers give a
//! range ([`mtrr`]). The kernel binary links it; the host runs
//! its tests, because none of it reads a register, a clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod mtrr;
pub mod pcid;
pub mod proclife;
pub mod sched;
