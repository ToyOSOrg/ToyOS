//! The judgements osbench makes over what it read, apart from the reading, so
//! a host tests them: whether a munmap pair's figure stands ([`pair`]), where
//! the CPU crunch stops for heat ([`heat`]), how far an energy counter
//! advanced across its wrap ([`energy`]), whether two counters readings
//! describe the row they were read around ([`window`]), and when a CPU's
//! clock has settled ([`settle`]).

pub mod pair {
    //! A munmap pair is an initiator that unmaps and a sibling that touched
    //! the page from another CPU. Each side reads its APIC ID before and
    //! after the timed window; the figure stands only if neither side moved
    //! and the two were on different CPUs.

    /// One side's APIC ID before and after the window.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Readings {
        pub before: u32,
        pub after: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Side {
        Initiator,
        Sibling,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Refusal {
        /// A side read one ID before the window and another after it.
        Moved { side: Side, before: u32, after: u32 },
        /// Both sides read this one ID.
        SameCpu(u32),
    }

    impl std::fmt::Display for Refusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Refusal::Moved { side, before, after } => {
                    write!(f, "the {side:?} moved from APIC ID {before} to {after} across the window")
                }
                Refusal::SameCpu(id) => write!(f, "both sides ran on APIC ID {id}"),
            }
        }
    }

    /// Whether a window's figure stands on its four readings.
    pub fn admit(initiator: Readings, sibling: Readings) -> Result<(), Refusal> {
        for (side, r) in [(Side::Initiator, initiator), (Side::Sibling, sibling)] {
            if r.before != r.after {
                return Err(Refusal::Moved { side, before: r.before, after: r.after });
            }
        }
        if initiator.before == sibling.before {
            return Err(Refusal::SameCpu(initiator.before));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const fn on(id: u32) -> Readings {
            Readings { before: id, after: id }
        }

        #[test]
        fn a_pair_apart_and_still_stands() {
            assert_eq!(admit(on(0), on(4)), Ok(()));
        }

        #[test]
        fn a_pair_on_one_cpu_is_refused() {
            assert_eq!(admit(on(6), on(6)), Err(Refusal::SameCpu(6)));
        }

        #[test]
        fn a_sibling_that_moved_onto_the_initiators_cpu_inside_the_window_is_refused() {
            let sibling = Readings { before: 2, after: 0 };
            assert_eq!(admit(on(0), sibling), Err(Refusal::Moved { side: Side::Sibling, before: 2, after: 0 }));
        }

        #[test]
        fn an_initiator_that_moved_is_refused_even_when_it_stayed_apart() {
            let initiator = Readings { before: 0, after: 8 };
            assert_eq!(admit(initiator, on(4)), Err(Refusal::Moved { side: Side::Initiator, before: 0, after: 8 }));
        }

        #[test]
        fn a_sibling_that_moved_elsewhere_is_refused() {
            let sibling = Readings { before: 2, after: 4 };
            assert_eq!(admit(on(0), sibling), Err(Refusal::Moved { side: Side::Sibling, before: 2, after: 4 }));
        }
    }
}

pub mod heat {
    //! Where the crunch stops: [`MARGIN_MC`] below the CPU's TjMax, the
    //! temperature at which the CPU throttles itself. Every temperature is in
    //! millidegrees Celsius, as Linux's hwmon reports it.

    /// How far below TjMax a phase stops. Policy: far enough that a laptop
    /// never reaches the CPU's own throttling inside a measurement.
    pub const MARGIN_MC: i64 = 15_000;

    /// Where the stop is when nothing says the CPU's TjMax: the lowest TjMax
    /// in [`PUBLISHED`] less the margin.
    pub const ASSUMED_STOP_MC: i64 = {
        let (mut lowest, mut i) = (i64::MAX, 0);
        while i < PUBLISHED.len() {
            if PUBLISHED[i].1 < lowest {
                lowest = PUBLISHED[i].1;
            }
            i += 1;
        }
        lowest - MARGIN_MC
    };

    /// A cooled CPU reads within this of where its group started.
    pub const COOLED_WITHIN_MC: i64 = 5_000;

    /// TjMax as each bench's vendor publishes it, for a sensor that reports
    /// none: Linux's `k10temp` has no critical temperature, where `coretemp`
    /// reports the CPU's own (`IA32_TEMPERATURE_TARGET`) as `temp*_crit`.
    pub const PUBLISHED: [(&str, i64, &str); 2] = [
        ("Ryzen 7 4800U", 105_000, "AMD's Ryzen 7 4800U specifications, Max. Operating Temperature (Tjmax) 105 C"),
        ("i5-1135G7", 100_000, "Intel's Core i5-1135G7 specifications, TJUNCTION 100 C"),
    ];

    /// A sensor's critical temperature outside this is no TjMax.
    const PLAUSIBLE_TJMAX_MC: std::ops::RangeInclusive<i64> = 60_000..=130_000;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Source {
        /// The sensor's critical temperature.
        Sensor,
        /// The vendor's published TjMax for this model.
        Published(&'static str),
        /// Nothing said: [`ASSUMED_STOP_MC`].
        Assumed,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Limit {
        pub stop_mc: i64,
        pub tjmax_mc: Option<i64>,
        pub source: Source,
    }

    impl Limit {
        pub fn too_hot(&self, reading_mc: i64) -> bool {
            reading_mc >= self.stop_mc
        }
    }

    /// The stop for a CPU whose sensor reported `sensor_crit_mc` and whose
    /// model string is `cpu_model`.
    pub fn limit(sensor_crit_mc: Option<i64>, cpu_model: &str) -> Limit {
        let from = |tjmax_mc: i64, source| Limit { stop_mc: tjmax_mc - MARGIN_MC, tjmax_mc: Some(tjmax_mc), source };
        if let Some(crit) = sensor_crit_mc.filter(|c| PLAUSIBLE_TJMAX_MC.contains(c)) {
            return from(crit, Source::Sensor);
        }
        match PUBLISHED.iter().find(|(model, _, _)| cpu_model.contains(model)) {
            Some(&(_, tjmax, said)) => from(tjmax, Source::Published(said)),
            None => Limit { stop_mc: ASSUMED_STOP_MC, tjmax_mc: None, source: Source::Assumed },
        }
    }

    /// Whether a CPU reading `now_mc` has cooled to where its group started.
    pub fn cooled(start_mc: i64, now_mc: i64) -> bool {
        now_mc <= start_mc + COOLED_WITHIN_MC
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_sensors_tjmax_comes_first() {
            let l = limit(Some(100_000), "AMD Ryzen 7 4800U with Radeon Graphics");
            assert_eq!(l, Limit { stop_mc: 85_000, tjmax_mc: Some(100_000), source: Source::Sensor });
        }

        #[test]
        fn a_sensor_without_one_takes_the_vendors_figure_for_the_model() {
            let l = limit(None, "AMD Ryzen 7 4800U with Radeon Graphics");
            assert_eq!((l.stop_mc, l.tjmax_mc), (90_000, Some(105_000)));
            assert!(matches!(l.source, Source::Published(said) if said.contains("105")));
        }

        #[test]
        fn an_implausible_critical_temperature_is_not_a_tjmax() {
            let l = limit(Some(0), "11th Gen Intel(R) Core(TM) i5-1135G7 @ 2.40GHz");
            assert_eq!((l.stop_mc, l.tjmax_mc), (85_000, Some(100_000)));
        }

        #[test]
        fn an_unknown_cpu_stops_at_the_assumed_limit() {
            let l = limit(None, "some CPU");
            assert_eq!(l, Limit { stop_mc: ASSUMED_STOP_MC, tjmax_mc: None, source: Source::Assumed });
            assert!(!l.too_hot(ASSUMED_STOP_MC - 1));
            assert!(l.too_hot(ASSUMED_STOP_MC));
        }

        #[test]
        fn cooled_is_within_the_slack_of_the_start() {
            assert!(cooled(45_000, 50_000));
            assert!(!cooled(45_000, 50_001));
        }
    }
}

pub mod energy {
    //! An energy counter that counts up from zero to its range and wraps to
    //! zero, as Linux's powercap `energy_uj` does at `max_energy_range_uj`.

    /// What the counter advanced from `before` to `after`, where it wrapped at
    /// most once between them; `None` for a reading outside `0..=range`.
    pub fn span(before: u64, after: u64, range: u64) -> Option<u64> {
        if before > range || after > range {
            return None;
        }
        Some(if after >= before { after - before } else { range - before + 1 + after })
    }

    #[cfg(test)]
    mod tests {
        use super::span;

        #[test]
        fn a_counter_that_did_not_wrap_spans_the_difference() {
            assert_eq!(span(100, 350, 1000), Some(250));
        }

        #[test]
        fn a_wrap_counts_the_steps_to_the_range_and_on_from_zero() {
            assert_eq!(span(990, 9, 999), Some(19));
            assert_eq!(span(999, 0, 999), Some(1));
        }

        #[test]
        fn a_reading_past_the_range_is_refused() {
            assert_eq!(span(1001, 5, 1000), None);
            assert_eq!(span(5, 1001, 1000), None);
        }
    }
}

pub mod window {
    //! Whether one CPU's two counters readings describe the row they were
    //! read around. Each reading carries the CPU's own counter as it
    //! answered, so the readings' window is the difference of the two,
    //! whatever the kernel served.
    //!
    //! The row is the span from the first read's call to the second read's
    //! return. A CPU answers each read inside its call, so its window lies
    //! within the row and is shorter by at most the two calls' cost, except
    //! that a read issued within [`JOIN_NS`] of another round is served that
    //! round, issued up to [`JOIN_NS`] before the call. A window outside
    //! those bounds is not the row's.

    /// The kernel's counters join interval (`kernel/src/counters.rs`'s
    /// `JOIN`): how long before its call a read may be served a round.
    pub const JOIN_NS: u64 = 10_000_000;

    /// The row's span and what its two reads cost, in nanoseconds.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Span {
        pub row_ns: u64,
        pub reads_ns: u64,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Refusal {
        /// The kernel read the CPU stale: it did not answer within the
        /// read's bound, and its values are the last it published.
        Stale,
        /// A reading without the CPU's counter: no window can be read off it.
        NoStamp,
        /// Both readings are one: nothing was measured.
        OneReading,
        /// The readings span `window_ns`, outside what a row of `row_ns`
        /// read at a cost of `reads_ns` allows.
        NotTheRow { window_ns: u64, row_ns: u64, reads_ns: u64 },
    }

    impl std::fmt::Display for Refusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Refusal::Stale => write!(f, "the kernel read this CPU stale"),
                Refusal::NoStamp => write!(f, "a reading carries no counter"),
                Refusal::OneReading => write!(f, "both reads were served one reading"),
                Refusal::NotTheRow { window_ns, row_ns, reads_ns } => write!(
                    f,
                    "the readings span {} us against a row of {} us read in {} us",
                    window_ns / 1000,
                    row_ns / 1000,
                    reads_ns / 1000
                ),
            }
        }
    }

    /// The readings' window in counter ticks and nanoseconds, where it is
    /// the row's. `to_ns` converts ticks of the CPU's counter to nanoseconds.
    pub fn of(
        stale: bool,
        before: Option<u64>,
        after: Option<u64>,
        span: Span,
        to_ns: impl Fn(u64) -> u64,
    ) -> Result<(u64, u64), Refusal> {
        if stale {
            return Err(Refusal::Stale);
        }
        let (Some(before), Some(after)) = (before, after) else { return Err(Refusal::NoStamp) };
        let ticks = after.wrapping_sub(before);
        if ticks == 0 {
            return Err(Refusal::OneReading);
        }
        let window_ns = to_ns(ticks);
        let shortest = span.row_ns.saturating_sub(span.reads_ns);
        let longest = span.row_ns.saturating_add(JOIN_NS);
        if !(shortest..=longest).contains(&window_ns) {
            return Err(Refusal::NotTheRow { window_ns, row_ns: span.row_ns, reads_ns: span.reads_ns });
        }
        Ok((ticks, window_ns))
    }

    /// `part` of the window's `ticks`, in thousandths: for a counter that
    /// counts at the window counter's rate.
    pub fn permille(part: u64, ticks: u64) -> u64 {
        part.saturating_mul(1000) / ticks.max(1)
    }

    /// APERF over MPERF, in thousandths: the one figure SDM Vol. 3B §17.2
    /// and AMD's APM Vol. 2 §17.3 define of the pair. Neither defines
    /// MPERF's rate, so neither a clock nor a busy fraction is read off it.
    pub fn aperf_mperf_permille(aperf: u64, mperf: u64) -> Option<u64> {
        (mperf > 0).then(|| aperf.saturating_mul(1000) / mperf)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// A 2 GHz counter: two ticks a nanosecond.
        fn ns(ticks: u64) -> u64 {
            ticks / 2
        }

        const ROW: Span = Span { row_ns: 1_000_000_000, reads_ns: 200_000 };

        #[test]
        fn a_window_inside_the_row_by_less_than_its_reads_stands() {
            assert_eq!(of(false, Some(1_000), Some(1_000 + 1_999_800_000), ROW, ns), Ok((1_999_800_000, 999_900_000)));
            assert_eq!(of(false, Some(0), Some(1_999_600_000), ROW, ns), Ok((1_999_600_000, 999_800_000)), "short by the reads exactly");
        }

        #[test]
        fn a_before_served_from_a_joined_round_stands_within_the_join() {
            assert_eq!(of(false, Some(0), Some(2_020_000_000), ROW, ns), Ok((2_020_000_000, 1_010_000_000)));
            assert_eq!(
                of(false, Some(0), Some(2_020_000_002), ROW, ns),
                Err(Refusal::NotTheRow { window_ns: 1_010_000_001, row_ns: 1_000_000_000, reads_ns: 200_000 })
            );
        }

        #[test]
        fn a_window_shorter_than_the_row_less_its_reads_is_refused() {
            // An `after` served a round issued inside the row.
            assert_eq!(
                of(false, Some(0), Some(1_900_000_000), ROW, ns),
                Err(Refusal::NotTheRow { window_ns: 950_000_000, row_ns: 1_000_000_000, reads_ns: 200_000 })
            );
        }

        #[test]
        fn one_reading_served_twice_is_refused() {
            assert_eq!(of(false, Some(77), Some(77), ROW, ns), Err(Refusal::OneReading));
        }

        #[test]
        fn a_stale_or_unstamped_reading_is_refused() {
            assert_eq!(of(true, Some(0), Some(2_000_000_000), ROW, ns), Err(Refusal::Stale));
            assert_eq!(of(false, None, Some(5), ROW, ns), Err(Refusal::NoStamp));
        }

        #[test]
        fn residency_is_a_fraction_of_the_window_and_the_pair_a_ratio() {
            let (ticks, _) = of(false, Some(0), Some(2_000_000_000), ROW, ns).unwrap();
            assert_eq!(permille(1_995_000_000, ticks), 997);
            assert_eq!(aperf_mperf_permille(4_200, 2_400), Some(1750));
            assert_eq!(aperf_mperf_permille(5, 0), None, "a CPU that never ran has no ratio");
        }
    }
}

pub mod settle {
    //! A clock has settled once every reading over [`STEADY_NS`] agrees
    //! within 3%. A frequency governor can hold a CPU that just turned busy
    //! at a lower clock for tens of milliseconds before it steps up, so a
    //! span much shorter than that takes the step for the settled clock. The
    //! smallest steps measured are the AMD bench's 1.65 to 1.85 GHz and the
    //! T14's 3.79 to 4.19 GHz, 11% and 9.5%; a boosting CPU's readings
    //! agreed within 1% wherever reading the time was cheap.

    /// How long the readings must agree.
    pub const STEADY_NS: u64 = 50_000_000;

    /// The readings since the last one that disagreed.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Plateau {
        since_ns: u64,
        lo: u64,
        hi: u64,
    }

    impl Plateau {
        pub fn new(at_ns: u64, mhz: u64) -> Self {
            Plateau { since_ns: at_ns, lo: mhz, hi: mhz }
        }

        /// The reading `mhz` taken at `at_ns`: whether every reading for
        /// [`STEADY_NS`] up to it agrees within 3%. A reading that does not
        /// agree with those before it starts the span again from itself.
        pub fn settled_with(&mut self, at_ns: u64, mhz: u64) -> bool {
            let (lo, hi) = (self.lo.min(mhz), self.hi.max(mhz));
            if (hi - lo) * 100 > hi * 3 {
                *self = Plateau::new(at_ns, mhz);
            } else {
                (self.lo, self.hi) = (lo, hi);
            }
            at_ns - self.since_ns >= STEADY_NS
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const MS: u64 = 1_000_000;

        /// The first time a probe every quarter millisecond from `at` settles.
        fn settles(readings: impl Fn(u64) -> u64) -> Option<(u64, u64)> {
            let mut plateau = Plateau::new(0, readings(0));
            (1..4000).map(|i| i * MS / 4).find_map(|at| plateau.settled_with(at, readings(at)).then(|| (at, readings(at))))
        }

        #[test]
        fn a_governors_30_ms_step_is_not_the_settled_clock() {
            let ramp = |at: u64| if at < 30 * MS { 1362 } else { 4033 };
            assert_eq!(settles(ramp), Some((80 * MS, 4033)));
        }

        #[test]
        fn the_smallest_measured_steps_are_not_the_settled_clock() {
            for (from, to) in [(1650, 1850), (3789, 4189)] {
                let step = |at: u64| if at < 30 * MS { from } else { to };
                assert_eq!(settles(step), Some((80 * MS, to)), "{from} to {to} MHz");
            }
        }

        #[test]
        fn readings_within_a_percent_or_two_hold_the_span() {
            let wobble = |at: u64| if (at / MS).is_multiple_of(2) { 4241 } else { 4170 };
            assert_eq!(settles(wobble), Some((50 * MS, wobble(50 * MS))));
        }

        #[test]
        fn a_reading_that_disagrees_starts_the_span_again() {
            let mut plateau = Plateau::new(0, 4000);
            assert!(!plateau.settled_with(40 * MS, 4000));
            assert!(!plateau.settled_with(41 * MS, 3700), "7.5% below");
            assert!(!plateau.settled_with(90 * MS, 3700), "49 ms since the disagreement");
            assert!(plateau.settled_with(91 * MS, 3700));
        }

        #[test]
        fn a_slow_drift_settles_only_once_it_stops() {
            let drift = |at: u64| 3000 + (at / MS).min(200) * 5;
            assert_eq!(settles(drift).map(|(_, mhz)| mhz), Some(4000));
        }
    }
}
