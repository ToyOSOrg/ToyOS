//! The grammar of what a device boot says about itself: the exit codes
//! `userland/metalprobe` refuses with, the inventory records every metal boot
//! owes, and the account the shutdown seals into the black box.
//!
//! **Readers only.** What a number may be lives in `tests/metal-profile.toml`
//! and what a boot's verdict is lives in the harness's judge; this is the one
//! place the *shape* of both is written down, so a host and a guest that must
//! agree about a word agree about it here.

#![forbid(unsafe_code)]

use std::fmt;

/// Why a `metalprobe` job has no number, mirroring `Refusal` in
/// `userland/metalprobe/src/main.rs`.
///
/// Held to that file by [`tests::the_probe_declares_the_refusals_this_names`],
/// because nothing links the two crates: the probe is built for the guest
/// triple and never for the host.
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
    pub const ALL: &'static [Self] = &[
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
    pub fn of(code: i32) -> Option<Self> {
        Self::ALL.iter().copied().find(|r| *r as i32 == code)
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
            Self::NoDuration => "the measurement took no time the clock could tell from zero",
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

/// The device inventory, asserted on every device boot out of `logd`'s file.
///
/// **Every needle is a prefix of a record the shipping kernel writes**, so
/// nothing here needs an actuator and nothing here is about a number. What is
/// about *this* machine — how many controllers, which silicon, which mode — is
/// read out by [`inventory`] and reported, because a count nobody has taken is
/// not a bound.
pub const RECORDS: &[Record] = &[
    // The framebuffer's memory type, which is what makes a fill and a readback
    // two different measurements rather than one.
    Record { about: "PAT", needle: "PAT: IA32_PAT=", presence: Says },
    Record { about: "scanout", needle: "GOP: scanout memory type WC ", presence: Says },
    // The stick this boot came off, through the controller that carries it.
    Record { about: "xhci", needle: "xHCI: found at PCI ", presence: Says },
    Record { about: "usb-storage", needle: "usb-storage: ", presence: Says },
    Record { about: "xhci-refused", needle: "xHCI: NOT INITIALISED", presence: NeverSays },
    // The keyboard controller, with no key pressed all boot.
    Record { about: "i8042", needle: "i8042: ok selftest=0x55", presence: Says },
    Record { about: "i8042-quarantine", needle: "i8042: quarantined", presence: NeverSays },
    Record {
        about: "i8042-lost-edge",
        needle: "i8042: bytes with no IRQ record",
        presence: NeverSays,
    },
    // The internal disk: identified, and — the whole safety argument for
    // running on the bench at all — never written.
    Record { about: "nvme", needle: "NVMe: NS1 size=", presence: Says },
    Record { about: "nvme-refused", needle: "NVMe: NOT INITIALISED", presence: NeverSays },
    Record {
        about: "nvme-offline",
        needle: "NVMe: this controller is offline",
        presence: NeverSays,
    },
    Record { about: "nvme-census", needle: NVME_CENSUS, presence: Says },
    // Every disk's write cache, emptied before anything was taken down. This
    // one *is* a log record: it is made above the boot's last word, while the
    // volume that carries it is still there.
    Record { about: "usb-flush", needle: "usb-quiesce: disk ", presence: Says },
];

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

/// The NVMe command census, and the one field a boot may not have moved: a
/// write to the disk this project never writes.
pub const NVME_CENSUS: &str = "nvme: commands ";
pub const NVME_NO_WRITES: &[&str] = &["write=0", "io-other=0"];

/// The head of the shutdown's own account, as `kernel/src/drivers/xhci/mod.rs`
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
    pub halted: u32,
    pub controllers: u32,
    pub reset: u32,
    pub unpowered: u32,
    pub ports: u32,
}

impl Quiesced {
    /// Whether every device the boot had was handed back: every disk's cache
    /// emptied, every controller halted and reset.
    ///
    /// Ports are reported and not judged: `PORTSC.PP` is writable only on a
    /// controller with Port Power Control, so a count under the total is a fact
    /// about the silicon and not about the shutdown.
    pub fn complete(&self) -> bool {
        self.flushed == self.disks
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
    let (halted, controllers) = pair(" controller(s) halted")?;
    let (unpowered, ports) = pair(" port(s) unpowered")?;
    let reset = line.split(" controller(s) halted, ").nth(1)?.split_whitespace().next()?;
    Some(Quiesced {
        flushed,
        disks,
        halted,
        controllers,
        reset: reset.parse().ok()?,
        unpowered,
        ports,
    })
}

/// Every record that had to be there and was not, and every one that had to be
/// absent and was not — the loader's file and `logd`'s judged by their own
/// tables. Each line opens with the `about` it failed, so a caller can name
/// them without reading the prose.
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

/// Every value a boot reported that a later profile row would hold: the lines a
/// reader needs to write the next version of `tests/metal-profile.toml`.
pub fn inventory(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for needle in
        ["xHCI: ", "usb-storage: ", "i8042: ", "hda: ", "NVMe: ", NVME_CENSUS, "GOP: ", "PAT: "]
    {
        for line in log.lines().filter(|l| l.contains(needle)) {
            if !out.iter().any(|seen| seen == line) {
                out.push(line.to_string());
            }
        }
    }
    out
}

/// The first line carrying `needle`, trimmed, for a verdict to quote.
fn quoted(text: &str, needle: &str) -> String {
    text.lines().find(|line| line.contains(needle)).map(str::trim).unwrap_or_default().to_string()
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
            line("0.140", "usb-storage: 1 device(s)"),
            line("0.150", "i8042: ok selftest=0x55 cfg=0x45->0x44 port1=ok port2=ok"),
            line("0.200", "NVMe: NS1 size=1000215216 sectors, sector_size=512"),
            line("0.319", "GOP: scanout memory type WC (MTRR WB, PAT entry 4)"),
            line("0.368", "Boot: complete (368ms)"),
            line("1.100", "exit: usbwrite pid=6 code=402000 cpu=140ms"),
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
         | usb-quiesce: 2/2 disk cache(s) flushed, 2/2 controller(s) halted, 2 reset, \
         12/16 port(s) unpowered\n\
         Loader log: the last boot is accounted for, so this pass resets the machine\n"
            .to_string()
    }

    #[test]
    fn the_shutdowns_own_account_is_read_out_of_the_loaders_file() {
        assert_eq!(
            quiesced(&a_good_loader()),
            Some(Quiesced {
                flushed: 2,
                disks: 2,
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
        // Each of the three that *are* judged, failing on its own.
        for (from, to) in [
            ("2/2 disk", "1/2 disk"),
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
