//! A free-running counter held to a reference timer: what a reading of the
//! reference between two of the counter's says, the rate two such readings
//! measure and how far it can be off, and whether the counter still keeps the
//! rate a clock was given.
//!
//! Every bound on a rate and a step is derived from the readings themselves:
//! the counter readings either side of a reference reading bracket the moment
//! it latched, and the reference's tick bounds where in that tick it was, so a
//! bound that fails is a counter, a reference or a read that did not do what
//! it says. The one estimate is a third reference read by its interrupts
//! ([`Edges`]): their least latency, and its crystal's drift carried across a
//! span, which only choose between a step and none ([`third_says`]).

use toyos_abi::clock::MULT_SHIFT;

/// Parts per billion in one.
pub const PPB: u64 = 1_000_000_000;

const FS_PER_NS: u128 = 1_000_000;

/// A reference timer: its tick, and how far its rate may wander from that tick
/// by its own specification, which every check that compares rates over time
/// allows on top of what the readings bound: `tolerance_ppb` over a span of
/// `settled_ns` or more, and `short_tolerance_ppb` over a shorter one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reference {
    pub period_fs: u64,
    pub tolerance_ppb: u64,
    pub settled_ns: u64,
    pub short_tolerance_ppb: u64,
}

impl Reference {
    /// The span from `a` to `b` the reference counted, zero where it read
    /// backwards.
    pub const fn span_ns(&self, a: Sample, b: Sample) -> u64 {
        self.ns_of(b.reference.saturating_sub(a.reference))
    }

    /// `ticks` of the reference, in nanoseconds.
    pub const fn ns_of(&self, ticks: u64) -> u64 {
        (ticks as u128 * self.period_fs as u128 / FS_PER_NS) as u64
    }

    /// What the reference's own rate may be off by over `span_ns`.
    pub const fn tolerance_over(&self, span_ns: u64) -> u64 {
        if span_ns >= self.settled_ns { self.tolerance_ppb } else { self.short_tolerance_ppb }
    }
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

/// The counter and the reference over one or more segments, each from one
/// sample to a later one: what a rate is measured over, and what a held span
/// is checked over. Segments add, so a span can leave out the stretches the
/// reference cannot be held across.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    /// The counter's span between the brackets' midpoints, doubled.
    counter2: i128,
    /// The reference's ticks.
    ticks: i128,
    /// The brackets' widths at every segment's ends.
    widths: u128,
    /// Segments, each of which knows its reference span to one tick.
    segments: u64,
    /// The shortest segment's reference span, for the reference's tolerance.
    shortest_ns: u64,
}

impl Span {
    pub const EMPTY: Self = Self { counter2: 0, ticks: 0, widths: 0, segments: 0, shortest_ns: u64::MAX };

    pub fn between(reference: Reference, a: Sample, b: Sample) -> Self {
        Self {
            counter2: b.mid2() as i128 - a.mid2() as i128,
            ticks: b.reference as i128 - a.reference as i128,
            widths: a.width() as u128 + b.width() as u128,
            segments: 1,
            shortest_ns: reference.span_ns(a, b),
        }
    }

    pub fn then(self, more: Self) -> Self {
        Self {
            counter2: self.counter2 + more.counter2,
            ticks: self.ticks + more.ticks,
            widths: self.widths + more.widths,
            segments: self.segments + more.segments,
            shortest_ns: self.shortest_ns.min(more.shortest_ns),
        }
    }

    /// The reference's count over the span, in nanoseconds; zero for one
    /// that read backwards.
    pub fn reference_ns(&self, reference: Reference) -> u64 {
        (self.ticks.max(0) as u128 * reference.period_fs as u128 / FS_PER_NS) as u64
    }
}

/// The rate from `a` to `b`, which is later.
pub fn rate(reference: Reference, a: Sample, b: Sample) -> Result<Rate, Unmeasurable> {
    rate_over(reference, Span::between(reference, a, b))
}

/// The rate over `span`.
///
/// The counter moved `Δc` between the latches, known to the brackets' half
/// widths, while the reference moved `Δn` ticks, known to one a segment: so
/// the relative error is `u = Σw / (2 Δc)` plus `r = segments / Δn`, each
/// taken against the measured span, and with the second-order terms of both
/// the ratio and those spans `(u + r)·(1 + 6·max(u, r))`, which holds while
/// both are under a quarter. `mult` is rounded down, which is one unit of it
/// more.
pub fn rate_over(reference: Reference, span: Span) -> Result<Rate, Unmeasurable> {
    if span.counter2 <= 0 || span.ticks <= 0 {
        return Err(Unmeasurable::NotLater);
    }
    let counter2 = span.counter2 as u128;
    let ticks = span.ticks as u128;
    let span_fs = ticks * reference.period_fs as u128;
    if span.widths * 4 >= counter2 || ticks < 4 * span.segments as u128 {
        return Err(Unmeasurable::TooWide);
    }
    let u = (span.widths * PPB as u128).div_ceil(counter2);
    let r = (span.segments as u128 * PPB as u128).div_ceil(ticks);
    let first = u + r;
    // ns per tick, times 2^32: `(span_fs / 10⁶) / (counter2 / 2) · 2^32`.
    let mult = (span_fs << 33) / (FS_PER_NS * counter2);
    let bound = first + (6 * first * u.max(r)).div_ceil(PPB as u128) + (PPB as u128).div_ceil(mult.max(1));
    Ok(Rate { mult: mult as u64, bound_ppb: bound as u64, span_ns: (span_fs / FS_PER_NS) as u64 })
}

/// How the counter and the reference agree over a span, at a given rate.
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
pub fn agree(reference: Reference, a: Sample, b: Sample, mult: u64, tolerance_ppb: u64) -> Agreement {
    agree_over(reference, Span::between(reference, a, b), mult, tolerance_ppb)
}

/// The counter's span over `span` at `mult`, against the reference's.
///
/// Allowed: the brackets' half widths at `mult`, one reference tick a
/// segment, and `tolerance_ppb` of the span, which is what the rate itself
/// may be off by; two nanoseconds a segment more cover the rounding of each
/// conversion. A segment whose reference reads behind its start counts as a
/// span of zero, so a reference that went backwards is a disagreement.
pub fn agree_over(reference: Reference, span: Span, mult: u64, tolerance_ppb: u64) -> Agreement {
    let nanos = |ticks2: u128| (ticks2 * mult as u128) >> 33;
    let counter_ns = if span.counter2 >= 0 {
        nanos(span.counter2 as u128) as i128
    } else {
        -(nanos(span.counter2.unsigned_abs()) as i128)
    };
    let reference_ns = span.reference_ns(reference);
    let readings = nanos(span.widths) as u64;
    let tick = (reference.period_fs as u128).div_ceil(FS_PER_NS) as u64;
    let rated = (reference_ns as u128 * tolerance_ppb as u128).div_ceil(PPB as u128) as u64;
    Agreement {
        reference_ns,
        apart_ns: (counter_ns - reference_ns as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        allowed_ns: readings + span.segments * (tick + 2) + rated,
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
/// window larger than the readings resolve, which would skew the rate, is
/// refused rather than averaged in.
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
/// and against every held span since the first, which catches a rate that
/// has moved. A span the reference cannot be held across is left out whole.
#[derive(Clone, Copy, Debug)]
pub struct Watch {
    reference: Reference,
    /// The held segments before the one now open.
    closed: Span,
    /// Where the open segment starts.
    opened: Sample,
    last: Sample,
    kept: Kept,
    next_report_ns: u64,
}

/// What a sample showed, where it held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// The rate measured over every held span.
    pub measured: Rate,
    /// The rate the clock keeps from here: a better measurement where the kept
    /// one was measured, and otherwise the one it kept.
    pub kept: Kept,
    /// Whether `kept` replaced the rate the clock was keeping.
    pub retuned: bool,
}

/// What a sample showed, where it did not hold: the counter and the reference
/// disagree, and the samples cannot say which of the two moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// Against the sample before it: one of them stepped.
    Step { from: Sample, to: Sample, agreement: Agreement },
    /// Over every held span: they count at rates further apart than the kept
    /// rate's bound and the reference's tolerance.
    Drift { since: Sample, to: Sample, agreement: Agreement },
}

impl Watch {
    pub const fn new(reference: Reference, anchor: Sample, kept: Kept) -> Self {
        Self { reference, closed: Span::EMPTY, opened: anchor, last: anchor, kept, next_report_ns: FIRST_REPORT_NS }
    }

    pub const fn kept(&self) -> Kept {
        self.kept
    }

    /// The sample taken last, every one before it having held.
    pub const fn last(&self) -> Sample {
        self.last
    }

    /// Take `sample`, taken after every sample before it. `held` says whether
    /// the reference can be held to the counter across the span from the last
    /// sample; one that cannot is left out, and checks nothing. `Some` at each
    /// report: the first at a second of held spans, then at twice the span of
    /// the one before.
    pub fn take(&mut self, sample: Sample, held: bool) -> Result<Option<Report>, Failure> {
        if !held {
            if self.last != self.opened {
                self.closed = self.closed.then(Span::between(self.reference, self.opened, self.last));
            }
            self.opened = sample;
            self.last = sample;
            return Ok(None);
        }
        let tolerance = |span: &Span| self.kept.bound_ppb + self.reference.tolerance_over(span.shortest_ns);
        let since_last = Span::between(self.reference, self.last, sample);
        let step = agree_over(self.reference, since_last, self.kept.mult, tolerance(&since_last));
        if !step.holds() {
            return Err(Failure::Step { from: self.last, to: sample, agreement: step });
        }
        let whole = self.closed.then(Span::between(self.reference, self.opened, sample));
        let drift = agree_over(self.reference, whole, self.kept.mult, tolerance(&whole));
        if !drift.holds() {
            return Err(Failure::Drift { since: self.opened, to: sample, agreement: drift });
        }
        self.last = sample;
        if drift.reference_ns < self.next_report_ns {
            return Ok(None);
        }
        while self.next_report_ns <= drift.reference_ns {
            self.next_report_ns = self.next_report_ns.saturating_mul(REPORT_FACTOR);
        }
        let Ok(measured) = rate_over(self.reference, whole) else {
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

/// The 64-bit reading of a reference whose counter is `bits` wide: the value
/// with `low` as its low bits nearest `predicted`, the earlier 64-bit reading
/// plus the ticks the counter says have passed since. Right while that
/// prediction is within half the counter's range.
pub const fn extend(predicted: u64, low: u64, bits: u32) -> u64 {
    let range = 1u64 << bits;
    let off = low.wrapping_sub(predicted) & (range - 1);
    if off >= range / 2 {
        predicted.wrapping_add(off).wrapping_sub(range)
    } else {
        predicted.wrapping_add(off)
    }
}

/// What a second reference says of a step the counter and the first
/// reference disagree on, over the same span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arbitration {
    /// The second reference agrees with the counter: the first moved.
    FirstMoved,
    /// The second reference disagrees with the counter by what the first
    /// does, within both agreements' allowances: the counter moved, or both
    /// references did together.
    CounterMoved,
    /// The two references disagree with the counter and with each other.
    AllDisagree,
}

/// `first` and `second`: the counter against each reference over one span,
/// the first's an agreement that failed.
pub const fn arbitrate(first: Agreement, second: Agreement) -> Arbitration {
    if second.holds() {
        Arbitration::FirstMoved
    } else if first.apart_ns.abs_diff(second.apart_ns) <= first.allowed_ns + second.allowed_ns {
        Arbitration::CounterMoved
    } else {
        Arbitration::AllDisagree
    }
}

/// The RTC's edges the sampler has read, the latest [`EDGES_KEPT`]: each
/// edge's count and the counter as its interrupt was taken, which is the edge
/// plus the interrupt's latency, never earlier.
#[derive(Clone, Copy, Debug)]
pub struct Edges {
    kept: [(u64, u64); EDGES_KEPT],
    len: usize,
}

pub const EDGES_KEPT: usize = 16;
/// The edges either side of a span its least-latency edge is taken from.
pub const EDGES_EACH_SIDE: usize = 3;

impl Edges {
    pub const fn new() -> Self {
        Self { kept: [(0, 0); EDGES_KEPT], len: 0 }
    }

    /// Keep the edge `seen` as the counter read `at`, where it is later than
    /// the latest kept.
    pub fn push(&mut self, seen: u64, at: u64) {
        if self.len > 0 && self.kept[self.len - 1].0 >= seen {
            return;
        }
        if self.len == EDGES_KEPT {
            self.kept.copy_within(1.., 0);
            self.len -= 1;
        }
        self.kept[self.len] = (seen, at);
        self.len += 1;
    }

    /// Drop the edges taken before the counter read `at`, where the counter
    /// stepped: an edge from before a step and one from after it measure the
    /// step, not the RTC crystal's drift, so neither a drift base nor a side
    /// of a later span may reach across it.
    pub fn forget_before(&mut self, at: u64) {
        let kept = self.kept[..self.len].iter().position(|&(_, taken)| taken >= at).unwrap_or(self.len);
        self.kept.copy_within(kept..self.len, 0);
        self.len -= kept;
    }

    /// Whether [`EDGES_EACH_SIDE`] edges have come after the counter read `to`.
    pub fn complete_after(&self, to: u64) -> bool {
        self.kept[..self.len].iter().filter(|&&(_, at)| at >= to).count() >= EDGES_EACH_SIDE
    }

    /// The counter's span, at `mult`, less the RTC's seconds, from before
    /// `from` to after `to`: from the least-latency of the [`EDGES_EACH_SIDE`]
    /// edges before `from` to that of those after `to`. An edge reads late by
    /// its interrupt's latency and never early, so the least on each side is
    /// the nearest its edge. Less the RTC crystal's own drift from the
    /// counter, where edges at least [`DRIFT_BASE_NS`] before give it: the
    /// least-latency of the oldest kept to that before `from`, carried on
    /// across the span. `None` without an edge either side.
    pub fn apart_across(&self, from: u64, to: u64, mult: u64) -> Option<i64> {
        let kept = &self.kept[..self.len];
        let &(seen_ref, at_ref) = kept.iter().rev().find(|&&(_, at)| at <= from)?;
        let offset = |&(seen, at): &(u64, u64)| {
            let ns = ((at as i128 - at_ref as i128) * mult as i128) >> MULT_SHIFT;
            (ns - (seen as i128 - seen_ref as i128) * 1_000_000_000, at)
        };
        let least = |edges: &mut dyn Iterator<Item = &(u64, u64)>| edges.map(offset).min_by_key(|&(ns, _)| ns);
        let before = least(&mut kept.iter().rev().filter(|&&(_, at)| at <= from).take(EDGES_EACH_SIDE))?;
        let after = least(&mut kept.iter().filter(|&&(_, at)| at >= to).take(EDGES_EACH_SIDE))?;
        let base_ticks = ((DRIFT_BASE_NS as u128) << MULT_SHIFT) / mult as u128;
        let older = least(&mut kept.iter().filter(|&&(_, at)| (at as u128) + base_ticks <= before.1 as u128).take(EDGES_EACH_SIDE));
        let drift = older.map_or(0, |older| (before.0 - older.0) * (after.1 as i128 - before.1 as i128) / (before.1 as i128 - older.1 as i128));
        Some((after.0 - before.0 - drift) as i64)
    }
}

/// How far back edges must reach for [`Edges::apart_across`] to measure the
/// RTC crystal's drift from the counter: over this, a few microseconds of
/// latency are a part per million.
pub const DRIFT_BASE_NS: u64 = 4_000_000_000;

impl Default for Edges {
    fn default() -> Self {
        Self::new()
    }
}

/// Which of the counter's step and no step at all a third reference, on
/// another oscillator, reads nearest: within a quarter of the step of
/// either, or of neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Third {
    /// It reads the step: the counter moved.
    Counter,
    /// It reads no step: the two references moved together.
    References,
    /// It reads neither, or has no edge either side of the span yet.
    Unsure,
}

pub fn third_says(step_ns: i64, apart_ns: Option<i64>) -> Third {
    let Some(apart) = apart_ns else { return Third::Unsure };
    let zone = step_ns.unsigned_abs() / 4;
    if apart.abs_diff(step_ns) <= zone {
        Third::Counter
    } else if apart.unsigned_abs() <= zone {
        Third::References
    } else {
        Third::Unsure
    }
}

/// What the clock does about a step of its counter: the boot's first, forward,
/// is slewed away; a second, or one back, leaves the counter for the
/// reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Response {
    Slew,
    Demote,
}

pub const fn respond(step_ns: i64, slewed_before: u32) -> Response {
    if step_ns > 0 && slewed_before == 0 { Response::Slew } else { Response::Demote }
}

/// The most a slew moves the clock's rate: 500 ppm, the frequency tolerance
/// NTP disciplines a clock within (RFC 5905, Appendix A.1.1, `MAXFREQ`), so
/// a reader that tolerates an NTP-disciplined clock tolerates this one. A
/// step of `d` is slewed at this rate for `2000 d` less the last millisecond
/// or so, which halves each period: 1.7 ms in about 12 s.
pub const SLEW_MOST_PPB: i64 = 500_000;
/// A debt this small is paid.
const SLEW_SETTLED_NS: u64 = 1_000;

/// How far the clock stands ahead of the reference's time, which a slew pays
/// off by running the clock slower than the kept rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slew {
    debt_ns: i64,
}

impl Slew {
    pub const fn new(debt_ns: i64) -> Self {
        Self { debt_ns }
    }

    pub const fn debt_ns(&self) -> i64 {
        self.debt_ns
    }

    pub const fn settled(&self) -> bool {
        self.debt_ns.unsigned_abs() <= SLEW_SETTLED_NS
    }

    /// `ticks` the clock ran at `ran` where the reference's time runs at
    /// `kept`: what it fell behind is paid off the debt.
    pub fn ran(&mut self, ticks: u64, kept: u64, ran: u64) {
        let line = |mult| ((ticks as u128 * mult as u128) >> MULT_SHIFT) as i128;
        self.debt_ns -= (line(kept) - line(ran)) as i64;
    }

    /// The rate to run at until the next sample, due in `period_ns`: the debt
    /// paid over two periods, at most [`SLEW_MOST_PPB`] from `kept`, and `kept`
    /// once settled. A sample up to a period late pays no more than the debt;
    /// a later one pays more, which the next pays back.
    pub fn mult(&self, kept: u64, period_ns: u64) -> u64 {
        if self.settled() {
            return kept;
        }
        let ppb = (self.debt_ns as i128 * 1_000_000_000 / (2 * period_ns as i128)).clamp(-(SLEW_MOST_PPB as i128), SLEW_MOST_PPB as i128);
        (kept as i128 - kept as i128 * ppb / 1_000_000_000) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// The AMD bench's HPET: 14.318 MHz, and the HPET specification's 0.05%
    /// from 1 ms and 0.2% under it.
    const HPET: Reference =
        Reference { period_fs: 69_841_279, tolerance_ppb: 500_000, settled_ns: 1_000_000, short_tolerance_ppb: 2_000_000 };
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
            if let Some(report) = watch.take(sample_at(hz, 1_000 + s * SECOND, (s * 977) % READ, step), true)? {
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
        assert_eq!(watch.take(early, true).map(|_| ()), Ok(()));
        assert_eq!(watch.take(late, true).map(|_| ()), Ok(()));
    }

    /// A reference that stood still for 30 s while the counter ran: held, the
    /// span reads as a step; left out, it is none, and the rate over the held
    /// spans either side is the counter's.
    #[test]
    fn a_span_left_out_checks_nothing() {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..20, 0).unwrap();
        // The reference resumes where it stopped; the counter is 30 s on.
        let stood = (30 * SECOND as u128 * HPET.period_fs as u128 * HZ / 1_000_000_000_000_000) as i64;
        let resumed = sample_at(HZ, 1_000 + 20 * SECOND, 0, stood);
        assert!(matches!(watch.take(resumed, true), Err(Failure::Step { .. })), "the stood reference reads as a step when held");
        let mut watch = watch_after(20);
        assert_eq!(watch.take(resumed, false), Ok(None));
        let reports = run(&mut watch, HZ, 21..300, stood).unwrap();
        let last = reports.last().expect("held spans past the gap report");
        assert!(ppb_apart(last.measured.mult, HZ) <= last.measured.bound_ppb as u128, "{last:?}");
        assert!(last.measured.span_ns / 1_000_000_000 >= 256);
    }

    fn watch_after(seconds: u64) -> Watch {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..seconds, 0).unwrap();
        watch
    }

    /// A reference that reads backwards is a disagreement, never a span.
    #[test]
    fn a_reference_that_goes_back_fails() {
        let mut watch = watch(true);
        run(&mut watch, HZ, 1..5, 0).unwrap();
        let back = Sample { reference: watch.last().reference - 10, ..sample(1_000 + 6 * SECOND) };
        assert!(watch.take(back, true).is_err());
    }

    #[test]
    fn round_trips_bound_an_offset_or_prove_one() {
        assert_eq!(Offset { least_out: 120, least_back: 80 }.bound(), Ok((-80, 120)));
        assert_eq!(Offset { least_out: -30, least_back: 200 }.bound(), Err(-30));
        assert_eq!(Offset { least_out: 200, least_back: -45 }.bound(), Err(45));
    }

    /// The PM timer, at 3.579545 MHz, across the same spans as the HPET.
    const PM: Reference =
        Reference { period_fs: 279_365_114, tolerance_ppb: 500_000, settled_ns: 1_000_000, short_tolerance_ppb: 2_000_000 };

    /// The PM timer's sample a perfect counter takes at `ns`, its step `step`.
    fn pm_at(ns: u64, step: i64) -> Sample {
        let n = (ns as u128 * 1_000_000 / PM.period_fs as u128) as u64;
        let counter = (n as u128 * PM.period_fs as u128 * HZ / 1_000_000_000_000_000) as u64;
        let latch = counter.wrapping_add_signed(step);
        Sample { before: latch - READ / 2, after: latch - READ / 2 + READ, reference: n }
    }

    fn ns_of(n: u64) -> u64 {
        (n as u128 * HPET.period_fs as u128 / 1_000_000) as u64
    }

    /// The AMD's 1.7 ms over 680 ms: as the TSC's own step, both references
    /// read it alike; as the HPET's stall, the PM timer reads the TSC right;
    /// as a step of each by a different size, nothing agrees.
    #[test]
    fn a_second_reference_tells_which_moved() {
        let mult = ((1_000_000_000u128 << 32) / HZ) as u64;
        let tolerance = 500_000;
        let (n0, n1) = (100 * SECOND, 100 * SECOND + 9_739_369);
        let step = 3_096_525i64;
        let hpet = |n, step| sample_at(HZ, n, READ / 2, step);
        let tsc_moved = arbitrate(
            agree(HPET, hpet(n0, 0), hpet(n1, step), mult, tolerance),
            agree(PM, pm_at(ns_of(n0), 0), pm_at(ns_of(n1), step), mult, tolerance),
        );
        assert_eq!(tsc_moved, Arbitration::CounterMoved);
        let stalled = n1 - 24_675;
        let hpet_moved = arbitrate(
            agree(HPET, hpet(n0, 0), Sample { reference: stalled, ..hpet(n1, 0) }, mult, tolerance),
            agree(PM, pm_at(ns_of(n0), 0), pm_at(ns_of(n1), 0), mult, tolerance),
        );
        assert_eq!(hpet_moved, Arbitration::FirstMoved);
        let neither = arbitrate(
            agree(HPET, hpet(n0, 0), hpet(n1, step), mult, tolerance),
            agree(PM, pm_at(ns_of(n0), 0), pm_at(ns_of(n1), step / 3), mult, tolerance),
        );
        assert_eq!(neither, Arbitration::AllDisagree);
    }

    #[test]
    fn a_32_bit_reference_is_extended_across_its_wrap() {
        assert_eq!(extend(0x1_ffff_fff0, 0x0000_0010, 32), 0x2_0000_0010);
        assert_eq!(extend(0x2_0000_0010, 0xffff_fff0, 32), 0x1_ffff_fff0);
        assert_eq!(extend(0x5_1234_5678, 0x1234_5679, 32), 0x5_1234_5679);
        // The ACPI PM timer's 24 bits, across their wrap and back.
        assert_eq!(extend(0x03ff_fff0, 0x0010, 24), 0x0400_0010);
        assert_eq!(extend(0x0400_0010, 0x00ff_fff0, 24), 0x03ff_fff0);
    }

    /// A 1 GHz counter, so ticks are nanoseconds.
    const NS: u64 = 1 << MULT_SHIFT;

    /// Edges a second apart on the RTC, each read as late as its latency;
    /// the counter steps by `step` at `step_at`.
    fn edges(latency_ns: &[u64], step_at: u64, step: u64) -> Edges {
        let mut edges = Edges::new();
        for (n, latency) in latency_ns.iter().enumerate() {
            let edge = 10_000_000_000 + n as u64 * 1_000_000_000;
            edges.push(n as u64 + 1, edge + latency + if edge >= step_at { step } else { 0 });
        }
        edges
    }

    /// The least-latency edge either side reads the step, however late the
    /// edge nearest it was taken; and reads none where there was none.
    #[test]
    fn the_rtc_reads_a_step_through_its_interrupts_latency() {
        let latency = [40_000, 700_000, 3_000, 650_000, 520_000, 9_000, 700_000, 2_000];
        let step = 1_700_000;
        let stepped = edges(&latency, 14_500_000_000, step);
        let (from, to) = (13_900_000_000, 14_900_000_000);
        assert!(stepped.complete_after(to + step));
        let apart = stepped.apart_across(from, to + step, NS).expect("edges either side");
        assert!(apart.abs_diff(step as i64) <= 10_000, "read {apart} ns for a {step} ns step");
        assert_eq!(third_says(step as i64, Some(apart)), Third::Counter);

        let still = edges(&latency, u64::MAX, 0);
        let apart = still.apart_across(from, to, NS).expect("edges either side");
        assert!(apart.unsigned_abs() <= 10_000, "read {apart} ns where nothing stepped");
        assert_eq!(third_says(step as i64, Some(apart)), Third::References);
    }

    #[test]
    fn the_rtc_is_unsure_without_an_edge_either_side_or_between_the_two() {
        let few = edges(&[1_000, 1_000], u64::MAX, 0);
        assert_eq!(few.apart_across(9_000_000_000, 10_500_000_000, NS), None);
        assert!(!few.complete_after(10_500_000_000));
        assert_eq!(third_says(1_700_000, None), Third::Unsure);
        assert_eq!(third_says(1_700_000, Some(850_000)), Third::Unsure);
        assert_eq!(third_says(-1_700_000, Some(-1_600_000)), Third::Counter);
    }

    #[test]
    fn only_a_first_forward_step_is_slewed() {
        assert_eq!(respond(1_700_000, 0), Response::Slew);
        assert_eq!(respond(1_700_000, 1), Response::Demote);
        assert_eq!(respond(-1_700_000, 0), Response::Demote);
    }

    /// Samples land 0.8 to 1.4 s apart: the slew never moves the rate past
    /// 500 ppm, and the debt is paid within the time that rate takes and two
    /// periods more.
    #[test]
    fn a_slew_pays_its_debt_within_its_bound() {
        let kept = toyos_abi::clock::Line::mult_of_hz(1_796_624_780);
        let ticks_per_ns = 1.796_624_78;
        let debt = 1_700_000i64;
        let mut slew = Slew::new(debt);
        let gaps = [1_000_000_000u64, 1_400_000_000, 800_000_000, 1_200_000_000, 1_000_000_000, 1_300_000_000];
        let mut elapsed = 0u64;
        for gap in gaps.iter().cycle().take(64) {
            if slew.settled() {
                break;
            }
            let ran = slew.mult(kept, 1_000_000_000);
            let ppb = (kept as i128 - ran as i128) * 1_000_000_000 / kept as i128;
            assert!(ppb.unsigned_abs() <= SLEW_MOST_PPB as u128, "{ppb} ppb");
            slew.ran((*gap as f64 * ticks_per_ns) as u64, kept, ran);
            elapsed += gap;
        }
        assert!(slew.settled(), "{} ns left", slew.debt_ns());
        // At the most rate down to two periods' worth, then halving at worst
        // to six tenths a period, from a millisecond to the settled microsecond.
        assert!(elapsed <= 2_000 * debt as u64 + 14 * 1_400_000_000, "took {elapsed} ns");
    }

    /// A sample a period late pays the debt and no more; one ten periods late
    /// pays more, and the next runs the clock faster to pay it back.
    #[test]
    fn a_late_sample_does_not_overpay_and_a_later_one_is_paid_back() {
        let kept = toyos_abi::clock::Line::mult_of_hz(1_000_000_000);
        let period = 1_000_000_000u64;
        let mut late = Slew::new(300_000);
        late.ran(2 * period, kept, late.mult(kept, period));
        assert!(late.debt_ns() >= 0 && late.settled(), "{} ns left", late.debt_ns());
        let mut later = Slew::new(300_000);
        later.ran(10 * period, kept, later.mult(kept, period));
        assert!(later.debt_ns() < 0);
        assert!(later.mult(kept, period) > kept, "an overpaid slew runs the clock faster");
    }

    /// An RTC crystal 60 ppm off the counter drifts 420 µs over the edges
    /// either side of a span, about a quarter of a 1.7 ms step: corrected by
    /// its own earlier edges, it reads the step and reads a stall as none.
    #[test]
    fn the_rtcs_crystal_drift_is_taken_out() {
        let drifting = |step: u64| {
            let mut edges = Edges::new();
            for n in 0..EDGES_KEPT as u64 {
                let edge = 10_000_000_000 + n * 1_000_060_000;
                let latency = [3_000, 650_000, 20_000][n as usize % 3];
                edges.push(n + 1, edge + latency + if n >= 11 { step } else { 0 });
            }
            edges
        };
        let (from, to) = (20_900_000_000, 21_900_000_000);
        let apart = drifting(1_700_000).apart_across(from, to + 1_700_000, NS).expect("edges either side");
        assert_eq!(third_says(1_700_000, Some(apart)), Third::Counter, "read {apart} ns");
        let apart = drifting(0).apart_across(from, to, NS).expect("edges either side");
        assert!(apart.unsigned_abs() <= 30_000, "a stall read {apart} ns through the drift");
        assert_eq!(third_says(1_700_000, Some(apart)), Third::References);
    }

    /// A 1.7 ms step after a 4 ms one reads as itself once the edges from
    /// before the first are forgotten; with them, the drift base reaches
    /// across the first step and takes a quarter of it for the crystal's drift.
    #[test]
    fn a_drift_base_never_reaches_across_a_step() {
        let (first, second) = (4_000_000u64, 1_700_000u64);
        let stepped = || {
            let mut edges = Edges::new();
            for n in 0..EDGES_KEPT as u64 {
                let edge = 10_000_000_000 + n * 1_000_000_000;
                let latency = [3_000, 650_000, 20_000][n as usize % 3];
                let steps = if n >= 4 { first } else { 0 } + if n >= 13 { second } else { 0 };
                edges.push(n + 1, edge + latency + steps);
            }
            edges
        };
        let first_taken_at = 14_000_000_000 + first;
        let (from, to) = (22_500_000_000 + first, 22_900_000_000 + first + second);

        let mut forgotten = stepped();
        forgotten.forget_before(first_taken_at);
        let apart = forgotten.apart_across(from, to, NS).expect("edges either side");
        assert_eq!(third_says(second as i64, Some(apart)), Third::Counter, "read {apart} ns");

        let apart = stepped().apart_across(from, to, NS).expect("edges either side");
        assert_ne!(third_says(second as i64, Some(apart)), Third::Counter, "read {apart} ns across the first step");
    }
}
