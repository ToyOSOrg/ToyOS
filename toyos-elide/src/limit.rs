//! A whole record elided: how many one site may say in a window, and what the
//! record after a suppression says about it.
//!
//! **A storm from one site is a storm in the log, and the log is shared.** A
//! thread-churn loop makes one `exit:` record per thread; a program printing in
//! a loop fills its ring and then the volume. Past [`Limit`]'s burst in a
//! window a site's records are suppressed and counted, and the next record it
//! is allowed carries the count, so nothing goes silently: the last record a
//! site says before suppressing says so, and the first it says after it says
//! how many.
//!
//! Atomic, so the kernel's call sites — any CPU, any context `log!` runs in —
//! share one per site with no lock; one reader-thread caller uses it the same
//! way. The counts are exact; which of two racing records is the one a window
//! admits last is not.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// One site's allowance.
pub struct Limit {
    burst: u64,
    window_ns: u64,
    /// When the current window began.
    began: AtomicU64,
    /// Records admitted or refused in the current window.
    seen: AtomicU64,
    /// Records refused and not yet reported.
    suppressed: AtomicU64,
}

/// What one record may do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admit {
    /// Say it. `suppressed` records from this site went unsaid before it, and
    /// `last` is whether it is the last this window admits.
    Say { suppressed: u64, last: bool },
    /// Count it and say nothing.
    Suppress,
}

impl Limit {
    /// `burst` records per `window_ns`; `burst` at least one.
    pub const fn new(burst: u64, window_ns: u64) -> Self {
        assert!(burst >= 1, "a limit that admits nothing is not a limit");
        Self {
            burst,
            window_ns,
            began: AtomicU64::new(0),
            seen: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
        }
    }

    /// Whether a record at `now_ns` is said.
    pub fn admit(&self, now_ns: u64) -> Admit {
        let began = self.began.load(Relaxed);
        if now_ns.saturating_sub(began) >= self.window_ns
            && self.began.compare_exchange(began, now_ns, Relaxed, Relaxed).is_ok()
        {
            // This record opens the window, so it is the one that reports what
            // the last one refused.
            self.seen.store(1, Relaxed);
            let suppressed = self.suppressed.swap(0, Relaxed);
            return Admit::Say { suppressed, last: self.burst == 1 };
        }
        let seen = self.seen.fetch_add(1, Relaxed) + 1;
        if seen <= self.burst {
            return Admit::Say { suppressed: 0, last: seen == self.burst };
        }
        self.suppressed.fetch_add(1, Relaxed);
        Admit::Suppress
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: u64 = 1_000_000_000;

    /// A burst is said whole and its last record says it is the last; past it
    /// records are counted; the next window's first record carries the count.
    #[test]
    fn a_storm_is_said_to_its_burst_counted_after_and_reported_once() {
        let limit = Limit::new(3, SECOND);
        let t = 5 * SECOND;
        assert_eq!(limit.admit(t), Admit::Say { suppressed: 0, last: false });
        assert_eq!(limit.admit(t + 1), Admit::Say { suppressed: 0, last: false });
        assert_eq!(limit.admit(t + 2), Admit::Say { suppressed: 0, last: true });
        for i in 0..40 {
            assert_eq!(limit.admit(t + 3 + i), Admit::Suppress);
        }
        assert_eq!(limit.admit(t + SECOND), Admit::Say { suppressed: 40, last: false });
        assert_eq!(limit.admit(t + SECOND + 1), Admit::Say { suppressed: 0, last: false });
    }

    /// A quiet site is never limited: one record a window is always said.
    #[test]
    fn a_quiet_site_is_never_limited() {
        let limit = Limit::new(2, SECOND);
        for i in 1..100 {
            assert_eq!(limit.admit(i * SECOND), Admit::Say { suppressed: 0, last: false });
        }
    }

    /// Four threads storm one site: every record is said or counted, and the
    /// count reported afterwards is exactly what was refused.
    #[test]
    fn racing_sites_count_exactly() {
        use std::sync::Arc;
        let limit = Arc::new(Limit::new(10, u64::MAX));
        assert_eq!(limit.admit(1), Admit::Say { suppressed: 0, last: false });
        let threads: std::vec::Vec<_> = (0..4)
            .map(|_| {
                let limit = Arc::clone(&limit);
                std::thread::spawn(move || {
                    (0..1000).filter(|_| matches!(limit.admit(2), Admit::Say { .. })).count()
                })
            })
            .collect();
        let said: usize = threads.into_iter().map(|t| t.join().unwrap()).sum();
        assert_eq!(said, 9, "the window admits its burst and no more");
        assert_eq!(limit.suppressed.load(Relaxed), 4000 - 9);
    }
}
