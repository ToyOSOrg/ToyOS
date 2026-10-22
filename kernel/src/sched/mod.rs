//! The kernel's half of the scheduler core; `kernel::sched` decides.

#![warn(clippy::undocumented_unsafe_blocks)]

pub mod driver;
pub mod kthread;
pub mod payload;
pub mod reap_gate;
pub mod futex;
pub mod idle_stack;
