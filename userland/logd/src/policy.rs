//! What a refused batch does to this boot's log volume.
//!
//! **One pure function, because this is the whole refused-batch policy and it used to be
//! `if let Err(_) = … { volume = None }`.** Everything above [`fate`] is I/O
//! and everything below it is what the console says; the decision itself is a
//! function of what a host test can hand it, which is why this file exists
//! rather than the two lines it replaces living in the loop.

use std::time::Duration;

/// The round time past which a volume that answered every call is announced
/// slow. Nothing in this program ends a volume for slowness: only the device's
/// own refusal does.
///
/// **A policy number, and it says so**: nothing about the device supplies one.
/// Five seconds: long enough that a slow stick under a boot's worth of other
/// I/O is not called degraded, short enough that a person watching the console
/// learns about it while they are still watching.
pub const LOG_WRITE_BUDGET: Duration = Duration::from_secs(5);

/// Which call refused.
///
/// Named rather than a `&str`, because [`fate`] matches on it: the append and
/// the flush have different answers and a string cannot be matched
/// exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// One of the batch's lines did not reach the file.
    Append,
    /// The bytes are in the file and did not reach the device.
    Flush,
    /// Everything answered, and the round took longer than
    /// [`LOG_WRITE_BUDGET`].
    TooSlow,
}

impl Step {
    /// The word this program's console line uses for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "the append",
            Self::Flush => "the sync",
            Self::TooSlow => "the write",
        }
    }
}

/// What happens to this boot's volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Keep it, publish this batch — every call answered, so the records are
    /// durable — and say once that the volume is slow. The state the whole
    /// slow-vs-failed split exists for: degraded is not dead, and a log that
    /// arrives late beats a log that was thrown away.
    Degraded,
    /// Stop feeding the volume for the rest of the boot. The log is on the
    /// console only from here.
    GiveUp,
}

/// The decision, from the call that refused.
pub fn fate(step: Step) -> Fate {
    match step {
        // Every call answered: the records are durable, however long the round
        // took, and a volume is never ended on elapsed time.
        Step::TooSlow => Fate::Degraded,
        Step::Append | Step::Flush => Fate::GiveUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stick that cannot flush ends the boot's log: the writes are not
    /// durable and no retry makes them so. A refused append is a hole in the
    /// file: the records have left the cursor and there is nothing to
    /// re-write.
    #[test]
    fn a_refused_append_or_flush_ends_the_volume() {
        assert_eq!(fate(Step::Flush), Fate::GiveUp);
        assert_eq!(fate(Step::Append), Fate::GiveUp);
    }

    /// A volume that answered every call and took too long over it is
    /// degraded, never dead: the records are durable and elapsed time is not
    /// one of the evidences a volume may be declared failed on.
    #[test]
    fn a_slow_round_degrades_and_keeps_the_volume() {
        assert_eq!(fate(Step::TooSlow), Fate::Degraded);
    }

    /// The console words, which the table in `main` is written against.
    #[test]
    fn every_step_names_itself() {
        assert_eq!(Step::Append.as_str(), "the append");
        assert_eq!(Step::Flush.as_str(), "the sync");
        assert_eq!(Step::TooSlow.as_str(), "the write");
    }
}
