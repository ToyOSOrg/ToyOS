//! Which run a registered test belongs to.
//!
//! **The table is the registration.** Every row of `tests/toyos.rs`'s
//! `MACHINE_TESTS`, `SCREEN_TESTS` and `AUDIO_TESTS` carries its [`Tier`] or
//! does not compile, and the shared boot's discovered tests share one. Moving a
//! test between tiers is editing that one word.
//!
//! The fast tier is what every `cargo test` and every pull request runs; the
//! nightly tier is `--nightly`, run by `.github/workflows/ci.yml`'s schedule.
//! [`FAST_CEILING_MS`] is the line between them, and it refuses nothing: every
//! sharded run's `durations` job prints, as warnings, which Fast names measured
//! over it and which Nightly names measured under it ([`off_the_line`]). A name
//! that is Nightly because its verdict is anchored to real time, or because it
//! shares a boot with one that is slow, stays where it is whatever it measures.

/// The line the fast tier is defined by, in milliseconds, measured on CI's
/// hosted shards. A test at exactly the line is fast.
pub const FAST_CEILING_MS: u64 = 10_000;

/// Which run a registered test belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Every `cargo test`.
    Fast,
    /// `cargo test --test toyos-build -- --nightly`.
    Nightly,
}

impl Tier {
    /// The word a shard's duration file carries beside each measurement.
    pub fn token(self) -> &'static str {
        match self {
            Tier::Fast => "fast",
            Tier::Nightly => "nightly",
        }
    }

    pub fn from_token(token: &str) -> Option<Tier> {
        [Tier::Fast, Tier::Nightly].into_iter().find(|t| t.token() == token)
    }
}

/// One sentence per measurement on the wrong side of [`FAST_CEILING_MS`] for
/// the tier it ran in. Information for whoever moves a test, never a refusal.
pub fn off_the_line<'a>(measured: impl IntoIterator<Item = (&'a str, u64, Tier)>) -> Vec<String> {
    measured
        .into_iter()
        .filter_map(|(label, ms, tier)| match tier {
            Tier::Fast if ms > FAST_CEILING_MS => Some(format!(
                "{label} is Fast and measured {ms} ms, over the {FAST_CEILING_MS} ms line"
            )),
            Tier::Nightly if ms <= FAST_CEILING_MS => Some(format!(
                "{label} is Nightly and measured {ms} ms, under the {FAST_CEILING_MS} ms line"
            )),
            Tier::Fast | Tier::Nightly => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_measurement_is_named_only_on_the_wrong_side_of_the_line() {
        let said = off_the_line([
            ("fast_and_fast", FAST_CEILING_MS, Tier::Fast),
            ("fast_and_slow", FAST_CEILING_MS + 1, Tier::Fast),
            ("nightly_and_slow", FAST_CEILING_MS + 1, Tier::Nightly),
            ("nightly_and_fast", FAST_CEILING_MS, Tier::Nightly),
        ]);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].starts_with("fast_and_slow is Fast") && said[0].contains("over"));
        assert!(said[1].starts_with("nightly_and_fast is Nightly") && said[1].contains("under"));
    }

    #[test]
    fn a_tier_round_trips_through_its_token() {
        for tier in [Tier::Fast, Tier::Nightly] {
            assert_eq!(Tier::from_token(tier.token()), Some(tier));
        }
        assert_eq!(Tier::from_token("12"), None);
    }
}
