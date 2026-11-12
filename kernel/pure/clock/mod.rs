//! What the clock computes: the Unix second at boot that a wall-clock reading
//! fixes ([`anchor`]), the counter held to a reference timer
//! ([`reference`]), and a span in counter ticks whatever is known of the
//! counter's rate ([`ticks_at_least`]).

#![forbid(unsafe_code)]

pub mod reference;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// What opens the kernel's report each time its refinement holds the TSC to
/// the HPET over a span (`clock: over <span> s the TSC counted ...`): written
/// by the first scheduler pass after each doubling span.
pub const TSC_REFINED: &str = "clock: over ";

/// What opens the kernel's report of a span in which the TSC stepped against
/// the chipset's counters (`clock: TSC STEP: the TSC stepped <n> ns ...`),
/// written as the sampler finds it.
pub const TSC_STEP: &str = "clock: TSC STEP: ";

/// Unix seconds at boot, from the loader's reading of `secs`, true when the
/// clock read `at`, before boot; both are stamps, nanoseconds since the
/// counter's zero. Only the whole seconds between the two move the answer.
pub const fn anchor(secs: u64, at: u64, boot: u64) -> u64 {
    secs.saturating_add(boot.saturating_sub(at) / NANOS_PER_SEC)
}

/// The rate a span was converted to counter ticks at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rate {
    /// The rate the boot measured.
    Calibrated,
    /// The rate the CPU states.
    Stated,
    /// No rate, but a ceiling on any the counter may have: the ticks take at
    /// least the span.
    Ceiling,
}

/// `nanos` as ticks of a counter at `hz`, a span under a second included.
pub const fn ticks_at(nanos: u64, hz: u64) -> u64 {
    let ticks = nanos as u128 * hz as u128 / NANOS_PER_SEC as u128;
    if ticks > u64::MAX as u128 { u64::MAX } else { ticks as u64 }
}

/// `nanos` as counter ticks that take at least that long: `measured`, the
/// span at the rate the boot measured, else at the rate the CPU states, else
/// at a ceiling on its rate. `None` where none of the three is known.
pub fn ticks_at_least(
    nanos: u64,
    measured: Option<u64>,
    stated_hz: Option<u64>,
    ceiling_hz: Option<u64>,
) -> Option<(u64, Rate)> {
    match (measured, stated_hz, ceiling_hz) {
        (Some(ticks), _, _) => Some((ticks, Rate::Calibrated)),
        (None, Some(hz), _) => Some((ticks_at(nanos, hz), Rate::Stated)),
        (None, None, Some(hz)) => Some((ticks_at(nanos, hz), Rate::Ceiling)),
        (None, None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECS: u64 = 1_993_799_665;
    const BOOT: u64 = 50_000_000_000;

    #[test]
    fn a_reading_before_boot_is_carried_forward_to_it() {
        assert_eq!(anchor(SECS, BOOT - 2_500_000_000, BOOT), SECS + 2);
    }

    #[test]
    fn a_reading_at_boot_is_boot() {
        assert_eq!(anchor(SECS, BOOT, BOOT), SECS);
    }

    const MINUTE: u64 = 60 * NANOS_PER_SEC;
    const CEILING: u64 = 10_000_000_000;

    #[test]
    fn the_best_known_rate_converts_the_span() {
        assert_eq!(ticks_at_least(MINUTE, Some(7), Some(1), Some(CEILING)), Some((7, Rate::Calibrated)));
        assert_eq!(ticks_at_least(MINUTE, None, Some(2_000_000_000), Some(CEILING)), Some((120_000_000_000, Rate::Stated)));
        assert_eq!(ticks_at_least(MINUTE, None, None, Some(CEILING)), Some((600_000_000_000, Rate::Ceiling)));
        assert_eq!(ticks_at_least(MINUTE, None, None, None), None);
    }

    /// The ceiling's ticks last the span or longer at every rate under it, and
    /// at a rate past it end sooner, never unbounded.
    #[test]
    fn a_ceiling_count_lasts_at_least_the_span() {
        let (ticks, _) = ticks_at_least(MINUTE, None, None, Some(CEILING)).unwrap();
        for hz in [1_000_000, 2_419_200_000, 5_800_000_000, CEILING] {
            assert!(u128::from(ticks) * u128::from(NANOS_PER_SEC) / u128::from(hz) >= u128::from(MINUTE), "{hz} Hz");
        }
        assert!(u128::from(ticks) * u128::from(NANOS_PER_SEC) / u128::from(2 * CEILING) == u128::from(MINUTE / 2));
    }

    #[test]
    fn a_span_under_a_second_keeps_its_ticks() {
        assert_eq!(ticks_at(1_000_000, 3_000_000_000), 3_000_000);
        assert_eq!(ticks_at(u64::MAX, u64::MAX), u64::MAX);
    }
}
