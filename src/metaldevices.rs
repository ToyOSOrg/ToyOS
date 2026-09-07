//! What a metal boot says about itself, as both profiles read it: the exit
//! codes `userland/metalprobe` refuses with, the device inventory every boot
//! owes, the account the shutdown seals into the black box, and the one
//! arithmetic a bound is judged by.
//!
//! **Text in, verdicts out.** Everything here reads the two strings
//! `src/metal.rs` brings back off the stick — the loader's file and `logd`'s —
//! and nothing here touches a machine. `tests/metaldevicecase` is the boot that
//! produces them: `userland/metalprobe`'s jobs measure, exit with their spans,
//! and the kernel's own `exit:` record carries each one off the machine.
//!
//! **What a span may be is not here.** Every device number is priced by name in
//! `tests/metal-profile.toml`, which is one file for the whole suite; what this
//! holds is the *shape* both ends have to agree about, so a host and a guest
//! cannot come to different answers about a word. [`Bound`] is the exception
//! and is here for the same reason: `crate::metalkernel` judges against it too,
//! and one arithmetic is what stops two profiles meaning different things by a
//! ceiling.

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

    /// What an exit code names, or `None` where it is a measurement.
    pub fn of(code: i64) -> Option<Self> {
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
    // Every disk's write cache, emptied before anything was taken down. This
    // one *is* a log record: it is made above the boot's last word, while the
    // volume that carries it is still there.
    Record { about: "usb-flush", needle: "usb-quiesce: disk ", presence: Says },
];

/// The one census field a boot may not have moved: a write to a disk this
/// project never writes.
pub const NVME_CENSUS: &str = "nvme: commands ";
pub const NVME_NO_WRITES: &[&str] = &["write=0", "io-other=0"];

/// What `loader.log` must carry, which is a different file and a different
/// reader.
///
/// **The end of the shutdown is here and nowhere else.** Everything the kernel
/// does after `log::wait_for_durable` is done to the volume that would have
/// carried the record, so it reaches no file: it is sealed into the black box
/// and printed by the next loader pass under that pass's `|` prefix.
pub const LOADER_RECORDS: &[Record] = &[
    Record { about: "chain", needle: "the last boot read DONE", presence: Says },
    Record { about: "usb-quiesce", needle: QUIESCE_HEAD, presence: Says },
    // A kernel that died on the way to the reset seals ARMED, not DONE.
    Record { about: "no-death", needle: "died without reaching", presence: NeverSays },
];

/// The head of the reset's own account, as `kernel/src/drivers/xhci/stop.rs`
/// spells it.
pub const QUIESCE_HEAD: &str = "usb-quiesce:";

/// What the shutdown did, out of its summary line.
///
/// **Read as pairs and never as totals**, because "one controller halted" and
/// "one of two controllers halted" are the difference the whole path exists
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiesced {
    pub flushed: u32,
    pub disks: u32,
    /// Connected ports whose reset finished, of the connected ports there were.
    /// **The act an attached device sees**, and the one that ends a transfer.
    pub reset_ports: u32,
    pub connected: u32,
    pub halted: u32,
    pub controllers: u32,
    pub reset: u32,
    pub unpowered: u32,
    pub ports: u32,
}

impl Quiesced {
    /// Whether every device the boot had was handed back: every disk's cache
    /// emptied, every connected port reset, every controller halted and reset.
    ///
    /// Port *power* is reported and not judged: `PORTSC.PP` is writable only on
    /// a controller with Port Power Control, so a count under the total is a
    /// fact about the silicon and not about the shutdown. The port *reset* is
    /// judged, because it is the one act an attached device sees and no
    /// controller may decline it.
    pub fn complete(&self) -> bool {
        self.flushed == self.disks
            && self.reset_ports == self.connected
            && self.halted == self.controllers
            && self.reset == self.controllers
            && self.controllers > 0
    }
}

/// The shutdown's summary out of `loader.log`, or `None` where the pass carries
/// none.
pub fn quiesced(loader: &str) -> Option<Quiesced> {
    let line = loader.lines().find(|l| l.contains(" disk cache(s) flushed"))?;
    let pair = |what: &str| -> Option<(u32, u32)> {
        let (a, b) = line.split(what).next()?.split_whitespace().next_back()?.split_once('/')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let (flushed, disks) = pair(" disk cache(s) flushed")?;
    let (reset_ports, connected) = pair(" connected port(s) reset")?;
    let (halted, controllers) = pair(" controller(s) halted")?;
    let (unpowered, ports) = pair(" port(s) unpowered")?;
    let reset = line.split(" controller(s) halted, ").nth(1)?.split_whitespace().next()?;
    Some(Quiesced {
        flushed,
        disks,
        reset_ports,
        connected,
        halted,
        controllers,
        reset: reset.parse().ok()?,
        unpowered,
        ports,
    })
}

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

/// Every record that had to be there and was not, and every one that had to be
/// absent and was not — the loader's file and `logd`'s judged by their own
/// tables, plus the two whose *content* is the assertion. Each line opens with
/// the `about` it failed, so a caller can name them without reading the prose.
///
/// A `Vec` of failures and not a `Vec<Verdict>`: the device suite's numbers are
/// priced in `tests/metal-profile.toml` and judged there, so what is left here
/// is the half that is about records.
pub fn unmet(loader: &str, log: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (record, text) in
        RECORDS.iter().map(|r| (r, log)).chain(LOADER_RECORDS.iter().map(|r| (r, loader)))
    {
        match (record.presence, text.contains(record.needle)) {
            (Says, false) => {
                out.push(format!("{}: no record carries {:?}", record.about, record.needle))
            }
            (NeverSays, true) => out.push(format!(
                "{}: a record carries {:?}: {}",
                record.about,
                record.needle,
                quoted(text, record.needle)
            )),
            _ => {}
        }
    }
    if let Some(census) = log.lines().find(|l| l.contains(NVME_CENSUS)) {
        for want in NVME_NO_WRITES {
            if !census.contains(want) {
                out.push(format!("nvme-census: {census:?}, and this boot owed {want}"));
            }
        }
    }
    match quiesced(loader) {
        Some(said) if !said.complete() => out.push(format!(
            "usb-quiesce: the shutdown left a device behind: {said:?}. A reset with a transfer \
             in flight is what a mass-storage device does not survive"
        )),
        Some(_) => {}
        None => out.push(format!("usb-quiesce: no {QUIESCE_HEAD} summary in loader.log")),
    }
    out
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
            line("1.100", "exit: usbwrite pid=6 code=402000 cpu=140ms"),
            line("2.100", "exit: usbread pid=7 code=30500 cpu=90ms"),
            line("3.950", "nvme: commands identify=2 admin-other=2 read=1 write=0 io-other=0"),
            line("3.955", "usb-quiesce: disk 0 SYNCHRONIZE CACHE ok"),
            line("3.960", "Rebooting."),
        ]
        .concat()
    }

    /// `loader.log`'s pass after the reset, with the shutdown's own account
    /// under the `|` the loader prefixes a report's lines with.
    fn a_good_loader() -> String {
        "ToyOS Bootloader 1.0\n\
         Black box: the last boot read DONE, so it handed the machine back on purpose and this \
         chain ends here\n\
         | usb-quiesce: xHCI 00:14.0 halted=true USBSTS=0x00000009\n\
         | usb-quiesce: 2/2 disk cache(s) flushed, 5/5 connected port(s) reset, \
         2/2 controller(s) halted, 2 reset, 12/16 port(s) unpowered\n\
         Loader log: the last boot is accounted for, so this pass resets the machine\n"
            .to_string()
    }

    #[test]
    fn an_exit_record_is_read_as_its_number() {
        let log = a_good_boot();
        assert_eq!(exit_of(&log, "usbwrite"), Some(Exit { code: 402_000, cpu_ms: 140 }));
        assert_eq!(exit_of(&log, "never_ran"), None);
        // A name that is a prefix of another's is not that other one.
        assert_eq!(exit_of(&log, "usb"), None);
        // The last of two, because that is the answer the boot ended with.
        let twice = format!("{log}{}", line("4.0", "exit: usbread pid=11 code=99 cpu=1ms"));
        assert_eq!(exit_of(&twice, "usbread"), Some(Exit { code: 99, cpu_ms: 1 }));
    }

    #[test]
    fn the_shutdowns_own_account_is_read_out_of_the_loaders_file() {
        assert_eq!(
            quiesced(&a_good_loader()),
            Some(Quiesced {
                flushed: 2,
                disks: 2,
                reset_ports: 5,
                connected: 5,
                halted: 2,
                controllers: 2,
                reset: 2,
                unpowered: 12,
                ports: 16,
            })
        );
        assert!(quiesced(&a_good_loader()).unwrap().complete());
        // Ports are reported, not judged: a controller with no Port Power
        // Control ignores the write and 0/16 is the silicon's answer.
        assert!(quiesced(&a_good_loader().replace("12/16 port", "0/16 port")).unwrap().complete());
        // Each of the four that *are* judged, failing on its own.
        for (from, to) in [
            ("2/2 disk", "1/2 disk"),
            ("5/5 connected", "4/5 connected"),
            ("2/2 controller(s) halted", "1/2 controller(s) halted"),
            ("halted, 2 reset", "halted, 1 reset"),
            // A shutdown that found no controller is not one that handed
            // everything back; it is a machine this table is not about.
            ("2/2 controller(s) halted, 2 reset", "0/0 controller(s) halted, 0 reset"),
        ] {
            let moved = a_good_loader().replace(from, to);
            assert!(!quiesced(&moved).unwrap().complete(), "{from} -> {to}");
        }
        assert_eq!(quiesced("ToyOS Bootloader 1.0\n"), None);
    }

    /// The negative control for every predicate: a boot that fails each one
    /// exactly, so nothing in the tables is a spelling of `true`.
    #[test]
    fn each_record_has_teeth() {
        assert!(unmet(&a_good_loader(), &a_good_boot()).is_empty());
        let about = |unmet: Vec<String>| -> Vec<String> {
            unmet.iter().map(|why| why.split(':').next().unwrap_or_default().to_string()).collect()
        };

        // A record that must be there, taken away.
        let no_pat = a_good_boot().replace("PAT: IA32_PAT=", "PAT: nothing at all ");
        assert_eq!(about(unmet(&a_good_loader(), &no_pat)), ["PAT"]);
        // A record that must not be there, put in.
        let quarantined = format!("{}{}", a_good_boot(), line("1.0", "i8042: quarantined - x"));
        assert_eq!(about(unmet(&a_good_loader(), &quarantined)), ["i8042-quarantine"]);
        // The scanout mapped as something other than write-combining.
        let uncached = a_good_boot().replace("memory type WC ", "memory type UC ");
        assert_eq!(about(unmet(&a_good_loader(), &uncached)), ["scanout"]);
        // One write to the disk this project never writes.
        let wrote = a_good_boot().replace("write=0 io-other=0", "write=1 io-other=0");
        assert_eq!(about(unmet(&a_good_loader(), &wrote)), ["nvme-census"]);
        // A shutdown that never emptied a cache.
        let unflushed = a_good_boot()
            .lines()
            .filter(|l| !l.contains("SYNCHRONIZE"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(about(unmet(&a_good_loader(), &unflushed)), ["usb-flush"]);

        // And the loader's file, which carries what no log record can.
        let no_quiesce = a_good_loader()
            .lines()
            .filter(|l| !l.contains(QUIESCE_HEAD))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(about(unmet(&no_quiesce, &a_good_boot())), ["usb-quiesce", "usb-quiesce"]);
        let left = a_good_loader().replace("2/2 disk", "1/2 disk");
        assert_eq!(about(unmet(&left, &a_good_boot())), ["usb-quiesce"]);
        let died = a_good_loader().replace(
            "the last boot read DONE",
            "the page still reads ARMED, so that kernel died without reaching",
        );
        assert_eq!(about(unmet(&died, &a_good_boot())), ["chain", "no-death"]);
    }

    #[test]
    fn a_negative_exit_code_names_its_refusal() {
        assert_eq!(Refused::of(-5), Some(Refused::Disagreed));
        assert_eq!(Refused::of(-1), Some(Refused::NoCapability));
        // A measurement, and a negative that names nothing, are both `None`:
        // the caller says which of the two it is.
        assert_eq!(Refused::of(402_000), None);
        assert_eq!(Refused::of(-7), None);
        assert!(format!("{}", Refused::Disagreed).contains("read back"));
    }

    /// The one arithmetic both profiles judge by, in all four of its states.
    #[test]
    fn a_bound_is_judged_once_it_is_taken() {
        assert!(matches!(Bound::Unmeasured.check(7, "us"), Outcome::Measured { value: 7, .. }));
        assert!(matches!(Bound::AtLeast(20_000).check(30_500, "KiB/s"), Outcome::Held(_)));
        assert!(Bound::AtLeast(40_000).check(30_500, "KiB/s").is_failure());
        assert!(matches!(Bound::AtMost(500).check(402, "us"), Outcome::Held(_)));
        assert!(Bound::AtMost(400).check(402, "us").is_failure());
        assert!(matches!(Bound::Exactly(9).check(9, "fold"), Outcome::Held(_)));
        assert!(Bound::Exactly(9).check(8, "fold").is_failure());
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
            assert!(source.contains(&declared), "{} declares no `{declared}`", path.display());
        }
        // And the other way: a refusal the probe grew and this file has not.
        let arms = source
            .lines()
            .filter_map(|line| line.trim().strip_suffix(','))
            .filter(|line| line.contains(" = -"))
            .count();
        assert_eq!(arms, Refused::ALL.len(), "{} declares {arms} refusals", path.display());
    }
}
