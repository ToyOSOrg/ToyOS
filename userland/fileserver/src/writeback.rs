//! When the sync nobody asked for is due: [`WRITEBACK`] after the first write
//! no sync has answered for, and again after a sync that left a file unwritten.

use std::time::{Duration, Instant};

/// How long a write waits for a sync nobody asked for. Policy: the kernel's
/// write-back drained a closed file's pages on its next pass.
pub const WRITEBACK: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct WriteBack {
    since: Option<Instant>,
}

impl WriteBack {
    /// The volume was written at `now`.
    pub fn dirtied(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    /// How long until it is due, while one is owed.
    pub fn left(&self, now: Instant) -> Option<Duration> {
        self.since.map(|at| WRITEBACK.saturating_sub(now.duration_since(at)))
    }

    /// Whether it is due at `now`; one that is, is spent.
    pub fn take_due(&mut self, now: Instant) -> bool {
        let due = self.since.is_some_and(|at| now.duration_since(at) >= WRITEBACK);
        if due {
            self.since = None;
        }
        due
    }

    /// A sync answered at `now`: one that left a file unwritten keeps the
    /// write-back due, so the next one tries it again.
    pub fn synced(&mut self, unwritten: bool, now: Instant) {
        self.since = unwritten.then_some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_that_left_a_file_unwritten_is_due_again() {
        let t0 = Instant::now();
        let mut wb = WriteBack::default();
        assert_eq!(wb.left(t0), None, "nothing written, nothing owed");
        wb.dirtied(t0);
        wb.dirtied(t0 + WRITEBACK / 2);
        assert!(!wb.take_due(t0 + WRITEBACK / 2), "due from the first write, not the last");
        assert!(wb.take_due(t0 + WRITEBACK));
        assert_eq!(wb.left(t0 + WRITEBACK), None, "spent");

        let t1 = t0 + 2 * WRITEBACK;
        wb.synced(true, t1);
        assert_eq!(wb.left(t1), Some(WRITEBACK));
        assert!(wb.take_due(t1 + WRITEBACK), "tried again");
        wb.synced(false, t1 + WRITEBACK);
        assert_eq!(wb.left(t1 + WRITEBACK), None, "every file written");
    }
}
