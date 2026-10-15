//! The rule `toyos-crypto` is held to beside ring
//! (`issues/toyos-crypto-is-in-the-tree-on-par-with-ring-and-secure.md`, stage
//! 3), and the lines that carry it from the guest that measures to the judge
//! that reads.
//!
//! A row runs ours beside two instances, A and B, of the reference. Each
//! backend's batch size is calibrated to at least [`BATCH_NS`]; a sample is
//! the mean time of one operation over batches spanning at least that. The
//! row's first sample of each backend is a discarded warmup. Then come
//! [`ROUNDS`] rounds of [`PER_ROUND`] samples, each round a pass over the whole
//! table, so a disturbance lasting part of a run lands in one round of many
//! rows rather than in every sample of one. Sample `i` runs the backends in
//! [`order`]`(i)`.

use std::fmt;

pub const ROUNDS: usize = 3;
pub const PER_ROUND: usize = 7;
pub const SAMPLES: usize = ROUNDS * PER_ROUND;
pub const BATCH_NS: u64 = 1_000_000;
/// A row whose spread exceeds this gets no verdict and is measured again.
pub const SPREAD_LIMIT: f64 = 0.05;

/// The prefix of every line this crate writes and reads.
pub const MARK: &str = "parity: ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Ours,
    A,
    B,
}

/// The order sample `i` runs the backends in: each takes each position once
/// in every three samples, so [`SAMPLES`] gives each position to each backend
/// [`PER_ROUND`] times.
pub fn order(i: usize) -> [Backend; 3] {
    const ALL: [Backend; 3] = [Backend::Ours, Backend::A, Backend::B];
    [ALL[i % 3], ALL[(i + 1) % 3], ALL[(i + 2) % 3]]
}

/// One row of the table, as the guest measures it.
pub trait Row {
    fn name(&self) -> &str;
    /// Run `ops` operations on `backend`.
    fn run(&mut self, backend: Backend, ops: u64);
}

/// One row's samples: nanoseconds per operation, in sample order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Times {
    pub ours: Vec<f64>,
    pub a: Vec<f64>,
    pub b: Vec<f64>,
}

impl Times {
    fn of(&mut self, backend: Backend) -> &mut Vec<f64> {
        match backend {
            Backend::Ours => &mut self.ours,
            Backend::A => &mut self.a,
            Backend::B => &mut self.b,
        }
    }
}

/// Measure every row on `clock`, in nanoseconds, by the schedule the module
/// names. The result is in `rows` order.
pub fn measure(rows: &mut [Box<dyn Row>], clock: &mut dyn FnMut() -> u64) -> Vec<Times> {
    let mut batches = vec![[0u64; 3]; rows.len()];
    let mut times = vec![Times::default(); rows.len()];
    for round in 0..ROUNDS {
        for ((row, batch), times) in rows.iter_mut().zip(&mut batches).zip(&mut times) {
            if round == 0 {
                for backend in order(0) {
                    batch[backend as usize] = calibrate(row.as_mut(), backend, clock);
                }
                for backend in order(0) {
                    sample(row.as_mut(), backend, batch[backend as usize], clock);
                }
            }
            for i in round * PER_ROUND..(round + 1) * PER_ROUND {
                for backend in order(i) {
                    let ns = sample(row.as_mut(), backend, batch[backend as usize], clock);
                    times.of(backend).push(ns);
                }
            }
        }
    }
    times
}

/// The fewest operations, by doubling, that take at least [`BATCH_NS`].
fn calibrate(row: &mut dyn Row, backend: Backend, clock: &mut dyn FnMut() -> u64) -> u64 {
    let mut ops = 1;
    loop {
        let start = clock();
        row.run(backend, ops);
        if clock() - start >= BATCH_NS {
            return ops;
        }
        ops *= 2;
    }
}

/// Nanoseconds per operation over batches of `ops` spanning at least
/// [`BATCH_NS`].
fn sample(row: &mut dyn Row, backend: Backend, ops: u64, clock: &mut dyn FnMut() -> u64) -> f64 {
    let start = clock();
    let mut done = 0;
    loop {
        row.run(backend, ops);
        done += ops;
        let took = clock() - start;
        if took >= BATCH_NS {
            return took as f64 / done as f64;
        }
    }
}

/// The nearest-rank `num/den` quantile: the smallest value at least that
/// fraction of the values are at or below.
fn quantile(values: impl IntoIterator<Item = f64>, num: usize, den: usize) -> f64 {
    let mut sorted: Vec<f64> = values.into_iter().collect();
    assert!(!sorted.is_empty(), "a quantile of no values");
    sorted.sort_by(f64::total_cmp);
    let rank = (num * sorted.len()).div_ceil(den);
    sorted[rank.max(1) - 1]
}

/// The 90th percentile of |A_i / B_i − 1|.
pub fn spread(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len(), "A and B are paired");
    quantile(a.iter().zip(b).map(|(a, b)| (a / b - 1.0).abs()), 9, 10)
}

/// The median of ours_i / ((A_i + B_i) / 2).
pub fn ratio(ours: &[f64], a: &[f64], b: &[f64]) -> f64 {
    assert!(ours.len() == a.len() && a.len() == b.len(), "the samples are paired");
    quantile(ours.iter().zip(a).zip(b).map(|((o, a), b)| o / ((a + b) / 2.0)), 1, 2)
}

/// ML-KEM is measured beside RustCrypto's `ml-kem`, since ring has none, and
/// that comparison decides nothing.
pub fn excluded(row: &str) -> bool {
    row.starts_with("ml-kem")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    /// The spread exceeds [`SPREAD_LIMIT`]: measure again.
    Remeasure,
    Excluded,
}

impl Verdict {
    pub fn of(row: &str, spread: f64, ratio: f64) -> Verdict {
        if excluded(row) {
            Verdict::Excluded
        } else if spread > SPREAD_LIMIT {
            Verdict::Remeasure
        } else if ratio <= 1.0 + spread {
            Verdict::Pass
        } else {
            Verdict::Fail
        }
    }

    fn word(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Fail => "fail",
            Verdict::Remeasure => "remeasure",
            Verdict::Excluded => "excluded",
        }
    }

    fn from_word(word: &str) -> Option<Verdict> {
        [Verdict::Pass, Verdict::Fail, Verdict::Remeasure, Verdict::Excluded].into_iter().find(|v| v.word() == word)
    }
}

/// Which build printed a line: the shipped profile, or the same build with
/// debug assertions off, whose column decides nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Build {
    Asserted,
    Unasserted,
}

impl Build {
    fn word(self) -> &'static str {
        match self {
            Build::Asserted => "asserted",
            Build::Unasserted => "unasserted",
        }
    }

    fn from_word(word: &str) -> Option<Build> {
        [Build::Asserted, Build::Unasserted].into_iter().find(|b| b.word() == word)
    }
}

/// One row of one build's table: medians in nanoseconds per operation.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub name: String,
    pub ours: f64,
    pub a: f64,
    pub b: f64,
    pub spread: f64,
    pub ratio: f64,
    pub verdict: Verdict,
}

impl Summary {
    pub fn of(name: &str, times: &Times) -> Summary {
        let median = |v: &[f64]| quantile(v.iter().copied(), 1, 2);
        let spread = spread(&times.a, &times.b);
        let ratio = ratio(&times.ours, &times.a, &times.b);
        Summary {
            name: name.to_string(),
            ours: median(&times.ours),
            a: median(&times.a),
            b: median(&times.b),
            spread,
            ratio,
            verdict: Verdict::of(name, spread, ratio),
        }
    }
}

/// `build`'s line for one row, as the judge reads it back with [`read`].
pub struct RowLine<'a>(pub Build, pub &'a Summary);

impl fmt::Display for RowLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let RowLine(build, s) = self;
        write!(
            f,
            "{MARK}{} row {} ours={:.1} a={:.1} b={:.1} spread={:.4} ratio={:.3} verdict={}",
            build.word(),
            s.name,
            s.ours,
            s.a,
            s.b,
            s.spread,
            s.ratio,
            s.verdict.word()
        )
    }
}

/// `build`'s raw samples of one row, each at the shortest precision that reads
/// back as the same `f64`, so that a row recomputes from the log exactly. The
/// judge does not read them.
pub struct SamplesLine<'a>(pub Build, pub &'a str, pub &'a Times);

impl fmt::Display for SamplesLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let SamplesLine(build, name, times) = self;
        let list = |v: &[f64]| v.iter().map(|ns| format!("{ns}")).collect::<Vec<_>>().join(",");
        write!(
            f,
            "{MARK}{} samples {name} ours={} a={} b={}",
            build.word(),
            list(&times.ours),
            list(&times.a),
            list(&times.b)
        )
    }
}

/// The extensions `build`'s own feature detection admits, printed before its
/// rows: the ISA its table was measured on.
pub struct IsaLine<'a>(pub Build, pub &'a [&'a str]);

impl fmt::Display for IsaLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let IsaLine(build, features) = self;
        let features = if features.is_empty() { "none".to_string() } else { features.join(",") };
        write!(f, "{MARK}{} isa {features}", build.word())
    }
}

/// `build`'s last line: how many rows it printed.
pub struct EndLine(pub Build, pub usize);

impl fmt::Display for EndLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{MARK}{} rows {}", self.0.word(), self.1)
    }
}

/// Both builds' rows out of one log, in the order each printed them, and the
/// ISA each recorded.
#[derive(Debug, PartialEq)]
pub struct Table {
    pub asserted: Vec<Summary>,
    pub unasserted: Vec<Summary>,
    pub asserted_isa: String,
    pub unasserted_isa: String,
}

/// The table in a program's `lines`, as it wrote them: refused unless each
/// build printed its ISA once before its rows and its end line once after
/// exactly that many rows of distinct names, and both builds printed the same
/// rows in the same order.
pub fn read<'a>(lines: impl IntoIterator<Item = &'a str>) -> Result<Table, String> {
    let mut rows: [Vec<Summary>; 2] = [Vec::new(), Vec::new()];
    let mut ends: [Option<usize>; 2] = [None, None];
    let mut isas: [Option<String>; 2] = [None, None];
    for line in lines {
        let Some(said) = line.strip_prefix(MARK) else { continue };
        let mut words = said.split(' ');
        let (Some(build), Some(kind)) = (words.next(), words.next()) else {
            return Err(format!("an unreadable parity line: {line:?}"));
        };
        let build = Build::from_word(build).ok_or_else(|| format!("no build {build:?}: {line:?}"))?;
        let at = build as usize;
        match kind {
            "isa" => {
                let features = words.next().filter(|f| !f.is_empty());
                if isas[at].is_some() || !rows[at].is_empty() || features.is_none() || words.next().is_some() {
                    return Err(format!("an unreadable, late or second ISA line: {line:?}"));
                }
                isas[at] = features.map(str::to_string);
            }
            "samples" => {}
            "row" => {
                if isas[at].is_none() {
                    return Err(format!("a {} row before its ISA line: {line:?}", build.word()));
                }
                if ends[at].is_some() {
                    return Err(format!("a {} row after its end line: {line:?}", build.word()));
                }
                rows[at].push(summary(words).ok_or_else(|| format!("an unreadable row: {line:?}"))?);
            }
            "rows" => {
                let count = words.next().and_then(|n| n.parse().ok());
                if ends[at].is_some() || count.is_none() || words.next().is_some() {
                    return Err(format!("an unreadable or second end line: {line:?}"));
                }
                ends[at] = count;
            }
            _ => return Err(format!("an unreadable parity line: {line:?}")),
        }
    }
    let [asserted, unasserted] = rows;
    for (build, rows, end) in [(Build::Asserted, &asserted, ends[0]), (Build::Unasserted, &unasserted, ends[1])] {
        let Some(end) = end else {
            return Err(format!("the {} build printed {} row(s) and never its end line", build.word(), rows.len()));
        };
        if end != rows.len() {
            return Err(format!("the {} build says {end} rows and printed {}", build.word(), rows.len()));
        }
        let mut names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        names.sort_unstable();
        if let Some(twice) = names.windows(2).find(|w| w[0] == w[1]) {
            return Err(format!("the {} build printed row {} twice", build.word(), twice[0]));
        }
    }
    let names = |rows: &[Summary]| rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>();
    if names(&asserted) != names(&unasserted) {
        return Err(format!(
            "the builds printed different rows: asserted {:?}, unasserted {:?}",
            names(&asserted),
            names(&unasserted)
        ));
    }
    let [Some(asserted_isa), Some(unasserted_isa)] = isas else {
        return Err("a build printed no ISA line".to_string());
    };
    Ok(Table { asserted, unasserted, asserted_isa, unasserted_isa })
}

fn summary<'a>(mut words: impl Iterator<Item = &'a str>) -> Option<Summary> {
    let name = words.next()?.to_string();
    let mut field = |key: &str| words.next()?.strip_prefix(key)?.strip_prefix('=');
    let mut number = |key: &str| field(key)?.parse::<f64>().ok();
    let (ours, a, b, spread, ratio) = (number("ours")?, number("a")?, number("b")?, number("spread")?, number("ratio")?);
    let verdict = Verdict::from_word(field("verdict")?)?;
    words.next().is_none().then_some(Summary { name, ours, a, b, spread, ratio, verdict })
}

impl Table {
    /// The table as evidence: per row both builds' columns, the second build's
    /// deciding nothing.
    pub fn render(&self) -> Vec<String> {
        let mut out = vec![
            format!("ISA {} (assertions off: {})", self.asserted_isa, self.unasserted_isa),
            format!(
                "{:<34} {:>12} {:>12} {:>12} {:>7} {:>7} {:>9} | {:>12} {:>7}",
                "row", "ours ns", "A ns", "B ns", "spread", "ratio", "verdict", "ours ns off", "ratio off"
            ),
        ];
        for (on, off) in self.asserted.iter().zip(&self.unasserted) {
            out.push(format!(
                "{:<34} {:>12.1} {:>12.1} {:>12.1} {:>6.2}% {:>7.3} {:>9} | {:>12.1} {:>7.3}",
                on.name,
                on.ours,
                on.a,
                on.b,
                on.spread * 100.0,
                on.ratio,
                on.verdict.word(),
                off.ours,
                off.ratio
            ));
        }
        out
    }

    /// Stage 3's verdict, taken on the asserted build: every row not
    /// excluded passes.
    pub fn verdict(&self) -> Result<(), String> {
        let short: Vec<String> = self
            .asserted
            .iter()
            .filter(|r| matches!(r.verdict, Verdict::Fail | Verdict::Remeasure))
            .map(|r| format!("{} {} (ratio {:.3}, spread {:.2}%)", r.name, r.verdict.word(), r.ratio, r.spread * 100.0))
            .collect();
        if short.is_empty() {
            Ok(())
        } else {
            Err(format!("{} row(s) without a pass: {}", short.len(), short.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 21 samples from a pattern, so the quantiles land on known values.
    fn samples(f: impl Fn(usize) -> f64) -> Vec<f64> {
        (0..SAMPLES).map(f).collect()
    }

    #[test]
    fn each_backend_takes_each_position_equally_often() {
        for position in 0..3 {
            for backend in [Backend::Ours, Backend::A, Backend::B] {
                let times = (0..SAMPLES).filter(|&i| order(i)[position] == backend).count();
                assert_eq!(times, PER_ROUND, "{backend:?} at position {position}");
            }
        }
        assert_eq!(order(0), [Backend::Ours, Backend::A, Backend::B]);
        assert_eq!(order(1), [Backend::A, Backend::B, Backend::Ours]);
        assert_eq!(order(2), [Backend::B, Backend::Ours, Backend::A]);
    }

    /// The 19th smallest of 21 deviations is the 90th percentile: the two
    /// largest are outside it.
    #[test]
    fn the_spread_is_the_nineteenth_of_twenty_one_deviations() {
        let b = samples(|_| 100.0);
        // Deviations 0%, 0.1%, ... 2.0%, with the two largest 50% and 40%.
        let a = samples(|i| match i {
            0 => 150.0,
            1 => 140.0,
            i => 100.0 + 0.1 * (i as f64 - 2.0),
        });
        assert!((spread(&a, &b) - 0.018).abs() < 1e-9, "{}", spread(&a, &b));
        // Below B counts as much as above it.
        let a = samples(|i| if i < 19 { 97.0 } else { 100.0 });
        assert!((spread(&a, &b) - 0.03).abs() < 1e-9);
    }

    #[test]
    fn the_ratio_is_the_median_against_the_mean_of_a_and_b() {
        let a = samples(|_| 90.0);
        let b = samples(|_| 110.0);
        // Ten samples at half, ten at triple: the eleventh decides.
        let ours = samples(|i| match i {
            0..=9 => 50.0,
            10 => 120.0,
            _ => 300.0,
        });
        assert!((ratio(&ours, &a, &b) - 1.2).abs() < 1e-9);
    }

    #[test]
    fn a_ratio_within_one_plus_the_spread_passes_and_above_it_fails() {
        assert_eq!(Verdict::of("sha-256/64", 0.02, 1.02), Verdict::Pass);
        assert_eq!(Verdict::of("sha-256/64", 0.02, 0.5), Verdict::Pass);
        // The negative control: just above 1 + spread.
        assert_eq!(Verdict::of("sha-256/64", 0.02, 1.0201), Verdict::Fail);
        assert_eq!(Verdict::of("sha-256/64", 0.0, 1.0001), Verdict::Fail);
    }

    #[test]
    fn a_spread_above_five_percent_gets_no_verdict() {
        // At the limit a verdict is taken; above it none is, however the ratio
        // falls.
        assert_eq!(Verdict::of("x25519/ecdh", 0.05, 1.0), Verdict::Pass);
        assert_eq!(Verdict::of("x25519/ecdh", 0.06, 1.0), Verdict::Remeasure);
        assert_eq!(Verdict::of("x25519/ecdh", 0.06, 9.0), Verdict::Remeasure);
        // And from samples: A 6% above B in every pair.
        let times = Times { ours: samples(|_| 100.0), a: samples(|_| 106.0), b: samples(|_| 100.0) };
        let summary = Summary::of("x25519/ecdh", &times);
        assert!((summary.spread - 0.06).abs() < 1e-9);
        assert_eq!(summary.verdict, Verdict::Remeasure);
    }

    #[test]
    fn ml_kem_rows_are_excluded_by_name() {
        assert_eq!(Verdict::of("ml-kem-768/keygen", 0.0, 9.0), Verdict::Excluded);
        assert_eq!(Verdict::of("ml-kem-768/keygen", 0.5, 0.5), Verdict::Excluded);
        assert_eq!(Verdict::of("x25519/ecdh", 0.0, 9.0), Verdict::Fail);
    }

    /// A row whose operation costs a fixed number of ticks per backend, on a
    /// clock that only `run` advances, logging what ran.
    struct Ticking {
        name: String,
        cost: [u64; 3],
        now: std::rc::Rc<std::cell::Cell<u64>>,
        log: std::rc::Rc<std::cell::RefCell<Vec<(String, Backend, u64)>>>,
    }

    impl Row for Ticking {
        fn name(&self) -> &str {
            &self.name
        }
        fn run(&mut self, backend: Backend, ops: u64) {
            self.now.set(self.now.get() + self.cost[backend as usize] * ops);
            self.log.borrow_mut().push((self.name.clone(), backend, ops));
        }
    }

    #[test]
    fn the_schedule_calibrates_discards_a_warmup_and_passes_the_table_in_rounds() {
        let now = std::rc::Rc::new(std::cell::Cell::new(0));
        let log = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let row = |name: &str, cost| -> Box<dyn Row> {
            Box::new(Ticking { name: name.into(), cost, now: now.clone(), log: log.clone() })
        };
        // `x`'s ours takes 300 µs an operation, its reference 400 µs and
        // 2.5 ms; `y` is a single 1 ms operation.
        let mut rows = vec![row("x", [300_000, 400_000, 2_500_000]), row("y", [1_000_000; 3])];
        let clock = now.clone();
        let times = measure(&mut rows, &mut || clock.get());

        // Calibration doubles to the first batch of at least a millisecond.
        let log = log.borrow();
        let x: Vec<_> = log.iter().filter(|(n, _, _)| n == "x").map(|(_, b, o)| (*b, *o)).collect();
        assert_eq!(
            &x[..9],
            &[
                (Backend::Ours, 1),
                (Backend::Ours, 2),
                (Backend::Ours, 4),
                (Backend::A, 1),
                (Backend::A, 2),
                (Backend::A, 4),
                (Backend::B, 1),
                // The warmup, in sample 0's order, at the calibrated batches.
                (Backend::Ours, 4),
                (Backend::A, 4),
            ]
        );
        assert_eq!(x[9], (Backend::B, 1));
        // Then sample 0 itself, which is kept.
        assert_eq!(&x[10..13], &[(Backend::Ours, 4), (Backend::A, 4), (Backend::B, 1)]);
        assert_eq!(x[13..16].iter().map(|(b, _)| *b).collect::<Vec<_>>(), order(1));

        // Rounds pass the table: x's first round, y's whole first round, then
        // x's second.
        let names: Vec<&str> = log.iter().map(|(n, _, _)| n.as_str()).collect();
        let runs_of = |name| names.iter().filter(|n| **n == name).count();
        let (x_runs, y_runs) = (runs_of("x"), runs_of("y"));
        // x: 7 calibration runs, 3 warmup, then 3 per sample; y: 3 + 3, then 3 per sample.
        assert_eq!((x_runs, y_runs), (7 + 3 + 3 * SAMPLES, 3 + 3 + 3 * SAMPLES));
        let first_y = names.iter().position(|n| *n == "y").unwrap();
        assert_eq!(first_y, 7 + 3 + 3 * PER_ROUND);
        assert_eq!(names[first_y + 3 + 3 + 3 * PER_ROUND], "x");

        // The warmup is not among the samples, and each is the mean per
        // operation over its batch.
        assert_eq!(times.len(), 2);
        assert_eq!(times[0].ours, vec![300_000.0; SAMPLES]);
        assert_eq!(times[0].a, vec![400_000.0; SAMPLES]);
        assert_eq!(times[0].b, vec![2_500_000.0; SAMPLES]);
        assert_eq!(times[1].ours, vec![1_000_000.0; SAMPLES]);
    }

    #[test]
    fn a_sample_spans_a_millisecond_even_when_its_batch_comes_in_short() {
        // Calibration sees 600 µs per operation; afterwards each costs 300 µs.
        struct Speeding(std::rc::Rc<std::cell::Cell<u64>>, u32);
        impl Row for Speeding {
            fn name(&self) -> &str {
                "s"
            }
            fn run(&mut self, _: Backend, ops: u64) {
                self.1 += 1;
                let cost = if self.1 <= 2 { 600_000 } else { 300_000 };
                self.0.set(self.0.get() + cost * ops);
            }
        }
        let now = std::rc::Rc::new(std::cell::Cell::new(0));
        let mut row = Speeding(now.clone(), 0);
        assert_eq!(calibrate(&mut row, Backend::Ours, &mut || now.get()), 2);
        // Two batches of two: 1.2 ms over four operations.
        assert_eq!(sample(&mut row, Backend::Ours, 2, &mut || now.get()), 300_000.0);
    }

    #[test]
    fn a_table_reads_back_what_each_build_printed() {
        let row = |name: &str, ratio| Summary {
            name: name.into(),
            ours: 1234.5,
            a: 1000.0,
            b: 1010.0,
            spread: 0.0123,
            ratio,
            verdict: Verdict::of(name, 0.0123, ratio),
        };
        let asserted = vec![row("aes-128-gcm/seal/16384", 1.25), row("ml-kem-768/keygen", 0.5)];
        let unasserted = vec![row("aes-128-gcm/seal/16384", 1.0), row("ml-kem-768/keygen", 0.5)];
        // A value whose shortest spelling needs all of an f64's digits.
        let times = Times { ours: vec![0.1 + 0.2], a: vec![2.0], b: vec![1.0 / 3.0] };
        let mut log = String::new();
        for (build, rows) in [(Build::Asserted, &asserted), (Build::Unasserted, &unasserted)] {
            log.push_str(&format!("{}\n", IsaLine(build, &["aes", "avx2"])));
            for r in rows {
                log.push_str(&format!("{}\n", SamplesLine(build, &r.name, &times)));
                log.push_str(&format!("{}\n", RowLine(build, r)));
            }
            log.push_str(&format!("{}\n", EndLine(build, rows.len())));
        }
        let table = read(log.lines()).unwrap();
        let isa = "aes,avx2".to_string();
        assert_eq!(table, Table { asserted: asserted.clone(), unasserted, asserted_isa: isa.clone(), unasserted_isa: isa });
        assert_eq!(table.render().len(), 4);
        // The samples read back exactly.
        let samples = log.lines().find(|l| l.contains(" samples ")).unwrap();
        let ours: f64 = samples.split("ours=").nth(1).unwrap().split(' ').next().unwrap().parse().unwrap();
        assert_eq!(ours, 0.1 + 0.2);
        let refused = table.verdict().unwrap_err();
        assert!(refused.starts_with("1 row(s)") && refused.contains("aes-128-gcm/seal/16384 fail"), "{refused}");

        // Every shape the judge refuses.
        let lines: Vec<&str> = log.lines().collect();
        let without = |skip: &dyn Fn(&str) -> bool| lines.iter().filter(|l| !skip(l)).cloned().collect::<Vec<_>>().join("\n");
        let refusals = [
            without(&|l| l.contains("unasserted rows")),
            without(&|l| l.contains("unasserted row ml-kem")),
            without(&|l| l.contains("asserted row aes") && !l.contains("unasserted")),
            format!("{log}{}\n", RowLine(Build::Asserted, &asserted[0])),
            log.replace("verdict=fail", "verdict=maybe"),
            log.replace("asserted rows 2", "asserted rows 3"),
            log.replace("ratio=", "ratio=x"),
            without(&|l| l.contains("unasserted isa")),
            log.replacen("asserted isa aes,avx2\n", "", 1),
            format!("{log}{}\n", IsaLine(Build::Asserted, &[])),
        ];
        for bad in refusals {
            assert!(read(bad.lines()).is_err(), "read:\n{bad}");
        }
    }
}
