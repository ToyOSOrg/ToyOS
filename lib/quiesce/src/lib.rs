//! The supervisor's stop: how long it waits on each step before it asks the
//! kernel to end the machine, and what a disk driver is told the machine
//! stops by. The kernel waits for none of it.

#![no_std]
#![forbid(unsafe_code)]

/// A policy number: how long the supervisor waits on one call into a file server that
/// is alive and has not answered — and so how long a stop request can wait
/// behind one, since the supervisor serves `power` between its loop's passes.
pub const FILES_MS: u64 = 30_000;

/// A policy number: how long the supervisor waits for `logkeeper`'s flush before a stop.
pub const FLUSH_MS: u64 = 5_000;

/// A policy number: how long the supervisor waits for every file server's sync, run
/// together after the flush, before a stop.
pub const SYNC_MS: u64 = 5_000;

/// A policy number: how long the supervisor waits for every disk driver to end once
/// told the machine stops, after the sync. Above usbd's own bound on its stop
/// (`toyos_xhci::hc::STOP_NS`), so a driver that keeps its bound is waited out.
pub const DRIVERS_MS: u64 = 3_000;

/// The endowment a disk driver is told the machine stops by: a bell whose
/// waker the supervisor closes once every file server has synced; it is
/// readable from then. The driver hands its devices back, leaves its
/// controller as firmware would find it, and ends.
pub const DRIVER_STOP: &str = "stop";
