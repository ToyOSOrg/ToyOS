//! The device suite's profile: what the T14's own devices must say about
//! themselves on every boot, and what every number that boot measured may be.
//!
//! **Text in, verdicts out.** Everything here reads the two strings
//! `src/metal.rs` brings back off the stick — the loader's file and `logd`'s —
//! and nothing here touches a machine. `tests/metaldevicecase` is the boot that
//! produces them: `userland/metalprobe`'s jobs measure, exit with their numbers,
//! and the kernel's own `exit:` record carries each one off the machine.
//!
//! A number that has never been taken on the machine is [`Bound::Unmeasured`]
//! and is reported rather than judged; the first boot's number becomes its
//! bound, with [`MARGIN_PERCENT`] of room, and a boot that then falls outside
//! it is a red. That is the whole ceiling discipline: nothing here is a
//! datasheet figure and nothing here was widened to pass.

#![forbid(unsafe_code)]

use std::fmt;

/// How far a measured rate may fall below the number that first stood for it
/// before the profile calls it a regression.
///
/// **Wide, because one boot is one sample.** These are single measurements on
/// one machine with no distribution behind them; a bound tight enough to catch
/// a 10% drift would red on the first boot that scheduled its jobs differently.
/// It narrows when a rate has been measured often enough to have a spread.
pub const MARGIN_PERCENT: i64 = 40;

/// The floor a first measurement stands for.
pub fn floor_from(first: i64) -> i64 {
    first * (100 - MARGIN_PERCENT) / 100
}

/// What a job's number is held to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// Never taken on the machine. Reported with the number it measured and
    /// judged on nothing but the job having produced one at all.
    Unmeasured,
    /// A rate, which may not fall below this.
    AtLeast(i64),
    /// A latency, a duration or a disagreement, which may not rise above this.
    AtMost(i64),
    /// A value the machine has no freedom about, given its mode.
    Exactly(i64),
}

impl Bound {
    /// One number against this bound. **The whole ceiling discipline lives
    /// here**, so the device profile and the kernel profile cannot come to
    /// different answers about what a bound means.
    pub fn check(self, value: i64, unit: &'static str) -> Outcome {
        match self {
            Self::Unmeasured => Outcome::Measured { value, unit },
            Self::AtLeast(floor) if value < floor => {
                Outcome::Failed(format!("{value} {unit} is under this profile's floor of {floor}"))
            }
            Self::AtLeast(floor) => Outcome::Held(format!("{value} {unit} (floor {floor})")),
            Self::AtMost(ceiling) if value > ceiling => {
                Outcome::Failed(format!("{value} {unit} is over this profile's ceiling of {ceiling}"))
            }
            Self::AtMost(ceiling) => Outcome::Held(format!("{value} {unit} (ceiling {ceiling})")),
            Self::Exactly(want) if value != want => {
                Outcome::Failed(format!("{value} {unit} where this profile holds {want}"))
            }
            Self::Exactly(want) => Outcome::Held(format!("{want} {unit}")),
        }
    }
}

/// Why a `metalprobe` job has no number, mirroring `Refusal` in
/// `userland/metalprobe/src/main.rs`. Held to that file by
/// [`tests::the_probe_declares_the_refusals_this_names`], because nothing links
/// the two crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    NoCapability = -1,
    NoDevice = -2,
    NoScanout = -3,
    NoVolume = -4,
    Disagreed = -5,
    NoDuration = -6,
}

impl Refused {
    const ALL: &'static [Self] = &[
        Self::NoCapability,
        Self::NoDevice,
        Self::NoScanout,
        Self::NoVolume,
        Self::Disagreed,
        Self::NoDuration,
    ];

    /// The name the probe spells this refusal with, which is what a reader is
    /// told instead of a negative number.
    fn spelling(self) -> &'static str {
        match self {
            Self::NoCapability => "NoCapability",
            Self::NoDevice => "NoDevice",
            Self::NoScanout => "NoScanout",
            Self::NoVolume => "NoVolume",
            Self::Disagreed => "Disagreed",
            Self::NoDuration => "NoDuration",
        }
    }

    fn of(code: i64) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| i64::from(*r as i32) == code)
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let why = match self {
            Self::NoCapability => "the job was endowed no device-minting capability",
            Self::NoDevice => "the kernel refused the device claim",
            Self::NoScanout => "the claim described a display this job cannot measure",
            Self::NoVolume => "a filesystem call the measurement rests on was refused",
            Self::Disagreed => "what was read back is not what was written",
            Self::NoDuration => "the measurement took no measurable time",
        };
        write!(f, "{} ({why})", self.spelling())
    }
}

/// One measurement the boot is asked for, by the name its symlink — and so the
/// kernel's `exit:` record — spells it.
#[derive(Debug, Clone, Copy)]
pub struct Job {
    pub name: &'static str,
    /// The unit its exit code is in, for the reader; nothing computes with it.
    pub unit: &'static str,
    pub bound: Bound,
}

/// Every job `tests/metaldevicecase`'s runner list names, in that order.
///
/// **The order is the config's**, because the framebuffer jobs repaint the
/// panel and the storage jobs share one 34 MiB volume with `logd`.
pub const JOBS: &[Job] = &[
    Job { name: "usbwrite", unit: "KiB/s", bound: Bound::Unmeasured },
    Job { name: "usbread", unit: "KiB/s", bound: Bound::Unmeasured },
    // Not a rate: the fold of what the scanout read back, which is a function
    // of the mode alone. It moves when the display's mode moves and at no
    // other time, so it is `Exactly` once taken.
    Job { name: "fbhash", unit: "fold", bound: Bound::Unmeasured },
    Job { name: "fbfill", unit: "KiB/s", bound: Bound::Unmeasured },
    Job { name: "fbread", unit: "KiB/s", bound: Bound::Unmeasured },
];

/// Whether a record must be there or must not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Says,
    NeverSays,
}

/// One thing a metal boot's log must or must not carry, whatever the numbers
/// are.
#[derive(Debug, Clone, Copy)]
pub struct Record {
    pub about: &'static str,
    pub needle: &'static str,
    pub presence: Presence,
}

use Presence::{NeverSays, Says};

/// The device inventory, asserted on every boot.
///
/// **Every needle is a prefix of a record the shipping kernel writes**, so
/// nothing here needs an actuator and nothing here is about a number. The
/// counts and identities that *are* about this machine — how many controllers,
/// which silicon, which mode — are read out and reported by [`inventory`], and
/// become bounds once the machine has answered once.
pub const RECORDS: &[Record] = &[
    // The framebuffer's memory type, which is what makes `fbfill` and `fbread`
    // two different numbers rather than one.
    Record { about: "PAT", needle: "PAT: IA32_PAT=", presence: Says },
    Record { about: "scanout", needle: "GOP: scanout memory type WC ", presence: Says },
    // The stick this boot came off, through the controller that carries it.
    Record { about: "xhci", needle: "xHCI: found at PCI ", presence: Says },
    Record { about: "usb-storage", needle: "usb-storage: ", presence: Says },
    Record { about: "xhci-refused", needle: "xHCI: NOT INITIALISED", presence: NeverSays },
    // The keyboard controller, with no key pressed all boot.
    Record { about: "i8042", needle: "i8042: ok selftest=0x55", presence: Says },
    Record { about: "i8042-quarantine", needle: "i8042: quarantined", presence: NeverSays },
    Record { about: "i8042-lost-edge", needle: "i8042: bytes with no IRQ record", presence: NeverSays },
    // The internal disk: identified, and — the whole safety argument for
    // running on this machine at all — never written.
    Record { about: "nvme", needle: "NVMe: NS1 size=", presence: Says },
    Record { about: "nvme-refused", needle: "NVMe: NOT INITIALISED", presence: NeverSays },
    Record { about: "nvme-offline", needle: "NVMe: this controller is offline", presence: NeverSays },
    Record { about: "nvme-census", needle: "nvme: commands ", presence: Says },
    // The boot ended the way the loop's verdict needs it to.
    Record { about: "boot", needle: "Boot: complete (", presence: Says },
];

/// The one census field a boot may not have moved: a write to a disk this
/// project never writes.
pub const NVME_CENSUS: &str = "nvme: commands ";
pub const NVME_NO_WRITES: &[&str] = &["write=0", "io-other=0"];

/// The loader lines that say the black-box chain closed — the boot before this
/// one was accounted for and this pass reset rather than booting again.
pub const CHAIN_LINES: &[&str] = &[crate::bootlog::BLACKBOX_HEAD, crate::bootlog::PREVIOUS_PANIC];

/// What one job's `exit:` record said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    pub code: i64,
    pub cpu_ms: u64,
}

/// The `exit: <name> pid=N code=N cpu=Nms` record for `name`, or `None` where
/// the boot has none — which is a job that never ran, never returned, or was
/// still running when the machine reset.
///
/// The **last** such record, because a name could in principle run twice and
/// the boot's answer is the one it ended with.
pub fn exit_of(log: &str, name: &str) -> Option<Exit> {
    let head = format!("exit: {name} pid=");
    log.lines().rev().find_map(|line| {
        let rest = line.split(&head).nth(1)?;
        let code = field(rest, "code=")?.parse().ok()?;
        let cpu = field(rest, "cpu=")?;
        let cpu_ms = cpu.strip_suffix("ms")?.parse().ok()?;
        Some(Exit { code, cpu_ms })
    })
}

/// The word after `key` in `rest`, up to the next space.
fn field<'a>(rest: &'a str, key: &str) -> Option<&'a str> {
    rest.split(key).nth(1)?.split_whitespace().next()
}

/// One line of a boot's own report about itself, in the order a reader wants
/// them: what the devices said, then what each job measured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub about: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The record was where it had to be, or absent where it had to be.
    Held(String),
    /// A number with no bound yet: this is what the machine answered.
    Measured { value: i64, unit: &'static str },
    Failed(String),
}

impl Outcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.outcome {
            Outcome::Held(what) => write!(f, "  ok    {:<16} {what}", self.about),
            Outcome::Measured { value, unit } => {
                write!(f, "  first {:<16} {value} {unit} — unmeasured, this is its number", self.about)
            }
            Outcome::Failed(why) => write!(f, "  RED   {:<16} {why}", self.about),
        }
    }
}

/// Every value a boot reported that a profile entry would hold, whether or not
/// this profile holds one yet: the lines a reader needs to write the next
/// version of this file.
pub fn inventory(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    for needle in [
        "xHCI: found at PCI ",
        "xHCI: max_slots=",
        "xHCI: USB ",
        "xHCI: ",
        "usb-storage: ",
        "i8042: ",
        "hda: ",
        "NVMe: ",
        "nvme: commands ",
        "GOP: ",
        "PAT: ",
    ] {
        for line in log.lines().filter(|l| l.contains(needle)) {
            let line = line.to_string();
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
    out
}

/// Judge one boot's log against this profile.
pub fn judge(log: &str) -> Vec<Verdict> {
    let mut out = Vec::new();
    for record in RECORDS {
        let saw = log.contains(record.needle);
        let outcome = match (record.presence, saw) {
            (Says, true) => Outcome::Held(quoted(log, record.needle)),
            (Says, false) => Outcome::Failed(format!("no record carries {:?}", record.needle)),
            (NeverSays, false) => Outcome::Held(format!("nothing said {:?}", record.needle)),
            (NeverSays, true) => {
                Outcome::Failed(format!("a record carries {:?}: {}", record.needle, quoted(log, record.needle)))
            }
        };
        out.push(Verdict { about: record.about.to_string(), outcome });
    }

    // The census is the one record whose *content* is the assertion.
    if let Some(census) = log.lines().find(|l| l.contains(NVME_CENSUS)) {
        for want in NVME_NO_WRITES {
            let outcome = if census.contains(want) {
                Outcome::Held((*want).to_string())
            } else {
                Outcome::Failed(format!("the census says {census:?}, and this boot owed {want}"))
            };
            out.push(Verdict { about: format!("nvme-{want}"), outcome });
        }
    }

    for job in JOBS {
        out.push(Verdict { about: job.name.to_string(), outcome: judged(log, job) });
    }
    out
}

fn judged(log: &str, job: &Job) -> Outcome {
    let Some(exit) = exit_of(log, job.name) else {
        return Outcome::Failed(format!(
            "the boot carries no `exit: {} pid=…` record, so the job never ended",
            job.name
        ));
    };
    if let Some(refused) = Refused::of(exit.code) {
        return Outcome::Failed(format!("the job refused: {refused}"));
    }
    if exit.code < 0 {
        return Outcome::Failed(format!("the job exited {}, which names no refusal", exit.code));
    }
    // The bound's own arithmetic, plus the one thing only a job's record has:
    // what the measurement cost the CPU it ran on.
    match job.bound.check(exit.code, job.unit) {
        Outcome::Held(what) => Outcome::Held(format!("{what}, cpu {}ms", exit.cpu_ms)),
        other => other,
    }
}

/// The first line carrying `needle`, trimmed, for a verdict to quote.
fn quoted(log: &str, needle: &str) -> String {
    log.lines()
        .find(|line| line.contains(needle))
        .map(|line| line.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boot log's shape, as `logd` writes one: the wall-clock tag, the
    /// boot-relative stamp, the CPU, then the record.
    fn line(at: &str, text: &str) -> String {
        format!("[2026-09-07 06:45:03 {at} cpu0] {text}\n")
    }

    fn a_good_boot() -> String {
        [
            line("0.000", "PAT: IA32_PAT=0x0007040100070406, entry 4 = WC"),
            line("0.087", "xHCI: found at PCI 00:14.0 8086:a36d"),
            line("0.090", "xHCI: max_slots=32 max_ports=16 ctx_size=64 pagesize=0x1"),
            line("0.140", "usb-storage: 1 device(s)"),
            line("0.150", "i8042: ok selftest=0x55 cfg=0x45->0x44 port1=ok port2=ok"),
            line("0.200", "NVMe: NS1 size=1000215216 sectors, sector_size=512"),
            line("0.319", "GOP: scanout memory type WC (MTRR WB, PAT entry 4)"),
            line("0.368", "Boot: complete (368ms)"),
            line("1.100", "exit: usbwrite pid=6 code=11200 cpu=140ms"),
            line("2.100", "exit: usbread pid=7 code=30500 cpu=90ms"),
            line("2.500", "exit: fbhash pid=8 code=209270153 cpu=44ms"),
            line("2.600", "exit: fbfill pid=9 code=3440663 cpu=70ms"),
            line("3.900", "exit: fbread pid=10 code=41000 cpu=33ms"),
            line("3.950", "nvme: commands identify=2 admin-other=2 read=1 write=0 io-other=0"),
            line("3.960", "Rebooting."),
        ]
        .concat()
    }

    #[test]
    fn an_exit_record_is_read_as_its_number() {
        let log = a_good_boot();
        assert_eq!(exit_of(&log, "usbwrite"), Some(Exit { code: 11200, cpu_ms: 140 }));
        assert_eq!(exit_of(&log, "fbhash"), Some(Exit { code: 209_270_153, cpu_ms: 44 }));
        assert_eq!(exit_of(&log, "never_ran"), None);
        // A name that is a prefix of another's is not that other one.
        assert_eq!(exit_of(&log, "usb"), None);
        // The last of two, because that is the answer the boot ended with.
        let twice = format!("{log}{}", line("4.0", "exit: usbread pid=11 code=99 cpu=1ms"));
        assert_eq!(exit_of(&twice, "usbread"), Some(Exit { code: 99, cpu_ms: 1 }));
    }

    #[test]
    fn a_good_boot_reds_on_nothing_and_reports_every_number() {
        let verdicts = judge(&a_good_boot());
        let red: Vec<&Verdict> = verdicts.iter().filter(|v| v.outcome.is_failure()).collect();
        assert!(red.is_empty(), "{red:#?}");
        let measured: Vec<&Verdict> = verdicts
            .iter()
            .filter(|v| matches!(v.outcome, Outcome::Measured { .. }))
            .collect();
        assert_eq!(measured.len(), JOBS.len(), "{measured:#?}");
    }

    /// The negative control for every predicate: a boot that fails each one
    /// exactly, so nothing above is a spelling of `true`.
    #[test]
    fn each_predicate_has_teeth() {
        let failures = |log: &str| -> Vec<String> {
            judge(log)
                .into_iter()
                .filter(|v| v.outcome.is_failure())
                .map(|v| v.about)
                .collect()
        };
        assert!(failures(&a_good_boot()).is_empty());

        // A record that must be there, taken away.
        let no_pat = a_good_boot().replace("PAT: IA32_PAT=", "PAT: nothing at all ");
        assert_eq!(failures(&no_pat), ["PAT"]);

        // A record that must not be there, put in.
        let quarantined = format!("{}{}", a_good_boot(), line("1.0", "i8042: quarantined — ..."));
        assert_eq!(failures(&quarantined), ["i8042-quarantine"]);

        // The scanout mapped as something other than write-combining.
        let uncached =
            a_good_boot().replace("scanout memory type WC ", "scanout memory type UC ");
        assert_eq!(failures(&uncached), ["scanout"]);

        // One write to the disk this project never writes.
        let wrote = a_good_boot().replace("write=0 io-other=0", "write=1 io-other=0");
        assert_eq!(failures(&wrote), ["nvme-write=0"]);

        // A job that refused rather than measured, named rather than numbered.
        let refused = a_good_boot().replace("exit: fbhash pid=8 code=209270153", "exit: fbhash pid=8 code=-5");
        assert_eq!(failures(&refused), ["fbhash"]);
        let said = judge(&refused).into_iter().find(|v| v.about == "fbhash").unwrap();
        assert!(format!("{said}").contains("Disagreed"), "{said}");

        // A job that never ended: the record it owes is simply not there.
        let gone = a_good_boot().replace("exit: usbread pid=7 code=30500 cpu=90ms", "");
        assert_eq!(failures(&gone), ["usbread"]);
    }

    #[test]
    fn a_bound_is_judged_once_it_is_taken() {
        let log = a_good_boot();
        let floor = Job { name: "usbread", unit: "KiB/s", bound: Bound::AtLeast(20_000) };
        assert!(matches!(judged(&log, &floor), Outcome::Held(_)));
        let steep = Job { bound: Bound::AtLeast(40_000), ..floor };
        assert!(judged(&log, &steep).is_failure());

        let exact = Job { name: "fbhash", unit: "fold", bound: Bound::Exactly(209_270_153) };
        assert!(matches!(judged(&log, &exact), Outcome::Held(_)));
        let moved = Job { bound: Bound::Exactly(1), ..exact };
        assert!(judged(&log, &moved).is_failure());

        assert_eq!(floor_from(10_000), 6_000);
    }

    /// Nothing links this crate to `userland/metalprobe`: it is built for
    /// another target and never for the host. The refusal codes are one
    /// contract in two files, so they are held to each other by reading the
    /// other file, the way `bootlog` holds the loader's lines.
    #[test]
    fn the_probe_declares_the_refusals_this_names() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("userland/metalprobe/src/main.rs");
        let source = std::fs::read_to_string(&path).expect("the probe's own source");
        for refusal in Refused::ALL {
            let declared = format!("{} = {},", refusal.spelling(), *refusal as i32);
            assert!(
                source.contains(&declared),
                "{} declares no `{declared}`",
                path.display()
            );
        }
        // And the other way: a refusal the probe grew and this file has not.
        let arms = source
            .lines()
            .filter_map(|line| line.trim().strip_suffix(','))
            .filter(|line| line.contains(" = -"))
            .count();
        assert_eq!(arms, Refused::ALL.len(), "{} declares {arms} refusals", path.display());
    }

    /// Every job this profile holds is a job the boot config actually runs,
    /// under the name the kernel's record will spell.
    #[test]
    fn the_boot_config_runs_exactly_these_jobs() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/metaldevicecase/system.toml");
        let config = std::fs::read_to_string(&path).expect("the device case's config");
        let args = config
            .lines()
            .find_map(|line| line.trim().strip_prefix("args = ["))
            .expect("the runner's job list");
        let named: Vec<&str> = args
            .trim_end_matches([']'])
            .split(',')
            .map(|word| word.trim().trim_matches('"'))
            .filter(|word| !word.is_empty())
            .collect();
        // The list ends with the job that hands the machine back, which
        // measures nothing.
        assert_eq!(named.last(), Some(&"reboot"), "{named:?}");
        let measured: Vec<&str> = named[..named.len() - 1].to_vec();
        let held: Vec<&str> = JOBS.iter().map(|j| j.name).collect();
        assert_eq!(measured, held);
        for name in &held {
            assert!(
                config.contains(&format!("\"bin/{name}\" = \"/system/bin/metalprobe\"")),
                "{name} has no symlink, so the runner would spawn nothing"
            );
        }
    }
}
