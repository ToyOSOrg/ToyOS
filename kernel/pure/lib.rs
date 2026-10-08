//! What the kernel decides without touching the machine: the scheduler core
//! ([`sched`]), the process and thread lifecycle ([`proclife`]), which PCID an
//! address space is handed ([`pcid`]) and what a pipe's ends are told of each
//! other ([`pipe`]). The kernel binary links it; the host runs
//! its tests, because none of it reads a register, a clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod pcid;
pub mod pipe;
pub mod proclife;
pub mod sched;
