//! Which run a registered test belongs to.
//!
//! **The table is the registration.** Every row of `tests/toyos.rs`'s
//! `MACHINE_TESTS`, `SCREEN_TESTS` and `AUDIO_TESTS` carries its [`Tier`] or
//! does not compile, and the shared boot's discovered tests share one. A
//! [`Schedule`] is every one of those names at its one tier. Moving a test
//! between tiers is editing that one word.
//!
//! The tiers nest: a plain `cargo test` reaches `Fast`, `--nightly` adds
//! `Nightly`, and `--weekly` adds `Weekly` to that. `.github/workflows/nightly.yml`
//! runs the nightly reach six nights a week and the weekly reach on the seventh
//! (`src/ci.rs`). No pull request boots a guest.
//!
//! A test's tier is what it has caught: `Fast` is the shared boot and the tests
//! that caught a real defect for at most 5 s of guest time, `Nightly` the other
//! tests that caught one, and `Weekly` those that never did; a group sharing one
//! boot takes its most frequent member's tier. A new test enters `Nightly`.
//!
//! The local tier is the fourth, and the only one CI never runs: its guests are
//! of an architecture no hosted runner has been measured to boot.

use std::collections::BTreeMap;

use crate::testargs::{NIGHTLY, SUITE, WEEKLY};

/// Which run a registered test belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tier {
    /// Every `cargo test`.
    Fast,
    /// `cargo test --test toyos-build -- --nightly`, and every weekly run.
    Nightly,
    /// `cargo test --test toyos-build -- --weekly`, and before a release.
    Weekly,
    /// Every `cargo test` on a developer's machine and no sharded run: a guest
    /// of an architecture no CI runner boots yet. The AArch64 port's stage 8
    /// (`issues/kernel/toyos-runs-on-arm64.md`) measures the runners and
    /// moves these rows to `Fast` or `Nightly`.
    Local,
}

/// How far down the nested tiers a run reaches.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Reach {
    Fast,
    Nightly,
    Weekly,
}

impl Reach {
    /// The reach `args` ask for; `testargs::parse` refuses both flags at once.
    pub fn of(args: &[String]) -> Self {
        if SUITE.present(args, &WEEKLY) {
            Self::Weekly
        } else if SUITE.present(args, &NIGHTLY) {
            Self::Nightly
        } else {
            Self::Fast
        }
    }
}

impl Tier {
    /// Whether a run that reaches `reach` selects this tier; `sharded` is a
    /// `--shard`, which only CI's jobs pass.
    pub fn selected(self, reach: Reach, sharded: bool) -> bool {
        match self {
            Self::Fast => true,
            Self::Nightly => reach >= Reach::Nightly,
            Self::Weekly => reach >= Reach::Weekly,
            Self::Local => !sharded,
        }
    }

    /// The flag of the narrowest run that selects this tier, for a run that
    /// held it back to name; `None` for the tiers every unsharded run takes.
    pub fn flag(self) -> Option<&'static str> {
        match self {
            Self::Nightly => Some(NIGHTLY.name),
            Self::Weekly => Some(WEEKLY.name),
            Self::Fast | Self::Local => None,
        }
    }
}

/// Every registered test at its one tier.
pub struct Schedule<'a>(BTreeMap<&'a str, Tier>);

impl<'a> Schedule<'a> {
    /// Refuses a name registered twice, by name, at one tier or two: two rows
    /// are two verdicts under one name.
    pub fn new(rows: impl IntoIterator<Item = (&'a str, Tier)>) -> Result<Self, String> {
        let mut tiers = BTreeMap::new();
        for (name, tier) in rows {
            if let Some(first) = tiers.insert(name, tier) {
                return Err(format!(
                    "{name} is registered twice, at {first:?} and at {tier:?}: one name is one \
                     verdict at one tier"
                ));
            }
        }
        Ok(Self(tiers))
    }

    /// `name`'s tier, refused by name when nothing registers it.
    pub fn tier(&self, name: &str) -> Result<Tier, String> {
        self.0
            .get(name)
            .copied()
            .ok_or_else(|| format!("{name} is not a registered test, so it has no tier"))
    }

    /// Every registered name and its tier, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&'a str, Tier)> + '_ {
        self.0.iter().map(|(name, tier)| (*name, *tier))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVERY: [Tier; 4] = [Tier::Fast, Tier::Nightly, Tier::Weekly, Tier::Local];
    const NAMES: [&str; 4] = ["fast_one", "nightly_one", "weekly_one", "local_one"];

    #[test]
    fn every_registered_test_has_exactly_one_tier() {
        let schedule = Schedule::new(NAMES.into_iter().zip(EVERY)).unwrap();
        for (name, tier) in NAMES.into_iter().zip(EVERY) {
            assert_eq!(schedule.tier(name), Ok(tier), "{name}");
        }
        assert_eq!(schedule.iter().count(), EVERY.len());
        for second in EVERY {
            let refusal =
                Schedule::new([("twice", Tier::Fast), ("once", Tier::Weekly), ("twice", second)])
                    .err()
                    .expect("a second row for one name is refused");
            assert!(refusal.starts_with("twice is registered twice"), "{refusal}");
        }
    }

    #[test]
    fn an_unregistered_test_is_refused_by_name() {
        let schedule = Schedule::new([("registered", Tier::Weekly)]).unwrap();
        let refusal = schedule.tier("never_registered").unwrap_err();
        assert!(refusal.starts_with("never_registered is not a registered test"), "{refusal}");
        assert!(schedule.tier("registere").is_err(), "a prefix is not the name");
    }

    /// Each reach selects its own tier and every narrower one, and nothing
    /// wider; a shard alone decides `Local`.
    #[test]
    fn the_tiers_nest() {
        let selected = |reach| -> Vec<Tier> {
            EVERY.into_iter().filter(|tier| tier.selected(reach, true)).collect()
        };
        assert_eq!(selected(Reach::Fast), [Tier::Fast]);
        assert_eq!(selected(Reach::Nightly), [Tier::Fast, Tier::Nightly]);
        assert_eq!(selected(Reach::Weekly), [Tier::Fast, Tier::Nightly, Tier::Weekly]);
        for reach in [Reach::Fast, Reach::Nightly, Reach::Weekly] {
            assert!(Tier::Local.selected(reach, false) && !Tier::Local.selected(reach, true));
        }
    }

    /// The flag a held-back tier names is the one whose reach selects it.
    #[test]
    fn a_held_tiers_flag_selects_it() {
        for tier in EVERY {
            let Some(flag) = tier.flag() else {
                assert!(tier.selected(Reach::Fast, false), "{tier:?} names no flag, so a plain run takes it");
                continue;
            };
            let reach = Reach::of(&[flag.to_string()]);
            assert!(tier.selected(reach, true), "{flag} does not select {tier:?}");
            assert!(!tier.selected(Reach::Fast, true), "{tier:?} names {flag} and needs none");
        }
        assert_eq!(Reach::of(&[]), Reach::Fast);
    }
}
