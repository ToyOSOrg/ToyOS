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
