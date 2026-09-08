//! Every number the metal suite measures on the T14, with the ceiling past
//! which it is a red.
//!
//! One committed file, [`PATH`], read by the judge — so a slower boot or a
//! wider tail reds against a number somebody wrote down and not against
//! nothing. The judge asks by name and a name with no row is a **refusal**: a
//! measurement nobody has priced must not pass by having no ceiling.
//!
//! `ceiling` is the verdict and `ceiling_from` says where it came from.
//! `measured` is the last reading the machine gave and is absent until a run
//! has taken one; it is evidence for the next author, never the gate — a bound
//! asserted against its own measurement is not a bound.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

/// Where the committed file lives, relative to the repository root.
pub const PATH: &str = "tests/metal-profile.toml";

/// What the boot around a job list costs it, in milliseconds.
///
/// The runner's bound is measured from boot rather than from its first job
/// (`userland/test-runner`'s deadline reads `clock_nanos`), so everything
/// before the list comes off the bound before the members get any of it — and
/// the `reboot` job the image derivation appends comes off the other end.
///
/// A tenth of the bound, derived rather than measured, and several times what
/// either end has ever cost: `boot.*.complete_ms` reads 1,199-1,267 ms on every
/// T14 boot this suite has taken, the runner itself spawns about 500 ms after
/// that, and `reboot`'s own job spawned 122 ms before `Rebooting.` on run 24's
/// `ccorpus`. Nine tenths is what the members may spend.
pub const AROUND_THE_LIST_MS: u64 = toyos_tco::JOB_BOUND_MS / 10;

/// The name under which one boot's per-member allowance is priced.
pub fn job_ms_row(boot: &str) -> String {
    format!("list.{boot}.job_ms")
}

/// How many members a job list may carry when one member is allowed `job_ms`.
///
/// **The runner's bound ends the whole list and not the job it is inside**,
/// which is why a suite that runs seventy-two binaries on one boot needs this
/// step at all: past this count the members never run, and the boot reports
/// them as missing exit records rather than as a list nobody sized.
pub fn members_per_boot(job_ms: u64) -> usize {
    let spendable = toyos_tco::JOB_BOUND_MS.saturating_sub(AROUND_THE_LIST_MS);
    // A member priced above the whole allowance still gets a boot of its own:
    // one member per boot is the smallest a list can be cut to, and the boot
    // then reds on its own measured cost rather than on being unsplittable.
    usize::try_from(spendable / job_ms.max(1)).unwrap_or(usize::MAX).max(1)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    #[serde(default)]
    pub number: Vec<Row>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// What the judge asks for it by. One row per name.
    pub name: String,
    pub unit: String,
    /// Past this is a red.
    pub ceiling: u64,
    /// Where the ceiling came from: a constant this repository declares, or the
    /// margin over a reading. A ceiling with no derivation is a guess.
    pub ceiling_from: String,
    /// The last reading the machine gave, if one has been taken.
    #[serde(default)]
    pub measured: Option<u64>,
}

/// Why a profile could not be read, or why a reading is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unfit {
    /// The file itself.
    File(String),
    /// Two rows under one name: the judge would answer with whichever it found.
    Duplicate(String),
    /// A row whose committed reading is already past its own ceiling.
    Stale { name: String, measured: u64, ceiling: u64 },
    /// A number nobody has priced. **The gate fails closed here.**
    Unpriced(String),
    /// A reading past the ceiling.
    Over { name: String, unit: String, value: u64, ceiling: u64, from: String },
}

impl std::fmt::Display for Unfit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File(why) => write!(f, "{PATH}: {why}"),
            Self::Duplicate(name) => {
                write!(f, "{PATH} carries two rows named {name:?}, and a judge reading it would \
                          answer with whichever it found first")
            }
            Self::Stale { name, measured, ceiling } => write!(
                f,
                "{PATH}'s {name:?} records {measured} against its own ceiling of {ceiling}: the \
                 committed reading is already a red"
            ),
            Self::Unpriced(name) => write!(
                f,
                "the metal suite measured {name:?} and {PATH} prices no such number; add a row \
                 with a ceiling and where it came from, because a measurement with no ceiling \
                 cannot fail"
            ),
            Self::Over { name, unit, value, ceiling, from } => write!(
                f,
                "{name} is {value} {unit} against a ceiling of {ceiling} ({from})"
            ),
        }
    }
}

impl Profile {
    /// Read and check the committed file. The checks are the file's own
    /// well-formedness, never a machine's.
    pub fn load(root: &Path) -> Result<Self, Unfit> {
        let at = root.join(PATH);
        let text = std::fs::read_to_string(&at)
            .map_err(|e| Unfit::File(format!("{} — {e}", at.display())))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, Unfit> {
        let profile: Self = toml::from_str(text).map_err(|e| Unfit::File(e.to_string()))?;
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for row in &profile.number {
            if !seen.insert(row.name.as_str()) {
                return Err(Unfit::Duplicate(row.name.clone()));
            }
            if let Some(measured) = row.measured {
                if measured > row.ceiling {
                    return Err(Unfit::Stale {
                        name: row.name.clone(),
                        measured,
                        ceiling: row.ceiling,
                    });
                }
            }
        }
        Ok(profile)
    }

    pub fn row(&self, name: &str) -> Option<&Row> {
        self.number.iter().find(|row| row.name == name)
    }

    /// How many members `boot`'s job list may carry, from the allowance this
    /// file prices for it. A boot with no allowance is refused, like any other
    /// unpriced number: a list nobody has priced cannot be sized to the bound.
    pub fn members_per_boot(&self, boot: &str) -> Result<usize, Unfit> {
        let name = job_ms_row(boot);
        let row = self.row(&name).ok_or(Unfit::Unpriced(name))?;
        Ok(members_per_boot(row.ceiling))
    }

    /// One reading against its row. A name with no row is refused.
    pub fn judge(&self, name: &str, value: u64) -> Result<(), Unfit> {
        let Some(row) = self.row(name) else { return Err(Unfit::Unpriced(name.to_string())) };
        if value > row.ceiling {
            return Err(Unfit::Over {
                name: row.name.clone(),
                unit: row.unit.clone(),
                value,
                ceiling: row.ceiling,
                from: row.ceiling_from.clone(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    /// Every constant a `ceiling_from` may name, and what it holds.
    ///
    /// **The file says where a ceiling came from, and this is what makes that
    /// true.** A row naming a constant carried a literal nobody re-derived when
    /// the constant moved: fifteen rows read 360 against a `return_secs` of 420,
    /// so a boot back in 361-420 s was inside the driver's own wait and red on a
    /// ceiling nobody wrote.
    const NAMED: &[(&str, u64)] = &[
        ("toyos_build::metal::return_secs", 420),
        ("toyos_build::metal::STICK_SECS", 30),
        ("toyos_tco::JOB_BOUND_MS", toyos_tco::JOB_BOUND_MS),
        ("toyos_tco::BOUND_MS", toyos_tco::BOUND_MS),
        ("toyos_tco::WEDGE_BOUND_MS", toyos_tco::WEDGE_BOUND_MS),
        ("toyos_tco::HARD_LOCKUP_BOUND_MS", toyos_tco::HARD_LOCKUP_BOUND_MS),
    ];

    /// A row whose ceiling *is* a constant this repository declares carries that
    /// constant's value.
    #[test]
    fn a_ceiling_that_names_a_constant_carries_it() {
        assert_eq!(crate::metal::return_secs(), 420, "the derivation moved");
        assert_eq!(crate::metal::STICK_SECS, 30);
        let profile = Profile::load(root()).expect(PATH);
        let mut checked = 0usize;
        for row in &profile.number {
            let Some((named, want)) =
                NAMED.iter().find(|(named, _)| row.ceiling_from.starts_with(named))
            else {
                continue;
            };
            checked += 1;
            assert_eq!(
                row.ceiling, *want,
                "{:?} says its ceiling is {named}, and {named} is {want}",
                row.name
            );
        }
        assert!(checked > 0, "no row names a constant, so this gate holds nothing");
    }

    /// The committed file parses, no name is priced twice, and no row's own
    /// recorded reading is already past the ceiling beside it.
    #[test]
    fn the_committed_profile_is_well_formed() {
        let profile = Profile::load(root()).expect("tests/metal-profile.toml");
        assert!(
            !profile.number.is_empty(),
            "{PATH} prices nothing, so every number the suite measures would be unpriced"
        );
        for row in &profile.number {
            assert!(!row.unit.is_empty(), "{:?} has no unit", row.name);
            assert!(
                !row.ceiling_from.is_empty(),
                "{:?}'s ceiling says nowhere it came from, which makes it a guess",
                row.name
            );
        }
    }

    /// The gate is the ceiling and not the reading: a number nobody priced is
    /// refused rather than passed.
    #[test]
    fn an_unpriced_number_is_refused_and_not_passed() {
        let profile = Profile::parse(
            "[[number]]\nname = \"boot.a.complete_ms\"\nunit = \"ms\"\nceiling = 100\n\
             ceiling_from = \"a constant\"\n",
        )
        .unwrap();
        assert_eq!(profile.judge("boot.a.complete_ms", 100), Ok(()));
        assert!(matches!(profile.judge("boot.a.complete_ms", 101), Err(Unfit::Over { .. })));
        assert_eq!(
            profile.judge("boot.b.complete_ms", 1),
            Err(Unfit::Unpriced("boot.b.complete_ms".to_string()))
        );
        let said = profile.judge("boot.b.complete_ms", 1).unwrap_err().to_string();
        assert!(said.contains("cannot fail"), "{said}");
    }

    #[test]
    fn a_row_whose_reading_is_past_its_own_ceiling_is_refused_at_the_file() {
        let refusal = Profile::parse(
            "[[number]]\nname = \"n\"\nunit = \"ms\"\nceiling = 10\nceiling_from = \"c\"\n\
             measured = 11\n",
        )
        .unwrap_err();
        assert_eq!(
            refusal,
            Unfit::Stale { name: "n".to_string(), measured: 11, ceiling: 10 }
        );
        assert!(matches!(
            Profile::parse(
                "[[number]]\nname = \"n\"\nunit = \"ms\"\nceiling = 10\nceiling_from = \"c\"\n\
                 [[number]]\nname = \"n\"\nunit = \"ms\"\nceiling = 20\nceiling_from = \"c\"\n"
            ),
            Err(Unfit::Duplicate(_))
        ));
    }
}

#[cfg(test)]
mod sizing_tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    /// **The overrun this rule exists for, in the machine's own numbers.**
    ///
    /// Run 24's `shared` boot ran 72 members at a measured 845 ms each. That is
    /// 60.8 s of members inside a 60 s bound, so the runner reset the machine
    /// with 47 of them unrun — and each was reported as a missing exit record
    /// rather than as a list too long for the bound. The committed allowance
    /// cuts that list where the bound does.
    #[test]
    fn run_24s_shared_list_does_not_fit_one_boot() {
        let profile = Profile::load(root()).expect(PATH);
        let per = profile.members_per_boot("shared").expect("shared is priced");
        assert!(
            per < 72,
            "the allowance leaves room for {per} members and run 24 tried 72 of them in one boot"
        );
        // Two chunks and not three: a list cut finer costs another minute of
        // the machine for nothing.
        assert_eq!(72_usize.div_ceil(per), 2, "{per} members a boot");
        let measured = profile.row(&job_ms_row("shared")).and_then(|r| r.measured);
        assert_eq!(measured, Some(845), "the reading the ceiling is a margin over");
        assert!(72 * 845 > toyos_tco::JOB_BOUND_MS, "the overrun the rule is derived from");
    }

    /// `ccorpus` was *green* at 49.5 s of the 60 s bound, and the same rule
    /// still cuts it: 82 % of a bound is no margin for a slower stick, and the
    /// price of being wrong is every member after the cut losing its verdict.
    #[test]
    fn run_24s_c_corpus_is_cut_although_it_passed() {
        let profile = Profile::load(root()).expect(PATH);
        let per = profile.members_per_boot("ccorpus").expect("ccorpus is priced");
        assert_eq!(118_usize.div_ceil(per), 2, "{per} members a boot");
    }

    /// A member priced above the whole bound still gets a boot, rather than a
    /// division that yields zero and a list that can hold nothing.
    #[test]
    fn a_member_priced_beyond_the_bound_still_gets_a_boot() {
        assert_eq!(members_per_boot(toyos_tco::JOB_BOUND_MS * 10), 1);
        let spendable = toyos_tco::JOB_BOUND_MS - AROUND_THE_LIST_MS;
        assert_eq!(members_per_boot(spendable), 1);
        assert_eq!(members_per_boot(spendable / 2), 2);
    }

    /// A boot whose allowance nobody wrote down is refused, not given the
    /// bound: the whole point of the row is that a list is cut to a number
    /// somebody committed.
    #[test]
    fn a_boot_with_no_allowance_is_refused() {
        let profile = Profile::parse("").expect("an empty profile parses");
        assert_eq!(
            profile.members_per_boot("nobody"),
            Err(Unfit::Unpriced("list.nobody.job_ms".to_string()))
        );
    }
}
