use std::time::{Duration, Instant};

/// The round time past which a volume that answered every call is announced
/// slow. Nothing in this program ends a volume for slowness: only the device's
/// own refusal does.
///
/// **A policy number, and it says so**: nothing about the device supplies one.
/// Five seconds: long enough that a slow stick under a boot's worth of other
/// I/O is not called degraded, short enough that a person watching the console
/// learns about it while they are still watching.
pub const LOG_WRITE_BUDGET: Duration = Duration::from_secs(5);

/// How soon a volume that refused is asked again, and the longest the wait
/// doubles to. Policy: how often a dead volume is asked, and its refusal
/// said, against how soon one that answers again is written; the file loses
/// nothing either way while the replay holds what it is owed.
pub const RETRY_FIRST: Duration = Duration::from_secs(1);
pub const RETRY_MAX: Duration = Duration::from_secs(64);

/// When a volume that refused is next asked again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    at: Instant,
    wait: Duration,
}

impl Retry {
    /// A first refusal at `now`.
    pub fn after(now: Instant) -> Self {
        Self { at: now + RETRY_FIRST, wait: RETRY_FIRST }
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.at
    }

    /// Asked again at `now` and refused: twice the wait, to [`RETRY_MAX`].
    pub fn again(&mut self, now: Instant) {
        self.wait = (self.wait * 2).min(RETRY_MAX);
        self.at = now + self.wait;
    }
}

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

    #[test]
    fn a_refused_volume_is_asked_again_at_a_doubling_wait_to_a_bound() {
        let mut now = Instant::now();
        let mut retry = Retry::after(now);
        let mut waits = Vec::new();
        for _ in 0..9 {
            let next = retry.at;
            assert!(!retry.due(next - Duration::from_millis(1)));
            assert!(retry.due(next));
            waits.push((next - now).as_secs());
            now = next;
            retry.again(now);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 64, 64, 64]);
    }

    /// The console words, which the table in `main` is written against.
    #[test]
    fn every_step_names_itself() {
        assert_eq!(Step::Append.as_str(), "the append");
        assert_eq!(Step::Flush.as_str(), "the sync");
        assert_eq!(Step::TooSlow.as_str(), "the write");
    }
}
