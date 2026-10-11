//! What a process reads off its own CPU without a syscall, one module per
//! architecture: the counter the clock page describes, the thread's id in
//! its thread control block, and `in` and `out` where there is a port space.

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::{counter, current_tid, ioport, TCB_TID};
#[cfg(target_arch = "x86_64")]
pub use x86_64::{counter, current_tid, ioport, TCB_TID};
