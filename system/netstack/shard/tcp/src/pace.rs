//! Pacing (RFC 9002 §7.7): new data leaves at `N · cwnd / srtt` through a token bucket, and every
//! data segment sent spends from it.
//!
//! The bucket holds what the rate brings in [`GRANULARITY`], at most half of cwnd, and never less
//! than the initial window. §7.7 bounds a burst at the initial window unless the path is known to
//! absorb more. The millisecond departs from that so that a fast sender's bursts, and its wakes,
//! are sized by time rather than by the initial window. The half-window bound keeps the bucket
//! below cwnd at any srtt, so a window past the initial one always leaves in more than one burst.
//! That is what lets HyStart++ take L = infinity (RFC 9406 §4.3) whatever the path's round trip.
//!
//! Some data spends from the bucket but is never held:
//! - retransmissions: recovery's pipe, and the window after a timeout from one segment, bound
//!   them;
//! - new data in SACK recovery: RFC 6675's pipe and the ACK clock bound it, as they do
//!   retransmissions;
//! - a loss probe of new data: RFC 8985 §7.3's single segment at the PTO, which a hold would only
//!   delay;
//! - a persist probe of new data: one segment into a closed window (RFC 9293 §3.8.6.1).
//!
//! A held sender is offered its data again once the bucket holds half its capacity: it wakes once
//! per half the bucket's refill time, and a wake up to that much late loses no rate. That is half
//! a [`GRANULARITY`] while the millisecond sets the bucket, less where half the window sets it,
//! and more where the initial window does. In a small window the wait is half the initial
//! window's time at the rate, even when one segment would do. Without a smoothed RTT nothing is
//! paced; cwnd is at most the initial window until the first sample.

use core::num::{NonZeroU128, NonZeroU64};
use core::time::Duration;

use crate::cc::{initial_window, Cc};
use crate::Instant;

/// What a full bucket holds at the pacing rate, while that is under half of cwnd and over the
/// initial window: the span that sizes a paced sender's bursts and its wakes.
const GRANULARITY: Duration = Duration::from_millis(1);

/// RFC 9002 §7.7's N, in quarters, where cwnd holds across the round trip.
const N_AVOIDANCE: u64 = 5;
/// Slow start doubles cwnd across the round trip, so the round's data is twice the window it
/// began with: a smaller N would hold the doubling back.
const N_SLOW_START: u64 = 8;

/// `bytes` every `per_ns`.
#[derive(Clone, Copy, Debug)]
struct Rate {
    bytes: NonZeroU64,
    per_ns: NonZeroU64,
}

impl Rate {
    fn of(cc: &Cc, srtt: Option<Duration>) -> Option<Self> {
        let per_ns = NonZeroU64::new(u64::try_from(srtt?.as_nanos()).unwrap_or(u64::MAX))?;
        let quarters = if cc.cwnd < cc.ssthresh { N_SLOW_START } else { N_AVOIDANCE };
        let bytes = NonZeroU64::new(u64::from(cc.cwnd).saturating_mul(quarters) / 4)?;
        Some(Self { bytes, per_ns })
    }

    fn over(self, d: Duration) -> u64 {
        let n = u128::from(self.bytes.get()).saturating_mul(d.as_nanos()) / NonZeroU128::from(self.per_ns);
        u64::try_from(n).unwrap_or(u64::MAX)
    }

    /// How long the rate takes to bring `bytes`, rounded up.
    fn time_for(self, bytes: u64) -> Duration {
        let ns = u128::from(bytes).saturating_mul(u128::from(self.per_ns.get())).div_ceil(u128::from(self.bytes.get()));
        Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
    }

    fn capacity(self, cc: &Cc) -> u64 {
        self.over(GRANULARITY).min(u64::from(cc.cwnd / 2)).max(u64::from(initial_window(cc.smss)))
    }
}

/// A connection's bucket: full until it first spends.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pacer {
    tokens: u64,
    at: Option<Instant>,
}

impl Pacer {
    fn refill(&mut self, now: Instant, rate: Rate, capacity: u64) {
        self.tokens = match self.at {
            Some(at) => self.tokens.saturating_add(rate.over(now.since(at))).min(capacity),
            None => capacity,
        };
        self.at = Some(now);
    }

    /// `None` if a segment of `len` bytes may leave now; else when to offer the held data again.
    pub fn hold(&mut self, now: Instant, len: u32, cc: &Cc, srtt: Option<Duration>) -> Option<Instant> {
        let rate = Rate::of(cc, srtt)?;
        let capacity = rate.capacity(cc);
        self.refill(now, rate, capacity);
        let len = u64::from(len);
        if self.tokens >= len {
            return None;
        }
        let release = (capacity / 2).max(len);
        Some(now.after(rate.time_for(release.saturating_sub(self.tokens))))
    }

    /// `len` bytes of data left, new or resent.
    pub fn spend(&mut self, now: Instant, len: u32, cc: &Cc, srtt: Option<Duration>) {
        if let Some(rate) = Rate::of(cc, srtt) {
            self.refill(now, rate, rate.capacity(cc));
            self.tokens = self.tokens.saturating_sub(u64::from(len));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn at(us: u64) -> Instant {
        Instant::from_nanos(us * 1_000)
    }

    /// cwnd, in slow start or in avoidance, with SMSS 1,460 and an initial window of 14,600.
    fn cc(cwnd: u32, slow_start: bool) -> Cc {
        let mut cc = Cc::new(1460, 0);
        cc.cwnd = cwnd;
        if !slow_start {
            cc.ssthresh = cwnd;
        }
        cc
    }

    /// Segments of 1,460 a pacer lets leave at `now` before it holds one, and when it releases it.
    fn burst(p: &mut Pacer, now: Instant, cc: &Cc, srtt: Duration) -> (u32, Option<Instant>) {
        let mut n = 0;
        loop {
            if let Some(release) = p.hold(now, 1460, cc, Some(srtt)) {
                return (n, Some(release));
            }
            p.spend(now, 1460, cc, Some(srtt));
            n += 1;
            assert!(n < 100_000, "never held");
        }
    }

    #[test]
    fn without_a_smoothed_rtt_nothing_is_paced() {
        let mut p = Pacer::default();
        let cc = cc(14_600, true);
        for _ in 0..100 {
            assert_eq!(p.hold(at(0), 1460, &cc, None), None);
            p.spend(at(0), 1460, &cc, None);
        }
        assert_eq!(p.hold(at(0), 1460, &cc, Some(Duration::ZERO)), None);
    }

    /// RFC 9002 §7.7: a burst is the initial window while the rate brings less in a millisecond. In
    /// slow start the rate is 2 · 29,200 / 10 ms, 5,840 bytes a millisecond, so the held segment
    /// leaves once half the bucket, 7,300 bytes, has come: 1.25 ms.
    #[test]
    fn a_slow_bucket_holds_the_initial_window_and_releases_at_half() {
        let mut p = Pacer::default();
        let cc = cc(29_200, true);
        assert_eq!(burst(&mut p, at(0), &cc, 10 * MS), (10, Some(at(1_250))));
        assert_eq!(burst(&mut p, at(1_250), &cc, 10 * MS), (5, Some(at(2_500))));
    }

    /// RFC 9002 §7.7's N = 1.25 once cwnd holds across the round trip: the same half bucket takes
    /// 7,300 / (1.25 · 29,200 / 10 ms), 2 ms.
    #[test]
    fn avoidance_paces_at_a_quarter_over_the_window() {
        let mut p = Pacer::default();
        let cc = cc(29_200, false);
        assert_eq!(burst(&mut p, at(0), &cc, 10 * MS), (10, Some(at(2_000))));
    }

    /// Past the initial window the bucket holds a millisecond at the rate: 2 · 1,460,000 / 8 ms is
    /// 365,000 bytes a millisecond, 250 segments, under half the window's thousand.
    #[test]
    fn a_fast_bucket_holds_a_millisecond() {
        let mut p = Pacer::default();
        let cc = cc(1_460_000, true);
        assert_eq!(burst(&mut p, at(0), &cc, 8 * MS), (250, Some(at(500))));
    }

    /// On a short round trip a millisecond at the rate is more than the window: at 2 · 146,000 /
    /// 1 ms the bucket holds half the window, 50 of its 100 segments, so the window still leaves in
    /// bursts, the next half bucket 125 µs later.
    #[test]
    fn a_bucket_holds_at_most_half_the_window() {
        let mut p = Pacer::default();
        let cc = cc(146_000, true);
        assert_eq!(burst(&mut p, at(0), &cc, MS), (50, Some(at(125))));
    }

    /// However long the sender was idle, the bucket holds no more than its capacity.
    #[test]
    fn an_idle_bucket_fills_only_to_its_capacity() {
        let mut p = Pacer::default();
        let cc = cc(29_200, true);
        burst(&mut p, at(0), &cc, 10 * MS);
        assert_eq!(burst(&mut p, at(60_000_000), &cc, 10 * MS).0, 10);
    }
}
