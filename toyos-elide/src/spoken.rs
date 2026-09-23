//! Which of a program's console lines become records, and how each one is
//! tagged.
//!
//! **A ring shared with the kernel is a ring a program can flood.** Every line
//! a console holder writes is a record in the same per-CPU shards the kernel's
//! own records live in, and a shard drops its oldest record to take a new one.
//! So one program's share is bounded here — a burst, then a steady rate — and
//! a line past it is counted rather than recorded. The count is the elision:
//! the next line that is recorded carries how many went before it, so a
//! withheld line is a number in the log and never a silence.
//!
//! The numbers are the caller's, because the reason for them is the ring's
//! size and a reader's pace, which are the kernel's.

/// One program's share of the record ring: `BURST` lines at once, and
/// `PER_SEC` a second after that.
///
/// Credit is kept in nanoseconds of the clock rather than in lines, so the
/// rate is exact at any `PER_SEC` without a fractional token.
pub struct Share<const BURST: u64, const PER_SEC: u64> {
    /// Nanoseconds of credit, at most [`Self::FULL_NS`]; a line costs [`Self::COST_NS`].
    credit_ns: u64,
    /// The newest clock reading seen; a reading older than this earns nothing.
    last_ns: u64,
    /// Lines refused since the last one recorded.
    withheld: u64,
}

/// What becomes of one line.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// It is recorded, after a note of the `withheld` lines refused since the
    /// last one that was — zero when there were none.
    Record { withheld: u64 },
    /// It is not recorded, and is counted.
    Withhold,
}

impl<const BURST: u64, const PER_SEC: u64> Share<BURST, PER_SEC> {
    const COST_NS: u64 = 1_000_000_000 / PER_SEC;
    const FULL_NS: u64 = BURST * Self::COST_NS;

    /// A share that has spent nothing: a program's first `BURST` lines are
    /// recorded whenever it writes them.
    pub const fn new(now_ns: u64) -> Self {
        const {
            assert!(BURST >= 1, "a share that records nothing is a console with no log");
            assert!(PER_SEC >= 1 && PER_SEC <= 1_000_000_000, "a rate the clock can charge");
            assert!(BURST <= u64::MAX / (1_000_000_000 / PER_SEC), "a burst the credit can hold");
        }
        Self { credit_ns: Self::FULL_NS, last_ns: now_ns, withheld: 0 }
    }

    /// Charge one line at `now_ns`.
    pub fn admit(&mut self, now_ns: u64) -> Verdict {
        let earned = now_ns.saturating_sub(self.last_ns);
        self.last_ns = self.last_ns.max(now_ns);
        self.credit_ns = self.credit_ns.saturating_add(earned).min(Self::FULL_NS);
        if self.credit_ns < Self::COST_NS {
            self.withheld = self.withheld.saturating_add(1);
            return Verdict::Withhold;
        }
        self.credit_ns -= Self::COST_NS;
        Verdict::Record { withheld: core::mem::take(&mut self.withheld) }
    }

    /// The lines refused since the last one recorded, for a speaker that is
    /// going away and will write no line to carry them.
    pub fn take_withheld(&mut self) -> u64 {
        core::mem::take(&mut self.withheld)
    }
}

/// Whether `line` already opens with `name: `, the tag a program's record
/// carries.
///
/// A program that names itself is not named twice; one that names anybody else
/// is named in front of it, so the first `name: ` of a spoken record is always
/// its writer's.
pub fn opens_with_tag(name: &[u8], line: &[u8]) -> bool {
    !name.is_empty() && line.strip_prefix(name).is_some_and(|rest| rest.starts_with(b": "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEC: u64 = 1_000_000_000;

    type Small = Share<4, 2>;

    fn admitted(share: &mut Small, now: u64) -> bool {
        matches!(share.admit(now), Verdict::Record { .. })
    }

    #[test]
    fn the_burst_is_recorded_and_the_line_after_it_is_counted() {
        let mut share = Small::new(0);
        for _ in 0..4 {
            assert_eq!(share.admit(0), Verdict::Record { withheld: 0 });
        }
        assert_eq!(share.admit(0), Verdict::Withhold);
        assert_eq!(share.admit(0), Verdict::Withhold);
        // Half a second is one line at two a second, and it carries the two
        // that were refused before it.
        assert_eq!(share.admit(SEC / 2), Verdict::Record { withheld: 2 });
        assert_eq!(share.admit(SEC / 2), Verdict::Withhold);
    }

    #[test]
    fn idle_time_refills_no_further_than_the_burst() {
        let mut share = Small::new(0);
        for _ in 0..4 {
            share.admit(0);
        }
        let later = 3600 * SEC;
        let got = (0..10).filter(|_| admitted(&mut share, later)).count();
        assert_eq!(got, 4, "an hour of silence buys the burst and not an hour of lines");
    }

    /// A clock read on another CPU can be a little behind the last one; it
    /// earns nothing and costs nothing extra.
    #[test]
    fn a_reading_behind_the_last_earns_nothing() {
        let mut share = Small::new(10 * SEC);
        for _ in 0..4 {
            share.admit(10 * SEC);
        }
        assert_eq!(share.admit(0), Verdict::Withhold);
        assert_eq!(share.admit(10 * SEC), Verdict::Withhold, "going back did not reset the clock");
        assert_eq!(share.admit(10 * SEC + SEC / 2), Verdict::Record { withheld: 2 });
    }

    #[test]
    fn a_speaker_going_away_hands_over_its_count_once() {
        let mut share = Small::new(0);
        for _ in 0..7 {
            share.admit(0);
        }
        assert_eq!(share.take_withheld(), 3);
        assert_eq!(share.take_withheld(), 0);
    }

    /// **The bound, over every schedule this generator makes**: in any window
    /// `[t0, t1]`, no more than `BURST + (t1 - t0) * PER_SEC` lines are
    /// recorded, and every line is either recorded or counted.
    #[test]
    fn no_schedule_records_more_than_the_burst_and_the_rate() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..50 {
            let mut share = Share::<16, 8>::new(0);
            let mut now = 0u64;
            let mut stamps = std::vec::Vec::new();
            let (mut recorded, mut counted) = (0u64, 0u64);
            for _ in 0..400 {
                // Four times the rate on average, so the share is spent most of the run.
                now += next() % (SEC / 16);
                match share.admit(now) {
                    Verdict::Record { withheld } => {
                        recorded += 1;
                        counted += withheld;
                        stamps.push(now);
                    }
                    Verdict::Withhold => {}
                }
            }
            counted += share.take_withheld();
            assert_eq!(recorded + counted, 400, "a line was neither recorded nor counted");
            for (i, &t0) in stamps.iter().enumerate() {
                for (j, &t1) in stamps.iter().enumerate().skip(i) {
                    let lines = (j - i + 1) as u64;
                    let bound = 16 + (t1 - t0) * 8 / SEC;
                    assert!(lines <= bound, "{lines} lines in {} ns, bound {bound}", t1 - t0);
                }
            }
        }
    }

    #[test]
    fn a_line_that_names_its_writer_is_not_named_twice() {
        assert!(opens_with_tag(b"netd", b"netd: MAC 52:54:00:12:34:56"));
        assert!(opens_with_tag(b"netd", b"netd: "));
        assert!(!opens_with_tag(b"netd", b"netd:MAC"));
        assert!(!opens_with_tag(b"netd", b"netdx: MAC"));
        assert!(!opens_with_tag(b"netd", b"logd: netd: MAC"));
        assert!(!opens_with_tag(b"netd", b"hello"));
        assert!(!opens_with_tag(b"", b": hello"), "an empty name is no tag");
    }
}
