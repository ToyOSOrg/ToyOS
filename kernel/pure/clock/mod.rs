//! What the clock computes: the Unix second at boot that a wall-clock reading
//! fixes ([`anchor`]), and the counter held to a reference timer
//! ([`reference`]).

#![forbid(unsafe_code)]

pub mod reference;

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Unix seconds at boot, from a reading of `secs` true when the clock read
/// `at`, which is after boot where the kernel read the wall clock and before
/// it where the loader did; both are stamps, nanoseconds since the counter's
/// zero. Only the whole seconds between the two move the answer.
pub const fn anchor(secs: u64, at: u64, boot: u64) -> u64 {
    if at >= boot {
        secs.saturating_sub((at - boot) / NANOS_PER_SEC)
    } else {
        secs.saturating_add((boot - at) / NANOS_PER_SEC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECS: u64 = 1_993_799_665;
    const BOOT: u64 = 50_000_000_000;

    #[test]
    fn a_reading_after_boot_is_carried_back_to_it() {
        assert_eq!(anchor(SECS, BOOT + 3_700_000_000, BOOT), SECS - 3);
    }

    #[test]
    fn a_reading_before_boot_is_carried_forward_to_it() {
        assert_eq!(anchor(SECS, BOOT - 2_500_000_000, BOOT), SECS + 2);
    }

    #[test]
    fn a_reading_at_boot_is_boot() {
        assert_eq!(anchor(SECS, BOOT, BOOT), SECS);
    }
}
