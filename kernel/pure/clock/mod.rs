//! What the clock computes: the Unix second at boot that a wall-clock reading
//! fixes ([`anchor`]), and the counter held to a reference timer
//! ([`reference`]).

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
}
