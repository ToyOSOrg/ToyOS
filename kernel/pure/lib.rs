//! What the kernel decides without touching the machine: the scheduler core
//! ([`sched`]), the process and thread lifecycle ([`proclife`]), which PCID an
//! address space is handed ([`pcid`]), what type the range registers give a
//! range ([`mtrr`]), what a pipe's ends are told of each other ([`pipe`]), what
//! a CPU waiting on the boot processor's write to `SMI_CMD` decides
//! ([`bootwrite`]), what the clock computes from counter ticks ([`clock`]),
//! which number a kernel volume's directory answers ([`dirnodes`]), which
//! number a USB pointer's records carry ([`pointers`]) and which
//! state components a CPU's `XCR0` enables ([`xsave`]).
//! The kernel binary links it; the host runs
//! its tests, because none of it reads a register, a clock or a kernel lock.

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod bootwrite;
pub mod clock;
pub mod dirnodes;
pub mod mtrr;
pub mod pcid;
pub mod pipe;
pub mod pointers;
pub mod proclife;
pub mod sched;
pub mod xsave;
