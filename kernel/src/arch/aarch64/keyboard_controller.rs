//! The platform's own keyboard controller. An Arm machine has none: its
//! keyboards are USB, and the panic panel's key poll reads nothing here.

/// Whether the panic path reads a key here.
pub const PANIC_KEYS: bool = false;

/// No byte ever waits.
pub fn poll_byte() -> Option<(u8, bool)> {
    None
}

/// Nothing to decide about a controller that is not there.
pub fn verdict_due() -> bool {
    false
}

/// Nothing to service.
pub fn service() {}

/// Nothing to report.
pub fn report_line() {}
