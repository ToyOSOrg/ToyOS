//! Every number the metal suite measures, recorded per machine and judged only
//! against the record of the machine that measured it.
//!
//! One file per machine under [`DIR`], named for its SMBIOS vendor and product,
//! written by a run and committed by whoever ran it. A run on a machine with no
//! record is recorded and not judged; a name the record lacks is added and not
//! judged, and only off a boot with no failure of its own; a recorded value is
//! never moved by a run, so a slow run cannot become the baseline the next is
//! judged by. A run under a BIOS other than the record's is judged against it,
//! fails naming both, and records nothing. Deleting a row is how a number is
//! re-recorded, and deleting the file is how a machine is, firmware and all.
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
        path(&self.vendor, &self.product)
    }
}

fn path(vendor: &str, product: &str) -> PathBuf {
    let mut slug = String::new();
    for c in format!("{vendor} {product}").chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    Path::new(DIR).join(format!("{}.toml", slug.trim_end_matches('-')))
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
        let record = match Self::read(&at) {
            Ok(record) => record,
            Err(Unread::Missing) => return Ok(None),
            Err(Unread::Refused(why)) => return Err(why),
        };
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

    fn read(at: &Path) -> Result<Self, Unread> {
        let text = std::fs::read_to_string(at).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Unread::Missing,
            _ => Unread::Refused(format!("{}: {e}", at.display())),
        })?;
        toml::from_str(&text).map_err(|e| Unread::Refused(format!("{}: {e}", at.display())))
    }

    pub fn save(&self, root: &Path) -> Result<PathBuf, String> {
        let at = root.join(path(&self.vendor, &self.product));
        let text = toml::to_string(self).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(root.join(DIR)).map_err(|e| format!("{DIR}: {e}"))?;
        std::fs::write(&at, text).map_err(|e| format!("{}: {e}", at.display()))?;
        Ok(at)
    }
}

enum Unread {
    Missing,
    Refused(String),
}

/// One number a run measured, and whether the boot that measured it passed:
/// every reading is judged, and only a passing boot's becomes a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub value: u64,
    pub passed: bool,
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

/// A run under a BIOS other than the one its machine's record was taken under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firmware {
    pub recorded: String,
    pub ran: String,
}

impl std::fmt::Display for Firmware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "this machine's record was taken under BIOS {:?} and this run booted under {:?}, so \
             it records nothing; deleting the record is how this firmware is recorded afresh",
            self.recorded, self.ran
        )
    }
}

/// What one run's readings come to on `machine`.
#[derive(Debug)]
pub struct Judged {
    pub over: Vec<Over>,
    pub firmware: Option<Firmware>,
    /// Recorded names this run measured nothing for.
    pub unmeasured: Vec<String>,
    /// The record after this run, and whether this run changed it.
    pub record: Record,
    pub changed: bool,
}

pub fn judge(
    machine: &Machine,
    record: Option<Record>,
    measured: &BTreeMap<String, Reading>,
) -> Judged {
    let mut record = record.unwrap_or_else(|| Record {
        vendor: machine.vendor.clone(),
        product: machine.product.clone(),
        bios: machine.bios.clone(),
        measured: BTreeMap::new(),
    });
    let firmware = (record.bios != machine.bios)
        .then(|| Firmware { recorded: record.bios.clone(), ran: machine.bios.clone() });
    let unmeasured =
        record.measured.keys().filter(|name| !measured.contains_key(*name)).cloned().collect();
    let mut over = Vec::new();
    let mut changed = false;
    for (name, reading) in measured {
        match record.measured.get(name) {
            Some(&recorded) if reading.value > ceiling(recorded) => {
                over.push(Over {
                    name: name.clone(),
                    value: reading.value,
                    recorded,
                });
            }
            Some(_) => {}
            None if reading.passed && firmware.is_none() => {
                record.measured.insert(name.clone(), reading.value);
                changed = true;
            }
            None => {}
        }
    }
    Judged {
        over,
        firmware,
        unmeasured,
        record,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIOS: &str = "N34ET71W (1.71 )";

    fn t14(bios: &str) -> Machine {
        Machine {
            vendor: "LENOVO".to_string(),
            product: "20W0003AMZ".to_string(),
            bios: bios.to_string(),
        }
    }

    fn readings(pairs: &[(&str, u64)]) -> BTreeMap<String, Reading> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), Reading { value: *value, passed: true }))
            .collect()
    }

    fn values(pairs: &[(&str, u64)]) -> BTreeMap<String, u64> {
        pairs.iter().map(|(name, value)| ((*name).to_string(), *value)).collect()
    }

    fn recorded(machine: &Machine, pairs: &[(&str, u64)]) -> Record {
        judge(machine, None, &readings(pairs)).record
    }

    #[test]
    fn a_known_machine_within_its_record_passes() {
        let machine = t14(BIOS);
        let record = recorded(
            &machine,
            &[("boot.a.complete_ms", 1166), ("boot.a.panel_max_us", 0)],
        );
        let judged = judge(
            &machine,
            Some(record.clone()),
            &readings(&[("boot.a.complete_ms", 2332), ("boot.a.panel_max_us", 2)]),
        );
        assert_eq!(judged.over, Vec::new());
        assert_eq!(judged.firmware, None);
        assert!(!judged.changed);
        assert_eq!(judged.record, record);
    }

    #[test]
    fn a_known_machine_outside_its_record_fails_and_keeps_its_record() {
        let machine = t14(BIOS);
        let record = recorded(
            &machine,
            &[("boot.a.complete_ms", 1166), ("boot.a.panel_max_us", 0)],
        );
        let judged = judge(
            &machine,
            Some(record.clone()),
            &readings(&[("boot.a.complete_ms", 2333), ("boot.a.panel_max_us", 3)]),
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
                    name: "boot.a.panel_max_us".to_string(),
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
        let machine = t14(BIOS);
        let judged = judge(
            &machine,
            None,
            &readings(&[("boot.a.complete_ms", u64::MAX)]),
        );
        assert_eq!(judged.over, Vec::new());
        assert!(judged.changed);
        assert_eq!(
            judged.record.measured,
            values(&[("boot.a.complete_ms", u64::MAX)])
        );
        assert_eq!(judged.record.bios, machine.bios);
    }

    /// **A firmware update is no re-baseline.** The run is still judged
    /// against what the machine recorded, fails naming both strings, and adds
    /// nothing a later run under either BIOS would be judged by.
    #[test]
    fn another_bios_fails_naming_both_and_records_nothing() {
        let old = recorded(
            &t14("N34ET50W (1.50 )"),
            &[("boot.a.complete_ms", 1000), ("boot.a.gone_ms", 5)],
        );
        let judged = judge(
            &t14(BIOS),
            Some(old.clone()),
            &readings(&[("boot.a.complete_ms", 9000), ("boot.a.new_ms", 1)]),
        );
        assert_eq!(
            judged.firmware,
            Some(Firmware { recorded: "N34ET50W (1.50 )".to_string(), ran: BIOS.to_string() })
        );
        let said = judged.firmware.as_ref().expect("a firmware finding").to_string();
        assert!(said.contains("N34ET50W (1.50 )") && said.contains(BIOS), "{said}");
        assert_eq!(
            judged.over,
            vec![Over { name: "boot.a.complete_ms".to_string(), value: 9000, recorded: 1000 }]
        );
        assert!(!judged.changed);
        assert_eq!(judged.record, old);
    }

    #[test]
    fn a_new_name_on_a_known_machine_is_recorded_and_not_failed() {
        let machine = t14(BIOS);
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
            values(&[
                ("boot.a.complete_ms", 1166),
                ("boot.usbload.panel_us", 21012)
            ])
        );
    }

    /// **A failed boot's numbers are judged and never become a baseline.**
    #[test]
    fn a_failing_boots_reading_is_judged_and_not_recorded() {
        let machine = t14(BIOS);
        let record = recorded(&machine, &[("boot.a.complete_ms", 1000)]);
        let failed = |value| Reading { value, passed: false };
        let measured: BTreeMap<String, Reading> = [
            ("boot.a.complete_ms".to_string(), failed(2001)),
            ("boot.b.complete_ms".to_string(), failed(7)),
            ("boot.c.complete_ms".to_string(), Reading { value: 9, passed: true }),
        ]
        .into_iter()
        .collect();
        let judged = judge(&machine, Some(record), &measured);
        assert_eq!(
            judged.over,
            vec![Over { name: "boot.a.complete_ms".to_string(), value: 2001, recorded: 1000 }]
        );
        assert_eq!(
            judged.record.measured,
            values(&[("boot.a.complete_ms", 1000), ("boot.c.complete_ms", 9)])
        );
    }

    #[test]
    fn a_recorded_name_nothing_measured_is_named() {
        let machine = t14(BIOS);
        let record = recorded(&machine, &[("boot.a.complete_ms", 1), ("boot.b.complete_ms", 1)]);
        let judged = judge(&machine, Some(record), &readings(&[("boot.a.complete_ms", 1)]));
        assert_eq!(judged.unmeasured, vec!["boot.b.complete_ms".to_string()]);
    }

    #[test]
    fn a_record_survives_the_file_it_is_written_to() {
        let root = toyos_tmpdir::TempDir::new("metaltimings");
        let machine = t14(BIOS);
        assert_eq!(Record::load(&root, &machine), Ok(None));
        let record = recorded(
            &machine,
            &[("boot.a.complete_ms", 1166), ("tlb.a.p50_ns", 2258)],
        );
        let at = record.save(&root).expect("save");
        assert_eq!(at, root.join("tests/metal/lenovo-20w0003amz.toml"));
        assert_eq!(Record::load(&root, &machine), Ok(Some(record)));
    }

    /// The file name folds case and punctuation, so two spellings of one
    /// vendor land on one file, and the record inside says which it holds.
    #[test]
    fn two_machines_sharing_a_file_name_are_refused() {
        let root = toyos_tmpdir::TempDir::new("metaltimings");
        let lenovo = Machine { vendor: "Lenovo".to_string(), ..t14(BIOS) };
        recorded(&lenovo, &[("boot.a.complete_ms", 1)]).save(&root).expect("save");
        let why = Record::load(&root, &t14(BIOS)).expect_err("read as another machine");
        assert!(why.contains("two machines share one file name"), "{why}");
    }

    #[test]
    fn a_name_recorded_twice_is_refused() {
        let root = toyos_tmpdir::TempDir::new("metaltimings");
        let machine = t14(BIOS);
        let at = recorded(&machine, &[("boot.a.complete_ms", 1)]).save(&root).expect("save");
        let text = std::fs::read_to_string(&at).expect("the saved record");
        std::fs::write(&at, format!("{text}\n\"boot.a.complete_ms\" = 2\n")).expect("write");
        let why = Record::load(&root, &machine).expect_err("a second value under one name");
        assert!(why.contains("duplicate"), "{why}");
    }

    /// **Every committed record, as the next run will read it.** A hand edit
    /// that breaks a file, or a file not at the path its own machine derives,
    /// is red here rather than at the end of a run on the machine.
    #[test]
    fn every_committed_record_loads_from_its_own_path() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut read = 0;
        for entry in std::fs::read_dir(root.join(DIR)).expect(DIR) {
            let at = entry.expect(DIR).path();
            let named = at.strip_prefix(root).expect("under the root").to_path_buf();
            let record = match Record::read(&at) {
                Ok(record) => record,
                Err(Unread::Missing) => panic!("{} went while this read it", named.display()),
                Err(Unread::Refused(why)) => panic!("{why}"),
            };
            let machine = Machine {
                vendor: record.vendor.clone(),
                product: record.product.clone(),
                bios: record.bios.clone(),
            };
            assert_eq!(machine.path(), named, "a record is not at its machine's path");
            assert_eq!(Record::load(root, &machine), Ok(Some(record)));
            read += 1;
        }
        assert!(read > 0, "{DIR} holds no record, so this read nothing");
    }

    #[test]
    fn a_machine_is_three_non_empty_lines() {
        assert_eq!(
            Machine::parse("LENOVO\n20W0003AMZ\nN34ET71W (1.71 )\n"),
            Ok(t14(BIOS))
        );
        assert!(Machine::parse("LENOVO\n20W0003AMZ\n").is_err());
        assert!(Machine::parse("LENOVO\n\nN34ET71W (1.71 )\n").is_err());
    }
}
