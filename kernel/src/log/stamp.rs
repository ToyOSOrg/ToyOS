//! A log line's time, the clock a record's `at_ns` is on: nanoseconds since
//! the counter's zero (`crate::clock::stamp`). A type of its own because
//! `clock::nanos_since_boot` counts the same nanoseconds from another zero,
//! and a window over the log taken on that clock lands that far from the
//! records it means.

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LogStamp(u64);

impl LogStamp {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u64::MAX);

    /// `nanos` since the counter's zero: a record's `at_ns`, or the clock's
    /// own reading of it.
    pub const fn since_zero(nanos: u64) -> Self {
        Self(nanos)
    }

    pub const fn nanos(self) -> u64 {
        self.0
    }
}
