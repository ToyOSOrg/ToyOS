//! ToyOS scheduler core — a sans-IO state machine driven by both the kernel and
//! the host simulator through one [`hw::Hw`] boundary. Nothing here reads a
//! clock, sends an IPI or touches a register: every effect is a value the caller
//! is handed and has to perform, which is what lets the same sources run under a
//! simulator and under loom.
//!
//! Two host-side harnesses compile against these same sources: the simulator
//! (`kernel/sim/`), which explores the protocol against invariants
//! I1–I13, and the `kernel-loom` package (`kernel/loom/`), which
//! swaps in loom's instrumented atomics — see [`sync`] for why loom is a
//! separate package rather than a `cfg(loom)` dependency.

#![deny(unsafe_code)]

pub mod cpu;
pub mod fair;
pub mod hw;
pub mod invariants;
pub mod mailbox;
pub mod msg;
pub mod park;
pub mod queue;
pub mod retire;
pub mod sync;
pub mod task;
pub mod timer;
pub mod watch;
#[cfg(test)]
mod watch_list;
pub mod windows;
