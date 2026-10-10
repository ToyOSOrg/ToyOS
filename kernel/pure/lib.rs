//! What the kernel decides without touching the machine: the scheduler core
//! ([`sched`]), the process and thread lifecycle ([`proclife`]), which PCID an
//! address space is handed ([`pcid`]), what type the range registers give a
//! range ([`mtrr`]), what a pipe's ends are told of each other ([`pipe`]), what
//! a CPU waiting on the boot processor's write to `SMI_CMD` decides
//! ([`bootwrite`]) and what the clock computes from counter ticks ([`clock`]).
//! The kernel binary links it; the host runs
//! its tests, because none of it reads a register, a clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod bootwrite;
pub mod clock;
pub mod mtrr;
pub mod pcid;
pub mod pipe;
pub mod proclife;
pub mod sched;
