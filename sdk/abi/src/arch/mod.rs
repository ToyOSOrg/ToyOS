//! What a process reads off its own CPU without a syscall, one module per
//! architecture: the counter the clock page describes, `in` and `out` where
//! there is a port space, and the processor's CPUID identity.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::{counter, ioport, processor_id};
#[cfg(target_arch = "x86_64")]
pub use x86_64::{counter, ioport, processor_id};
