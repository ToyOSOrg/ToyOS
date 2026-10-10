//! What the kernel decides without touching the machine: the scheduler core
//! ([`sched`]), the process and thread lifecycle ([`proclife`]), which PCID an
//! address space is handed ([`pcid`]), what every x86-64 CPU's `IA32_EFER`
//! holds ([`efer`]), what type the range registers give a
//! range ([`mtrr`]), what a pipe's ends are told of each other ([`pipe`]) and
//! what a CPU waiting on the boot processor's write to `SMI_CMD` decides
//! ([`bootwrite`]).
//! The kernel binary links it; the host runs
//! its tests, because none of it reads a register, a clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod bootwrite;
pub mod efer;
pub mod mtrr;
pub mod pcid;
pub mod pipe;
pub mod proclife;
pub mod sched;
