//! The I/O port space: x86-64 has one, reached by `in` and `out`.

/// Whether this architecture has an I/O port space at all. Firmware tables
/// that name a port are only honoured where it does.
pub const EXISTS: bool = true;

pub use super::cpu::{outb, outw};
