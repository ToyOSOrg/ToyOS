//! `toyos-metal` — the loop that writes a ToyOS image to a metal bench's
//! internal NVMe ([`internal`]), boots it once through `BootNext`, and answers
//! with [`toyos_build::bootlog`]'s verdict on what that boot wrote to its log
//! partition.
//!
//! It runs on the development host and reaches the machine over `ssh`: the
//! T14 bench unless `--host` names another, whose account's home holds the
//! log's mount point and which has passwordless sudo. [`Target::argv`] is the
//! one place a root command line ([`Job`]) is written.
//!
//! **The bench's own partitions are never written.** The machine must answer
//! as a recorded bench by its SMBIOS, the disk must be one
//! [`internal::recognise`] knows, and every write names one of the partitions
//! after the bench's four, but for the partition table itself, which `sgdisk`
//! rewrites whole on every flash with entries 1–4 as they were. The
//! firmware's order keeps Ubuntu first: ToyOS's entry is never in it, which
//! every boot checks before and after, so a ToyOS boot that fails or is reset
//! hands the next boot to Ubuntu.
//!
//! Beside the loop: [`testargs`], the suite's command line, which carries the
//! metal profile's flags; [`image`], the config a metal boot derives; and
//! [`timings`], each bench's record.

use std::fmt;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use toyos_build::bootlog;
use toyos_build::disk::Node;
use toyos_build::flags::{declare_flags, Flag};
use toyos_gpt::Guid;

use crate::internal::Owned;
use crate::timings::{Machine, Record};

pub mod image;
pub mod internal;
pub mod testargs;
pub mod timings;

const CONNECT_SECS: u64 = 10;

/// The port the machine's ssh server listens on, which is the protocol's own.
const SSH_PORT: u16 = 22;

/// How long the machine has to go quiet after `reboot`.
const GOING_DOWN_SECS: u64 = 120;

/// What the machine spends getting back to Ubuntu's ssh server once a ToyOS boot is over:
/// the firmware's pass and Ubuntu's own boot.
const RETURN_ALLOWANCE_SECS: u64 = 300;

/// Every bound a metal boot runs under that its image does not carry, by the
/// constant that arms it: the firmware's over the span before the handoff, the
/// TCO the loader arms there and the kernel keeps feeding, and the panicked
/// kernel's own over its panel.
const WATCHDOG_BOUNDS_MS: &[u64] =
    &[toyos_tco::FIRMWARE_BOUND_MS, toyos_tco::BOUND_MS, toyos_tco::PANIC_BOUND_MS];

/// How long a machine flashed with an image armed with `deadline_ms` has to
/// answer `ssh` again after `reboot`.
///
/// **Derived, because a literal is wrong the day any of it moves.** A boot is
/// only certainly over once the longest of [`WATCHDOG_BOUNDS_MS`] and the
/// kernel's own bound over the whole boot could have fired: that one is the
/// image's, the only one that reaches a machine still executing and making no
/// progress, and it outlasts the runner's bound over its list, which
/// `toyos_tco::wedge_bound_ms` derives it from. [`RETURN_ALLOWANCE_SECS`] is
/// what coming back costs after that.
pub fn return_secs(deadline_ms: u64) -> u64 {
    let longest = WATCHDOG_BOUNDS_MS.iter().copied().fold(deadline_ms, u64::max);
    longest.div_ceil(1_000) + RETURN_ALLOWANCE_SECS
}

/// How often the loop asks whether the machine is there.
const POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Each tool at the path the bench's Ubuntu installs it, so `sudo` runs that
/// one whatever the account's `PATH`.
const BLOCKDEV: &str = "/usr/sbin/blockdev";
const DD: &str = "/usr/bin/dd";
const EFIBOOTMGR: &str = "/usr/bin/efibootmgr";
const MOUNT: &str = "/usr/bin/mount";
const PARTX: &str = "/usr/bin/partx";
const SGDISK: &str = "/usr/sbin/sgdisk";
const UMOUNT: &str = "/usr/bin/umount";
const REBOOT: &str = "/usr/sbin/reboot";
const TRUE: &str = "/usr/bin/true";
const UDEVADM: &str = "/usr/bin/udevadm";
const WIPEFS: &str = "/usr/sbin/wipefs";

/// How long udev's queue gets to drain, a failure bound: udevd kills a stuck
/// event at 180 s (systemd-udevd(8), `--event-timeout`).
const SETTLE_SECS: u64 = 180;

/// What the firmware loads off the internal ESP.
const LOADER: &str = r"\EFI\BOOT\BOOTX64.EFI";

/// The internal install's boot entry in the firmware's list.
const INTERNAL_LABEL: &str = "ToyOS-internal";

/// The entry [`fall_through`] boots once, on the internal ESP, and deletes.
const UNBOOTABLE_LABEL: &str = "ToyOS-unbootable";

/// The three logind keys that keep a closed-lid machine awake.
const LID_KEYS: &[&str] =
    &["HandleLidSwitch", "HandleLidSwitchExternalPower", "HandleLidSwitchDocked"];

/// What `toyos-metal`'s last statement on its stderr opens with, past the
/// printer's stamp, when it refuses: the harness reads the refusal back by it,
/// to name it beside the exit status.
pub const REFUSAL_HEAD: &str = "toyos-metal: ";

/// The refusal a `toyos-metal` run's stderr ends on, from its [`REFUSAL_HEAD`]
/// line to the end, or `None` where it said none. Only a statement's first line
/// carries the stamp, so only that line is read past it.
pub fn said_refusal(stderr: &str) -> Option<String> {
    let lines: Vec<&str> = stderr.lines().collect();
    let at = lines
        .iter()
        .rposition(|line| toyos_build::printer::unstamped(line).starts_with(REFUSAL_HEAD))?;
    let mut said = vec![&toyos_build::printer::unstamped(lines[at])[REFUSAL_HEAD.len()..]];
    said.extend(&lines[at + 1..]);
    Some(said.join("\n").trim_end().to_string())
}

/// Every way this loop refuses, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No `HOME`, so the default key path would be rooted at `/`.
    NoHome,
    /// The disk's identity did not read, or is not a removable disk's.
    Identity { field: &'static str, want: String, got: String },
    /// The disk named to be adopted as scratch is not one this loop erases.
    Adopt(String),
    File { path: String, why: String },
    /// The image is armed with a parameter [`FLASHABLE`] does not clear.
    Armed { name: String },
    /// **The image carries no bound on its own boot.** Every metal image is
    /// built with `boot-deadline=<ms>`; one without it is not a metal-staged
    /// image, and flashing it puts the machine somewhere only a hand gets it out
    /// of. `staged_a_wedge` is the sharpest form: an image that stops every CPU
    /// on purpose and has nothing to end the boot it stopped.
    NoBound { staged_a_wedge: bool },
    /// **The outside judge's verdict on the volume the boot left.** What
    /// `toyos-fat32-check` said about the log partition read straight off the
    /// disk — the bytes, not a driver's opinion of them, and not the FAT32
    /// implementation that wrote them.
    Fat32(String),
    /// What landed on the disk is not what the image says.
    Landed { what: String, want: u64, got: u64 },
    /// `efibootmgr` named no four-hex-digit boot entry.
    BootEntry(String),
    /// The firmware's boot order moved `during` something this loop did.
    BootOrder { during: &'static str, before: String, now: String },
    /// The entry about to be booted once is in the firmware's boot order, so
    /// a failed ToyOS boot would come back to ToyOS rather than Ubuntu.
    Ordered { entry: String, order: String },
    Sudo(String),
    /// A lid key that no longer reads `ignore`, which is what keeps the machine up.
    Lid { key: &'static str, got: String },
    Remote { what: String, status: String, stderr: String },
    /// The machine did not go down, or did not come back.
    Silent { what: &'static str, secs: u64 },
    /// The firmware booted the entry [`fall_through`] made unbootable, or
    /// kept the `BootNext` naming it.
    FellThrough(String),
    /// The log the boot left is not a passing boot's.
    Log(bootlog::Unfit),
    /// **The loader reported a record and booted no kernel.** Not the same thing
    /// as an absent boot record, and the difference is the whole diagnosis: the
    /// pass that reads the black box ends the chain by design, so a machine that
    /// came back with `loader.log` carrying a reporting pass and `logkeeper` carrying
    /// nothing did exactly what it was told — the question is why it had a
    /// record to report at all.
    ReportedAndBootedNothing { said: String },
    /// **The last boot of this image was handed the machine and hung.** The
    /// loader counts attempts on the log partition, and this pass is the second of an
    /// image whose first never reported — so it booted no kernel and handed the
    /// machine back rather than starting the same hang again. What is owed is a
    /// look at why that kernel stopped, and the boot before this one is where.
    HungWithoutARecord,
    /// **An image armed to stop itself did not, or was not ended by its own
    /// bound.** Said only of an image carrying [`WEDGE_ARM`], for which
    /// `Rebooting.` is the failure and a sealed `WEDGED` record is the pass —
    /// the one boot in this loop whose verdict is not `bootlog::verdict`'s.
    Wedge { why: &'static str },
    /// The machine did not name itself: its SMBIOS answer is not
    /// [`Machine::QUERY`]'s three lines.
    Machine(String),
    /// The account's home on the machine is no normal absolute path.
    Home(String),
    /// The machine has no record, so it is no bench this loop writes.
    Unrecorded { path: String },
    /// The machine's record is there and does not read.
    Record(String),
    /// The internal disk is no layout [`internal::recognise`] knows, or its
    /// install area is not as the last step left it.
    Layout(String),
    /// The image is not one an internal install writes.
    Install(String),
    /// A GUID the write would give a partition is already another's.
    Collision(String),
    Usage(String),
}

impl Refusal {
    /// Whether the machine failed rather than the loop: the boot is the subject
    /// only once the disk is written and the reboot asked for.
    pub fn about_the_boot(&self) -> bool {
        matches!(
            self,
            Self::Silent { .. }
                | Self::FellThrough(_)
                | Self::Log(_)
                | Self::Fat32(_)
                | Self::ReportedAndBootedNothing { .. }
                | Self::HungWithoutARecord
                | Self::Wedge { .. }
        )
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHome => write!(f, "no HOME, so there is no default key path; pass --key"),
            Self::Identity { field, want, got } => {
                write!(f, "the disk's {field} reads {got:?} where this loop was given {want:?}")
            }
            Self::Adopt(why) => write!(f, "nothing was erased: {why}"),
            Self::File { path, why } => write!(f, "{path}: {why}"),
            Self::NoBound { staged_a_wedge } => write!(
                f,
                "the image carries no `{}<ms>` on its parameter line{}. Every image the \
                 metal profile builds carries one, so this is not one of them — and a \
                 flashed boot with no bound on itself is what this loop exists to prevent: \
                 a kernel that stops making progress without panicking is bounded by \
                 nothing else, and the machine has no power switch",
                toyos_tco::DEADLINE_PARAM,
                if *staged_a_wedge {
                    ", and it stages a wedge that stops every CPU on purpose"
                } else {
                    ""
                }
            ),
            Self::Fat32(said) => write!(
                f,
                "the log partition the boot left does not check out: {said}. This is \
                 `toyos-fat32-check` reading the volume off the disk, so what it names is \
                 what the kernel wrote and not what a driver read back"
            ),
            Self::Armed { name } => write!(
                f,
                "the image is armed with {name:?}, and nothing in this tree has ruled on whether \
                 the machine survives it. Every parameter a flashed image carries needs a row \
                 in `toyos_metal::FLASHABLE`"
            ),
            Self::Landed { what, want, got } => {
                write!(f, "{what} on the disk is {got} where the image says {want}")
            }
            Self::BootEntry(saw) => {
                write!(f, "efibootmgr named no four-hex-digit boot entry: {saw:?}")
            }
            Self::BootOrder { during, before, now } => write!(
                f,
                "{during} moved BootOrder from {before:?} to {now:?}: this loop buys one boot with \
                 --bootnext and leaves the order alone"
            ),
            Self::Ordered { entry, order } => write!(
                f,
                "Boot{entry} is in BootOrder {order:?}, so a ToyOS boot that fails would come back to \
                 ToyOS: the firmware put it there, and it is taken out by hand"
            ),
            Self::Sudo(saw) => {
                write!(f, "`sudo -n` on the machine did not answer: {saw}. The bench account needs passwordless sudo")
            }
            Self::Lid { key, got } => write!(
                f,
                "logind's {key} reads {got:?} and not \"ignore\": the machine suspends on a \
                 closed lid and no loop survives that"
            ),
            Self::Remote { what, status, stderr } => {
                write!(f, "{what} on the machine {status}: {stderr}")
            }
            Self::Silent { what, secs } => write!(
                f,
                "the machine did not {what} within {secs} s, which is longer than every watchdog \
                 a boot runs under plus the time coming back costs; why it did not is what the \
                 panel and the log partition say, and neither is readable from here"
            ),
            Self::FellThrough(why) => write!(f, "the firmware did not fall through past an unbootable entry: {why}"),
            Self::Log(unfit) => write!(f, "the log partition came back and {unfit}"),
            Self::Wedge { why } => write!(
                f,
                "this image is armed to stop itself, so it is judged by the record its own \
                 deadline sealed and not by the word a shutdown writes — and {why}"
            ),
            Self::HungWithoutARecord => write!(
                f,
                "the last boot of this image was handed the machine and never reported: no panic, \
                 no fault and no deliberate handover, which is a hang. This pass refused to \
                 boot the same kernel again and gave the machine back, so the machine is free \
                 and nothing needs a hand — but this boot measured no test, and why that kernel \
                 stopped is the boot before it"
            ),
            Self::ReportedAndBootedNothing { said } => write!(
                f,
                "the loader reported a record and booted no kernel: `loader.log` carries a \
                 pass that read the black box and ended the chain, and `logkeeper` wrote nothing \
                 because nothing ran. This boot measured no test. What the pass said was: \
                 {said}"
            ),
            Self::Machine(why) => write!(f, "the machine did not say what it is: {why}"),
            Self::Home(got) => write!(
                f,
                "the account's home on the machine reads {got:?}, which is no normal absolute path"
            ),
            Self::Unrecorded { path } => write!(
                f,
                "this machine has no record at {path}, so it is no bench this loop writes. Record \
                 it once: `cargo run -p toyos-metal -- --record --host <user>@<machine>` writes the \
                 record from what the machine's SMBIOS answers"
            ),
            Self::Record(why) => write!(f, "the machine's record does not read: {why}"),
            Self::Layout(why) => write!(f, "the internal disk is not installed on: {why}"),
            Self::Install(why) => write!(f, "the image is not installed: {why}"),
            Self::Collision(why) => write!(f, "nothing was written: {why}"),
            Self::Usage(why) => write!(f, "{why}"),
        }
    }
}

/// A whole SCSI disk, `/dev/sd<letter>`: what a USB stick enumerates as, and
/// the one kind of disk [`adopt_scratch`] erases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Sd(u8);

impl Sd {
    fn parse(path: &str) -> Result<Self, Refusal> {
        match Node::parse(path) {
            Ok(Node::Scsi(letter)) => Ok(Self(letter)),
            _ => Err(Refusal::Usage(format!("{path:?} is no /dev/sd<letter> disk, which is what a scratch disk is"))),
        }
    }

    fn node(self) -> Node {
        Node::Scsi(self.0)
    }

    /// The read [`Identity::parse`] answers.
    fn identity(self) -> String {
        let at = self.node().sysfs();
        format!("cat {at}/size {at}/device/vendor {at}/device/model {at}/removable")
    }
}

/// What `/sys/class/block/<disk>/` answered, in the order [`Sd::identity`]
/// asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    sectors: u64,
    vendor: String,
    model: String,
    removable: bool,
}

impl Identity {
    /// The four `sysfs` files as `cat` concatenates them. The trailing padding
    /// is the SCSI inquiry's own: vendor and model are fixed-width fields.
    fn parse(text: &str) -> Result<Self, Refusal> {
        let fields: Vec<&str> = text.lines().map(str::trim).collect();
        let [size, vendor, model, removable] = fields[..] else {
            return Err(Refusal::Identity {
                field: "the whole query",
                want: "four lines".to_string(),
                got: text.to_string(),
            });
        };
        Ok(Self {
            sectors: size.parse().map_err(|_| Refusal::Identity {
                field: "size",
                want: "a sector count".to_string(),
                got: size.to_string(),
            })?,
            vendor: vendor.to_string(),
            model: model.to_string(),
            removable: removable == "1",
        })
    }
}

/// Every block device with its partitions' numbers, types, GUIDs and
/// filesystems, as udev records them, for [`adoptable`].
const DISKS: &str = "lsblk --noheadings --paths --pairs --output NAME,TYPE,PKNAME,PARTN,PARTTYPE,PARTUUID,FSTYPE";

/// One partition [`DISKS`] listed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    index: u32,
    kind: Option<Guid>,
    unique: Option<Guid>,
    /// The filesystem whose signature `blkid` found in it.
    filesystem: Option<String>,
}

/// Each `/dev/sd<letter>` disk [`DISKS`] answered, with its partitions.
fn sd_disks(listing: &str) -> Result<Vec<(Sd, Vec<Listed>)>, Refusal> {
    let unread = |why: String| Refusal::Remote {
        what: "listing the disks".to_string(),
        status: "answered no lsblk listing".to_string(),
        stderr: why,
    };
    let rows: Vec<_> = listing
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(internal::pairs)
        .collect::<Result<_, _>>()
        .map_err(unread)?;
    let field = |row: &std::collections::BTreeMap<String, String>, key: &str| row.get(key).cloned().unwrap_or_default();
    let mut disks: Vec<(Sd, Vec<Listed>)> = rows
        .iter()
        .filter(|row| field(row, "TYPE") == "disk")
        .filter_map(|row| Sd::parse(&field(row, "NAME")).ok())
        .map(|sd| (sd, Vec::new()))
        .collect();
    for row in rows.iter().filter(|row| field(row, "TYPE") == "part") {
        let Some((_, parts)) = disks.iter_mut().find(|(sd, _)| sd.node().whole() == field(row, "PKNAME")) else {
            continue;
        };
        let index = field(row, "PARTN")
            .parse()
            .map_err(|_| unread(format!("{} reads PARTN={:?}", field(row, "NAME"), field(row, "PARTN"))))?;
        parts.push(Listed {
            index,
            kind: internal::guid(&field(row, "PARTTYPE")),
            unique: internal::guid(&field(row, "PARTUUID")),
            filesystem: Some(field(row, "FSTYPE")).filter(|fs| !fs.is_empty()),
        });
    }
    Ok(disks)
}

/// What a disk holds, which is what tells a scratch disk from one carrying an
/// image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrying {
    /// A ToyOS image's table, as `build/image.rs` writes one: one ESP, one
    /// TOYOS-LOG, one slot table, and one or two slots.
    Layout,
    /// A scratch disk's table, as [`adopt_scratch`] writes it: one partition,
    /// of [`Guid::TOYOS_SCRATCH`], so no disk holding a ToyOS image is one.
    /// The mark is the whole of a scratch disk's identity: only adoption
    /// writes it, and what carries it is erasable by definition.
    Scratch,
}

impl Carrying {
    fn holds(self, parts: &[Listed]) -> bool {
        let count = |kind: Guid| parts.iter().filter(|p| p.kind == Some(kind)).count();
        match self {
            Self::Layout => {
                count(Guid::EFI_SYSTEM) == 1
                    && count(Guid::MICROSOFT_BASIC) == 1
                    && count(Guid::TOYOS_SLOTS) == 1
                    && (1..=2).contains(&count(Guid::TOYOS_BOOT))
                    && (1..=2).contains(&count(Guid::TOYOS_ROOT))
            }
            Self::Scratch => matches!(parts, [only] if only.kind == Some(Guid::TOYOS_SCRATCH)),
        }
    }
}

impl fmt::Display for Carrying {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout => write!(f, "a ToyOS image's partition table"),
            Self::Scratch => write!(f, "a scratch disk's one partition"),
        }
    }
}

/// `machine`'s record in `dir`, refused where it has none.
fn recorded(dir: &Path, machine: &Machine) -> Result<Record, Refusal> {
    Record::load(dir, machine)
        .map_err(Refusal::Record)?
        .ok_or_else(|| Refusal::Unrecorded { path: dir.join(machine.file()).display().to_string() })
}

/// Record `machine` in `dir` where it has no record, answering the record's
/// path and whether this wrote it. **It never moves a record**: a machine is
/// re-recorded by deleting its file.
fn record_machine(dir: &Path, machine: &Machine) -> Result<(PathBuf, bool), Refusal> {
    let wrote = match Record::load(dir, machine).map_err(Refusal::Record)? {
        Some(_) => false,
        None => {
            Record::new(machine).save(dir).map_err(Refusal::Record)?;
            true
        }
    };
    Ok((dir.join(machine.file()), wrote))
}

/// Every command this loop runs as root, with what it acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Job {
    Wipe(Sd),
    /// The signatures inside one partition, which a wipe of the whole disk
    /// leaves.
    WipePartition { sd: Sd, part: u32 },
    Settle,
    /// The kernel's table of the disk, read again off the disk.
    Reread(Sd),
    /// The log volume's first `bytes` read as bytes rather than as files:
    /// what `toyos-fat32-check` judges is the volume `logkeeper` wrote, and a
    /// `mount` has already had a driver's opinion about it.
    ReadLog { bytes: u64 },
    /// A boot entry naming `loader` on the internal ESP.
    Create { label: &'static str, loader: &'static str },
    Delete(String),
    BootNext(String),
    /// One of the install's FAT volumes, read-only, at [`Home::mount`].
    Mount(Owned),
    Umount,
    Reboot,
    Probe,
    Partition,
    AddPartitions,
    Retag(Vec<(Owned, Guid)>),
    Write(Owned),
    Scratch(Sd),
}

/// One word as the machine's login shell must receive it: `ssh` hands the whole
/// command to that shell.
fn shell_word(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The account's home on the machine, as its login shell answers [`Home::QUERY`]
/// and never as a convention guesses it: the log is mounted under it, so no
/// root command creates the mount point and nothing lands in a
/// world-writable directory.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Home(String);

impl Home {
    const QUERY: &str = "printf %s \"$HOME\"";

    /// Normal and absolute.
    fn parse(text: &str) -> Result<Self, Refusal> {
        let refused = || Refusal::Home(text.to_string());
        if text.chars().any(char::is_control) {
            return Err(refused());
        }
        let rest = text.strip_prefix('/').ok_or_else(refused)?;
        if rest.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return Err(refused());
        }
        Ok(Self(text.to_string()))
    }

    /// Where the log partition is mounted.
    fn mount(&self) -> String {
        format!("{}/toyos-log", self.0)
    }
}

/// Where the loop runs and what it may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    user: String,
    host: String,
    key: PathBuf,
    /// The bench's own NVMe, which an install writes only the [`Owned`]
    /// partitions of.
    internal: Node,
}

impl Target {
    /// The T14 bench, with the runner key this host holds.
    fn bench_t14() -> Result<Self, Refusal> {
        let home = std::env::var_os("HOME").ok_or(Refusal::NoHome)?;
        Ok(Self {
            user: "toyos".to_string(),
            host: "bench-t14".to_string(),
            key: PathBuf::from(home).join(".ssh/id_ed25519_toyos_runner"),
            internal: Node::Nvme { controller: b'0', namespace: b'1' },
        })
    }

    /// **The one place a root command line is written.**
    fn argv(&self, job: &Job, home: &Home) -> Vec<String> {
        let internal = self.internal.whole();
        let words = |words: &[&str]| words.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        match job {
            Job::Wipe(sd) => words(&[WIPEFS, "--all", &sd.node().whole()]),
            Job::WipePartition { sd, part } => words(&[WIPEFS, "--all", &sd.node().partition(*part)]),
            // Root's, because only root's settle first waits for udevd to
            // have handled what is already pending, such as a writer's close
            // it has yet to turn into an event.
            Job::Settle => words(&[UDEVADM, "settle", &format!("--timeout={SETTLE_SECS}")]),
            Job::Reread(sd) => words(&[BLOCKDEV, "--rereadpt", &sd.node().whole()]),
            // The volume and not the partition, which is larger than any
            // image's (coreutils' `count_bytes`).
            Job::ReadLog { bytes } => words(&[
                DD,
                &format!("if={}", Owned::Log.node(&self.internal)),
                "bs=4M",
                "iflag=count_bytes",
                &format!("count={bytes}"),
            ]),
            // `--create-only`, never `--create`: the latter "add[s] to
            // bootorder" (efibootmgr(8)) at the top, which would replace
            // Ubuntu as the machine's default and leave a failed ToyOS boot
            // nothing to fall back to.
            Job::Create { label, loader } => words(&[
                EFIBOOTMGR,
                "--create-only",
                "--disk",
                &internal,
                "--part",
                &Owned::Esp.index().to_string(),
                "--label",
                label,
                "--loader",
                loader,
            ]),
            Job::Delete(id) => words(&[EFIBOOTMGR, "--delete-bootnum", "--bootnum", id]),
            Job::BootNext(id) => words(&[EFIBOOTMGR, "--bootnext", id]),
            Job::Mount(o) => words(&[MOUNT, "-o", "ro", &o.node(&self.internal), &home.mount()]),
            Job::Umount => words(&[UMOUNT, &home.mount()]),
            Job::Reboot => words(&[REBOOT]),
            Job::Probe => words(&[TRUE]),
            // Every owned partition in one line, each at the start of the
            // largest free block as it stands (sgdisk(8), `--new`'s start of
            // 0), which `internal::recognise` established stays the one after
            // DATA until the last is made.
            Job::Partition => {
                let mut out = words(&[SGDISK]);
                for o in Owned::ALL {
                    let n = o.index();
                    out.push(format!("--new={n}:0:+{}M", o.mib()));
                    out.push(format!("--typecode={n}:{}", o.kind()));
                    out.push(format!("--change-name={n}:{}", o.label()));
                }
                out.push(internal);
                out
            }
            // sgdisk's own re-read of the table is refused on a disk whose
            // other partitions are mounted, as Ubuntu's are; `partx` adds the
            // named range alone.
            Job::AddPartitions => {
                let range = format!("{}:{}", Owned::ALL[0].index(), Owned::ALL[Owned::ALL.len() - 1].index());
                words(&[PARTX, "--add", "--nr", &range, &internal])
            }
            Job::Retag(named) => {
                let mut out = words(&[SGDISK]);
                out.extend(named.iter().map(|(o, guid)| format!("--partition-guid={}:{guid}", o.index())));
                out.push(internal);
                out
            }
            // A fresh table whose one partition spans the disk (sgdisk(8),
            // `--new`'s 0 for the first and last sector), marked scratch.
            Job::Scratch(sd) => words(&[
                SGDISK,
                "--clear",
                "--new=1:0:0",
                &format!("--typecode=1:{}", Guid::TOYOS_SCRATCH),
                "--change-name=1:ToyOS-scratch",
                &sd.node().whole(),
            ]),
            // `nocreat`: a node that is not there is refused, rather than
            // created as a file in `/dev` that every later write lands in.
            Job::Write(o) => words(&[DD, &format!("of={}", o.node(&self.internal)), "bs=4M", "conv=fsync,nocreat"]),
        }
    }

    /// One job as the machine's login shell must receive it.
    fn remote(&self, job: &Job, home: &Home) -> String {
        let words: Vec<String> = self.argv(job, home).iter().map(|w| shell_word(w)).collect();
        format!("sudo -n {}", words.join(" "))
    }

    /// `ssh`'s argv without its own name, so a test reads the whole line.
    fn ssh_argv(&self, command: &str) -> Vec<String> {
        vec![
            "-i".to_string(),
            self.key.display().to_string(),
            "-o".to_string(),
            "IdentitiesOnly=yes".to_string(),
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            "-o".to_string(),
            format!("ConnectTimeout={CONNECT_SECS}"),
            format!("{}@{}", self.user, self.host),
            command.to_string(),
        ]
    }
}

/// Every parameter a bench survives — the boot ends and the machine is what it
/// was — and nothing else reaches its disk.
///
/// **The metal profile flashes test images**, so an actuator is admissible here
/// where `build::flashable_params` refuses it for the owner's own flash path.
/// What is not admissible is an arm that would leave the machine changed where
/// a flash cannot put it back: firmware state a reboot does not undo, or the
/// bench's own partitions, which hold the Ubuntu every failed boot falls back
/// to. The install area is not such state: the next flash writes it again.
///
/// **A deny-list fails open on the next actuator**, so this is the whole
/// judgement: an image armed with a name that has no row here is refused by
/// that name rather than flashed on the assumption it is harmless.
pub const FLASHABLE: &[&str] = &[
    // The kernel's own boot parameters. Both are what a shipped image carries,
    // and `build::flashable_params` already lets the owner flash them.
    "watchdog",
    "early-panel",
    // It issues machine-wide TLB shootdowns from the BSP after the roster is
    // released and before the idle loop, and reports how long each took. It
    // reaches no device, writes no register outside `CR3`, and leaves nothing
    // behind: the boot goes on to userland and ends the way an unarmed one does.
    "tlb-shootdown-bench",
    // **The in-kernel self-tests.** Each stages inputs the hardware cannot
    // produce — a crafted PCI capability list, a malformed USB descriptor, a
    // vector nothing claims — runs a check over them in memory and prints a
    // count. None reaches a device register, none writes firmware state, and
    // the boot goes on to userland and ends the way an unarmed one does; what
    // an armed image leaves behind is a longer log.
    "pci-cap-selftest",
    "revoked-backing-selftest",
    "lapic-spurious-selftest",
    "unclaimed-vector-selftest",
    // A probe rather than staged inputs, and a read: it runs inside the first
    // syscall's own context switch.
    "sysret-ss-probe",
    // It enters one port in the kernel's table of declared ports at boot. No
    // kernel path reads or writes a port for that entry, so it reaches no
    // device; what it changes is that a claim on the i8042's row is refused.
    "isa-port-declared",
    // Three nested `scheduler::Operation`s with known deadlines, in both homes,
    // each printing what it asked for and what it observed. It establishes and
    // drops them and reaches nothing else.
    // Every IOMMU unit left translating, remapping and queueing just before the
    // kernel programs it, which switches each of those off again first. It
    // writes only the units' registers, which a reset returns to firmware, and
    // tables in memory this boot owns.
    "iommu-firmware-left",
    // Every CPU is handed the loader's Intel update with its header's revision
    // raised by one and its checksum sealed again. Its payload is Intel's own,
    // the revision the CPU already runs, so the CPU refuses it or runs what it
    // ran, and the boot goes on to userland and ends the way an unarmed one
    // does. Nothing it writes outlives a hard reset (SDM Vol. 3A §12.11.6.1).
    "microcode-reseal",
    // Every CPU is handed the loader's AMD patch with one byte of its data
    // flipped. The AMD bench's CPUs refuse it with `#GP` and keep the level
    // their firmware loaded, and the boot goes on to userland and ends the
    // way an unarmed one does. Nothing it writes outlives a reset: the patch
    // level resets at warm and cold reset (55922 §1.4.4.11).
    "microcode-amd-flip",
    // It seals this boot's own record under an identity one bit from this
    // image's, so the pass that finds it clears it and boots a kernel. The page
    // is memory the loader allocated and the machine is what it was after.
    FOREIGN_RECORD_ARM,
    // **The one arm that deliberately stops this machine.** At the shutdown
    // syscall, after the job list, every CPU stops taking scheduler passes.
    // Admissible only because `kernel/src/deadline.rs` is what ends it, which
    // [`arms_are_admissible`] refuses an image without: it reaches no device
    // register, writes no firmware state, and the boot after it is ordinary.
    WEDGE_ARM,
    // **The other arm that deliberately stops this machine, and it stops one
    // CPU harder.** It takes a lock of its own, clears `IF` on the last CPU and
    // never gives either back, which is the state this machine hung in for
    // 420 s. Admissible for the same reason as the row above and one more:
    // `kernel/src/hardlockup` ends it at half the bound the image carries, and
    // the boot deadline is still armed behind that. It reaches no device
    // register and writes no firmware state; the kernel implies `WEDGE_ARM`
    // behind it, so the boot cannot end itself before its own bound.
    LOCKUP_ARM,
];

/// The arm that stops the machine, named once: [`FLASHABLE`] rules on it and
/// [`arms_are_admissible`] refuses an image carrying it with no bound to end it.
pub const WEDGE_ARM: &str = "wedge-before-reset";

/// The arm that stops one CPU harder — interrupts off, inside a lock — which
/// only `kernel/src/hardlockup`'s NMI sample reaches. A second name rather than
/// a second judge: what it owes a readback is exactly what [`WEDGE_ARM`] owes,
/// and which bound sealed the page is the page's own first line to say.
pub const LOCKUP_ARM: &str = "hard-lockup-probe";

/// Every arm that deliberately leaves this machine to be ended by a bound.
///
/// **One list and not a condition spelled out at each reader.** A further such
/// arm added to [`FLASHABLE`] and not here is an image whose boot is judged by
/// the word a shutdown writes — which it never reaches — so the reader that
/// forgot it reds every boot it was staged for.
pub const WEDGE_ARMS: &[&str] = &[WEDGE_ARM, LOCKUP_ARM];

/// The arm whose boot the pass after it reads as a hang on purpose: its record
/// is sealed under another image's identity, so that pass clears it and finds
/// an attempt nothing reported, and hands the machine back.
pub const FOREIGN_RECORD_ARM: &str = "blackbox-foreign-identity";

/// Whether this image is armed to stop itself, and so owes a sealed record
/// rather than `Rebooting.`.
pub fn stages_a_wedge(armed: &[impl AsRef<str>]) -> bool {
    armed.iter().any(|name| WEDGE_ARMS.contains(&name.as_ref()))
}

/// Whether this image is ended by its own boot deadline on purpose: armed to
/// wedge, or carrying a deadline below the least bound a runner gives its list
/// ([`toyos_tco::JOB_BOUND_MS`]). The harness gives such a deadline only to a
/// boot whose load runs until the reset, such as `the_disk_survives_a_reset_under_load`'s,
/// so the boot is judged by the record that bound sealed, as a wedge is.
pub fn ended_by_its_deadline(armed: &[impl AsRef<str>]) -> bool {
    stages_a_wedge(armed)
        || armed.iter().any(|name| {
            name.as_ref()
                .strip_prefix(toyos_tco::DEADLINE_PARAM)
                .and_then(|ms| ms.parse::<u64>().ok())
                .is_some_and(|ms| ms < toyos_tco::JOB_BOUND_MS)
        })
}

/// The `boot-deadline=` bound an image armed with `armed` carries, whose
/// runner bounds its list at `list_ms`. A wedge staged under the chipset
/// watchdog is the watchdog's to end, so its deadline stands behind it.
pub fn bound_for(armed: &[impl AsRef<str>], list_ms: u64) -> u64 {
    match (stages_a_wedge(armed), watched(armed)) {
        (true, true) => toyos_tco::WATCHED_WEDGE_BOUND_MS,
        (true, false) => toyos_tco::STAGED_BOUND_MS,
        (false, _) => toyos_tco::wedge_bound_ms(list_ms),
    }
}

/// Whether this image is armed with the chipset watchdog.
fn watched(armed: &[impl AsRef<str>]) -> bool {
    armed.iter().any(|name| name.as_ref() == toyos_tco::PARAM)
}

/// Whether this image is armed so the pass after its reset clears its record
/// as another image's: nothing crosses on the page but the record's state, and
/// that pass hands the machine back as a hang.
pub fn clears_its_own_page(armed: &[impl AsRef<str>]) -> bool {
    armed.iter().any(|name| name.as_ref() == FOREIGN_RECORD_ARM)
}

/// Whether [`FLASHABLE`] clears `name`.
pub fn flashable(name: &str) -> bool {
    // The two parameters that carry a value are not names: the black-box page's
    // address, which every image the harness builds has, and the boot
    // deadline's bound. Neither arms an instrument, and the second is what ends
    // a boot this loop would otherwise wait 360 s for and then need a hand on.
    toyos_build::build::is_valued_param(name) || FLASHABLE.contains(&name)
}

/// The pre-flash gate: what the image is armed with, judged before it is
/// written. Read off the image's own ESP, so it answers about the artifact
/// rather than about whoever built it.
fn arms_are_admissible(path: &Path) -> Result<(Vec<String>, u64), Refusal> {
    let armed = toyos_build::image::params_of(path)
        .map_err(|why| Refusal::File { path: path.display().to_string(), why })?;
    let deadline_ms = judge_arms(&armed)?;
    Ok((armed, deadline_ms))
}

/// The gate's decision, over the list alone — so it can be staged without an
/// image, which is the only way the refusals below get a test at all. An
/// admitted image answers with the bound on its own boot, in milliseconds,
/// which is what the loop waits it out by.
pub fn judge_arms(armed: &[String]) -> Result<u64, Refusal> {
    // **Every flashed image carries a bound on its own boot, and this is the
    // gate.** The bound was first asked for only of the image that stages a
    // wedge, which is the sharpest case and not the only one: a kernel that
    // stops making progress without panicking is bounded by nothing else, and a
    // flashed boot with no bound is precisely what this loop exists to prevent —
    // measured twice on the T14, once as a hang after the job list and once as
    // a capture that fell through into flashing an ordinary
    // `target/bootable.img`, which has no job list and no deadline at all.
    // A bound nothing can read is no bound: the kernel panics on it.
    let Some(Ok(deadline_ms)) = toyos_tco::deadline_in(&armed.join(",")) else {
        return Err(Refusal::NoBound { staged_a_wedge: stages_a_wedge(armed) });
    };
    match armed.iter().find(|name| !flashable(name)) {
        Some(name) => Err(Refusal::Armed { name: name.clone() }),
        None => Ok(deadline_ms),
    }
}

/// What `dd` said of a copy on its own last line,
/// `<bytes> bytes (…) copied, <secs> s, <rate>`.
#[derive(Debug, Clone, PartialEq)]
struct DdSaid {
    bytes: u64,
    secs: f64,
}

fn dd_said(stderr: &str) -> Option<DdSaid> {
    let line = stderr.lines().find(|l| l.contains(" bytes ") && l.contains("copied"))?;
    let bytes = line.split_whitespace().next()?.parse().ok()?;
    let (_, after) = line.split_once("copied, ")?;
    let (secs, _) = after.split_once(" s, ")?;
    Some(DdSaid { bytes, secs: secs.parse().ok()? })
}

/// The id and the partition GUID of every entry `efibootmgr` lists under
/// `label`, out of `Boot0027* ToyOS<tab>HD(1,GPT,<guid>,…)/File(…)`.
fn entries_labelled(listing: &str, label: &str) -> Vec<(String, String)> {
    listing
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("Boot")?;
            let id = rest.get(..4)?;
            if !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let named = rest.get(4..)?;
            let named = named.strip_prefix('*').unwrap_or(named);
            let (name, path) = named.trim_start().split_once('\t')?;
            if name.trim() != label {
                return None;
            }
            let guid =
                path.split_once("HD(").and_then(|(_, hd)| hd.split(',').nth(2)).unwrap_or_default();
            Some((id.to_string(), guid.to_ascii_lowercase()))
        })
        .collect()
}

/// Every boot entry id a listing carries under `label`.
fn ids(listing: &str, label: &str) -> Vec<String> {
    entries_labelled(listing, label).into_iter().map(|(id, _)| id).collect()
}

/// The id a create left behind, or the refusal it earned: the order it must not
/// have moved, and the entry it must have made.
fn entry_after_create(
    before: &str,
    after: &str,
    label: &str,
    guid: &str,
) -> Result<String, Refusal> {
    let (was, now) = (boot_order(before), boot_order(after));
    if was != now {
        return Err(Refusal::BootOrder { during: "creating the boot entry", before: was, now });
    }
    entries_labelled(after, label)
        .into_iter()
        .find(|(_, made)| made == guid)
        .map(|(id, _)| id)
        .ok_or_else(|| Refusal::BootEntry(after.trim().to_string()))
}

/// The firmware's boot order, out of `efibootmgr`'s `BootOrder:` line. Empty
/// where there is none, which is a machine whose order this loop did not move
/// either.
fn boot_order(listing: &str) -> String {
    listing
        .lines()
        .find_map(|line| line.trim().strip_prefix("BootOrder:"))
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Refused where `entry` is one of `order`'s, out of a listing's `BootOrder:`.
fn unordered(listing: &str, entry: &str) -> Result<(), Refusal> {
    let order = boot_order(listing);
    match order.split(',').any(|id| id.trim().eq_ignore_ascii_case(entry)) {
        true => Err(Refusal::Ordered { entry: entry.to_string(), order }),
        false => Ok(()),
    }
}

/// systemd's *effective* logind policy, out of `systemd-analyze cat-config`:
/// the last assignment of a key wins, and `Key = value` is legal spacing.
fn lid_policy(text: &str) -> Result<(), Refusal> {
    for key in LID_KEYS {
        let got = text
            .lines()
            .filter_map(|line| {
                let (name, value) = line.split_once('=')?;
                (name.trim() == *key).then(|| value.trim().to_string())
            })
            .next_back()
            .unwrap_or_default();
        if got != "ignore" {
            return Err(Refusal::Lid { key, got });
        }
    }
    Ok(())
}

/// The loop, over one target.
struct Driver {
    target: Target,
    home: Home,
    /// Print each write this run would make instead of making it; every read
    /// still happens, or the rehearsal would be of a different loop.
    dry_run: bool,
}

/// One command on `target`, as its account and never as root.
fn ssh(target: &Target, what: &str, command: &str) -> Result<String, Refusal> {
    let out = Command::new("ssh")
        .args(target.ssh_argv(command))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| unstarted(what, &e))?;
    answer(what, out).map(|out| String::from_utf8_lossy(&out.stdout).to_string())
}

impl Driver {
    /// The loop over `target`, once the machine has said where its account's
    /// home is.
    fn reach(target: Target, dry_run: bool) -> Result<Self, Refusal> {
        let home = Home::parse(&ssh(&target, "reading the account's home", Home::QUERY)?)?;
        Ok(Self { target, home, dry_run })
    }

    fn ssh(&self, what: &str, command: &str) -> Result<String, Refusal> {
        ssh(&self.target, what, command)
    }

    /// One root command, with `feed` streamed into it. Announced either way,
    /// and run only when this is not a dry run — which is what `None` means.
    fn as_root(&self, what: &str, job: Job, feed: Option<Feed<'_>>) -> Result<Option<Output>, Refusal> {
        let line = self.target.remote(&job, &self.home);
        println!("  {} {line}", if self.dry_run { "would run:" } else { "run:" });
        if self.dry_run {
            return Ok(None);
        }
        let mut command = Command::new("ssh");
        command.args(self.target.ssh_argv(&line)).stdout(Stdio::piped()).stderr(Stdio::piped());
        let Some(feed) = feed else {
            let out = command.stdin(Stdio::null()).output().map_err(|e| unstarted(what, &e))?;
            return answer(what, out).map(Some);
        };
        let unread = |e: std::io::Error| Refusal::File { path: feed.path.display().to_string(), why: e.to_string() };
        let mut source = std::fs::File::open(feed.path).map_err(unread)?;
        source.seek(std::io::SeekFrom::Start(feed.at)).map_err(unread)?;
        let mut child = command.stdin(Stdio::piped()).spawn().map_err(|e| unstarted(what, &e))?;
        let mut pipe = child.stdin.take().expect("the pipe was just asked for");
        // A copy that stops early is caught by the byte count `dd` reports,
        // which every caller compares; the child is waited on either way.
        let _ = std::io::copy(&mut source.take(feed.len), &mut pipe);
        drop(pipe);
        let out = child.wait_with_output().map_err(|e| unstarted(what, &e))?;
        answer(what, out).map(Some)
    }

    /// The loop refuses to run at all without passwordless sudo on the machine.
    fn require_sudo(&self) -> Result<(), Refusal> {
        let probe = self.target.remote(&Job::Probe, &self.home);
        self.ssh("sudo -n", &probe).map(|_| ()).map_err(|e| Refusal::Sudo(e.to_string()))
    }

    /// The machine as its SMBIOS names it, refused unless it is a recorded bench.
    fn recorded_machine(&self) -> Result<Machine, Refusal> {
        let machine = Machine::parse(&self.ssh("reading the machine's SMBIOS", Machine::QUERY)?)
            .map_err(Refusal::Machine)?;
        recorded(&timings::dir(), &machine)?;
        Ok(machine)
    }

    /// `dd`'s own count of what one [`Driver::as_root`] fed it, held to the
    /// feed's length: a short local read ends `ssh` and the remote `dd` exits 0
    /// on the prefix it got.
    fn fed(&self, what: &str, out: &Output, want: u64) -> Result<DdSaid, Refusal> {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let dd = dd_said(&stderr).ok_or_else(|| Refusal::Remote {
            what: what.to_string(),
            status: "printed no byte count and seconds".to_string(),
            stderr,
        })?;
        if dd.bytes != want {
            return Err(Refusal::Landed { what: format!("the byte count dd reported {what}"), want, got: dd.bytes });
        }
        Ok(dd)
    }

    /// **The kernel reads a disk's table only when asked to, so the loop asks**
    /// once it has written one. The ask udevd makes when a writer closes the disk
    /// is best effort and skipped while anything holds the disk's lock, as udevd's
    /// own probing of it does. udev is settled first, so no ask of its own is
    /// still to come; neither job opens the disk for writing, so neither starts
    /// another.
    fn reread(&self, sd: Sd) -> Result<(), Refusal> {
        self.as_root("settling udev", Job::Settle, None)?;
        self.as_root("re-reading the table", Job::Reread(sd), None)?;
        Ok(())
    }

    /// The writes that make `sd` a scratch disk, and its read-back; `false` is a
    /// dry run, which wrote nothing.
    ///
    /// **The new partition is wiped as well as the disk**: a wipe of the whole
    /// disk erases its tables and nothing inside a partition, and the new
    /// partition starts where the old first one did, so the old ESP's FAT would
    /// otherwise be read as the scratch partition's.
    fn make_scratch(&self, sd: Sd) -> Result<bool, Refusal> {
        self.as_root("wiping its signatures", Job::Wipe(sd), None)?;
        if self.as_root("writing the scratch table", Job::Scratch(sd), None)?.is_none() {
            return Ok(false);
        }
        self.reread(sd)?;
        self.as_root("wiping the new partition's signatures", Job::WipePartition { sd, part: 1 }, None)?;
        // The listing is udev's record, of the re-read and the wipe.
        self.as_root("settling udev", Job::Settle, None)?;
        scratch_read_back(&self.ssh("listing the disks", DISKS)?, sd)?;
        Ok(true)
    }

    /// The boot entry `create` makes under `label` for the ESP whose partition
    /// GUID is `esp`. An entry carrying the label but another partition's GUID
    /// names a partition that is gone, an install area made again, so it is
    /// deleted rather than reused.
    fn boot_entry(&self, label: &str, create: Job, esp: toyos_gpt::Guid) -> Result<String, Refusal> {
        let want = esp.to_string().to_ascii_lowercase();
        let ours = |listing: &str| {
            entries_labelled(listing, label).into_iter().partition::<Vec<_>, _>(|(_, guid)| *guid == want)
        };
        let listing = self.ssh("listing boot entries", EFIBOOTMGR)?;
        let (mine, stale) = ours(&listing);
        if let Some((id, _)) = mine.first() {
            return Ok(id.clone());
        }
        for (id, _) in stale {
            self.as_root("deleting a stale boot entry", Job::Delete(id), None)?;
        }
        // Read after those deletions, because a delete takes its entry out of
        // the order too: what the create is judged against is the state the
        // create acts on.
        let before = self.ssh("listing boot entries", EFIBOOTMGR)?;
        self.as_root("creating the boot entry", create, None)?;
        // Read back rather than parsed out of the create's own output: the
        // firmware's list is what the next command names, and one parser reads
        // it.
        let after = self.ssh("listing boot entries", EFIBOOTMGR)?;
        match entry_after_create(&before, &after, label, &want) {
            Ok(id) => Ok(id),
            // A dry run created nothing, so `0000` stands for the id the
            // firmware would have given it — the only value in the run that is
            // not the real run's.
            Err(Refusal::BootEntry(_)) if self.dry_run => Ok("0000".to_string()),
            Err(refusal) => {
                // Every entry this create added under the label, whatever
                // partition it names: one left behind is one the next run
                // inherits. A failure to clear it does not replace the refusal
                // being answered.
                let had: Vec<String> = ids(&before, label);
                for id in ids(&after, label) {
                    if had.contains(&id) {
                        continue;
                    }
                    let what = "deleting the entry this create made";
                    let _ = self.as_root(what, Job::Delete(id), None);
                }
                Err(refusal)
            }
        }
    }

    /// **`reboot` is `systemctl` and returns before the machine goes down**, so
    /// the machine is watched down before it is watched back up: a probe that
    /// caught dying Ubuntu would read a log ToyOS had never written. Answers
    /// the wall seconds going down took, and the whole seconds coming back did.
    fn ride_the_reboot(&self, secs: u64) -> Result<(f64, u64), Refusal> {
        let began = std::time::Instant::now();
        self.wait(GOING_DOWN_SECS, "go down", false)?;
        let down = began.elapsed().as_secs_f64();
        Ok((down, self.wait(secs, "come back", true)?))
    }

    /// [`wait_on`] the machine's `ssh`.
    fn wait(&self, secs: u64, what: &'static str, answering: bool) -> Result<u64, Refusal> {
        let host = self.target.host.as_str();
        wait_on(
            secs,
            what,
            answering,
            |within| port_accepts(host, SSH_PORT, within),
            || self.ssh("probing", "true").is_ok(),
        )
    }

    /// The loader's own file, and then everything `logkeeper` wrote, in name order:
    /// a freshly flashed volume holds one boot's files, and its newest file
    /// alone would miss the continuations `logkeeper` rotates into.
    ///
    /// Two strings and not one, because only the second is the boot's log and
    /// [`bootlog::verdict`] is about that.
    fn read_log(&self) -> Result<(String, String), Refusal> {
        self.mounted(Owned::Log, Self::read_mounted)
    }

    /// `read` over volume `o`, mounted read-only for it alone.
    fn mounted<T>(&self, o: Owned, read: impl FnOnce(&Self) -> Result<T, Refusal>) -> Result<T, Refusal> {
        self.ssh("making the mount point", &format!("mkdir -p {}", shell_word(&self.home.mount())))?;
        self.as_root(&format!("mounting the {o:?} volume"), Job::Mount(o), None)?;
        let read = read(self);
        // Unconditional, because a failure in the read would otherwise leave
        // the partition mounted and wedge the next run — and subordinate,
        // because the read's failure is the one worth answering with.
        let unmounted = self.as_root(&format!("unmounting the {o:?} volume"), Job::Umount, None);
        match (read, unmounted) {
            (Ok(read), Ok(_)) => Ok(read),
            (Ok(_), Err(umount)) => Err(umount),
            (Err(read), Ok(_)) => Err(read),
            (Err(read), Err(umount)) => Err(Refusal::Remote {
                what: format!("reading the {o:?} volume ({read}), and then unmounting it"),
                status: "both failed".to_string(),
                stderr: umount.to_string(),
            }),
        }
    }

    fn read_mounted(&self) -> Result<(String, String), Refusal> {
        let at = shell_word(&self.home.mount());
        let listing = self.ssh("listing the log", &format!("ls -1 {at}"))?;
        let (loader, logkeeper) = bootlog::split_listing(&listing);
        // Absence is an answer and not a failure: the boot is judged on what
        // `logkeeper` wrote either way.
        let loader = match loader {
            Some(name) => self.cat(name)?,
            None => format!("{}: the loader wrote none\n", bootlog::LOADER_LOG),
        };
        let mut text = String::new();
        for name in logkeeper {
            text.push_str(&self.cat(name)?);
        }
        Ok((loader, text))
    }

    /// The log volume the image wrote, `bytes` long, read whole as bytes.
    ///
    /// **The outside judge needs the volume and not a listing of it.** `read_log`
    /// mounts read-only and `cat`s, so everything it can say has already been
    /// filtered through a FAT driver — the same family of code that wrote the
    /// volume. This reads the sectors.
    fn raw_log(&self, bytes: u64) -> Result<Vec<u8>, Refusal> {
        let line = self.target.remote(&Job::ReadLog { bytes }, &self.home);
        println!("  run: {line}");
        let out = Command::new("ssh")
            .args(self.target.ssh_argv(&line))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| unstarted("reading the log partition", &e))?;
        let out = answer("reading the log partition", out)?;
        let got = out.stdout.len() as u64;
        if got != bytes {
            return Err(Refusal::Landed { what: "the log volume read back".to_string(), want: bytes, got });
        }
        Ok(out.stdout)
    }

    fn cat(&self, name: &str) -> Result<String, Refusal> {
        let file = shell_word(&format!("{}/{name}", self.home.mount()));
        self.ssh("reading a log file", &format!("cat {file}"))
    }

    /// The internal disk's table, as `lsblk` reads it without root.
    fn internal_disk(&self) -> Result<internal::Disk, Refusal> {
        let node = &self.target.internal;
        let text = self.ssh("reading the internal disk", &internal::query(node))?;
        internal::Disk::parse(&text, node).map_err(Refusal::Layout)
    }

    /// The internal disk's install area, which must be there.
    fn install_area(&self) -> Result<Vec<internal::Part>, Refusal> {
        match internal::recognise(&self.internal_disk()?).map_err(Refusal::Layout)? {
            internal::Area::Present(parts) => Ok(parts),
            internal::Area::Absent => Err(Refusal::Layout(
                "partitions 5 to 11 are not there, so nothing is installed".to_string(),
            )),
        }
    }

    /// Write `image` to the internal disk's install area, creating the area
    /// first where it is absent, and answer the boot entry for it — `None` on
    /// a dry run that stopped at the area it did not create.
    ///
    /// **Every write names an [`Owned`] partition or is the partition table**,
    /// and runs only once [`internal::recognise`] has said what the disk holds:
    /// the table is rewritten whole to give the area's partitions the GUIDs the
    /// image's contents name before their bytes are written, and the boot entry
    /// is made or refreshed last.
    fn install_internal(&self, image: &internal::Install) -> Result<Option<Installed>, Refusal> {
        let node = self.target.internal;
        if internal::recognise(&self.internal_disk()?).map_err(Refusal::Layout)? == internal::Area::Absent {
            self.make_area()?;
            if self.dry_run {
                println!(
                    "dry run: the install area was not made, so the lines after it were not written"
                );
                return Ok(None);
            }
        }
        let parts = self.install_area()?;
        if let Some(mounted) = parts.iter().find(|p| p.mounted) {
            return Err(Refusal::Layout(format!("{} is mounted", mounted.node)));
        }
        let own = |o: Owned| parts[Owned::ALL.iter().position(|x| *x == o).expect("listed")].clone();
        let kept = |o: Owned| own(o).unique.expect("recognise refuses a partition with no GUID");
        let named = image.guids(toyos_build::image::random_guid);
        let mut every: Vec<toyos_gpt::Guid> = vec![kept(Owned::Esp), kept(Owned::Slots)];
        every.extend(named.iter().map(|(_, guid)| *guid));
        let holders = self.ssh("listing every partition's GUID", internal::HOLDERS)?;
        let replaced: Vec<String> = Owned::ALL.iter().map(|o| o.node(&node)).collect();
        internal::collision(&holders, &every, |name| replaced.iter().any(|r| r == name))
            .map_err(Refusal::Collision)?;

        self.as_root("giving the partitions the image's GUIDs", Job::Retag(named.clone()), None)?;
        if !self.dry_run {
            self.as_root("settling udev", Job::Settle, None)?;
            let back = self.install_area()?;
            for (o, guid) in &named {
                let got = &back[Owned::ALL.iter().position(|x| x == o).expect("listed")];
                if got.unique != Some(*guid) {
                    return Err(Refusal::Layout(format!("{} does not read back as partition {guid}", got.node)));
                }
            }
        }
        let mut dd = 0.0;
        for o in Owned::ALL {
            let Some(piece) = image.piece(o) else { continue };
            let part = own(o);
            if piece.len > part.sectors * 512 {
                return Err(Refusal::Layout(format!(
                    "{} is {} sectors and the image's {o:?} is {} bytes",
                    part.node, part.sectors, piece.len
                )));
            }
            let what = format!("writing the image's {o:?} to {}", part.node);
            let feed = Feed { path: &image.path, at: piece.at, len: piece.len };
            if let Some(out) = self.as_root(&what, Job::Write(o), Some(feed))? {
                dd += self.fed(&what, &out, piece.len)?.secs;
            }
        }
        let create = Job::Create { label: INTERNAL_LABEL, loader: LOADER };
        let entry = self.boot_entry(INTERNAL_LABEL, create, kept(Owned::Esp))?;
        println!("installed {} on {}; Boot{entry} ({INTERNAL_LABEL}) boots it", image.path.display(), node.whole());
        Ok(Some(Installed { entry, dd }))
    }

    /// Make the install area, which the kernel's view says is absent.
    ///
    /// **An area a run made and the kernel never added is added first**: a
    /// `Job::Partition` that wrote the table, followed by a `Job::AddPartitions`
    /// that failed, leaves partitions 5–11 in the table and out of the kernel's
    /// view, where a second `Job::Partition` fails on numbers already taken.
    /// So the add is asked first, and refused harmlessly where the table holds
    /// no such range.
    fn make_area(&self) -> Result<(), Refusal> {
        let node = self.target.internal;
        if !self.dry_run && self.as_root("adding an area the table may already hold", Job::AddPartitions, None).is_ok() {
            self.as_root("settling udev", Job::Settle, None)?;
            if internal::recognise(&self.internal_disk()?).map_err(Refusal::Layout)? != internal::Area::Absent {
                println!("{}: the table already held the install area, and the kernel now does too", node.whole());
                return Ok(());
            }
        }
        println!("{}: no install area, so it is made after partition 4", node.whole());
        self.as_root("making the install partitions", Job::Partition, None)?;
        self.as_root("adding them to the kernel's view", Job::AddPartitions, None)?;
        if !self.dry_run {
            self.as_root("settling udev", Job::Settle, None)?;
        }
        Ok(())
    }

    /// The boot entry an earlier install made for the internal ESP.
    fn internal_entry(&self) -> Result<String, Refusal> {
        let esp = self.install_area()?[0].unique.expect("recognise refuses a partition with no GUID");
        let want = esp.to_string().to_ascii_lowercase();
        let listing = self.ssh("listing boot entries", EFIBOOTMGR)?;
        entries_labelled(&listing, INTERNAL_LABEL)
            .into_iter()
            .find(|(_, guid)| *guid == want)
            .map(|(id, _)| id)
            .ok_or_else(|| {
                Refusal::BootEntry(format!("no {INTERNAL_LABEL} entry names the internal ESP; install first"))
            })
    }
}

/// Poll once a [`POLL`] whether a port `accepts` a dial bounded by what it is
/// handed, and say how long the machine took to answer as asked. **Coming back
/// is `ssh` itself answering**: a port that accepts is only asked whether `ssh`
/// `answers`, since a listener can come up before the service behind it.
/// **Going down is a dial refused or unanswered for [`CONNECT_SECS`]**, what
/// `ssh`'s own connect is given: a SYN lost while Ubuntu is still up would
/// otherwise read as down, and the log be read before ToyOS booted it.
fn wait_on(
    secs: u64,
    what: &'static str,
    answering: bool,
    mut accepts: impl FnMut(std::time::Duration) -> bool,
    mut answers: impl FnMut() -> bool,
) -> Result<u64, Refusal> {
    let dial = if answering { POLL } else { std::time::Duration::from_secs(CONNECT_SECS) };
    let began = std::time::Instant::now();
    while began.elapsed().as_secs() < secs {
        let next = std::time::Instant::now() + POLL;
        let listening = accepts(dial);
        let answered = listening && (!answering || answers());
        if answered == answering {
            return Ok(began.elapsed().as_secs());
        }
        std::thread::sleep(next.saturating_duration_since(std::time::Instant::now()));
    }
    Err(Refusal::Silent { what, secs })
}

/// Whether `host`'s `port` accepts a connection `within`; a name that does not
/// resolve is a machine that is not there.
fn port_accepts(host: &str, port: u16, within: std::time::Duration) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| std::net::TcpStream::connect_timeout(&addr, within).is_ok())
}

fn unstarted(what: &str, e: &std::io::Error) -> Refusal {
    Refusal::Remote {
        what: what.to_string(),
        status: format!("could not be started: {e}"),
        stderr: String::new(),
    }
}

fn answer(what: &str, out: Output) -> Result<Output, Refusal> {
    if out.status.success() {
        return Ok(out);
    }
    Err(Refusal::Remote {
        what: what.to_string(),
        status: format!("exited {}", out.status),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

declare_flags!(METAL = {
    IMAGE = "--image", Next;
    HOST = "--host", Next;
    KEY = "--key", Next;
    ADOPT_SCRATCH = "--adopt-scratch", Next;
    RECORD = "--record", None;
    FALL_THROUGH = "--fall-through", Next;
    READBACK = "--readback", Next;
    FAT32_CHECK = "--fat32-check", None;
    DRY_RUN = "--dry-run", None;
    INSTALL_INTERNAL = "--install-internal", Next;
    BOOT_INTERNAL = "--boot-internal", None;
});

/// The flags that describe a judged boot, which [`Args::parse`] refuses beside
/// an install or boot that writes no readback.
const ABOUT_THE_JUDGED_BOOT: &[&Flag] = &[&IMAGE, &READBACK, &FAT32_CHECK];

/// What the binary was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// **No default, and that is the second half of the fix.** A default was
    /// something to fall through *to*: a capture recorded the machine and
    /// carried on into the ordinary loop with `target/bootable.img`, which
    /// in one worktree was a 184 MB image with no job list and no bound. There
    /// is nothing to fall through to now, and the gate below would refuse that
    /// image anyway — the loop flashes metal-staged images and nothing else.
    image: Option<PathBuf>,
    target: Target,
    dry_run: bool,
    /// Record the machine ([`record`]). **An action of its own, and it writes
    /// no disk.**
    record: bool,
    /// Where the log's two files and this boot's own facts are written, for a
    /// judge that is not this process. Absent leaves the run's only account its
    /// standard output, which no per-test predicate can be held to.
    readback: Option<PathBuf>,
    /// Read the log volume whole off the disk and hand it to
    /// `toyos-fat32-check`. The outside judge, and the only reader of that
    /// volume in this tree that is not the family of code that wrote it.
    fat32_check: bool,
    /// The image an install writes to the bench's own NVMe with no readback,
    /// such as a shipped image for a demo. An action of its own: it boots
    /// nothing unless [`Args::boot_internal`] asks.
    install_internal: Option<PathBuf>,
    /// Boot the internal install once, through `BootNext`, and leave the
    /// firmware's order — Ubuntu first — as it is.
    boot_internal: bool,
    /// The disk [`adopt_scratch`] erases into a scratch disk. An action of its
    /// own, beside nothing but the flags that say which machine and `--dry-run`.
    adopt_scratch: Option<Sd>,
    /// The unbootable entry [`fall_through`] boots once. An action of its own.
    fall_through: Option<Unbootable>,
}

impl Args {
    pub fn parse(args: &[String]) -> Result<Self, Refusal> {
        let line = METAL.walk(args);
        if let Some(word) = line.unknown.or_else(|| line.positionals.first().copied()) {
            return Err(Refusal::Usage(format!(
                "unknown argument {word:?}. toyos-metal accepts:\n{}",
                METAL.usage()
            )));
        }
        if let Some(why) = line.malformed() {
            return Err(Refusal::Usage(why));
        }
        let value = |flag: &Flag| METAL.value(args, flag);
        let mut out = Args {
            image: value(&IMAGE).map(PathBuf::from),
            target: Target::bench_t14()?,
            dry_run: METAL.present(args, &DRY_RUN),
            record: METAL.present(args, &RECORD),
            readback: value(&READBACK).map(PathBuf::from),
            fat32_check: METAL.present(args, &FAT32_CHECK),
            install_internal: value(&INSTALL_INTERNAL).map(PathBuf::from),
            boot_internal: METAL.present(args, &BOOT_INTERNAL),
            adopt_scratch: value(&ADOPT_SCRATCH).map(Sd::parse).transpose()?,
            fall_through: value(&FALL_THROUGH).map(Unbootable::parse).transpose()?,
        };
        if let Some(host) = value(&HOST) {
            let (user, machine) = host_of(host)?;
            out.target.user = user.to_string();
            out.target.host = machine.to_string();
        }
        if let Some(key) = value(&KEY) {
            out.target.key = PathBuf::from(key);
        }
        // **An action is not a mode**: it does its one thing and exits, so a
        // flag beside it that describes anything else is refused rather than
        // left for the action to fall through into — which is how a capture
        // once flashed whatever `--image` defaulted to.
        let actions: [(&Flag, &[&Flag]); 3] = [
            (&RECORD, &[&HOST, &KEY]),
            (&ADOPT_SCRATCH, &[&HOST, &KEY, &DRY_RUN]),
            (&FALL_THROUGH, &[&HOST, &KEY, &DRY_RUN]),
        ];
        for (action, alone) in actions {
            if !METAL.present(args, action) {
                continue;
            }
            let beside: Vec<&str> = line
                .seen
                .iter()
                .map(|seen| seen.flag.name)
                .filter(|name| *name != action.name && !alone.iter().any(|flag| flag.name == *name))
                .collect();
            if !beside.is_empty() {
                return Err(Refusal::Usage(format!(
                    "{} does its one thing and exits, so {} is not done beside it",
                    action.name,
                    beside.join(" and ")
                )));
            }
        }
        let judged: Vec<&str> = line
            .seen
            .iter()
            .map(|seen| seen.flag.name)
            .filter(|name| ABOUT_THE_JUDGED_BOOT.iter().any(|flag| flag.name == *name))
            .collect();
        if (out.install_internal.is_some() || out.boot_internal) && !judged.is_empty() {
            return Err(Refusal::Usage(format!(
                "--install-internal and --boot-internal write and boot an image nobody judges, \
                 so {} describes a boot they will not make",
                judged.join(" and ")
            )));
        }
        Ok(out)
    }
}

/// `--host`'s `<user>@<machine>`, split: two non-empty words, neither opening
/// with `-`, which `ssh` takes for a flag.
pub fn host_of(host: &str) -> Result<(&str, &str), Refusal> {
    let usage = || Refusal::Usage(format!("--host wants <user>@<machine>, not {host:?}"));
    let (user, machine) = host.split_once('@').ok_or_else(usage)?;
    let unfit = |half: &str| half.is_empty() || half.starts_with('-') || half.contains(|c: char| c.is_whitespace() || c == '@');
    if unfit(user) || unfit(machine) {
        return Err(usage());
    }
    Ok((user, machine))
}

/// Record the machine `target` reaches where it has no record, by what its
/// SMBIOS answers. It asks nothing: a bench holds nothing a wrong capture could
/// lose, and the loop reaches no other machine.
fn record(target: &Target) -> Result<(), Refusal> {
    let driver = Driver::reach(target.clone(), false)?;
    let machine = Machine::parse(&driver.ssh("reading the machine's SMBIOS", Machine::QUERY)?)
        .map_err(Refusal::Machine)?;
    let (at, wrote) = record_machine(&timings::dir(), &machine)?;
    let how = if wrote { "records this machine now" } else { "already recorded this machine" };
    println!("{} {how}: {} {}, BIOS {}", at.display(), machine.vendor, machine.product, machine.bios);
    Ok(())
}

/// `sd`'s partitions in `listing`, refused where the machine lists no `sd`.
fn parts_of(listing: &str, sd: Sd) -> Result<Vec<Listed>, Refusal> {
    sd_disks(listing)?
        .into_iter()
        .find(|(listed, _)| *listed == sd)
        .map(|(_, parts)| parts)
        .ok_or_else(|| Refusal::Adopt(format!("{} is no disk the machine lists", sd.node().whole())))
}

/// Whether `sd` of `listing` may be erased to make it a scratch disk: listed,
/// and holding no ToyOS image, which a disk somebody flashed may still be.
fn adoptable(listing: &str, sd: Sd, image: Carrying) -> Result<(), Refusal> {
    if image.holds(&parts_of(listing, sd)?) {
        return Err(Refusal::Adopt(format!(
            "{} carries {image}, and a scratch disk is never one",
            sd.node().whole()
        )));
    }
    Ok(())
}

/// Whether `sd` of `listing` reads back as [`make_scratch`](Driver::make_scratch)
/// leaves it: a scratch disk whose one partition carries no filesystem.
fn scratch_read_back(listing: &str, sd: Sd) -> Result<(), Refusal> {
    let parts = parts_of(listing, sd)?;
    let disk = sd.node().whole();
    if !Carrying::Scratch.holds(&parts) {
        return Err(Refusal::Adopt(format!("{disk} was written and does not read back as a scratch disk")));
    }
    if let Some(fs) = parts.iter().find_map(|p| p.filesystem.as_deref()) {
        return Err(Refusal::Adopt(format!("{disk}'s scratch partition still carries a {fs} signature")));
    }
    Ok(())
}

/// Make `sd`, named by hand, a scratch disk of a recorded machine: its
/// signatures wiped and a table of one [`Carrying::Scratch`] partition
/// written, which the disk is then read back as carrying. The disk's contents
/// are lost.
fn adopt_scratch(target: &Target, dry_run: bool, sd: Sd) -> Result<(), Refusal> {
    let driver = Driver::reach(target.clone(), dry_run)?;
    driver.require_sudo()?;
    driver.recorded_machine()?;
    adoptable(&driver.ssh("listing the disks", DISKS)?, sd, Carrying::Layout)?;
    let disk = Identity::parse(&driver.ssh("reading the disk", &sd.identity())?)?;
    if !disk.removable {
        return Err(Refusal::Identity { field: "removable", want: "1".to_string(), got: "0".to_string() });
    }
    if !driver.make_scratch(sd)? {
        println!("dry run: nothing was erased");
        return Ok(());
    }
    println!(
        "{} is a scratch disk: {} sectors, {} {}",
        sd.node().whole(),
        disk.sectors,
        disk.vendor,
        disk.model
    );
    Ok(())
}

/// Bytes of a local file one root command reads on its stdin.
#[derive(Debug, Clone, Copy)]
struct Feed<'a> {
    path: &'a Path,
    at: u64,
    len: u64,
}

/// The image an internal install writes, admitted: its partitions as
/// [`internal::admit`] matched them, and armed with nothing the owner's own
/// flash path would refuse — a demo boots what ships, and no actuator.
fn admit_install(path: &Path) -> Result<internal::Install, Refusal> {
    let install = internal::admit(path).map_err(Refusal::Install)?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let declared = toyos_build::build::declared_params(&root);
    let armed = toyos_build::image::params_of(path).map_err(Refusal::Install)?;
    if let Some(name) = armed.iter().find(|n| !declared.contains(n) && !toyos_build::build::is_valued_param(n)) {
        return Err(Refusal::Install(format!(
            "it is armed with {name:?}, which is no boot parameter the kernel declares, and an \
             installed image carries no actuator"
        )));
    }
    Ok(install)
}

/// An internal install, an internal boot, or both in that order.
fn internal_action(args: &Args) -> Result<(), Refusal> {
    let image = args.install_internal.as_deref().map(admit_install).transpose()?;
    let driver = Driver::reach(args.target.clone(), args.dry_run)?;
    driver.require_sudo()?;
    let machine = driver.recorded_machine()?;
    println!("machine {} {}, BIOS {}", machine.vendor, machine.product, machine.bios);
    let entry = match &image {
        Some(image) => match driver.install_internal(image)? {
            Some(installed) => installed.entry,
            None => return Ok(()),
        },
        None => driver.internal_entry()?,
    };
    if args.boot_internal {
        driver.as_root("setting bootnext", Job::BootNext(entry.clone()), None)?;
        driver.as_root("rebooting", Job::Reboot, None)?;
        println!("Boot{entry} ({INTERNAL_LABEL}) is the next boot, once; the firmware's order is unchanged");
    }
    Ok(())
}

/// The whole loop, answering this boot's own millisecond count. `None` is a
/// dry run, which reaches no boot record because it wrote nothing, and every
/// action that is not a judged boot.
pub fn run(args: &Args) -> Result<Option<u64>, Refusal> {
    if args.record {
        record(&args.target)?;
        return Ok(None);
    }
    if args.install_internal.is_some() || args.boot_internal {
        internal_action(args)?;
        return Ok(None);
    }
    if let Some(sd) = args.adopt_scratch {
        adopt_scratch(&args.target, args.dry_run, sd)?;
        return Ok(None);
    }
    if let Some(how) = args.fall_through {
        fall_through(&args.target, args.dry_run, how)?;
        return Ok(None);
    }
    let began = std::time::Instant::now();

    let Some(asked) = &args.image else {
        return Err(Refusal::Usage(String::from(
            "--image names the image to flash, and there is no default: the loop flashes \
             what the metal profile staged, and a default was something an action that \
             boots nothing could fall through to",
        )));
    };
    let image = internal::admit(asked).map_err(Refusal::Install)?;
    let log = image.piece(Owned::Log).expect("an admitted image carries a log");
    // **Before the flash, and before anything can refuse.** Every refusal below
    // returns without reaching `write_readback`, so a directory left holding the
    // last run's files is one a judge reads as this run's — which is how a boot
    // that never happened came back green.
    if let Some(dir) = &args.readback {
        clear_readback(dir)?;
    }
    let pieces: Vec<String> = Owned::ALL
        .iter()
        .filter_map(|o| image.piece(*o).map(|p| format!("{o:?} {} bytes", p.len)))
        .collect();
    println!("image {}: {}", image.path.display(), pieces.join(", "));
    // The pre-flash gate, before the disk is even asked what it is: what this
    // image will arm, judged against the only table that has ruled on any of it.
    let (armed, deadline_ms) = arms_are_admissible(asked)?;
    // Off the image and not off this build's constants: the bound is the
    // artifact's, and the wait for it is as long as that boot can last.
    let wait_secs = return_secs(deadline_ms);
    println!(
        "image {}: armed with {armed:?}, so it is waited {wait_secs} s to come back",
        image.path.display()
    );

    let driver = Driver::reach(args.target.clone(), args.dry_run)?;
    driver.require_sudo()?;
    let policy =
        driver.ssh("reading the lid policy", "systemd-analyze cat-config systemd/logind.conf")?;
    lid_policy(&policy)?;
    let machine = driver.recorded_machine()?;
    println!("machine {} {}, BIOS {}", machine.vendor, machine.product, machine.bios);
    let preamble = began.elapsed().as_secs_f64();

    let began = std::time::Instant::now();
    let Some(installed) = driver.install_internal(&image)? else { return Ok(None) };
    let flash = began.elapsed().as_secs_f64();
    let began = std::time::Instant::now();
    let before = driver.ssh("listing boot entries", EFIBOOTMGR)?;
    unordered(&before, &installed.entry)?;
    driver.as_root("setting bootnext", Job::BootNext(installed.entry), None)?;
    driver.as_root("rebooting", Job::Reboot, None)?;
    let entry = began.elapsed().as_secs_f64();
    if driver.dry_run {
        driver.as_root("mounting the log partition", Job::Mount(Owned::Log), None)?;
        driver.as_root("unmounting the log partition", Job::Umount, None)?;
        println!("dry run: nothing was written and the machine was not rebooted");
        return Ok(None);
    }

    let (down, back) = driver.ride_the_reboot(wait_secs)?;
    println!("the machine answered ssh again after {back} s");
    let began = std::time::Instant::now();
    let (loader, log_text) = driver.read_log()?;
    let read_log = began.elapsed().as_secs_f64();
    print!("{loader}{log_text}");
    // The outside judge, and it runs before the verdict: a volume this
    // cannot read is a finding about what the boot wrote, and the reason to
    // read the sectors rather than the mount is that a mount has already
    // had a FAT driver's opinion about them.
    let began = std::time::Instant::now();
    if args.fat32_check {
        let bytes = driver.raw_log(log.len)?;
        if let Some(dir) = &args.readback {
            let at = dir.join(READBACK_VOLUME);
            std::fs::write(&at, &bytes).map_err(|e| Refusal::File {
                path: at.display().to_string(),
                why: e.to_string(),
            })?;
        }
        let complaints = toyos_fat32_check::check(&bytes);
        fat32_verdict(&armed, &complaints)?;
        println!("toyos-fat32-check: the log partition's {} bytes check out", bytes.len());
    }
    let raw_log = args.fat32_check.then(|| began.elapsed().as_secs_f64());
    let phases = Phases { preamble, flash, dd_secs: installed.dd, entry, down, read_log, raw_log };
    println!("phases: {phases}");
    let boot = BootFile { back, machine, phases }.text();
    let verdict = judge_and_write_readback(&armed, &loader, &log_text, args.readback.as_deref(), &boot);
    // After the readback, so a boot that moved the order is still judged; the
    // order's refusal is the louder answer, since the next failure loops.
    order_kept(&before, &driver.ssh("listing boot entries", EFIBOOTMGR)?)?;
    verdict.map(Some)
}

/// Refused where the boot between `before` and `after` moved `BootOrder`.
fn order_kept(before: &str, after: &str) -> Result<(), Refusal> {
    let (was, now) = (boot_order(before), boot_order(after));
    match was == now {
        true => Ok(()),
        false => Err(Refusal::BootOrder { during: "the boot", before: was, now }),
    }
}

/// An entry on the internal ESP that no firmware can boot, as
/// [`fall_through`] names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unbootable {
    /// A loader path no file is at.
    Missing,
    /// A file that is no PE image: the image's own `\toyos\log.guid`.
    Garbage,
}

impl Unbootable {
    fn parse(word: &str) -> Result<Self, Refusal> {
        match word {
            "missing" => Ok(Self::Missing),
            "garbage" => Ok(Self::Garbage),
            _ => Err(Refusal::Usage(format!("--fall-through takes missing or garbage, not {word:?}"))),
        }
    }

    /// The entry's loader path, as firmware reads it.
    fn loader(self) -> &'static str {
        match self {
            Self::Missing => r"\EFI\TOYOS\ABSENT.EFI",
            Self::Garbage => r"\toyos\log.guid",
        }
    }

    /// The same file under the ESP's mount, and the size it has to have: none
    /// for the missing one.
    fn file(self) -> (&'static str, Option<u64>) {
        match self {
            Self::Missing => ("EFI/TOYOS/ABSENT.EFI", None),
            Self::Garbage => ("toyos/log.guid", Some(16)),
        }
    }
}

/// **Boot an entry the firmware cannot load, once, and see Ubuntu come back.**
/// UEFI 2.10 §3.1.2 has the boot manager try `BootNext` and then the
/// `BootOrder` list; a firmware that prompts instead is a bench a failed
/// install leaves waiting for a hand. The entry names a loader on the internal
/// ESP that `how` says no firmware can boot, the ESP is first read to hold
/// exactly that, and the entry is deleted once the machine is back.
fn fall_through(target: &Target, dry_run: bool, how: Unbootable) -> Result<(), Refusal> {
    let driver = Driver::reach(target.clone(), dry_run)?;
    driver.require_sudo()?;
    lid_policy(&driver.ssh("reading the lid policy", "systemd-analyze cat-config systemd/logind.conf")?)?;
    let machine = driver.recorded_machine()?;
    println!("machine {} {}, BIOS {}", machine.vendor, machine.product, machine.bios);
    let esp = driver.install_area()?[0].unique.expect("recognise refuses a partition with no GUID");
    let (file, size) = how.file();
    let at = shell_word(&format!("{}/{file}", driver.home.mount()));
    let said = driver.mounted(Owned::Esp, |d| d.ssh("reading the loader's file", &format!("stat -c %s {at} 2>/dev/null || echo none")))?;
    let found = said.trim().parse::<u64>().ok();
    if !dry_run && found != size {
        return Err(Refusal::Layout(format!(
            "the internal ESP's {file} is {} where this entry needs it {}",
            found.map_or_else(|| "absent".to_string(), |n| format!("{n} bytes")),
            size.map_or_else(|| "absent".to_string(), |n| format!("{n} bytes"))
        )));
    }
    // Every one, since a run that refused after its create left one whose
    // loader may be the other variant's.
    for id in ids(&driver.ssh("listing boot entries", EFIBOOTMGR)?, UNBOOTABLE_LABEL) {
        driver.as_root("deleting an earlier unbootable entry", Job::Delete(id), None)?;
    }
    let create = Job::Create { label: UNBOOTABLE_LABEL, loader: how.loader() };
    let entry = driver.boot_entry(UNBOOTABLE_LABEL, create, esp)?;
    let before = driver.ssh("listing boot entries", EFIBOOTMGR)?;
    unordered(&before, &entry)?;
    driver.as_root("setting bootnext", Job::BootNext(entry.clone()), None)?;
    driver.as_root("rebooting", Job::Reboot, None)?;
    if dry_run {
        println!("dry run: nothing was created and the machine was not rebooted");
        return Ok(());
    }
    let (_, back) = driver.ride_the_reboot(RETURN_ALLOWANCE_SECS)?;
    let listing = driver.ssh("listing boot entries", EFIBOOTMGR)?;
    let deleted = driver.as_root("deleting the unbootable entry", Job::Delete(entry.clone()), None);
    let current = listing.lines().find_map(|l| l.strip_prefix("BootCurrent:")).map(str::trim);
    if current == Some(entry.as_str()) {
        return Err(Refusal::FellThrough(format!("BootCurrent is Boot{entry}, the unbootable entry")));
    }
    if let Some(next) = listing.lines().find_map(|l| l.strip_prefix("BootNext:")) {
        return Err(Refusal::FellThrough(format!("BootNext still reads {}", next.trim())));
    }
    let first = boot_order(&listing).split(',').next().unwrap_or_default().trim().to_string();
    if current != Some(first.as_str()) {
        return Err(Refusal::FellThrough(format!("BootCurrent is {current:?} and BootOrder's first entry {first:?}")));
    }
    order_kept(&before, &listing)?;
    deleted?;
    println!(
        "the firmware fell through past Boot{entry} ({}) to Boot{} and answered ssh after {back} s",
        how.loader(),
        current.unwrap_or("????")
    );
    Ok(())
}

/// The outside judge's verdict on the log partition. A boot that hands the
/// machine back owes a volume with nothing to say about it. One its own
/// deadline ends ([`ended_by_its_deadline`]) stopped wherever logkeeper's
/// append was, and owes only what such a stop can leave
/// ([`toyos_fat32_check::unexplained_by_a_stop`]), each of which is printed.
fn fat32_verdict(armed: &[String], complaints: &[toyos_fat32_check::Complaint]) -> Result<(), Refusal> {
    let refused: Vec<toyos_fat32_check::Complaint> = if ended_by_its_deadline(armed) {
        let unexplained = toyos_fat32_check::unexplained_by_a_stop(complaints);
        for left in complaints.iter().filter(|c| !unexplained.iter().any(|u| std::ptr::eq(*u, *c))) {
            println!("toyos-fat32-check: what the reset's stop left: {}", toyos_fat32_check::describe(std::slice::from_ref(left)));
        }
        unexplained.into_iter().cloned().collect()
    } else {
        complaints.to_vec()
    };
    if refused.is_empty() {
        Ok(())
    } else {
        Err(Refusal::Fat32(toyos_fat32_check::describe(&refused)))
    }
}

/// What [`Driver::install_internal`] made.
struct Installed {
    /// The boot entry that boots it.
    entry: String,
    /// `dd`'s own seconds for every piece it wrote.
    dd: f64,
}

/// This boot's verdict, **judged before the readback is written, and written
/// into it**, so a judge reading the directory later reads the verdict this
/// run returns.
fn judge_and_write_readback(
    armed: &[String],
    loader: &str,
    log: &str,
    readback: Option<&Path>,
    boot: &str,
) -> Result<u64, Refusal> {
    let verdict = boot_verdict(armed, loader, log);
    if let Some(dir) = readback {
        write_readback(dir, loader, log, boot, &verdict_file(verdict.as_ref().err()))?;
        println!("readback written to {}", dir.display());
    }
    verdict
}

/// The log's own verdict on one boot, off what its image is armed with, the
/// loader's file and the log.
fn boot_verdict(armed: &[String], loader: &str, log: &str) -> Result<u64, Refusal> {
    let staged_hang = clears_its_own_page(armed);
    // **Named by evidence, before the boot record is missed.** A boot that
    // never happened and a boot that failed both leave no `Boot: complete`,
    // and `Unfit::NoBootRecord` says the second where it is often the first.
    // The loader's own file is what tells them apart: a pass that read the
    // black box says so and ends the chain, and this is the only way a
    // machine can come back with a whole `loader.log` and an empty kernel one.
    // **First, because it is the one refusal that is not a machine needing a
    // hand.** The loader bounded a hang and gave the machine back; every
    // other reading of an empty kernel log would send a reader to the wrong
    // place, and `NoBootRecord` would send them to a kernel that never ran.
    if loader.contains(bootlog::HUNG_WITHOUT_A_RECORD) && !staged_hang {
        return Err(Refusal::HungWithoutARecord);
    }
    if let Some(said) = reported_and_booted_nothing(loader, log) {
        return Err(Refusal::ReportedAndBootedNothing { said });
    }
    // **An image armed to stop itself is judged by the record its own bound
    // sealed, not by the word a shutdown writes.** Read off what the image
    // is armed with rather than off its label or its readback directory: the
    // arm comes out of the artifact, so no boot can be judged as something
    // it was not flashed as. *Which* bound sealed it is the page's to say
    // and not this list's — the arm says a bound was staged, and two of them
    // can reach a staged boot.
    if ended_by_its_deadline(armed) {
        return wedged_boot(watched(armed), loader, log);
    }
    let ms = bootlog::verdict(log).map_err(Refusal::Log)?;
    if staged_hang {
        bootlog::foreign_done(loader).map_err(Refusal::Log)?;
    } else {
        bootlog::handed_back(loader).map_err(Refusal::Log)?;
    }
    Ok(ms)
}

/// What an image armed to stop itself owes instead of `Rebooting.`, and the
/// `Boot: complete` it still owes as well.
///
/// **Both halves, because either alone passes for the wrong reason.** A boot
/// that reached `Rebooting.` was never wedged, and a boot with no sealed record
/// is one somebody's hand ended — which is the whole thing these arms exist to
/// prove cannot happen any more.
///
/// **The record's own first line is what says which bound ended the machine**,
/// and nothing else may: an arm staged for one of them is still a machine the
/// other can reach first, and the T14 read back a `hard-lockup-probe` boot as a
/// failure for exactly that reason — this judge knew one line and the page
/// carried the other. Under the chipset watchdog, its reset is the third
/// ending: the pass after it finds a page no kernel sealed, and the chipset
/// says its watchdog reset the machine. The wedge stops every feed, and the
/// deadline stands past the watchdog's bound.
fn wedged_boot(watched: bool, loader: &str, log: &str) -> Result<u64, Refusal> {
    if bootlog::handed_back(loader).is_ok() {
        return Err(Refusal::Wedge {
            why: "it reached the shutdown's own last word, so nothing about it was wedged",
        });
    }
    let Some(said) = sealed_by(loader).or_else(|| watched.then(|| reset_by_the_chipset(loader)).flatten()) else {
        return Err(Refusal::Wedge {
            why: "the pass after the reset reports neither of this kernel's bounds, nor a reset \
                  by the chipset watchdog it may be armed with, so what ended this boot was not \
                  one of them",
        });
    };
    let ms = bootlog::boot_millis(log).ok_or(Refusal::Wedge {
        why: "the kernel wrote no `Boot: complete`, so this boot stopped before the phase these \
              bounds are armed for",
    })?;
    println!("{}", said.trim_start_matches("| ").trim());
    Ok(ms)
}

/// The line the pass after the reset printed about whichever bound sealed the
/// page, or `None` where it printed neither.
fn sealed_by(loader: &str) -> Option<&str> {
    loader
        .lines()
        .find(|l| l.contains(bootlog::DEADLINE_EXPIRED) || l.contains(bootlog::LOCKED_UP))
}

/// The line the pass after the reset printed where the chipset says its
/// watchdog caused that reset, above a page no kernel sealed: a panicked
/// kernel the watchdog then reset is a panic. Only an FCH says so.
fn reset_by_the_chipset(loader: &str) -> Option<&str> {
    if !bootlog::sealed_nothing(loader) {
        return None;
    }
    let after = bootlog::after_the_reset(loader)?;
    after.lines().find(|l| l.contains(toyos_tco::fch::RESET_SAID))
}

/// The lateness of a deadline that expired, in milliseconds past the bound it
/// was armed with, out of the line the pass after the reset printed; `None` on
/// a boot whose deadline did not expire.
///
/// **Counted from the kernel's own record of arming it** (`log`): the bound
/// runs from the arm and the expiry is quoted since boot, so the difference
/// alone is the boot's first tens of milliseconds and not the poll's lateness.
/// Signed, because both instants are floored milliseconds: an expiry at its
/// bound can read one early, and one before it is a reading, not an absence.
pub fn deadline_lateness_ms(loader: &str, log: &str) -> Option<Result<i64, String>> {
    let said = loader.lines().find(|l| l.contains(bootlog::DEADLINE_EXPIRED))?;
    let fields = || {
        let (_, rest) = said.split_once("a bound of ")?;
        let (bound, rest) = rest.split_once(" ms, reached at ")?;
        let (reached, _) = rest.split_once(" ms,")?;
        Some((bound.parse::<i64>().ok()?, reached.parse::<i64>().ok()?))
    };
    let armed = || {
        let line = log.lines().find(|line| line.contains(bootlog::DEADLINE_ARMED))?;
        i64::try_from(bootlog::record_millis(line)?).ok()
    };
    Some(match (fields(), armed()) {
        (Some((bound, reached)), Some(armed)) => Ok(reached - armed - bound),
        (None, _) => Err(format!("{said:?} is not the expiry the kernel writes")),
        (_, None) => Err(format!(
            "the deadline expired and this boot's log carries no {:?} record to count its bound \
             from",
            bootlog::DEADLINE_ARMED
        )),
    })
}

/// What the kernel's stop wrote about itself in `text`: the loader's pass after
/// the reset, which prints the tail the stop sealed.
///
/// `None` on a boot that wrote none, which is every boot that reset without
/// going through `quiesce`.
pub fn park(text: &str) -> Option<toyos_quiesce::Record> {
    text.lines().find_map(toyos_quiesce::Record::parse)
}

/// The same for the other bound: how far past its own bound the hard-lockup
/// sample was when it found the CPU stuck.
///
/// **A number of its own and not [`deadline_lateness_ms`]'s**, because what
/// bounds it is a different thing: this one lands within one of that detector's
/// sample periods rather than within one timer period.
pub fn lockup_lateness_ms(loader: &str) -> Option<Result<i64, String>> {
    let said = loader.lines().find(|l| l.contains(bootlog::LOCKED_UP))?;
    let fields = || {
        let (_, rest) = said.split_once("has taken no interrupt for ")?;
        let (reached, rest) = rest.split_once(" ms,")?;
        let (_, rest) = rest.split_once("Its bound is ")?;
        let (bound, _) = rest.split_once(" ms")?;
        Some(reached.parse::<i64>().ok()? - bound.parse::<i64>().ok()?)
    };
    Some(fields().ok_or_else(|| format!("{said:?} is not the lockup record the kernel writes")))
}

/// What the two files and this boot's own facts are called under `--readback`.
///
/// **Written here rather than parsed back out of this program's output**: a
/// judge that split one stream into a loader half and a kernel half would be
/// guessing where one ended, and the guess is wrong on the boot that says
/// something unusual — which is every boot a per-test predicate is for.
pub const READBACK_LOADER: &str = "loader.log";
pub const READBACK_KERNEL: &str = "kernel.log";
pub const READBACK_BOOT: &str = "boot.txt";

/// The loop's own verdict on the boot: [`PASSED`], or [`REFUSED`] and the
/// refusal on the lines after it. Written last, so a directory holding it holds
/// the whole readback, and read by every judge of it, so a run judging these
/// files later rules on the boot as the run that wrote them did.
pub const READBACK_VERDICT: &str = "verdict.txt";
const PASSED: &str = "passed";
const REFUSED: &str = "refused";

/// [`READBACK_VERDICT`]'s text.
pub fn verdict_file(refused: Option<&Refusal>) -> String {
    match refused {
        None => format!("{PASSED}\n"),
        Some(why) => format!("{REFUSED}\n{why}\n"),
    }
}

/// The loop's verdict a readback carries: `Err` with the refusal's own words
/// for a boot it refused, or with why the file says neither.
pub fn loop_verdict(text: &str) -> Result<(), String> {
    match text.split_once('\n') {
        Some((PASSED, "")) => Ok(()),
        Some((REFUSED, why)) if !why.trim().is_empty() => Err(why.trim_end().to_string()),
        _ => Err(format!(
            "{READBACK_VERDICT} reads {text:?}, which is neither {PASSED:?} nor {REFUSED:?} and a \
             reason"
        )),
    }
}

/// The log partition's own bytes, kept beside them under `--fat32-check`: what
/// the outside judge read, so a complaint can be looked at rather than retold.
pub const READBACK_VOLUME: &str = "log-partition.img";

/// The keys [`READBACK_BOOT`] carries, one `<key> <value>` per line.
pub const BACK_SECS: &str = "back_secs";

/// The machine the boot ran on, as [`Machine::QUERY`] answered before the flash.
pub const VENDOR_KEY: &str = "machine_vendor";
pub const PRODUCT_KEY: &str = "machine_product";
pub const BIOS_KEY: &str = "machine_bios";

/// [`Phases`]' keys, each in wall seconds; [`RAW_LOG_KEY`] reads [`NOT_READ`]
/// on a run that read no volume.
const PREAMBLE_KEY: &str = "preamble_secs";
const FLASH_KEY: &str = "flash_secs";
const DD_SECS_KEY: &str = "dd_secs";
const ENTRY_KEY: &str = "entry_secs";
const DOWN_KEY: &str = "down_secs";
const READ_LOG_KEY: &str = "read_log_secs";
const RAW_LOG_KEY: &str = "raw_log_secs";
const NOT_READ: &str = "-";

/// Where one run of the loop spent its wall time, by the host's clock, phase
/// by phase; coming back is [`BACK_SECS`]. Any save of the log partition
/// before the flash is no phase of this loop's.
#[derive(Debug, Clone, PartialEq)]
pub struct Phases {
    /// Admitting the image, then the account's home, sudo, the lid policy and
    /// the machine read off the machine.
    pub preamble: f64,
    /// The install: the disk read and checked, the GUIDs given and read back,
    /// each piece's `dd` over `ssh`, and the boot entry.
    pub flash: f64,
    /// What `dd` itself said of the writes. It reads `ssh`'s stdin, so this is
    /// whichever of the wire and the disk is slower, and splits neither out.
    pub dd_secs: f64,
    /// `--bootnext` and `reboot`.
    pub entry: f64,
    pub down: f64,
    pub read_log: f64,
    /// The volume read whole and the outside judge's check of it; `None` on a
    /// run without `--fat32-check`.
    pub raw_log: Option<f64>,
}

impl fmt::Display for Phases {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "preamble {:.1} s, flash {:.1} s (dd {:.1} s), entry {:.1} s, down {:.1} s, read_log \
             {:.1} s, raw_log ",
            self.preamble,
            self.flash,
            self.dd_secs,
            self.entry,
            self.down,
            self.read_log
        )?;
        match self.raw_log {
            Some(secs) => write!(f, "{secs:.1} s"),
            None => f.write_str(NOT_READ),
        }
    }
}

/// [`READBACK_BOOT`]'s content: what the host measured about the boot, and the
/// machine it ran on.
pub struct BootFile {
    pub back: u64,
    pub machine: Machine,
    pub phases: Phases,
}

impl BootFile {
    pub fn text(&self) -> String {
        let Self { back, machine, phases } = self;
        let secs = |s: f64| format!("{s:.3}");
        let rows = [
            (BACK_SECS, back.to_string()),
            (VENDOR_KEY, machine.vendor.clone()),
            (PRODUCT_KEY, machine.product.clone()),
            (BIOS_KEY, machine.bios.clone()),
            (PREAMBLE_KEY, secs(phases.preamble)),
            (FLASH_KEY, secs(phases.flash)),
            (DD_SECS_KEY, secs(phases.dd_secs)),
            (ENTRY_KEY, secs(phases.entry)),
            (DOWN_KEY, secs(phases.down)),
            (READ_LOG_KEY, secs(phases.read_log)),
            (RAW_LOG_KEY, phases.raw_log.map_or_else(|| NOT_READ.to_string(), secs)),
        ];
        rows.iter().map(|(name, value)| format!("{name} {value}\n")).collect()
    }
}

/// The phases a readback names, or why it names none.
pub fn phases(text: &str) -> Result<Phases, String> {
    let named = |name: &str| word(text, name).ok_or_else(|| format!("names no `{name}`"));
    let secs = |name: &str| -> Result<f64, String> {
        let got = named(name)?;
        got.parse().map_err(|_| format!("reads `{name} {got}`, which is no count of seconds"))
    };
    Ok(Phases {
        preamble: secs(PREAMBLE_KEY)?,
        flash: secs(FLASH_KEY)?,
        dd_secs: secs(DD_SECS_KEY)?,
        entry: secs(ENTRY_KEY)?,
        down: secs(DOWN_KEY)?,
        read_log: secs(READ_LOG_KEY)?,
        raw_log: match named(RAW_LOG_KEY)?.as_str() {
            NOT_READ => None,
            _ => Some(secs(RAW_LOG_KEY)?),
        },
    })
}

/// Every file a readback directory carries, so a run that writes none of them
/// leaves none of the last run's behind.
pub const READBACK_FILES: &[&str] = &[
    READBACK_LOADER,
    READBACK_KERNEL,
    READBACK_BOOT,
    READBACK_VERDICT,
    READBACK_VOLUME,
];

/// Empty a readback directory, before this run can leave any of it standing.
///
/// **A file that is still there after this ran is one this boot wrote.**
/// [`write_readback`] is reached only after the machine came back and the log
/// was read, so every refusal before it — a machine that never returned, a
/// volume that would not mount, a volume the outside judge complained about —
/// used to leave the *previous* run's `kernel.log` in place for a judge that
/// cannot tell one boot's file from another's.
pub fn clear_readback(dir: &Path) -> Result<(), Refusal> {
    for name in READBACK_FILES {
        let at = dir.join(name);
        match std::fs::remove_file(&at) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Refusal::File {
                    path: at.display().to_string(),
                    why: e.to_string(),
                })
            }
        }
    }
    Ok(())
}

fn write_readback(
    dir: &Path,
    loader: &str,
    log: &str,
    boot: &str,
    verdict: &str,
) -> Result<(), Refusal> {
    let wrote = |path: &Path, text: &str| -> Result<(), Refusal> {
        std::fs::write(path, text)
            .map_err(|e| Refusal::File { path: path.display().to_string(), why: e.to_string() })
    };
    std::fs::create_dir_all(dir)
        .map_err(|e| Refusal::File { path: dir.display().to_string(), why: e.to_string() })?;
    wrote(&dir.join(READBACK_LOADER), loader)?;
    wrote(&dir.join(READBACK_KERNEL), log)?;
    wrote(&dir.join(READBACK_BOOT), boot)?;
    wrote(&dir.join(READBACK_VERDICT), verdict)
}

/// Whether this boot was a loader pass that reported a record and booted no
/// kernel, and what that pass said if so.
///
/// **Three things together, and no one of them alone.** `loader.log` carries a
/// pass that ended the chain; the loader never reached its handoff line, which
/// is what a pass that *did* boot a kernel writes; and the kernel log carries no
/// boot record. A boot whose kernel died early has the handoff line and no
/// chain-end; a boot that panicked has both, and a `logkeeper` file besides.
///
/// It exists because `Unfit::NoBootRecord` says "this boot failed" where the
/// answer is "this boot never happened", and the two send a reader to different
/// places. On the T14 the first flash of a run read a record another image had
/// left in the same DRAM, took itself for that record's reporting pass, and
/// handed the machine back in 24 s having run nothing.
///
/// Pure, so the reading can be staged: text in, a name out.
pub fn reported_and_booted_nothing(loader: &str, log: &str) -> Option<String> {
    let ended = loader.lines().find(|line| line.contains(bootlog::CHAIN_ENDS_LINE))?;
    if loader.contains(bootlog::LOADER_LAST_LINE) || bootlog::boot_millis(log).is_some() {
        return None;
    }
    // The report rather than the line that ends the chain, and the *last* of it:
    // the pass writes the record's date first and what it found second, and what
    // it found is the half that identifies the record.
    let said = loader
        .lines()
        .rfind(|line| {
            line.contains(bootlog::PREVIOUS_PANIC) || line.contains(bootlog::BLACKBOX_HEAD)
        })
        .unwrap_or(ended);
    Some(said.trim().to_string())
}
/// What the host measured about a boot, out of [`READBACK_BOOT`].
pub fn back_secs(text: &str) -> Option<u64> {
    key(text, BACK_SECS)
}

/// The machine a readback names, or why it names none.
pub fn machine(text: &str) -> Result<Machine, String> {
    match (word(text, VENDOR_KEY), word(text, PRODUCT_KEY), word(text, BIOS_KEY)) {
        (Some(vendor), Some(product), Some(bios)) => Ok(Machine { vendor, product, bios }),
        _ => Err(format!(
            "names no machine: {VENDOR_KEY}, {PRODUCT_KEY} and {BIOS_KEY} are owed together"
        )),
    }
}

fn word(text: &str, name: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(name))
        .map(|rest| rest.trim().to_string())
        .filter(|got| !got.is_empty())
}

fn key<T: std::str::FromStr>(text: &str, name: &str) -> Option<T> {
    text.lines()
        .find_map(|line| line.strip_prefix(name))
        .and_then(|rest| rest.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bench account's home, as its shell answers [`Home::QUERY`].
    fn home() -> Home {
        Home::parse("/home/toyos").expect("a home")
    }

    fn target() -> Target {
        Target {
            key: PathBuf::from("/home/dev/.ssh/id_ed25519_toyos_runner"),
            ..Target::bench_t14().expect("a test run has HOME")
        }
    }

    /// **A port that accepts is not a machine that came back**, and a machine
    /// going down is its port refusing.
    /// Staged through [`port_accepts`] on a listener of this host's own, which
    /// accepts and never speaks `ssh`, and on port 0, which nothing listens on:
    /// a port the listener let go of can be another socket's by the time it is
    /// dialled.
    #[test]
    fn coming_back_is_ssh_answering_and_not_its_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("its address").port();
        let accepts = |within| port_accepts("127.0.0.1", port, within);
        let mut asked = 0;
        let back = wait_on(1, "come back", true, accepts, || {
            asked += 1;
            false
        });
        assert_eq!(back, Err(Refusal::Silent { what: "come back", secs: 1 }));
        assert!(asked > 0, "`ssh` was never asked of a port that accepts");
        assert_eq!(wait_on(1, "come back", true, accepts, || true), Ok(0));
        let up = wait_on(1, "go down", false, accepts, || unreachable!("`ssh` asked of one going down"));
        assert_eq!(up, Err(Refusal::Silent { what: "go down", secs: 1 }));
        let refuses = |within| port_accepts("127.0.0.1", 0, within);
        assert_eq!(wait_on(1, "go down", false, refuses, || unreachable!()), Ok(0));
    }

    /// **The harness names the refusal, not only the exit status.** Cargo's own
    /// lines come first and the driver's statement under the printer's stamp,
    /// and the refusal it ends on is read back whole, past its first line.
    #[test]
    fn a_refusal_is_read_back_off_the_drivers_stderr() {
        let stderr = "    Blocking waiting for file lock on package cache\n\
            \x20   Finished `dev` profile [optimized + debuginfo] target(s) in 0.19s\n\
            \x20    Running `target/debug/toyos-metal --image i --readback r --fat32-check`\n\
            14:02:11 toyos-metal: reading the disk on the machine exited exit status: 255: the first line\n\
            \x20 and the second\n";
        let said = Refusal::Remote {
            what: "reading the disk".into(),
            status: "exited exit status: 255".into(),
            stderr: "the first line\n  and the second".into(),
        };
        assert_eq!(said_refusal(stderr), Some(said.to_string()));
        assert_eq!(said_refusal(&stderr[..stderr.find(REFUSAL_HEAD).unwrap()]), None, "cargo's lines are no refusal");
    }

    /// **An image with no bound on its own boot never reaches the bench.**
    ///
    /// The case, measured twice: a kernel that hung after its job list, and a
    /// capture that fell through into flashing an ordinary
    /// `target/bootable.img` — 184 MB, no job list, no deadline — which booted
    /// the T14 into a boot that never ends and cost the owner a power cut. Every
    /// image the metal profile builds carries `boot-deadline=`; one that does
    /// not is not one of them.
    #[test]
    fn an_image_with_no_bound_on_its_own_boot_is_refused() {
        let armed = |names: &[&str]| -> Vec<String> {
            names.iter().map(|n| (*n).to_string()).collect()
        };
        let bound = alloc_deadline();

        // The shape a stray `target/bootable.img` has: nothing armed at all.
        let refusal = judge_arms(&armed(&[])).unwrap_err();
        assert_eq!(refusal, Refusal::NoBound { staged_a_wedge: false });
        let said = refusal.to_string();
        assert!(said.contains(toyos_tco::DEADLINE_PARAM), "{said}");
        assert!(said.contains("no power switch"), "{said}");
        // A build fault and not the machine's, so exit 2 rather than 1.
        assert!(!refusal.about_the_boot());

        // The sharpest form, and it says so: an image that stops every CPU on
        // purpose with nothing to end the boot it stopped.
        let refusal = judge_arms(&armed(&[WEDGE_ARM])).unwrap_err();
        assert_eq!(refusal, Refusal::NoBound { staged_a_wedge: true });
        assert!(refusal.to_string().contains("stops every CPU on purpose"), "{refusal}");

        // And with the bound, both go through — the wedge because it is paired.
        // The gate answers with the bound the image carries, which is what the
        // loop waits it out by.
        assert_eq!(judge_arms(&armed(&[&bound])), Ok(toyos_tco::wedge_bound_ms(toyos_tco::JOB_BOUND_MS)));
        assert_eq!(judge_arms(&armed(&[WEDGE_ARM, &bound])), Ok(toyos_tco::wedge_bound_ms(toyos_tco::JOB_BOUND_MS)));
        let longer = format!("{}271360", toyos_tco::DEADLINE_PARAM);
        assert_eq!(judge_arms(&armed(&["watchdog", &longer])), Ok(271_360));
        // A bound nothing can read is no bound.
        for unread in ["soon", "", "0"] {
            let unread = format!("{}{unread}", toyos_tco::DEADLINE_PARAM);
            assert_eq!(
                judge_arms(&armed(&[&unread])),
                Err(Refusal::NoBound { staged_a_wedge: false }),
                "{unread}"
            );
        }
        // The bound does not excuse an arm nobody ruled on.
        assert!(matches!(
            judge_arms(&armed(&["nvme-write-selftest", &bound])),
            Err(Refusal::Armed { .. })
        ));
    }

    /// **The gate refuses an image with no bound and clears the bound itself.**
    /// Two rules meeting on one token: `judge_arms` asks for a `boot-deadline=`
    /// and then asks `flashable` about every arm including that one, so a
    /// bound the ruling table did not clear would make every metal image
    /// unflashable. It is cleared as one of `build::VALUED_PARAMS`, and this is
    /// what holds the two together.
    #[test]
    fn the_bound_the_gate_demands_is_a_bound_the_gate_clears() {
        let bound = alloc_deadline();
        assert!(toyos_build::build::is_valued_param(&bound), "{bound}");
        assert!(flashable(&bound));
        // And it is exactly what `tests/common/metal.rs` arms every image with:
        // the same two constants, so a change to either moves both.
        assert!(bound.starts_with(toyos_tco::DEADLINE_PARAM));
        assert_eq!(judge_arms(&[bound]), Ok(toyos_tco::wedge_bound_ms(toyos_tco::JOB_BOUND_MS)));
    }
    /// What every metal image is armed with, spelled the way `tests/common/metal.rs`
    /// spells it.
    fn alloc_deadline() -> String {
        format!("{}{}", toyos_tco::DEADLINE_PARAM, toyos_tco::wedge_bound_ms(toyos_tco::JOB_BOUND_MS))
    }

    /// **An unjudged install or boot is an action of its own**: it takes the
    /// flags that say which machine and a rehearsal, and refuses every flag
    /// that describes a judged boot or a capture beside it.
    #[test]
    fn an_unjudged_install_is_not_also_a_judged_boot() {
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        let both = Args::parse(&words(&["--install-internal", "demo.img", "--boot-internal", "--dry-run", "--host", "toyos@bench-amd"])).unwrap();
        assert_eq!(both.install_internal.as_deref(), Some(Path::new("demo.img")));
        assert!(both.boot_internal && both.dry_run);
        assert!(Args::parse(&words(&["--boot-internal"])).unwrap().install_internal.is_none());
        for judged in [&["--image", "x.img"][..], &["--readback", "r"], &["--fat32-check"]] {
            for internal in [&["--install-internal", "demo.img"][..], &["--boot-internal"]] {
                let line = [internal, judged].concat();
                let said = Args::parse(&words(&line)).unwrap_err().to_string();
                assert!(said.contains(judged[0]) && said.contains("describes a boot they will not make"), "{line:?}: {said}");
            }
        }
        let said = Args::parse(&words(&["--record", "--boot-internal"])).unwrap_err().to_string();
        assert!(said.contains("--boot-internal"), "{said}");
    }

    /// **There is nothing to fall through to.** A default image was what let the
    /// fall-through reach a disk at all; a command line that names none is
    /// refused by name rather than given one.
    #[test]
    fn a_command_line_that_names_no_image_flashes_none() {
        let bare: [String; 0] = [];
        let args = Args::parse(&bare).expect("an empty command line parses");
        assert_eq!(args.image, None);
        let refusal = run(&args).unwrap_err();
        let said = refusal.to_string();
        assert!(said.contains("there is no default"), "{said}");
        // A command line and not a machine: exit 2.
        assert!(!refusal.about_the_boot());
    }

    /// The gate fails closed: a name nobody has ruled on is refused, and it is
    /// refused *by that name* rather than by a count.
    #[test]
    fn an_arm_with_no_ruling_never_reaches_the_bench() {
        assert!(flashable("watchdog"));
        assert!(flashable("blackbox=0x8000000"));
        assert!(!flashable("nvme-write-selftest"));
        let refusal = Refusal::Armed { name: "nvme-write-selftest".to_string() };
        let said = refusal.to_string();
        assert!(said.contains("nvme-write-selftest"), "{said}");
        assert!(said.contains("FLASHABLE"), "{said}");
        // A refusal about the image is the loop's, never the machine's: exit 2.
        assert!(!refusal.about_the_boot());
    }

    /// **An arm that stops the machine and is judged as an ordinary boot reds
    /// every run it is staged for**, because `Rebooting.` is the word it never
    /// reaches. The two halves are `WEDGE_ARMS` and `FLASHABLE`, and this is
    /// what holds them together — a name in either that the other does not know
    /// about is the hole.
    #[test]
    fn every_arm_that_stops_this_machine_is_cleared_and_judged_as_one() {
        let bound = alloc_deadline();
        for arm in WEDGE_ARMS {
            assert!(flashable(arm), "{arm} reaches no bench");
            assert_eq!(judge_arms(&[arm.to_string(), bound.clone()]), Ok(toyos_tco::wedge_bound_ms(toyos_tco::JOB_BOUND_MS)), "{arm}");
            assert!(stages_a_wedge(&[arm.to_string()]), "{arm}");
            assert_eq!(bound_for(&[arm], 135_680), toyos_tco::STAGED_BOUND_MS, "{arm}");
            // And with no bound behind it, the sharpest refusal names it as the
            // wedge it is rather than as a plain image.
            assert_eq!(
                judge_arms(&[arm.to_string()]),
                Err(Refusal::NoBound { staged_a_wedge: true }),
                "{arm}"
            );
        }
        // The negative half: an arm that stops nothing is not judged as one,
        // and keeps the bound a wedge nobody staged is ended by, which is
        // twice its list's.
        assert!(!stages_a_wedge(&["watchdog".to_string(), bound]));
        assert_eq!(bound_for(&["watchdog"], 60_000), 120_000);
        assert_eq!(bound_for(&["watchdog"], 135_680), 271_360);
    }

    /// Under the chipset watchdog a staged wedge's deadline is past the
    /// watchdog's reset, which lands within one bound of the last feed.
    #[test]
    fn a_wedge_under_the_watchdog_is_the_watchdogs_to_end() {
        assert_eq!(bound_for(&[WEDGE_ARM, toyos_tco::PARAM], 60_000), 29_200);
        const { assert!(toyos_tco::WATCHED_WEDGE_BOUND_MS > toyos_tco::STAGED_BOUND_MS + toyos_tco::BOUND_MS) };
        assert_eq!(bound_for(&[WEDGE_ARM], 60_000), toyos_tco::STAGED_BOUND_MS);
    }

    /// A row for a name the kernel no longer declares is a ruling about
    /// nothing, and it would keep an image admissible after the arm it cleared
    /// was deleted. Read off the kernel's own two declarations.
    #[test]
    fn every_ruling_names_a_parameter_the_kernel_declares() {
        let root = &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut declared = toyos_build::build::declared_actuators(root);
        declared.extend(toyos_build::build::declared_params(root));
        for name in FLASHABLE {
            assert!(
                declared.iter().any(|d| toyos_build::build::arms(d, name)),
                "`FLASHABLE` rules on {name:?}, which the kernel declares as neither an \
                 actuator nor a boot parameter: {declared:?}"
            );
        }
    }

    /// **The case, off the machine that produced it.** A `DONE` record another
    /// image had left in the same DRAM is read as this boot's predecessor, and
    /// the driver's only word for it was "the log carries no `Boot: complete`
    /// record" — which sends a reader after a kernel that never ran.
    #[test]
    fn a_pass_that_reported_and_booted_nothing_is_named_as_that() {
        let stale = "ToyOS Bootloader 1.0\n\
             Black box: the record below is from the boot armed at 2026-09-07-045553\n\
             Black box: the last boot read DONE, so it handed the machine back on purpose \
             and this chain ends here\n\
             Loader log: the last boot is accounted for, so this pass resets the machine\n";
        let said = reported_and_booted_nothing(stale, "").expect("the case, named");
        assert!(said.contains("the last boot read DONE"), "{said}");
        let refusal = Refusal::ReportedAndBootedNothing { said };
        let words = refusal.to_string();
        assert!(words.contains("booted no kernel"), "{words}");
        assert!(words.contains("This boot measured no test."), "{words}");
        // The machine failed, not the loop: exit 1.
        assert!(refusal.about_the_boot());

        // **The three controls, and each is a boot this must not name.** A pass
        // that booted a kernel says so at its handoff line; a boot that got as
        // far as its own record is a boot that ran; and a machine with no
        // finding at all has no chain-end line to find.
        let booted = format!("{stale}{}\n", bootlog::LOADER_LAST_LINE);
        assert_eq!(reported_and_booted_nothing(&booted, ""), None);
        assert_eq!(
            reported_and_booted_nothing(stale, "[ 1.200 cpu0 kernel] Boot: complete (1220ms)\n"),
            None
        );
        assert_eq!(
            reported_and_booted_nothing("ToyOS Bootloader 1.0\nLoading kernel...\n", ""),
            None
        );
    }
    #[test]
    fn the_host_clock_crosses_in_the_boot_file() {
        assert_eq!(back_secs("back_secs 47\n"), Some(47));
        assert_eq!(back_secs("back_secs 47"), Some(47));
        assert_eq!(back_secs(""), None);
        assert_eq!(back_secs("back_secs later\n"), None);
    }

    fn t14() -> Machine {
        Machine::parse("LENOVO\n20W0003AMZ\nN34ET71W (1.71 )\n").expect("three lines")
    }

    fn phased(raw_log: Option<f64>) -> Phases {
        Phases {
            preamble: 3.104,
            flash: 121.5,
            dd_secs: 117.234,
            entry: 2.25,
            down: 8.0,
            read_log: 1.375,
            raw_log,
        }
    }

    #[test]
    fn the_machine_crosses_in_the_boot_file() {
        let boot = BootFile { back: 47, machine: t14(), phases: phased(None) }.text();
        assert_eq!(machine(&boot), Ok(t14()));
        assert_eq!(back_secs(&boot), Some(47));
        assert!(machine("back_secs 47\nmachine_vendor LENOVO\n").is_err());
    }

    /// Every phase the loop timed is the one a judge reads back, the volume
    /// read's absence included, and a file missing any phase names none.
    #[test]
    fn the_phases_cross_in_the_boot_file() {
        for raw_log in [Some(9.875), None] {
            let boot = BootFile { back: 141, machine: t14(), phases: phased(raw_log) }.text();
            assert_eq!(phases(&boot), Ok(phased(raw_log)), "{boot}");
            assert_eq!(back_secs(&boot), Some(141));
            for line in boot.lines().filter(|line| line.contains("_secs ")) {
                if line.starts_with(BACK_SECS) {
                    continue;
                }
                let short = boot.replace(&format!("{line}\n"), "");
                assert!(phases(&short).is_err(), "{line:?} gone still names phases");
            }
        }
        assert!(phases("back_secs 47\n").is_err());
        let garbled = BootFile { back: 1, machine: t14(), phases: phased(None) }
            .text()
            .replace("flash_secs 121.500", "flash_secs later");
        assert!(phases(&garbled).is_err());
    }

    /// A judge reading the readback later rules on the boot as the loop did.
    #[test]
    fn the_loops_verdict_crosses_in_the_readback() {
        assert_eq!(loop_verdict(&verdict_file(None)), Ok(()));
        let refused = Refusal::HungWithoutARecord;
        assert_eq!(
            loop_verdict(&verdict_file(Some(&refused))),
            Err(refused.to_string().trim_end().to_string())
        );
        for unread in ["", "passed", "passed\nand more\n", "refused\n", "refused\n  \n", "green\n"] {
            assert!(loop_verdict(unread).is_err(), "{unread:?}");
        }
    }

    /// The writer's half: the verdict the loop returns is the one its readback
    /// carries, for a boot it refused and for one it passed.
    #[test]
    fn the_readback_carries_the_verdict_the_loop_returns() {
        let dir = toyos_tmpdir::TempDir::new("verdict");
        let armed = [alloc_deadline()];
        let boot = "back_secs 40\n";
        let written = || {
            let at = dir.join(READBACK_VERDICT);
            loop_verdict(&std::fs::read_to_string(&at).expect("the verdict is written"))
        };

        let hung = format!("{}\n{}\n", bootlog::LOADER_FIRST_LINE, bootlog::HUNG_WITHOUT_A_RECORD);
        assert_eq!(
            judge_and_write_readback(&armed, &hung, "", Some(dir.path()), boot),
            Err(Refusal::HungWithoutARecord)
        );
        assert_eq!(written(), Err(Refusal::HungWithoutARecord.to_string().trim_end().to_string()));

        let loader = format!(
            "{}\n{}\n{}\n{}\nBlack box: {}, so it handed the machine back on purpose and this \
             chain ends here\n| {} (16)\n| {}[ 1.516 cpu0 kernel] {}\n{}\n",
            bootlog::LOADER_FIRST_LINE,
            bootlog::LOADER_LAST_LINE,
            bootlog::SEPARATOR,
            bootlog::LOADER_FIRST_LINE,
            bootlog::HANDED_BACK,
            bootlog::LOG_TAIL_HEAD,
            bootlog::LOG_TAIL,
            bootlog::REBOOTING,
            bootlog::CHAIN_ENDS_LINE,
        );
        let log = format!(
            "[ 1.151 cpu0 kernel] Boot: complete (1151ms)\n[ 1.203 supervisor] {} (Reboot)\n",
            bootlog::STOPPING
        );
        assert_eq!(
            judge_and_write_readback(&armed, &loader, &log, Some(dir.path()), boot),
            Ok(1151)
        );
        assert_eq!(written(), Ok(()));
    }

    /// **A machine with no record is refused with how to make one, and a
    /// record is written once**: a second capture finds it and moves nothing.
    #[test]
    fn a_machine_is_recorded_once_and_then_found_recorded() {
        let root = toyos_tmpdir::TempDir::new("metal-record");
        let bench = Machine::parse("ACME\nBench 2\nX1.0\n").expect("three lines");
        let at = root.join("acme-bench-2.toml");
        let path = at.display().to_string();
        let refused = recorded(&root, &bench);
        assert_eq!(refused, Err(Refusal::Unrecorded { path: path.clone() }));
        let said = refused.unwrap_err().to_string();
        assert!(said.contains("--record --host") && said.contains(&path), "{said}");
        assert!(!Refusal::Unrecorded { path }.about_the_boot());

        assert_eq!(record_machine(&root, &bench), Ok((at.clone(), true)));
        let record = recorded(&root, &bench).expect("a record");
        assert!(record.measured.is_empty());
        let mut measured = record;
        measured.measured.insert("boot.a.complete_ms".to_string(), 1166);
        measured.save(&root).expect("a record with a number");
        assert_eq!(record_machine(&root, &bench), Ok((at, false)));
        assert_eq!(recorded(&root, &bench), Ok(measured), "a second capture moved the record");
    }

    /// `node` and its partitions as [`DISKS`] lists them, each `(type, GUID)`.
    fn listed(node: &str, parts: &[(&str, &str)]) -> String {
        let mut out = format!("NAME=\"{node}\" TYPE=\"disk\" PKNAME=\"\" PARTN=\"\" PARTTYPE=\"\" PARTUUID=\"\"\n");
        for (i, (kind, unique)) in parts.iter().enumerate() {
            let n = i + 1;
            let part = if node.starts_with("/dev/nvme") { format!("{node}p{n}") } else { format!("{node}{n}") };
            out += &format!(
                "NAME=\"{part}\" TYPE=\"part\" PKNAME=\"{node}\" PARTN=\"{n}\" PARTTYPE=\"{kind}\" PARTUUID=\"{unique}\"\n"
            );
        }
        out
    }

    /// A ToyOS image's table, as a flashed stick carries one, its log carrying
    /// `log`.
    fn toyos(node: &str, log: &str) -> String {
        listed(
            node,
            &[
                ("c12a7328-f81f-11d2-ba4b-00a0c93ec93b", "10000000-0000-4000-8000-000000000000"),
                ("94464329-e06e-4288-a9da-7fc7154f5e92", "20000000-0000-4000-8000-000000000000"),
                ("ebd0a0a2-b9e5-4433-87c0-68b6b72699c7", log),
                ("037719d7-dea5-481a-aa07-6af8be6d51e2", "40000000-0000-4000-8000-000000000000"),
                ("b350bc93-bb6a-4c5e-9589-a5c3cfd555fd", "50000000-0000-4000-8000-000000000000"),
            ],
        )
    }

    /// A live ISO's hybrid table, which lsblk reads as MBR types.
    fn iso(node: &str) -> String {
        listed(node, &[("0x0", "0f1e2d3c-01"), ("0xef", "0f1e2d3c-02")])
    }

    const LOG: &str = "30000000-0000-4000-8000-000000000000";

    const LAYOUT: Carrying = Carrying::Layout;

    /// A disk adopted as scratch: one partition, of the scratch type.
    fn scratch(node: &str) -> String {
        listed(node, &[("51cc09f9-7442-435b-bc03-0fd8718b0fb5", "60000000-0000-4000-8000-000000000000")])
    }

    /// **A disk carrying an image and a scratch disk are never taken for each
    /// other**, in either direction and whichever node each has.
    #[test]
    fn an_image_and_a_scratch_disk_are_never_confused() {
        for (boot, spare) in [("/dev/sda", "/dev/sdb"), ("/dev/sdb", "/dev/sda")] {
            let listing = format!("{}{}", toyos(boot, LOG), scratch(spare));
            let parts = |sd: &str| parts_of(&listing, Sd::parse(sd).unwrap()).unwrap();
            assert!(Carrying::Scratch.holds(&parts(spare)) && !LAYOUT.holds(&parts(spare)));
            assert!(!Carrying::Scratch.holds(&parts(boot)));
        }
        // A scratch partition beside anything else is no scratch disk.
        let mut both = toyos("/dev/sda", LOG);
        both += "NAME=\"/dev/sda6\" TYPE=\"part\" PKNAME=\"/dev/sda\" PARTN=\"6\" \
                 PARTTYPE=\"51cc09f9-7442-435b-bc03-0fd8718b0fb5\" PARTUUID=\"\"\n";
        assert!(!Carrying::Scratch.holds(&parts_of(&both, Sd(b'a')).unwrap()));
    }

    /// **An adoption reads back the wipe it made**: a scratch partition still
    /// carrying the old ESP's FAT is refused, by name, as is a table that is
    /// not a scratch disk's.
    #[test]
    fn an_adopted_disk_reads_back_as_scratch_with_no_filesystem() {
        let sdb = Sd(b'b');
        assert_eq!(scratch_read_back(&scratch("/dev/sdb"), sdb), Ok(()));
        let stale = scratch("/dev/sdb").replace("PARTUUID=\"60000000-0000-4000-8000-000000000000\"", "PARTUUID=\"60000000-0000-4000-8000-000000000000\" FSTYPE=\"vfat\"");
        assert_ne!(stale, scratch("/dev/sdb"));
        let refused = scratch_read_back(&stale, sdb).unwrap_err();
        assert!(matches!(refused, Refusal::Adopt(ref why) if why.ends_with("carries a vfat signature")), "{refused:?}");
        let refused = scratch_read_back(&toyos("/dev/sdb", LOG), sdb).unwrap_err();
        assert!(matches!(refused, Refusal::Adopt(ref why) if why.ends_with("does not read back as a scratch disk")), "{refused:?}");
    }

    /// **Adoption never erases a disk holding a ToyOS image**, nor a node the
    /// machine lacks.
    #[test]
    fn a_disk_holding_a_toyos_image_is_never_adopted_as_scratch() {
        let listing = format!("{}{}", toyos("/dev/sda", LOG), iso("/dev/sdb"));
        let refused = adoptable(&listing, Sd(b'a'), LAYOUT).unwrap_err();
        assert!(matches!(refused, Refusal::Adopt(ref why) if why.starts_with("/dev/sda carries")), "{refused:?}");
        assert!(matches!(adoptable(&listing, Sd(b'c'), LAYOUT), Err(Refusal::Adopt(_))));
        assert_eq!(adoptable(&listing, Sd(b'b'), LAYOUT), Ok(()));
        assert_eq!(adoptable(&scratch("/dev/sdb"), Sd(b'b'), LAYOUT), Ok(()), "adopting again rewrites the table");
    }

    /// **A capture, an adoption and a fall-through are actions, not modes**:
    /// each stands beside nothing but the flags that say which machine, and
    /// the last two beside `--dry-run`. A capture once fell through into the
    /// loop with whatever `--image` defaulted to, and booted the T14 into a
    /// boot with no job list and no bound.
    #[test]
    fn a_capture_an_adoption_and_a_fall_through_are_actions_of_their_own() {
        let words = |w: &[&str]| w.iter().map(|w| (*w).to_string()).collect::<Vec<_>>();
        let adopt = Args::parse(&words(&["--adopt-scratch", "/dev/sdb", "--dry-run", "--host", "toyos@bench-amd"])).unwrap();
        assert_eq!(adopt.adopt_scratch, Some(Sd(b'b')));
        assert!(Args::parse(&words(&["--record", "--host", "dev@t14", "--key", "/tmp/k"])).unwrap().record);
        for (word, how) in [("missing", Unbootable::Missing), ("garbage", Unbootable::Garbage)] {
            let fall = Args::parse(&words(&["--fall-through", word, "--dry-run", "--host", "toyos@bench-amd"])).unwrap();
            assert_eq!(fall.fall_through, Some(how));
        }
        assert!(matches!(Args::parse(&words(&["--fall-through", "pxe"])), Err(Refusal::Usage(_))));
        let boot = [&["--image", "x.img"][..], &["--readback", "r"], &["--fat32-check"], &["--boot-internal"]];
        for action in [&["--adopt-scratch", "/dev/sdb"][..], &["--record"], &["--fall-through", "missing"]] {
            let dry: &[&str] = &["--dry-run"];
            for beside in boot.iter().copied().chain([dry].into_iter().filter(|_| action[0] == "--record")) {
                let line = [action, beside].concat();
                let refused = Args::parse(&words(&line)).unwrap_err();
                assert!(matches!(refused, Refusal::Usage(ref why) if why.contains(beside[0])), "{line:?}");
                assert!(!refused.about_the_boot());
            }
        }
        assert!(matches!(Args::parse(&words(&["--adopt-scratch", "/dev/mmcblk0"])), Err(Refusal::Usage(_))));
        let both = Args::parse(&words(&["--record", "--adopt-scratch", "/dev/sdb"])).unwrap_err();
        assert!(matches!(both, Refusal::Usage(_)));
    }

    /// **The mount point is under the home the machine names**, and a home
    /// the machine answers that is no normal absolute path is refused.
    #[test]
    fn the_mount_is_under_the_home_the_machine_named() {
        let words = ["--host", "dev@box"].map(String::from);
        let target = Args::parse(&words).unwrap().target;
        let home = Home::parse("/var/home/dev").expect("a home");
        assert_eq!(home.mount(), "/var/home/dev/toyos-log");
        assert_eq!(
            target.argv(&Job::Mount(Owned::Log), &home),
            ["/usr/bin/mount", "-o", "ro", "/dev/nvme0n1p7", "/var/home/dev/toyos-log"]
        );
        assert_eq!(target.argv(&Job::Umount, &home), ["/usr/bin/umount", "/var/home/dev/toyos-log"]);
        for answer in ["", "var/home/dev", "/", "/var/home/dev/", "/var//dev", "/var/./dev", "/var/../root", "/a\n"]
        {
            assert_eq!(Home::parse(answer), Err(Refusal::Home(answer.to_string())), "{answer:?}");
        }
    }

    #[test]
    fn a_host_is_a_user_and_a_machine_that_ssh_reads_as_no_flag() {
        assert_eq!(host_of("dev@box"), Ok(("dev", "box")));
        for host in ["box", "-oProxyCommand=x@box", "dev@-oProxyCommand=x", "dev@", "dev@a b"]
        {
            assert!(
                matches!(host_of(host), Err(Refusal::Usage(_))),
                "{host}: {:?}",
                host_of(host)
            );
            let words = ["--host".to_string(), host.to_string()];
            assert!(Args::parse(&words).is_err(), "{host}");
        }
    }

    /// **No root line names the bench's own partitions**: every write on the
    /// internal disk is to one of partitions 5–11, and the table is touched
    /// by the two `sgdisk` lines alone, each naming only those numbers.
    #[test]
    fn no_root_line_writes_the_benchs_own_partitions() {
        let t = target();
        let guid = Guid::parse("69DDC8F6-FAB2-423F-9818-93BB0BA7349C").unwrap();
        let named = Owned::ALL.iter().filter(|o| o.named_by_image()).map(|o| (*o, guid)).collect();
        let mut internal_jobs = vec![
            Job::Partition,
            Job::AddPartitions,
            Job::Retag(named),
            Job::Create { label: INTERNAL_LABEL, loader: LOADER },
            Job::Create { label: UNBOOTABLE_LABEL, loader: Unbootable::Missing.loader() },
            Job::Create { label: UNBOOTABLE_LABEL, loader: Unbootable::Garbage.loader() },
            Job::ReadLog { bytes: 35_651_584 },
        ];
        internal_jobs.extend(Owned::ALL.map(Job::Write));
        internal_jobs.extend(Owned::ALL.map(Job::Mount));
        for job in internal_jobs {
            let argv = t.argv(&job, &home());
            let internal = argv.iter().any(|w| w.contains("/dev/nvme0n1"));
            assert!(internal, "{job:?}: {argv:?}");
            for word in &argv {
                for bench in 1..=4 {
                    assert_ne!(word, &format!("/dev/nvme0n1p{bench}"), "{job:?}: {argv:?}");
                    assert_ne!(word, &format!("of=/dev/nvme0n1p{bench}"), "{job:?}: {argv:?}");
                    assert!(!internal || !word.contains(&format!("={bench}:")), "{job:?}: {argv:?}");
                }
            }
            if argv.iter().any(|w| w == "/dev/nvme0n1" || w == "of=/dev/nvme0n1") {
                assert!(
                    [SGDISK, PARTX, EFIBOOTMGR].contains(&argv[0].as_str()) && !argv.iter().any(|w| w.starts_with("of=/dev/nvme0n1")),
                    "{job:?} writes the whole internal disk: {argv:?}"
                );
            }
        }
    }

    /// The read the outside judge needs is a read: it carries no `of=` and no
    /// `conv=`, so it cannot write to the disk it names, and it reads the
    /// volume's bytes from the log partition and no more.
    #[test]
    fn the_outside_judges_read_can_write_nothing() {
        let argv = target().argv(&Job::ReadLog { bytes: 35_651_584 }, &home());
        assert!(!argv.iter().any(|w| w.starts_with("of=") || w.starts_with("conv=")), "{argv:?}");
        assert_eq!(argv[1..], ["if=/dev/nvme0n1p7", "bs=4M", "iflag=count_bytes", "count=35651584"]);
    }

    #[test]
    fn the_ssh_line_carries_the_key_and_nothing_interactive() {
        let t = target();
        assert_eq!(
            t.ssh_argv("true"),
            [
                "-i",
                "/home/dev/.ssh/id_ed25519_toyos_runner",
                "-o",
                "IdentitiesOnly=yes",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "toyos@bench-t14",
                "true",
            ]
        );
        assert_eq!(
            Sd(b'a').identity(),
            "cat /sys/class/block/sda/size /sys/class/block/sda/device/vendor \
             /sys/class/block/sda/device/model /sys/class/block/sda/removable"
        );
        assert_eq!(
            t.remote(&Job::Write(Owned::RootA), &home()),
            "sudo -n '/usr/bin/dd' 'of=/dev/nvme0n1p9' 'bs=4M' 'conv=fsync,nocreat'"
        );
        assert_eq!(t.remote(&Job::Settle, &home()), "sudo -n '/usr/bin/udevadm' 'settle' '--timeout=180'");
        assert_eq!(t.remote(&Job::Reread(Sd(b'a')), &home()), "sudo -n '/usr/sbin/blockdev' '--rereadpt' '/dev/sda'");
        assert_eq!(
            t.remote(&Job::WipePartition { sd: Sd(b'a'), part: 1 }, &home()),
            "sudo -n '/usr/sbin/wipefs' '--all' '/dev/sda1'"
        );
        assert_eq!(
            t.remote(&Job::Create { label: INTERNAL_LABEL, loader: LOADER }, &home()),
            "sudo -n '/usr/bin/efibootmgr' '--create-only' '--disk' '/dev/nvme0n1' '--part' '5' \
             '--label' 'ToyOS-internal' '--loader' '\\EFI\\BOOT\\BOOTX64.EFI'"
        );
        assert_eq!(
            t.remote(&Job::Create { label: UNBOOTABLE_LABEL, loader: Unbootable::Garbage.loader() }, &home()),
            "sudo -n '/usr/bin/efibootmgr' '--create-only' '--disk' '/dev/nvme0n1' '--part' '5' \
             '--label' 'ToyOS-unbootable' '--loader' '\\toyos\\log.guid'"
        );
    }

    #[test]
    fn a_create_that_moved_the_boot_order_is_refused() {
        let guid = "69ddc8f6-fab2-423f-9818-93bb0ba7349c";
        let before = "BootCurrent: 0001\nBootOrder: 0001,001D\n\
             Boot0001* Ubuntu\tHD(1,GPT,33333333-3333-3333-3333-333333333333,0x800,0x1000)\
             /File(\\EFI\\ubuntu\\shimx64.efi)\n";
        let made = format!(
            "Boot0002* ToyOS\tHD(1,GPT,{guid},0x800,0x11000)/File(\\EFI\\BOOT\\BOOTX64.EFI)\n"
        );

        let created_only = format!("{before}{made}");
        assert_eq!(entry_after_create(before, &created_only, "ToyOS", guid), Ok("0002".to_string()));

        let ordered = created_only.replace("BootOrder: 0001,001D", "BootOrder: 0002,0001,001D");
        assert_eq!(
            entry_after_create(before, &ordered, "ToyOS", guid),
            Err(Refusal::BootOrder {
                during: "creating the boot entry",
                before: "0001,001D".to_string(),
                now: "0002,0001,001D".to_string(),
            })
        );

        assert!(matches!(
            entry_after_create(before, before, "ToyOS", guid),
            Err(Refusal::BootEntry(_))
        ));
        let elsewhere = created_only.replace(guid, "11111111-1111-1111-1111-111111111111");
        assert!(matches!(
            entry_after_create(before, &elsewhere, "ToyOS", guid),
            Err(Refusal::BootEntry(_))
        ));

        assert_eq!(boot_order("BootCurrent: 0001\n"), "");
        assert_eq!(boot_order(""), "");
        assert!(!Refusal::BootOrder { during: "the boot", before: String::new(), now: "0002".to_string() }
            .about_the_boot());
    }

    /// **An entry in the boot order is never bought with `BootNext`, and a
    /// boot that moved the order is refused**: either is a bench whose next
    /// failed ToyOS boot comes back to ToyOS and not to Ubuntu.
    #[test]
    fn the_boot_order_holds_no_toyos_entry_and_no_boot_moves_it() {
        let listing = "BootCurrent: 0003\nBootOrder: 0003,2001,2002\n";
        assert_eq!(unordered(listing, "0004"), Ok(()));
        assert_eq!(
            unordered(listing, "2001"),
            Err(Refusal::Ordered { entry: "2001".to_string(), order: "0003,2001,2002".to_string() })
        );
        assert!(unordered("BootOrder: 0003,000a\n", "000A").is_err(), "the case efibootmgr prints is not the guard's");
        assert_eq!(unordered("BootCurrent: 0003\n", "0003"), Ok(()));
        assert_eq!(order_kept(listing, listing), Ok(()));
        let moved = listing.replace("BootOrder: 0003,", "BootOrder: 0004,0003,");
        assert_eq!(
            order_kept(listing, &moved),
            Err(Refusal::BootOrder {
                during: "the boot",
                before: "0003,2001,2002".to_string(),
                now: "0004,0003,2001,2002".to_string(),
            })
        );
    }

    /// **A disk's identity is the four `sysfs` lines [`Sd::identity`] asks
    /// for**, read as they come, and refused otherwise: adoption's refusal to
    /// erase a fixed disk rests on `removable`.
    #[test]
    fn a_disks_identity_reads_four_lines_or_is_refused() {
        let stick = Identity::parse("60062500\nSanDisk \nUltra           \n1\n").unwrap();
        assert_eq!(
            stick,
            Identity { sectors: 60_062_500, vendor: "SanDisk".to_string(), model: "Ultra".to_string(), removable: true }
        );
        assert!(!Identity::parse("500118192\nATA     \nSSD             \n0\n").unwrap().removable);
        for (answer, field) in [("60062500\nSanDisk \n", "the whole query"), ("many\nSanDisk \nUltra\n1\n", "size")] {
            assert!(matches!(Identity::parse(answer), Err(Refusal::Identity { field: f, .. }) if f == field), "{answer:?}");
        }
    }

    #[test]
    fn a_boot_entry_is_matched_on_the_partition_its_path_names() {
        let listing = "BootCurrent: 0001\nBootOrder: 0001,001D\n\
             Boot0001* Ubuntu\tHD(1,GPT,33333333-3333-3333-3333-333333333333,0x800,0x219800)\
             /File(\\EFI\\ubuntu\\shimx64.efi)\n\
             Boot0010  Setup\tFvFile(721c8b66-426c-4e86-8e99-3457c46ab0b9)\n\
             Boot0026* ToyOS\tHD(1,GPT,11111111-1111-1111-1111-111111111111,0x800,0x800)\
             /File(\\EFI\\BOOT\\BOOTX64.EFI)\n\
             Boot0027* ToyOS\tHD(1,GPT,22222222-2222-2222-2222-222222222222,0x800,0x800)\
             /File(\\EFI\\BOOT\\BOOTX64.EFI)\n";
        assert_eq!(
            entries_labelled(listing, "ToyOS"),
            [
                ("0026".to_string(), "11111111-1111-1111-1111-111111111111".to_string()),
                ("0027".to_string(), "22222222-2222-2222-2222-222222222222".to_string()),
            ]
        );
        assert_eq!(entries_labelled(listing, "Ubuntu").len(), 1);
        assert!(entries_labelled(listing, "ToyO").is_empty());
        assert!(entries_labelled("BootOrder: 0001,001D\n", "ToyOS").is_empty());
        assert_eq!(entries_labelled(listing, "Setup"), [("0010".to_string(), String::new())]);
    }

    /// The wait is the *longest* bound plus the allowance, computed here by
    /// hand from the constants rather than from the expression under test — so
    /// a `max` that became a `min` reds instead of agreeing with itself. The
    /// image's own deadline is one of them: a boot armed past every constant
    /// is waited for that long, and one armed under them for the longest
    /// constant.
    #[test]
    fn the_wait_outlasts_every_watchdog_a_boot_runs_under() {
        assert_eq!(toyos_tco::FIRMWARE_BOUND_MS, 60_000);
        assert_eq!(toyos_tco::BOUND_MS, 9_600);
        assert_eq!(toyos_tco::PANIC_BOUND_MS, 60_000);
        assert_eq!(RETURN_ALLOWANCE_SECS, 300);
        assert_eq!(return_secs(120_000), 420);
        assert_eq!(return_secs(271_360), 572);
        assert_eq!(return_secs(271_001), 572);
        assert_eq!(return_secs(toyos_tco::STAGED_BOUND_MS), 360);
        assert!(WATCHDOG_BOUNDS_MS.contains(&toyos_tco::FIRMWARE_BOUND_MS));
        assert!(WATCHDOG_BOUNDS_MS.contains(&toyos_tco::BOUND_MS));
        assert!(WATCHDOG_BOUNDS_MS.contains(&toyos_tco::PANIC_BOUND_MS));
    }

    /// **A refusal must leave no file for the next run's judge to read as its
    /// own**, and every refusal returns before `write_readback`.
    #[test]
    fn a_readback_directory_holds_nothing_the_last_run_left() {
        let dir = toyos_tmpdir::TempDir::new("readback");
        for name in READBACK_FILES {
            std::fs::write(dir.join(name), b"the last run").expect("stage a stale file");
        }
        // A file no readback names is not this directory's to remove.
        std::fs::write(dir.join("image.img"), b"the image").expect("stage the image");
        clear_readback(&dir).expect("clearing a directory of readable files");
        for name in READBACK_FILES {
            assert!(!dir.join(name).exists(), "{name} survived the clear");
        }
        assert!(dir.join("image.img").exists(), "the image the driver flashes was removed");
        // Idempotent: a first run has none of them and that is not a refusal.
        clear_readback(&dir).expect("clearing an empty directory");
    }

    #[test]
    fn a_failed_boot_and_a_loop_that_could_not_run_are_different_answers() {
        assert!(Refusal::Wedge { why: "x" }.about_the_boot());
        assert!(Refusal::Log(bootlog::Unfit::NoBootRecord).about_the_boot());
        assert!(Refusal::Log(bootlog::Unfit::Unfinished("x".to_string())).about_the_boot());
        assert!(Refusal::Silent { what: "come back", secs: 420 }.about_the_boot());
        assert!(Refusal::FellThrough("BootNext still reads 0002".to_string()).about_the_boot());
        assert!(!Refusal::Install("it carries no log".to_string()).about_the_boot());
        assert!(!Refusal::Sudo("a password is required".to_string()).about_the_boot());
        assert!(!Refusal::Landed { what: "dd".to_string(), want: 1, got: 2 }.about_the_boot());
        assert!(!Refusal::NoHome.about_the_boot());
    }

    #[test]
    fn dd_is_believed_only_where_it_states_a_count() {
        let real = "44+0 records in\n44+0 records out\n\
                    184549376 bytes (185 MB, 176 MiB) copied, 12.3169 s, 15.0 MB/s\n";
        assert_eq!(dd_said(real), Some(DdSaid { bytes: 184_549_376, secs: 12.3169 }));
        let short = "10+0 records in\n10+0 records out\n\
                     41943040 bytes (42 MB, 40 MiB) copied, 3.1 s, 13.5 MB/s\n";
        assert_eq!(dd_said(short), Some(DdSaid { bytes: 41_943_040, secs: 3.1 }));
        assert_eq!(dd_said("44+0 records in\n44+0 records out\n"), None);
        assert_eq!(dd_said(""), None);
        assert_eq!(dd_said("41943040 bytes (42 MB, 40 MiB) copied\n"), None);
    }

    #[test]
    fn the_lid_policy_is_systemds_effective_one() {
        // The commented compile-time defaults `cat-config` prints first, then
        // the drop-in that overrides them.
        let ignored = "[Login]\n#HandleLidSwitch=suspend\n#HandleLidSwitchExternalPower=suspend\n\
                       HandleLidSwitch=ignore\nHandleLidSwitchExternalPower=ignore\n\
                       HandleLidSwitchDocked=ignore\n";
        assert_eq!(lid_policy(ignored), Ok(()));
        // The spacing systemd accepts and a hand-written parser rejects.
        let spaced = "[Login]\nHandleLidSwitch = ignore\nHandleLidSwitchExternalPower = ignore \n\
                      HandleLidSwitchDocked =ignore\n";
        assert_eq!(lid_policy(spaced), Ok(()));
        // A later drop-in overriding an earlier one, which is what reading one
        // file instead of the merged configuration would miss.
        let overridden = format!("{ignored}HandleLidSwitchDocked=suspend\n");
        assert_eq!(
            lid_policy(&overridden),
            Err(Refusal::Lid { key: "HandleLidSwitchDocked", got: "suspend".to_string() })
        );
        assert!(matches!(lid_policy("[Login]\n"), Err(Refusal::Lid { .. })));
    }

    #[test]
    fn the_arguments_name_the_t14_bench_by_default() {
        let args = Args::parse(&[]).unwrap();
        assert_eq!((args.target.user.as_str(), args.target.host.as_str()), ("toyos", "bench-t14"));
        assert_eq!(args.target.internal.whole(), "/dev/nvme0n1");
        assert!(!args.dry_run);

        let words: Vec<String> = ["--dry-run", "--host", "runner@box"].iter().map(|w| (*w).to_string()).collect();
        let args = Args::parse(&words).unwrap();
        assert!(args.dry_run);
        assert_eq!(args.target.user, "runner");
        assert_eq!(args.target.host, "box");

        assert!(matches!(Args::parse(&["--device".to_string(), "/dev/sdb".to_string()]), Err(Refusal::Usage(_))));
        assert!(matches!(Args::parse(&["--image".to_string()]), Err(Refusal::Usage(_))));
        assert!(matches!(Args::parse(&["--flash".to_string()]), Err(Refusal::Usage(_))));
    }

    /// The lateness comes off the line the kernel writes, counted from the
    /// record of arming the bound, and a boot that wrote none measures nothing
    /// rather than zero.
    #[test]
    fn the_deadlines_lateness_is_counted_from_its_arm() {
        let said = format!(
            "| {}: a bound of 120000 ms, reached at 120064 ms, with this machine in `complete`. \
             The tail of the log ring follows.\n",
            bootlog::DEADLINE_EXPIRED
        );
        let log = format!(
            "[2026-09-29 18:22:39  0.060 cpu0 kernel] {}120000 ms, after which this kernel seals a \
             WEDGED record and writes the reset register itself\n",
            bootlog::DEADLINE_ARMED
        );
        assert_eq!(deadline_lateness_ms(&said, &log), Some(Ok(4)));
        assert_eq!(deadline_lateness_ms("Previous boot's panic: nothing of the kind\n", &log), None);
        // An expiry before its bound is a reading, not an absence.
        let early = said.replace("reached at 120064", "reached at 119000");
        assert_eq!(deadline_lateness_ms(&early, &log), Some(Ok(-1060)));
        // An expiry with no arm to count from, or one not in the kernel's shape.
        assert!(matches!(deadline_lateness_ms(&said, "nothing armed\n"), Some(Err(_))));
        let unread = said.replace("reached at", "reached by");
        assert!(matches!(deadline_lateness_ms(&unread, &log), Some(Err(_))));
    }

    /// The one boot judged by its sealed record and not by the shutdown's word:
    /// both halves refuse, and the pass needs both.
    #[test]
    fn an_image_armed_to_stop_itself_owes_a_record_and_no_reboot() {
        let sealed = format!(
            "Previous boot's panic: the last boot read WEDGED\n| {}: a bound of 120000 ms, \
             reached at 120153 ms, with this machine in `complete`.\n",
            bootlog::DEADLINE_EXPIRED
        );
        let booted = "[ 1.198 cpu0 kernel] Boot: complete (1198ms)\n";
        assert_eq!(wedged_boot(false, &sealed, booted), Ok(1198));

        let done = format!(
            "Black box: {}\n| {}[ 1.300 cpu0 kernel] {}\n",
            bootlog::HANDED_BACK,
            bootlog::LOG_TAIL,
            bootlog::REBOOTING
        );
        let why = wedged_boot(false, &done, booted).unwrap_err().to_string();
        assert!(why.contains("nothing about it was wedged"), "{why}");

        let why = wedged_boot(false, "no record here\n", booted).unwrap_err().to_string();
        assert!(why.contains("reports neither of this kernel's bounds"), "{why}");

        let why = wedged_boot(false, &sealed, "nothing at all\n").unwrap_err().to_string();
        assert!(why.contains("no `Boot: complete`"), "{why}");
    }

    /// A wedge under the chipset watchdog ends in the chipset's reset, which
    /// only the pass after it reports, and only an image armed with the
    /// watchdog may end that way.
    #[test]
    fn a_watched_wedge_is_ended_by_the_chipsets_reset() {
        let booted = "[ 1.300 cpu0 kernel] Boot: complete (1300ms)\n";
        let said = format!("watchdog: watchdog_issue_reset=1 fired=0, so {}\n", toyos_tco::fch::RESET_SAID);
        let after = format!(
            "{}\n{said}Previous boot's panic: the page still reads ARMED, so that kernel died \
             without reaching either its panic path or its shutdown\n",
            bootlog::SEPARATOR
        );
        assert_eq!(wedged_boot(true, &after, booted), Ok(1300));
        let why = wedged_boot(false, &after, booted).unwrap_err().to_string();
        assert!(why.contains("reports neither of this kernel's bounds"), "{why}");
        // The first pass's survey speaks of the boot before this one.
        let before = format!("{said}{}\nPrevious boot's panic: the page still reads ARMED\n", bootlog::SEPARATOR);
        let why = wedged_boot(true, &before, booted).unwrap_err().to_string();
        assert!(why.contains("reports neither of this kernel's bounds"), "{why}");
        // A kernel that panicked and then went unfed: the chipset's reset
        // ends it, and the page says it was a panic.
        let panicked = format!(
            "{}\n{said}Previous boot's panic: 812 bytes off 0x8000000\n",
            bootlog::SEPARATOR
        );
        let why = wedged_boot(true, &panicked, booted).unwrap_err().to_string();
        assert!(why.contains("reports neither of this kernel's bounds"), "{why}");
    }

    /// **The record's own first line is what says which bound ended the boot.**
    /// Both of them seal `WEDGED` and either can reach a staged machine first,
    /// so a judge that knew only one read the T14's `hard-lockup-probe` boot —
    /// which the detector ended exactly as designed — as a failure.
    #[test]
    fn either_bounds_record_is_a_wedged_boots_pass() {
        let booted = "[ 1.200 cpu0 kernel] Boot: complete (1200ms)\n";
        let locked = format!(
            "Previous boot's panic: the last boot read WEDGED\n| {}: cpu7 has taken no interrupt \
             for 60004 ms, with `IF` clear at every sample in that span. Its bound is 60000 ms.\n\
             |   pc=0xffff800060a5903f  <kernel::sync::Lock<..>>::lock+0x12f\n",
            bootlog::LOCKED_UP
        );
        assert_eq!(wedged_boot(false, &locked, booted), Ok(1200));
        assert_eq!(lockup_lateness_ms(&locked), Some(Ok(4)));
        // Each bound's lateness is read off its own record and off no other.
        assert_eq!(deadline_lateness_ms(&locked, booted), None);
        let expired = format!(
            "| {}: a bound of 120000 ms, reached at 120153 ms, with this machine in `complete`.\n",
            bootlog::DEADLINE_EXPIRED
        );
        assert_eq!(lockup_lateness_ms(&expired), None);
        // A sample that landed before the bound is a reading, not an absence.
        let early = locked.replace("for 60004 ms", "for 59000 ms");
        assert_eq!(lockup_lateness_ms(&early), Some(Ok(-1000)));
        let unread = locked.replace("Its bound is", "Its bound was");
        assert!(matches!(lockup_lateness_ms(&unread), Some(Err(_))));
    }

    /// The outside judge on what the T14's first unclean reset mid-append left
    /// (r5 at d36af3872): accepted for a boot its deadline ends, refused for
    /// one that handed back, and a short chain refused for both.
    #[test]
    fn a_deadline_boot_s_volume_may_hold_what_a_stop_leaves_and_no_more() {
        use toyos_fat32_check::Complaint;
        let left = [
            Complaint::ChainTooLong { path: "/2026-10-21-094046_0008.log".into(), size: 200_402, held: 394, needed: 392 },
            Complaint::FsInfoFreeCount { declared: 53_763, counted: 53_761 },
        ];
        let deadline = ["boot-deadline=40000".to_string()];
        let ordinary = ["boot-deadline=120000".to_string()];
        assert!(fat32_verdict(&deadline, &left).is_ok());
        assert!(fat32_verdict(&ordinary, &left).is_err());
        let short = [Complaint::ChainTooShort { path: "/a.log".into(), size: 200_402, held: 390, needed: 392 }];
        assert!(fat32_verdict(&deadline, &short).is_err());
    }

    /// A boot given a deadline below its runner's least bound is ended by it on
    /// purpose, as `the_disk_survives_a_reset_under_load`'s is: judged by the
    /// record the deadline sealed, never by a supervisor's stop it was never
    /// going to reach. The same boot under the ordinary deadline is the
    /// unfinished boot it would be, and one that handed back was not ended
    /// by its bound.
    #[test]
    fn a_boot_its_deadline_ends_on_purpose_is_judged_by_the_record_it_sealed() {
        let loader = format!(
            "Loader log: the kernel handoff begins, so this file ends here\n{}\n\
             ToyOS Bootloader 1.0\n\
             Previous boot's panic: the last boot read WEDGED, so a bound of its own ended it and this chain ends here\n\
             | {}: a bound of 40000 ms, reached at 52397 ms, with this machine in `complete`.\n\
             Loader log: the last boot is accounted for, so this pass resets the machine\n",
            bootlog::SEPARATOR,
            bootlog::DEADLINE_EXPIRED
        );
        let log = "[2026-10-21 09:02:08 12.400 cpu0 kernel] Boot: complete (12400ms)\n\
                   [2026-10-21 09:02:48 52.332 test-runner pid=12] stick_load: 32204 lines said\n";
        let armed = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_string()).collect() };
        assert!(ended_by_its_deadline(&armed(&["boot-deadline=40000"])));
        assert!(!ended_by_its_deadline(&armed(&["boot-deadline=120000"])));
        assert_eq!(boot_verdict(&armed(&["boot-deadline=40000"]), &loader, log), Ok(12400));
        assert!(matches!(
            boot_verdict(&armed(&["boot-deadline=120000"]), &loader, log),
            Err(Refusal::Log(bootlog::Unfit::Unfinished(_)))
        ));
        let handed_back = format!(
            "{}Black box: {} at 2026-10-21-090207\n| {}[52.400 cpu0 kernel] {}\n",
            loader,
            bootlog::HANDED_BACK,
            bootlog::LOG_TAIL,
            bootlog::REBOOTING
        );
        assert!(matches!(
            boot_verdict(&armed(&["boot-deadline=40000"]), &handed_back, log),
            Err(Refusal::Wedge { .. })
        ));
    }

    /// The foreign-record arm's pass after the reset: the hang it stages is its
    /// verdict's premise, the record it cleared says the stop finished, and the
    /// same pass under any other arm is the hang refusal.
    #[test]
    fn the_foreign_record_arm_reaches_its_judge_through_the_hang_it_stages() {
        let loader = format!(
            "Loader log: the kernel handoff begins, so this file ends here\n{}\n\
             ToyOS Bootloader 1.0\n\
             Black box: 0x8000000 held a DONE record another image left in this memory ([63, 12, \
             b3, 41, e8, dd, 26, 4a, b9, d6, e7, 17, 3a, 83, d6, 18], and this stick is [9c, 12, \
             b3, 41, e8, dd, 26, 4a, b9, d6, e7, 17, 3a, 83, d6, 18]), armed at \
             2026-09-29-103651. It has been cleared and this pass boots its kernel\n\
             Boot attempts: this image has had the machine 1 time(s) without reporting; now 0\n\
             Boot attempts: the previous boot of this image never reported; the machine is \
             handed back\n\
             Loader log: the last boot is accounted for, so this pass resets the machine\n",
            bootlog::SEPARATOR
        );
        let log = "[2026-09-29 10:36:53  1.171 cpu0 kernel] Boot: complete (1171ms)\n\
                   [2026-09-29 10:36:53  1.186 supervisor] supervisor: power: the machine stops, and logkeeper makes \
                   the log whole first (Reboot)\n";
        let armed = |names: &[&str]| -> Vec<String> { names.iter().map(|n| (*n).to_string()).collect() };
        assert_eq!(
            boot_verdict(&armed(&[FOREIGN_RECORD_ARM, "boot-deadline=120000"]), &loader, log),
            Ok(1171)
        );
        assert_eq!(
            boot_verdict(&armed(&["boot-deadline=120000"]), &loader, log),
            Err(Refusal::HungWithoutARecord)
        );
        // A stop that panicked or that a bound ended seals under the foreign
        // identity too, and hands the machine back the same way.
        for state in ["PANIC", "WEDGED"] {
            let ended = loader.replace("held a DONE record", &format!("held a {state} record"));
            assert_eq!(
                boot_verdict(&armed(&[FOREIGN_RECORD_ARM, "boot-deadline=120000"]), &ended, log),
                Err(Refusal::Log(bootlog::Unfit::NoForeignDone))
            );
            // The pass before the handoff cleared a stale foreign `DONE`.
            let stale = format!(
                "Black box: 0x8000000 held a DONE record another image left in this memory\n{ended}"
            );
            assert_eq!(
                boot_verdict(&armed(&[FOREIGN_RECORD_ARM, "boot-deadline=120000"]), &stale, log),
                Err(Refusal::Log(bootlog::Unfit::NoForeignDone))
            );
        }
        // A chain that never went round has no pass after the reset to read.
        let unreturned = format!(
            "Black box: 0x8000000 held a DONE record another image left in this memory\n{}\n",
            bootlog::LOADER_LAST_LINE
        );
        assert_eq!(
            boot_verdict(&armed(&[FOREIGN_RECORD_ARM, "boot-deadline=120000"]), &unreturned, log),
            Err(Refusal::Log(bootlog::Unfit::NoForeignDone))
        );
    }
}
