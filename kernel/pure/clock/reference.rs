//! A free-running counter held to a reference timer: what a reading of the
//! reference between two of the counter's says, the rate two such readings
//! measure and how far it can be off, and whether the counter still keeps the
//! rate a clock was given.
//!
//! Every bound here is derived from the readings themselves: the counter
//! readings either side of a reference reading bracket the moment it latched,
//! and the reference's tick bounds where in that tick it was. Nothing is a
//! statistic, so a bound that fails is a counter, a reference or a read that
//! did not do what it says.

/// Parts per billion in one.
pub const PPB: u64 = 1_000_000_000;

const FS_PER_NS: u128 = 1_000_000;

/// A reference timer: its tick, and how far its rate may wander from that tick
/// by its own specification, which every check that compares rates over time
/// allows on top of what the readings bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    pub period_fs: u64,
    pub tolerance_ppb: u64,
}

/// One reading of the reference, latched at some moment the counter read
/// between `before` and `after`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub before: u64,
    pub after: u64,
    pub reference: u64,
}

impl Sample {
    /// The counter ticks the reading's latch could have been anywhere in.
    pub const fn width(&self) -> u64 {
        self.after - self.before
    }

    /// The bracket's midpoint, doubled so it stays whole.
    const fn mid2(&self) -> u128 {
        self.before as u128 + self.after as u128
    }
}

/// A rate two samples measure: nanoseconds per counter tick in the clock's
/// fixed point (`toyos_abi::clock::Line::mult`), at most `bound_ppb` off, over
/// `span_ns` of the reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    pub mult: u64,
    pub bound_ppb: u64,
    pub span_ns: u64,
}

/// Why two samples measure no rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unmeasurable {
    /// The later sample's counter or reference reads no later than the
    /// earlier's.
    NotLater,
    /// The brackets are a quarter or more of the span between them, which
    /// bounds nothing worth keeping.
    TooWide,
}

/// The rate from `a` to `b`, which is later.
///
/// The counter moved `Δc` between the latches, known to the brackets' half
/// widths `(wa + wb) / 2`, while the reference moved `Δn` ticks, known to one:
/// so the relative error is `u = (wa + wb) / (2 Δc)` plus `r = 1 / Δn`, each
/// taken against the measured span, and with the second-order terms of both
/// the ratio and those spans `(u + r)·(1 + 6·max(u, r))`, which holds while
/// both are under a quarter. `mult` is rounded down, which is one unit of it
/// more.
pub fn rate(reference: Reference, a: Sample, b: Sample) -> Result<Rate, Unmeasurable> {
    if b.mid2() <= a.mid2() || b.reference <= a.reference {
        return Err(Unmeasurable::NotLater);
    }
    let counter2 = b.mid2() - a.mid2();
    let ticks = (b.reference - a.reference) as u128;
    let span_fs = ticks * reference.period_fs as u128;
    let widths = a.width() as u128 + b.width() as u128;
    if widths * 4 >= counter2 || ticks < 4 {
        return Err(Unmeasurable::TooWide);
    }
    let u = (widths * PPB as u128).div_ceil(counter2);
    let r = (PPB as u128).div_ceil(ticks);
    let first = u + r;
    // ns per tick, times 2^32: `(span_fs / 10⁶) / (counter2 / 2) · 2^32`.
    let mult = (span_fs << 33) / (FS_PER_NS * counter2);
    let bound = first + (6 * first * u.max(r)).div_ceil(PPB as u128) + (PPB as u128).div_ceil(mult.max(1));
    Ok(Rate { mult: mult as u64, bound_ppb: bound as u64, span_ns: (span_fs / FS_PER_NS) as u64 })
}

/// How the counter and the reference agree over the span from one sample to
/// a later one, at a given rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agreement {
    /// The span the reference counted, in nanoseconds.
    pub reference_ns: u64,
    /// The counter's span at the rate, less the reference's.
    pub apart_ns: i64,
    /// What the readings and the rate's tolerance allow `apart_ns` to be.
    pub allowed_ns: u64,
}

impl Agreement {
    pub const fn holds(&self) -> bool {
        self.apart_ns.unsigned_abs() <= self.allowed_ns
    }
}

/// The counter's span from `a` to `b` at `mult`, against the reference's.
///
/// Allowed: the brackets' half widths at `mult`, one reference tick, and
/// `tolerance_ppb` of the span, which is what the rate itself may be off by;
/// two nanoseconds more cover the rounding of each conversion. A `b` whose
/// reference reads behind `a`'s is a span of zero, so a reference that went
/// backwards is a disagreement.
pub fn agree(reference: Reference, a: Sample, b: Sample, mult: u64, tolerance_ppb: u64) -> Agreement {
    let nanos = |ticks2: u128| (ticks2 * mult as u128) >> 33;
    let counter_ns = if b.mid2() >= a.mid2() { nanos(b.mid2() - a.mid2()) as i128 } else { -(nanos(a.mid2() - b.mid2()) as i128) };
    let reference_ns = (b.reference.saturating_sub(a.reference) as u128 * reference.period_fs as u128 / FS_PER_NS) as u64;
    let readings = nanos(a.width() as u128 + b.width() as u128) as u64;
    let tick = (reference.period_fs as u128).div_ceil(FS_PER_NS) as u64;
    let rated = (reference_ns as u128 * tolerance_ppb as u128).div_ceil(PPB as u128) as u64;
    Agreement {
        reference_ns,
        apart_ns: (counter_ns - reference_ns as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        allowed_ns: readings + tick + rated + 2,
    }
}

/// Where a calibration's samples left the straight line between its first
/// and last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Calibration {
    Unmeasurable(Unmeasurable),
    /// The sample at `index` sits off the line by more than the readings
    /// allow: something moved the counter or the reference inside the window.
    Disturbed { index: usize, agreement: Agreement },
}

/// The rate over `samples`, first to last, once every sample between is shown
/// to sit on that line within what the readings allow: a step inside the
/// window, which would skew the rate, is refused rather than averaged in.
pub fn calibrate(reference: Reference, samples: &[Sample]) -> Result<Rate, Calibration> {
    let (Some(&first), Some(&last)) = (samples.first(), samples.last()) else {
        return Err(Calibration::Unmeasurable(Unmeasurable::NotLater));
    };
    let fit = rate(reference, first, last).map_err(Calibration::Unmeasurable)?;
    for (index, &sample) in samples.iter().enumerate().skip(1) {
        let agreement = agree(reference, first, sample, fit.mult, fit.bound_ppb);
        if !agreement.holds() {
            return Err(Calibration::Disturbed { index, agreement });
        }
    }
    Ok(fit)
}

/// The rate a clock keeps, and whether it was measured, so the watch may
/// replace it with a better measurement, or stated by the hardware.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept {
    pub mult: u64,
    pub bound_ppb: u64,
    pub measured: bool,
}

/// The first span [`Watch`] reports at, and the factor between reports.
const FIRST_REPORT_NS: u64 = 1_000_000_000;
const REPORT_FACTOR: u64 = 2;

/// A counter's agreement with the reference over the life of a clock kept on
/// it: each sample is held against the one before it, which catches a step,
/// and against the first, which catches a rate that has moved.
#[derive(Clone, Copy, Debug)]
pub struct Watch {
    reference: Reference,
    anchor: Sample,
    last: Sample,
    kept: Kept,
    next_report_ns: u64,
}

/// What a sample showed, where it held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// The rate measured since the first sample.
    pub measured: Rate,
    /// The rate the clock keeps from here: a better measurement where the kept
    /// one was measured, and otherwise the one it kept.
    pub kept: Kept,
    /// Whether `kept` replaced the rate the clock was keeping.
    pub retuned: bool,
}

/// What a sample showed, where it did not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Against the sample before it: the counter stepped.
    Step { from: Sample, to: Sample, agreement: Agreement },
    /// Against the first: the counter's rate is not the one the clock keeps.
    Drift { from: Sample, to: Sample, agreement: Agreement },
}

impl Watch {
    pub const fn new(reference: Reference, anchor: Sample, kept: Kept) -> Self {
        Self { reference, anchor, last: anchor, kept, next_report_ns: FIRST_REPORT_NS }
    }

    pub const fn kept(&self) -> Kept {
        self.kept
    }

    /// The sample taken last, every one before it having held.
    pub const fn last(&self) -> Sample {
        self.last
    }

    /// Hold `sample`, taken after every sample before it, to the kept rate.
    /// `Some` at each report: the first a second after the anchor, then at
    /// twice the span of the one before.
    pub fn take(&mut self, sample: Sample) -> Result<Option<Report>, Failure> {
        let tolerance = self.kept.bound_ppb + self.reference.tolerance_ppb;
        let step = agree(self.reference, self.last, sample, self.kept.mult, tolerance);
        if !step.holds() {
            return Err(Failure::Step { from: self.last, to: sample, agreement: step });
        }
        let drift = agree(self.reference, self.anchor, sample, self.kept.mult, tolerance);
        if !drift.holds() {
            return Err(Failure::Drift { from: self.anchor, to: sample, agreement: drift });
        }
        self.last = sample;
        if drift.reference_ns < self.next_report_ns {
            return Ok(None);
        }
        while self.next_report_ns <= drift.reference_ns {
            self.next_report_ns = self.next_report_ns.saturating_mul(REPORT_FACTOR);
        }
        let Ok(measured) = rate(self.reference, self.anchor, sample) else {
            return Ok(None);
        };
        let retuned = self.kept.measured && measured.bound_ppb < self.kept.bound_ppb;
        if retuned {
            self.kept = Kept { mult: measured.mult, bound_ppb: measured.bound_ppb, measured: true };
        }
        Ok(Some(Report { measured, kept: self.kept, retuned }))
    }
}

/// How far one CPU's counter can be from another's, from round trips of a
/// cache line between them: each side writes the counter it read just before
/// its store, and the other reads its own just after it sees the store.
///
/// With `δ` the second counter less the first at one instant, a trip out
/// reads `δ` plus the trip's latency, never less, and a trip back reads `-δ`
/// plus its own; so the least of each bounds `δ` to `[-least_back,
/// least_out]`. An interval that excludes zero is an offset proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Offset {
    pub least_out: i64,
    pub least_back: i64,
}

impl Offset {
    /// `[lo, hi]` for `δ`, or the offset the trips prove, in ticks: negative
    /// for a second counter behind the first.
    pub const fn bound(&self) -> Result<(i64, i64), i64> {
        if self.least_out < 0 {
            Err(self.least_out)
        } else if self.least_back < 0 {
            Err(-self.least_back)
        } else {
            Ok((-self.least_back, self.least_out))
        }
    }
}

/// The 64-bit reading of a reference whose counter is 32 bits wide: the value
/// with `low` as its low half nearest `predicted`, the earlier 64-bit reading
/// plus the ticks the counter says have passed since. Right while that
/// prediction is within 2^31 ticks.
pub const fn extend(predicted: u64, low: u32) -> u64 {
    let off = (low as u64).wrapping_sub(predicted) as u32 as i32;
    predicted.wrapping_add_signed(off as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// The AMD bench's HPET: 14.318 MHz, and the HPET specification's 0.05%.
    const HPET: Reference = Reference { period_fs: 69_841_279, tolerance_ppb: 500_000 };
    /// A counter at 1796.6248 MHz.
    const HZ: u128 = 1_796_624_800;
    /// What a read of the reference costs in counter ticks: 1.2 µs.
    const READ: u64 = 2_156;

    /// The sample a perfect counter at `hz` takes when the reference reads
    /// `n`, the latch landing `at` ticks into a bracket `READ` wide, the
    /// counter `step` ticks off from where it should be.
    fn sample_at(hz: u128, n: u64, at: u64, step: i64) -> Sample {
        let true_counter = (n as u128 * HPET.period_fs as u128 * hz / 1_000_000_000_000_000) as u64;
        let latch = true_counter.wrapping_add_signed(step);
        Sample { before: latch - at, after: latch - at + READ, reference: n }
    }

    fn sample(n: u64) -> Sample {
        sample_at(HZ, n, READ / 2, 0)
    }

    fn hz_of(mult: u64) -> u128 {
        (1_000_000_000u128 << 32) / mult as u128
    }

    fn ppb_apart(mult: u64, hz: u128) -> u128 {
        hz_of(mult).abs_diff(hz) * PPB as u128 / hz
    }

    const MS_50: u64 = 715_909;
    const SECOND: u64 = 14_318_180;

    #[test]
    fn a_rate_is_within_its_bound_wherever_the_latches_land() {
        for (at_a, at_b) in [(0, READ), (READ, 0), (READ / 2, READ / 2), (0, 0)] {
            let rate = rate(HPET, sample_at(HZ, 1_000, at_a, 0), sample_at(HZ, 1_000 + MS_50, at_b, 0)).unwrap();
            assert!(ppb_apart(rate.mult, HZ) <= rate.bound_ppb as u128, "{rate:?} at {at_a}, {at_b}");
            // Two 1.2 µs reads over 50 ms: 24 ppm, and the reference's tick 1.4.
            assert!((24_000..27_000).contains(&rate.bound_ppb), "{rate:?}");
        }
    }

    /// The same reads over ten minutes bound the rate to 6 ppb.
    #[test]
    fn a_long_span_bounds_the_rate_tightly() {
        let rate = rate(HPET, sample_at(HZ, 0, 0, 0), sample_at(HZ, 600 * SECOND, READ, 0)).unwrap();
        assert!(ppb_apart(rate.mult, HZ) <= rate.bound_ppb as u128);
        assert!(rate.bound_ppb <= 6, "{rate:?}");
        assert_eq!(rate.span_ns / 1_000_000_000, 600);
    }

    #[test]
    fn samples_out_of_order_or_too_wide_measure_nothing() {
        assert_eq!(rate(HPET, sample(10_000), sample(1_000)), Err(Unmeasurable::NotLater));
        assert_eq!(rate(HPET, sample(1_000), sample(1_000)), Err(Unmeasurable::NotLater));
        let near = sample(100_000);
        let wide = Sample { before: near.before - 1_000_000, after: near.after + 1_000_000, ..near };
        assert_eq!(rate(HPET, sample(99_000), wide), Err(Unmeasurable::TooWide));
    }

    fn window(steps: &[(usize, i64)]) -> Vec<Sample> {
        (0..=5u64)
            .map(|i| {
                let step = steps.iter().filter(|&&(at, _)| i as usize >= at).map(|&(_, by)| by).sum();
                sample_at(HZ, 1_000 + i * MS_50 / 5, (i * 431) % READ, step)
            })
            .collect()
    }

    #[test]
    fn a_still_window_calibrates() {
        let rate = calibrate(HPET, &window(&[])).unwrap();
        assert!(ppb_apart(rate.mult, HZ) <= rate.bound_ppb as u128);
    }

    /// The 1.7 ms the AMD bench's TSC once read ahead of its HPET, and a step
    /// of 5 µs, a tenth of a 50 ms window's 100 ppm, either way, wherever in the
    /// window it lands: each is refused, not averaged into the rate.
    #[test]
    fn a_step_inside_the_window_is_refused() {
        for step in [3_054_262i64, 8_983, -8_983] {
            for at in 1..=5 {
                match calibrate(HPET, &window(&[(at, step)])) {
                    Err(Calibration::Disturbed { agreement, .. }) => assert!(!agreement.holds()),
                    other => panic!("a {step}-tick step at sample {at} calibrated as {other:?}"),
                }
            }
        }
    }

    fn watch(measured: bool) -> Watch {
        let first = calibrate(HPET, &window(&[])).unwrap();
        Watch::new(HPET, sample(1_000), Kept { mult: first.mult, bound_ppb: first.bound_ppb, measured })
    }

    /// A second's samples, each across a reference read of its own.
    fn run(watch: &mut Watch, hz: u128, seconds: core::ops::Range<u64>, step: i64) -> Result<Vec<Report>, Failure> {
        let mut reports = Vec::new();
        for s in seconds {
            if let Some(report) = watch.take(sample_at(hz, 1_000 + s * SECOND, (s * 977) % READ, step))? {
                reports.push(report);
            }
        }
        Ok(reports)
    }

    /// A measured rate is replaced at each report by a better one, and the
    /// one an hour keeps is within its bound of the counter's, which is under
    /// 2 ppb.
    #[test]
    fn a_measured_rate_is_refined() {
        let mut watch = watch(true);
        let reports = run(&mut watch, HZ, 1..3_601, 0).unwrap();
        let spans: Vec<u64> = reports.iter().map(|r| r.measured.span_ns / 1_000_000_000).collect();
        assert_eq!(spans, [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048]);
        assert!(reports.iter().all(|r| r.retuned));
        let kept = watch.kept();
        assert!(ppb_apart(kept.mult, HZ) <= kept.bound_ppb as u128);
        assert!(kept.bound_ppb < 10, "{kept:?}");
    }

    /// A stated rate is reported against and never replaced.
    #[test]
    fn a_stated_rate_is_kept() {
        let mut watch = watch(false);
        let before = watch.kept();
        let reports = run(&mut watch, HZ, 1..100, 0).unwrap();
        assert!(!reports.is_empty() && reports.iter().all(|r| !r.retuned && r.kept == before));
    }

    /// A 1.7 ms step at runtime fails the sample after it, as a step.
    #[test]
    fn a_step_at_runtime_fails() {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..30, 0).unwrap();
        match run(&mut watch, HZ, 30..31, 3_054_262) {
            Err(Failure::Step { agreement, .. }) => assert!(agreement.apart_ns > 1_600_000),
            other => panic!("{other:?}"),
        }
    }

    /// A counter whose rate moves by 0.1% fails within seconds. One that moves
    /// by 0.04%, inside the reference's own tolerance, is measured and kept.
    #[test]
    fn a_rate_that_moves_past_the_reference_tolerance_fails() {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..10, 0).unwrap();
        let faster = HZ + HZ / 1_000;
        // The rate's change shows against the anchor as the seconds pass.
        let at = (1_000 + 10 * SECOND) as u128 * HPET.period_fs as u128;
        let offset = (at * HZ / 1_000_000_000_000_000) as i64 - (at * faster / 1_000_000_000_000_000) as i64;
        let failed = run(&mut watch, faster, 10..20, offset);
        assert!(matches!(failed, Err(Failure::Drift { .. }) | Err(Failure::Step { .. })), "{failed:?}");

        let mut tolerant = watch_fresh();
        let slightly = HZ + HZ * 4 / 10_000;
        assert!(run(&mut tolerant, slightly, 1..20, 0).is_ok());
    }

    fn watch_fresh() -> Watch {
        Watch::new(HPET, sample(1_000), Kept { mult: (1_000_000_000u128 << 32).div_ceil(HZ) as u64, bound_ppb: 10, measured: true })
    }

    /// Two CPUs' samples 100 µs apart, each read slowed to 20 µs by an SMI and
    /// latched at opposite ends of its bracket: a still counter, which the
    /// readings' own widths must allow, since the reference's tolerance over
    /// that span is 50 ns.
    #[test]
    fn close_samples_hold_across_wide_reads() {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..3, 0).unwrap();
        let wide = 35_932;
        let n = watch.last().reference + 1_432;
        let latch = |n: u64| sample_at(HZ, n, 0, 0).before;
        let early = Sample { before: latch(n), after: latch(n) + wide, reference: n };
        let late = Sample { before: latch(n + 1_432) - wide, after: latch(n + 1_432), reference: n + 1_432 };
        assert_eq!(watch.take(early).map(|_| ()), Ok(()));
        assert_eq!(watch.take(late).map(|_| ()), Ok(()));
    }

    /// A reference that reads backwards is a disagreement, never a span.
    #[test]
    fn a_reference_that_goes_back_fails() {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..5, 0).unwrap();
        let back = Sample { reference: watch.last().reference - 10, ..sample(1_000 + 6 * SECOND) };
        assert!(watch.take(back).is_err());
    }

    #[test]
    fn round_trips_bound_an_offset_or_prove_one() {
        assert_eq!(Offset { least_out: 120, least_back: 80 }.bound(), Ok((-80, 120)));
        assert_eq!(Offset { least_out: -30, least_back: 200 }.bound(), Err(-30));
        assert_eq!(Offset { least_out: 200, least_back: -45 }.bound(), Err(45));
    }

    #[test]
    fn a_32_bit_reference_is_extended_across_its_wrap() {
        assert_eq!(extend(0x1_ffff_fff0, 0x0000_0010), 0x2_0000_0010);
        assert_eq!(extend(0x2_0000_0010, 0xffff_fff0), 0x1_ffff_fff0);
        assert_eq!(extend(0x5_1234_5678, 0x1234_5679), 0x5_1234_5679);
    }
}
