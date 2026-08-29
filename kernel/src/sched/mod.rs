//! The kernel's half of the scheduler core; `toyos-sched` decides.

#![warn(clippy::undocumented_unsafe_blocks)]

pub mod driver;
pub mod dump;
pub mod kthread;
pub mod payload;
pub mod poison;
pub mod reap_gate;
pub mod waitqs;

/// Ceiling on CPUs the percpu arrays are sized for.
pub const MAX_CPUS: usize = 8;

// Duplicates `shootdown::MAX_CPUS`, pinned here because `kernel-loom` never compiles this file.
const _: () = assert!(MAX_CPUS == crate::shootdown::MAX_CPUS);
