//! The judgements osbench makes over what it read, apart from the reading, so
//! a host tests them: whether a munmap pair's figure stands ([`pair`]), where
//! the CPU crunch stops for heat ([`heat`]), and how far an energy counter
//! advanced across its wrap ([`energy`]).

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

    /// Where the stop is when nothing says the CPU's TjMax. Policy: the
    /// lowest TjMax in [`PUBLISHED`] less the margin.
    pub const ASSUMED_STOP_MC: i64 = 85_000;

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
