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

#[cfg(test)]
mod tests {
    use super::*;

    /// The console words, which the table in `main` is written against.
    #[test]
    fn every_step_names_itself() {
        assert_eq!(Step::Append.as_str(), "the append");
        assert_eq!(Step::Flush.as_str(), "the sync");
        assert_eq!(Step::TooSlow.as_str(), "the write");
    }
}
