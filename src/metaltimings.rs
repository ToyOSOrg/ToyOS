//! Every number the metal suite measures, recorded per machine and judged only
//! against the record of the machine that measured it.
//!
//! One file per machine under [`DIR`], named for its SMBIOS vendor and product,
//! written by a run and committed by whoever ran it. A run on a machine with no
//! record, or with one taken under another BIOS, is recorded and not judged; a
//! name the record lacks is added and not judged; a recorded value is never
//! moved by a run, so a slow run cannot become the baseline the next is judged
//! by. Deleting a row or the file is how a number is re-recorded.
//!
//! [`ceiling`] is the one rule.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the records live, relative to the repository root.
pub const DIR: &str = "tests/metal";

/// Past this a reading is a red: twice what the machine recorded, and a record
/// of zero counts as one unit, the resolution the number was read at.
pub fn ceiling(recorded: u64) -> u64 {
    recorded.max(1).saturating_mul(2)
}

/// A machine as its own firmware names it: SMBIOS type 1's manufacturer and
/// product and type 0's BIOS version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub vendor: String,
    pub product: String,
    pub bios: String,
}

impl Machine {
    /// Asked of the operating system the machine runs between boots; all three
    /// files are world-readable, unlike the serial and UUID beside them.
    pub const QUERY: &str =
        "cat /sys/class/dmi/id/sys_vendor /sys/class/dmi/id/product_name /sys/class/dmi/id/bios_version";

    /// [`Self::QUERY`]'s answer: one non-empty line per file.
    pub fn parse(text: &str) -> Result<Self, String> {
        let lines: Vec<&str> = text.lines().map(str::trim).collect();
        match lines[..] {
            [vendor, product, bios]
                if !vendor.is_empty() && !product.is_empty() && !bios.is_empty() =>
            {
                Ok(Self {
                    vendor: vendor.to_string(),
                    product: product.to_string(),
                    bios: bios.to_string(),
                })
            }
            _ => Err(format!(
                "`{}` answered {text:?}, not three non-empty lines",
                Self::QUERY
            )),
        }
    }

    /// Where this machine's record lives, relative to the repository root.
    pub fn path(&self) -> PathBuf {
        let mut slug = String::new();
        for c in format!("{} {}", self.vendor, self.product).chars() {
            if c.is_ascii_alphanumeric() {
                slug.push(c.to_ascii_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        Path::new(DIR).join(format!("{}.toml", slug.trim_end_matches('-')))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub vendor: String,
    pub product: String,
    pub bios: String,
    pub measured: BTreeMap<String, u64>,
}

impl Record {
    /// `machine`'s record under `root`, `None` where it has none.
    pub fn load(root: &Path, machine: &Machine) -> Result<Option<Self>, String> {
        let at = root.join(machine.path());
        let text = match std::fs::read_to_string(&at) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", at.display())),
        };
        let record: Self = toml::from_str(&text).map_err(|e| format!("{}: {e}", at.display()))?;
        if (record.vendor.as_str(), record.product.as_str())
            != (machine.vendor.as_str(), machine.product.as_str())
        {
            return Err(format!(
                "{} records {} {} and {} {} ran: two machines share one file name",
                at.display(),
                record.vendor,
                record.product,
                machine.vendor,
                machine.product
            ));
        }
        Ok(Some(record))
    }

    pub fn save(&self, root: &Path) -> Result<PathBuf, String> {
        let machine = Machine {
            vendor: self.vendor.clone(),
            product: self.product.clone(),
            bios: self.bios.clone(),
        };
        let at = root.join(machine.path());
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(root.join(DIR)).map_err(|e| format!("{DIR}: {e}"))?;
        std::fs::write(&at, text).map_err(|e| format!("{}: {e}", at.display()))?;
        Ok(at)
    }

    /// The recorded value, only where this record was taken under `machine`'s
    /// BIOS: a value from other firmware judges nothing.
    pub fn get(&self, machine: &Machine, name: &str) -> Option<u64> {
        (self.bios == machine.bios)
            .then(|| self.measured.get(name).copied())
            .flatten()
    }
}

/// A reading past [`ceiling`] of its record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Over {
    pub name: String,
    pub value: u64,
    pub recorded: u64,
}

impl std::fmt::Display for Over {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is {} against {} recorded on this machine, past the ceiling of {}",
            self.name,
            self.value,
            self.recorded,
            ceiling(self.recorded)
        )
    }
}

/// What one run's readings come to on `machine`.
#[derive(Debug)]
pub struct Judged {
    pub over: Vec<Over>,
    /// The record after this run, and whether this run changed it.
    pub record: Record,
    pub changed: bool,
}

pub fn judge(
    machine: &Machine,
    record: Option<Record>,
    measured: &BTreeMap<String, u64>,
) -> Judged {
    let mut record = match record {
        Some(record) if record.bios == machine.bios => record,
        _ => Record {
            vendor: machine.vendor.clone(),
            product: machine.product.clone(),
            bios: machine.bios.clone(),
            measured: BTreeMap::new(),
        },
    };
    let mut over = Vec::new();
    let mut changed = false;
    for (name, &value) in measured {
        match record.measured.get(name) {
            Some(&recorded) if value > ceiling(recorded) => {
                over.push(Over {
                    name: name.clone(),
                    value,
                    recorded,
                });
            }
            Some(_) => {}
            None => {
                record.measured.insert(name.clone(), value);
                changed = true;
            }
        }
    }
    Judged {
        over,
        record,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t14(bios: &str) -> Machine {
        Machine {
            vendor: "LENOVO".to_string(),
            product: "20W000T9GE".to_string(),
            bios: bios.to_string(),
        }
    }

    fn readings(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), *value))
            .collect()
    }

    fn recorded(machine: &Machine, pairs: &[(&str, u64)]) -> Record {
        judge(machine, None, &readings(pairs)).record
    }

    #[test]
    fn a_known_machine_within_its_record_passes() {
        let machine = t14("N34ET56W (1.56 )");
        let record = recorded(
            &machine,
            &[("boot.a.complete_ms", 1166), ("boot.a.stick_secs", 0)],
        );
        let judged = judge(
            &machine,
            Some(record.clone()),
            &readings(&[("boot.a.complete_ms", 2332), ("boot.a.stick_secs", 2)]),
        );
        assert_eq!(judged.over, Vec::new());
        assert!(!judged.changed);
        assert_eq!(judged.record, record);
    }

    #[test]
    fn a_known_machine_outside_its_record_fails_and_keeps_its_record() {
        let machine = t14("N34ET56W (1.56 )");
        let record = recorded(
            &machine,
            &[("boot.a.complete_ms", 1166), ("boot.a.stick_secs", 0)],
        );
        let judged = judge(
            &machine,
            Some(record.clone()),
            &readings(&[("boot.a.complete_ms", 2333), ("boot.a.stick_secs", 3)]),
        );
        assert_eq!(
            judged.over,
            vec![
                Over {
                    name: "boot.a.complete_ms".to_string(),
                    value: 2333,
                    recorded: 1166
                },
                Over {
                    name: "boot.a.stick_secs".to_string(),
                    value: 3,
                    recorded: 0
                },
            ]
        );
        assert_eq!(
            judged.record, record,
            "a red reading moved the record it is judged by"
        );
    }

    #[test]
    fn an_unknown_machine_is_recorded_and_not_judged() {
        let machine = t14("N34ET56W (1.56 )");
        let judged = judge(
            &machine,
            None,
            &readings(&[("boot.a.complete_ms", u64::MAX)]),
        );
        assert_eq!(judged.over, Vec::new());
        assert!(judged.changed);
        assert_eq!(
            judged.record.measured,
            readings(&[("boot.a.complete_ms", u64::MAX)])
        );
        assert_eq!(judged.record.bios, machine.bios);
    }

    #[test]
    fn another_bios_is_recorded_afresh_and_not_judged() {
        let old = recorded(
            &t14("N34ET50W (1.50 )"),
            &[("boot.a.complete_ms", 1000), ("boot.a.gone_ms", 5)],
        );
        let machine = t14("N34ET56W (1.56 )");
        assert_eq!(old.get(&machine, "boot.a.complete_ms"), None);
        let judged = judge(
            &machine,
            Some(old),
            &readings(&[("boot.a.complete_ms", 9000)]),
        );
        assert_eq!(judged.over, Vec::new());
        assert!(judged.changed);
        assert_eq!(judged.record.bios, machine.bios);
        assert_eq!(
            judged.record.measured,
            readings(&[("boot.a.complete_ms", 9000)])
        );
    }

    #[test]
    fn a_new_name_on_a_known_machine_is_recorded_and_not_failed() {
        let machine = t14("N34ET56W (1.56 )");
        let record = recorded(&machine, &[("boot.a.complete_ms", 1166)]);
        let judged = judge(
            &machine,
            Some(record),
            &readings(&[
                ("boot.a.complete_ms", 1166),
                ("boot.usbload.panel_us", 21012),
            ]),
        );
        assert_eq!(judged.over, Vec::new());
        assert!(judged.changed);
        assert_eq!(
            judged.record.measured,
            readings(&[
                ("boot.a.complete_ms", 1166),
                ("boot.usbload.panel_us", 21012)
            ])
        );
    }

    #[test]
    fn a_record_survives_the_file_it_is_written_to() {
        let root = toyos_tmpdir::TempDir::new("metaltimings");
        let machine = t14("N34ET56W (1.56 )");
        assert_eq!(Record::load(&root, &machine), Ok(None));
        let record = recorded(
            &machine,
            &[("boot.a.complete_ms", 1166), ("tlb.a.p50_ns", 2258)],
        );
        let at = record.save(&root).expect("save");
        assert_eq!(at, root.join("tests/metal/lenovo-20w000t9ge.toml"));
        assert_eq!(Record::load(&root, &machine), Ok(Some(record)));
    }

    #[test]
    fn a_machine_is_three_non_empty_lines() {
        assert_eq!(
            Machine::parse("LENOVO\n20W000T9GE\nN34ET56W (1.56 )\n"),
            Ok(t14("N34ET56W (1.56 )"))
        );
        assert!(Machine::parse("LENOVO\n20W000T9GE\n").is_err());
        assert!(Machine::parse("LENOVO\n\nN34ET56W (1.56 )\n").is_err());
    }
}
