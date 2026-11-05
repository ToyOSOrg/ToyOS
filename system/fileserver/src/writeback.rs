//! When the sync nobody asked for is due: [`WRITEBACK`] after the first write
//! no sync has answered for, and again after a sync that was refused, whole or
//! for one file, at an interval that doubles with each refusal up to
//! [`REFUSED_MAX`].

use std::time::{Duration, Instant};

/// How long a write waits for a sync nobody asked for. Policy: the kernel's
/// write-back drained a closed file's pages on its next pass.
pub const WRITEBACK: Duration = Duration::from_secs(2);

/// The longest a refused sync waits to be tried again. Policy: a device that
/// keeps refusing is said about once a minute, not once per [`WRITEBACK`].
pub const REFUSED_MAX: Duration = Duration::from_secs(64);

#[derive(Default)]
pub struct WriteBack {
    since: Option<Instant>,
    /// The interval since the last refusal; `None` is [`WRITEBACK`].
    refused: Option<Duration>,
}

impl WriteBack {
    /// The volume was written at `now`.
    pub fn dirtied(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    fn interval(&self) -> Duration {
        self.refused.unwrap_or(WRITEBACK)
    }

    /// How long until it is due, while one is owed.
    pub fn left(&self, now: Instant) -> Option<Duration> {
        self.since.map(|at| self.interval().saturating_sub(now.duration_since(at)))
    }

    /// Whether it is due at `now`; one that is, is spent.
    pub fn take_due(&mut self, now: Instant) -> bool {
        let due = self.since.is_some_and(|at| now.duration_since(at) >= self.interval());
        if due {
            self.since = None;
        }
        due
    }

    /// A sync made everything written durable: nothing is owed.
    pub fn synced(&mut self) {
        *self = Self::default();
    }

    /// A sync at `now` was refused, whole or for a file: the write-back stays
    /// owed, after twice the last refusal's interval.
    pub fn refused(&mut self, now: Instant) {
        self.refused = Some(self.refused.map_or(WRITEBACK, |last| (last * 2).min(REFUSED_MAX)));
        self.since = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_back_is_due_from_the_first_write_and_spent_when_taken() {
        let t0 = Instant::now();
        let mut wb = WriteBack::default();
        assert_eq!(wb.left(t0), None, "nothing written, nothing owed");
        wb.dirtied(t0);
        wb.dirtied(t0 + WRITEBACK / 2);
        assert!(!wb.take_due(t0 + WRITEBACK / 2), "due from the first write, not the last");
        assert!(wb.take_due(t0 + WRITEBACK));
        assert_eq!(wb.left(t0 + WRITEBACK), None, "spent");
    }

    /// **A refused write-back stays owed**, though nothing writes again, and
    /// a device that keeps refusing is tried at a doubling interval that
    /// stops at [`REFUSED_MAX`]; a sync that lands resets it.
    #[test]
    fn a_refused_sync_stays_due_at_a_backed_off_interval() {
        let mut now = Instant::now();
        let mut wb = WriteBack::default();
        wb.dirtied(now);
        now += WRITEBACK;
        assert!(wb.take_due(now));
        let mut intervals = Vec::new();
        for _ in 0..8 {
            wb.refused(now);
            let left = wb.left(now).expect("a refused sync is still owed");
            intervals.push(left.as_secs());
            assert!(!wb.take_due(now + left - Duration::from_millis(1)));
            now += left;
            assert!(wb.take_due(now));
        }
        assert_eq!(intervals, [2, 4, 8, 16, 32, 64, 64, 64]);

        wb.synced();
        assert_eq!(wb.left(now), None, "every write durable");
        wb.dirtied(now);
        assert_eq!(wb.left(now), Some(WRITEBACK), "the next write waits the ordinary interval");
    }
}
