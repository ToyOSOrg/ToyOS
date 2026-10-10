//! What the clock computes from counter ticks: their span in nanoseconds
//! ([`ticks_to_nanos`]), and the Unix second at boot that a wall-clock reading
//! taken at some counter value fixes ([`anchor`]).

#![forbid(unsafe_code)]

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// `ticks` of a counter whose tick is `period_fs` femtoseconds, in nanoseconds.
pub const fn ticks_to_nanos(ticks: u64, period_fs: u64) -> u64 {
    ((ticks as u128 * period_fs as u128) / 1_000_000) as u64
}

/// Unix seconds at the counter's `boot`, from a reading of `secs` true when the
/// counter read `at`, which is after boot where the kernel read the clock and
/// before it where the loader did. Only the whole seconds between the two move
/// the answer.
pub const fn anchor(secs: u64, at: u64, boot: u64, period_fs: u64) -> u64 {
    if at >= boot {
        secs.saturating_sub(ticks_to_nanos(at - boot, period_fs) / NANOS_PER_SEC)
    } else {
        secs.saturating_add(ticks_to_nanos(boot - at, period_fs) / NANOS_PER_SEC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-nanosecond tick, so a tick count reads as nanoseconds.
    const NS: u64 = 1_000_000;
    const SECS: u64 = 1_993_799_665;
    const BOOT: u64 = 50_000_000_000;

    #[test]
    fn a_reading_after_boot_is_carried_back_to_it() {
        assert_eq!(anchor(SECS, BOOT + 3_700_000_000, BOOT, NS), SECS - 3);
    }

    #[test]
    fn a_reading_before_boot_is_carried_forward_to_it() {
        assert_eq!(anchor(SECS, BOOT - 2_500_000_000, BOOT, NS), SECS + 2);
    }

    #[test]
    fn a_reading_at_boot_is_boot() {
        assert_eq!(anchor(SECS, BOOT, BOOT, NS), SECS);
    }

    #[test]
    fn a_tick_is_counted_at_its_period() {
        // A 24 MHz counter: 41_666_666 fs a tick, so 24e6 ticks fall just short of a second.
        assert_eq!(ticks_to_nanos(24_000_000, 41_666_666), 999_999_984);
        assert_eq!(anchor(SECS, BOOT - 48_000_001, BOOT, 41_666_666), SECS + 2);
    }
}
