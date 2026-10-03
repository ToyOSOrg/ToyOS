//! What the kernel decides without touching the machine: the scheduler core
//! ([`sched`]), the process and thread lifecycle ([`proclife`]) and which PCID an
//! address space is handed ([`pcid`]). The kernel binary links it; the host runs
//! its tests, because none of it reads a register, a clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod pcid;
pub mod proclife;
pub mod sched;
