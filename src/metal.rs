//! `toyos-metal` — the loop that flashes ToyOS to the T14's stick, boots it
//! once, and reads the verdict back off the log partition.
//!
//! It runs on the development host and reaches the machine over `ssh`. The
//! machine runs as root only what `/etc/sudoers.d/toyos-metal` permits, and
//! that file is rendered from the same [`Target::permitted`] list
//! [`Root::new`] admits an argv against — so "every command this loop runs as
//! root is one of a fixed list" holds by construction rather than by review.
//!
//! Two admission checks stand before any write, from
//! `issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`: the image
//! is a whole number of 512-byte sectors carrying `EFI PART` in its final one,
//! and the disk answers with the identity this loop was given. A node that is
//! not `/dev/sd<letter>` cannot be written down at all, which is how the
//! machine's own NVMe stays unnameable.
//!
//! **`/log` carries kernel records only**: a program's stdout is a console
//! write (`issues/diagnostics/the-log-staged-three-things-it-never-built.md`)
//! and the T14 has no serial port, so the kernel's own boot record is the
//! verdict a metal run can read there today.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How long `ssh` waits for the machine to accept a connection.
const CONNECT_SECS: u64 = 10;

/// How long the machine has to answer `ssh` again after `reboot`.
///
/// Five minutes is the watchdog budget the owner set for a wedged ToyOS and
/// the rest is a firmware pass plus a Linux boot: a derivation from that
/// budget, not a measurement.
pub const RETURN_SECS: u64 = 420;

/// How often the wait asks.
const POLL_SECS: u64 = 5;

/// The sector every disk and every GPT in this loop is counted in.
const SECTOR: u64 = 512;

/// The absolute paths `which` answered on the machine; sudo matches a path and
/// not a name, so nothing here is spelled relatively.
const DD: &str = "/usr/bin/dd";
const EFIBOOTMGR: &str = "/usr/bin/efibootmgr";
const MOUNT: &str = "/usr/bin/mount";
const UMOUNT: &str = "/usr/bin/umount";
const REBOOT: &str = "/usr/sbin/reboot";
const TRUE: &str = "/usr/bin/true";

/// What the firmware loads off the stick's ESP.
const LOADER: &str = r"\EFI\BOOT\BOOTX64.EFI";

/// The three logind keys that keep a closed-lid machine awake, and the drop-in
/// that sets them.
const LID_KEYS: &[&str] =
    &["HandleLidSwitch", "HandleLidSwitchExternalPower", "HandleLidSwitchDocked"];
const LID_CONF: &str = "/etc/systemd/logind.conf.d/50-headless-runner.conf";

/// Every way this loop refuses, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A device node that is not `/dev/sd<letter>`.
    Node(String),
    /// A word that cannot be written into a sudoers rule unambiguously.
    Word(String),
    /// The disk did not answer with the identity this loop was given.
    Identity { field: &'static str, want: String, got: String },
    /// A file the loop needed could not be read.
    File { path: String, why: String },
    /// The image's length is not a whole number of 512-byte sectors.
    Sectors { bytes: u64 },
    /// The final sector does not begin `EFI PART`: the backup table is gone.
    BackupHeader { at: u64, saw: String },
    /// The image's table did not parse.
    Table(String),
    /// The table does not hold exactly one partition of a type the loop needs.
    Partitions { what: &'static str, matched: u32 },
    /// The image puts a partition somewhere the installed rule does not name.
    PartitionIndex { what: &'static str, want: u32, got: u32 },
    /// An argv no permitted pattern admits: the sudoers rule would have to grow.
    Unpermitted(String),
    /// `efibootmgr` named no four-hex-digit boot entry.
    BootEntry(String),
    /// `sudo -n` could not run without a password.
    Sudo(String),
    /// A lid key that no longer reads `ignore`, which is what keeps the machine up.
    Lid { key: &'static str, got: String },
    /// A command on the machine failed.
    Remote { what: String, status: String, stderr: String },
    /// The machine did not answer `ssh` again inside the bound.
    Silent { secs: u64 },
    /// The loop was asked for something it does not do.
    Usage(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Node(got) => write!(
                f,
                "{got:?} is not /dev/sd<letter>; this loop writes a USB disk and can name no other"
            ),
            Self::Word(got) => {
                write!(f, "{got:?} cannot be written into a sudoers command line unambiguously")
            }
            Self::Identity { field, want, got } => {
                write!(f, "the disk's {field} reads {got:?} where this loop was given {want:?}")
            }
            Self::File { path, why } => write!(f, "{path}: {why}"),
            Self::Sectors { bytes } => write!(
                f,
                "the image is {bytes} bytes, which is not a whole number of {SECTOR}-byte sectors"
            ),
            Self::BackupHeader { at, saw } => write!(
                f,
                "the image's final sector at byte {at} begins {saw:?} and not \"EFI PART\": a \
                 healthy primary table hides a missing backup"
            ),
            Self::Table(why) => write!(f, "the image carries no readable partition table: {why}"),
            Self::Partitions { what, matched } => {
                write!(f, "the image carries {matched} {what} partitions and this loop needs one")
            }
            Self::PartitionIndex { what, want, got } => write!(
                f,
                "the image puts {what} at partition {got} where the installed rule names {want}"
            ),
            Self::Unpermitted(argv) => write!(
                f,
                "no permitted pattern admits `{argv}`, so the machine's rule does not either; \
                 widen Target::permitted and reinstall, or do not run it"
            ),
            Self::BootEntry(saw) => {
                write!(f, "efibootmgr named no four-hex-digit boot entry: {saw:?}")
            }
            Self::Sudo(saw) => {
                write!(f, "`sudo -n` on the machine did not answer: {saw}. Install the rule first")
            }
            Self::Lid { key, got } => write!(
                f,
                "logind's {key} reads {got:?} and not \"ignore\": the machine suspends on a \
                 closed lid and no loop survives that"
            ),
            Self::Remote { what, status, stderr } => {
                write!(f, "{what} on the machine {status}: {stderr}")
            }
            Self::Silent { secs } => {
                write!(f, "the machine did not answer ssh again within {secs} s")
            }
            Self::Usage(why) => write!(f, "{why}"),
        }
    }
}

/// A SCSI disk node, `/dev/sd` and one lower-case letter.
///
/// **An NVMe node cannot be written down.** The T14's Ubuntu install is on
/// `nvme0n1`, and this type is what keeps that true whatever a caller passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node(u8);

impl Node {
    pub fn parse(path: &str) -> Result<Self, Refusal> {
        let letter = path.strip_prefix("/dev/sd").and_then(|rest| {
            let [byte] = rest.as_bytes() else { return None };
            byte.is_ascii_lowercase().then_some(*byte)
        });
        letter.map(Node).ok_or_else(|| Refusal::Node(path.to_string()))
    }

    pub fn whole(&self) -> String {
        format!("/dev/sd{}", self.0 as char)
    }

    pub fn partition(&self, index: u32) -> String {
        format!("{}{index}", self.whole())
    }

    /// Where `/sys` answers for this disk, which is the identity check's source.
    pub fn sysfs(&self) -> String {
        format!("/sys/class/block/sd{}", self.0 as char)
    }
}

/// What a disk has to answer before this loop writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    pub sectors: u64,
    pub vendor: &'static str,
    pub model: &'static str,
}

/// The stick, as `/sys/class/block/sda` on the T14 answered: 60,062,500
/// sectors of removable SanDisk Ultra.
pub const STICK: Expect = Expect { sectors: 60_062_500, vendor: "SanDisk", model: "Ultra" };

/// What `/sys/class/block/<disk>/` answered, in the order
/// [`Target::identity_query`] asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub sectors: u64,
    pub vendor: String,
    pub model: String,
    pub removable: bool,
}

impl Identity {
    /// The four `sysfs` files as `cat` concatenates them. The trailing padding
    /// is the SCSI inquiry's own: vendor and model are fixed-width fields.
    pub fn parse(text: &str) -> Result<Self, Refusal> {
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

    pub fn bytes(&self) -> u64 {
        self.sectors * SECTOR
    }

    pub fn check(&self, want: &Expect) -> Result<(), Refusal> {
        let no = |field, want: String, got: String| Refusal::Identity { field, want, got };
        if self.sectors != want.sectors {
            return Err(no("size", want.sectors.to_string(), self.sectors.to_string()));
        }
        if self.vendor != want.vendor {
            return Err(no("vendor", want.vendor.to_string(), self.vendor.clone()));
        }
        if self.model != want.model {
            return Err(no("model", want.model.to_string(), self.model.clone()));
        }
        if !self.removable {
            return Err(no("removable", "1".to_string(), "0".to_string()));
        }
        Ok(())
    }
}

/// One word of a permitted root command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Word {
    Is(String),
    /// The four hex digits `efibootmgr` numbers a boot entry with.
    Hex4,
}

impl Word {
    /// A literal word this loop will neither shell-quote wrongly nor let sudo
    /// read as a glob. Whitespace and the metacharacters are refused rather
    /// than escaped: no command here needs one.
    fn literal(text: &str) -> Result<Self, Refusal> {
        let bad = |c: char| {
            c.is_whitespace() || matches!(c, '*' | '?' | '[' | ']' | '#' | '!' | '"' | '\'')
        };
        if text.is_empty() || text.chars().any(bad) {
            return Err(Refusal::Word(text.to_string()));
        }
        Ok(Self::Is(text.to_string()))
    }

    fn admits(&self, got: &str) -> bool {
        match self {
            Self::Is(text) => text == got,
            Self::Hex4 => got.len() == 4 && got.bytes().all(|b| b.is_ascii_hexdigit()),
        }
    }

    /// The word as `sudoers(5)` reads it: `,`, `:`, `=` and `\` carry meaning
    /// in a command argument there, and the EFI loader path is backslashes.
    fn sudoers(&self) -> String {
        match self {
            Self::Is(text) => {
                let mut out = String::new();
                for c in text.chars() {
                    if matches!(c, ',' | ':' | '=' | '\\') {
                        out.push('\\');
                    }
                    out.push(c);
                }
                out
            }
            Self::Hex4 => "[0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f]".to_string(),
        }
    }
}

/// One root command line the machine's sudoers rule permits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern(Vec<Word>);

impl Pattern {
    /// Every word literal but the ones `hex` names by position.
    fn new(words: &[&str], hex: &[usize]) -> Result<Self, Refusal> {
        words
            .iter()
            .enumerate()
            .map(|(at, word)| if hex.contains(&at) { Ok(Word::Hex4) } else { Word::literal(word) })
            .collect::<Result<Vec<_>, _>>()
            .map(Pattern)
    }

    fn admits(&self, argv: &[String]) -> bool {
        self.0.len() == argv.len() && self.0.iter().zip(argv).all(|(w, got)| w.admits(got))
    }

    pub fn sudoers(&self) -> String {
        self.0.iter().map(Word::sudoers).collect::<Vec<_>>().join(" ")
    }
}

/// A command this loop may run as root on the machine.
///
/// Constructed only through [`Root::new`], which refuses an argv no
/// [`Target::permitted`] pattern admits: the driver cannot form a command the
/// installed rule would have to be widened for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root(Vec<String>);

impl Root {
    pub fn new(target: &Target, argv: &[&str]) -> Result<Self, Refusal> {
        let argv: Vec<String> = argv.iter().map(|w| (*w).to_string()).collect();
        if target.permitted()?.iter().any(|p| p.admits(&argv)) {
            Ok(Self(argv))
        } else {
            Err(Refusal::Unpermitted(argv.join(" ")))
        }
    }

    /// The command as the machine's login shell must receive it.
    pub fn remote(&self) -> String {
        let words: Vec<String> = self.0.iter().map(|w| shell_word(w)).collect();
        format!("sudo -n {}", words.join(" "))
    }

    pub fn argv(&self) -> &[String] {
        &self.0
    }
}

/// One word as the machine's login shell must receive it: `ssh` hands the
/// whole command to that shell, and what `sudo` matches is the argv the shell
/// produced.
fn shell_word(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Where the loop runs and what it may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub user: String,
    pub host: String,
    pub key: PathBuf,
    pub node: Node,
    /// The image's ESP, the partition the firmware is pointed at.
    pub esp_part: u32,
    /// The image's TOYOS-LOG partition, the one channel out of a metal boot.
    pub log_part: u32,
    /// Where that partition is mounted on the machine; under the account's own
    /// home, so no root command creates it.
    pub mount: String,
    /// The boot entry's label in the firmware's list.
    pub label: String,
}

impl Target {
    /// The T14 as the track names it, with the runner key this host holds.
    pub fn t14() -> Self {
        Self {
            user: "t14".to_string(),
            host: "t14".to_string(),
            key: home().join(".ssh/id_ed25519_toyos_runner"),
            node: Node(b'a'),
            esp_part: 1,
            log_part: 3,
            mount: "/home/t14/toyos-log".to_string(),
            label: "ToyOS".to_string(),
        }
    }

    /// Every root command line the machine's rule permits, and the whole of it.
    pub fn permitted(&self) -> Result<Vec<Pattern>, Refusal> {
        let node = self.node.whole();
        let of = format!("of={node}");
        let log = self.node.partition(self.log_part);
        let part = self.esp_part.to_string();
        Ok(vec![
            Pattern::new(&[DD, &of, "bs=4M", "conv=fsync", "status=none"], &[])?,
            Pattern::new(
                &[
                    EFIBOOTMGR,
                    "--create",
                    "--disk",
                    &node,
                    "--part",
                    &part,
                    "--label",
                    &self.label,
                    "--loader",
                    LOADER,
                ],
                &[],
            )?,
            Pattern::new(&[EFIBOOTMGR, "--bootnext", "0000"], &[2])?,
            Pattern::new(&[MOUNT, "-o", "ro", &log, &self.mount], &[])?,
            Pattern::new(&[UMOUNT, &self.mount], &[])?,
            Pattern::new(&[REBOOT], &[])?,
            Pattern::new(&[TRUE], &[])?,
        ])
    }

    /// The rule itself, one line per permitted command, so a reader on the
    /// machine sees the same list this file holds.
    pub fn sudoers(&self) -> Result<String, Refusal> {
        let mut out = String::from("# toyos-metal: the whole of what the metal loop runs as root.\n");
        for pattern in self.permitted()? {
            out.push_str(&format!("{} ALL=(root) NOPASSWD: {}\n", self.user, pattern.sudoers()));
        }
        Ok(out)
    }

    fn root(&self, argv: &[&str]) -> Result<Root, Refusal> {
        Root::new(self, argv)
    }

    pub fn flash(&self) -> Result<Root, Refusal> {
        let of = format!("of={}", self.node.whole());
        self.root(&[DD, &of, "bs=4M", "conv=fsync", "status=none"])
    }

    pub fn create_entry(&self) -> Result<Root, Refusal> {
        let node = self.node.whole();
        let part = self.esp_part.to_string();
        self.root(&[
            EFIBOOTMGR,
            "--create",
            "--disk",
            &node,
            "--part",
            &part,
            "--label",
            &self.label,
            "--loader",
            LOADER,
        ])
    }

    pub fn bootnext(&self, entry: &str) -> Result<Root, Refusal> {
        self.root(&[EFIBOOTMGR, "--bootnext", entry])
    }

    pub fn mount_log(&self) -> Result<Root, Refusal> {
        let log = self.node.partition(self.log_part);
        self.root(&[MOUNT, "-o", "ro", &log, &self.mount])
    }

    pub fn umount_log(&self) -> Result<Root, Refusal> {
        self.root(&[UMOUNT, &self.mount])
    }

    pub fn reboot(&self) -> Result<Root, Refusal> {
        self.root(&[REBOOT])
    }

    pub fn probe(&self) -> Result<Root, Refusal> {
        self.root(&[TRUE])
    }

    /// The one read that decides whether anything is written.
    pub fn identity_query(&self) -> String {
        let at = self.node.sysfs();
        format!("cat {at}/size {at}/device/vendor {at}/device/model {at}/removable")
    }

    /// `ssh`'s argv without its own name, so a test reads the whole line.
    pub fn ssh_argv(&self, command: &str) -> Vec<String> {
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

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()))
}

/// An image that has passed the pre-flash admission check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flashable {
    pub path: PathBuf,
    pub bytes: u64,
    pub sectors: u64,
    pub esp_part: u32,
    pub log_part: u32,
}

/// Microsoft Basic Data, the type the log partition carries so that a Mac, a
/// Windows box and a Linux box all mount it on plug-in.
const BASIC_DATA: toyos_gpt::Guid = toyos_gpt::Guid::from_fields(
    0xEBD0_A0A2,
    0xB9E5,
    0x4433,
    [0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26, 0x99, 0xC7],
);

/// Items 1 and 2 of the pre-flash gate, against the file about to be written.
///
/// The whole-sector and backup-header checks are
/// `issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`'s. The
/// partition checks are what ties the artifact to the rule already installed on
/// the machine: a table that moved either partition refuses here rather than
/// flashing into a mount the rule does not name.
pub fn admit(path: &Path, target: &Target) -> Result<Flashable, Refusal> {
    let unreadable = |why: String| Refusal::File { path: path.display().to_string(), why };
    let mut file = std::fs::File::open(path).map_err(|e| unreadable(e.to_string()))?;
    let bytes = file.metadata().map_err(|e| unreadable(e.to_string()))?.len();
    if bytes == 0 || bytes % SECTOR != 0 {
        return Err(Refusal::Sectors { bytes });
    }

    let at = bytes - SECTOR;
    let mut last = [0u8; SECTOR as usize];
    file.seek(SeekFrom::Start(at)).map_err(|e| unreadable(e.to_string()))?;
    file.read_exact(&mut last).map_err(|e| unreadable(e.to_string()))?;
    if &last[..8] != b"EFI PART" {
        let saw = String::from_utf8_lossy(&last[..8]).to_string();
        return Err(Refusal::BackupHeader { at, saw });
    }

    let esp_part = one_partition(&mut file, toyos_gpt::Guid::EFI_SYSTEM, "ESP")?;
    let log_part = one_partition(&mut file, BASIC_DATA, "TOYOS-LOG")?;
    one_partition(&mut file, toyos_gpt::Guid::TOYOS_ROOT, "TOYOS-ROOT")?;
    if esp_part != target.esp_part {
        let what = "the ESP";
        return Err(Refusal::PartitionIndex { what, want: target.esp_part, got: esp_part });
    }
    if log_part != target.log_part {
        let what = "TOYOS-LOG";
        return Err(Refusal::PartitionIndex { what, want: target.log_part, got: log_part });
    }
    Ok(Flashable { path: path.to_path_buf(), bytes, sectors: bytes / SECTOR, esp_part, log_part })
}

/// The one partition of type `guid`, numbered as Linux numbers it: the entry
/// index plus one.
fn one_partition(
    file: &mut std::fs::File,
    guid: toyos_gpt::Guid,
    what: &'static str,
) -> Result<u32, Refusal> {
    let mut out = [BLANK; 2];
    let scan = toyos_gpt::locate_type(&mut crate::image::FileSectors(file), guid, &mut out)
        .map_err(|e| Refusal::Table(format!("{e:?}")))?;
    if scan.matched != 1 {
        return Err(Refusal::Partitions { what, matched: scan.matched });
    }
    Ok(out[0].index + 1)
}

const BLANK: toyos_gpt::Partition = toyos_gpt::Partition {
    index: 0,
    type_guid: toyos_gpt::Guid::ZERO,
    unique_guid: toyos_gpt::Guid::ZERO,
    first_lba: 0,
    last_lba: 0,
};

/// What the log a boot left says about that boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass { boot_ms: u64, test: Option<String> },
    Fail(String),
}

/// The kernel's own boot record, the one line a metal boot is certain to
/// leave: `Boot: complete (123ms)`.
pub fn boot_millis(log: &str) -> Option<u64> {
    let tail = log.lines().find_map(|line| line.split("Boot: complete (").nth(1))?;
    tail.split("ms)").next()?.parse().ok()
}

/// The marker `userland/test-runner` writes when a job ends. The last one
/// wins: a boot may run a name more than once.
pub fn test_end(log: &str, test: &str) -> Option<String> {
    let head = format!("===TEST_END {test} ");
    log.lines()
        .filter_map(|line| Some(line.split(&head).nth(1)?.split("===").next()?.to_string()))
        .next_back()
}

/// Pass or fail for one boot's log.
pub fn verdict(log: &str, test: Option<&str>) -> Verdict {
    let Some(boot_ms) = boot_millis(log) else {
        return Verdict::Fail("the log carries no `Boot: complete` record".to_string());
    };
    let Some(test) = test else {
        return Verdict::Pass { boot_ms, test: None };
    };
    match test_end(log, test) {
        Some(body) if body == "exit=0" => Verdict::Pass { boot_ms, test: Some(test.to_string()) },
        Some(body) => Verdict::Fail(format!("{test} ended {body}")),
        None => Verdict::Fail(format!("the log carries no ===TEST_END {test}=== marker")),
    }
}

/// The four-digit id `efibootmgr` prints beside `label`, out of a
/// `Boot0027* ToyOS<tab>HD(…)` listing.
pub fn entry_labelled(listing: &str, label: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let rest = line.strip_prefix("Boot")?;
        let id = rest.get(..4)?;
        if !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let named = rest.get(4..)?;
        let named = named.strip_prefix('*').unwrap_or(named);
        (named.trim_start().split('\t').next()?.trim() == label).then(|| id.to_string())
    })
}

/// The loop, over one target.
pub struct Driver {
    pub target: Target,
    /// Print each write this run would make instead of making it. Every read
    /// still happens: a dry run that skipped the identity check would be
    /// rehearsing a different loop.
    pub dry_run: bool,
}

impl Driver {
    /// One `ssh`, unprivileged: its output back, or a refusal.
    pub fn ssh(&self, what: &str, command: &str) -> Result<String, Refusal> {
        let out = Command::new("ssh")
            .args(self.target.ssh_argv(command))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| unstarted(what, &e))?;
        answer(what, &out)
    }

    /// One permitted root command, with `stdin` streamed into it. Announced
    /// either way, and run only when this is not a dry run.
    pub fn as_root(
        &self,
        what: &str,
        root: &Root,
        stdin: Option<&Path>,
    ) -> Result<String, Refusal> {
        let line = root.remote();
        println!("  {} {line}", if self.dry_run { "would run:" } else { "run:" });
        if self.dry_run {
            return Ok(String::new());
        }
        let mut command = Command::new("ssh");
        command.args(self.target.ssh_argv(&line));
        match stdin {
            Some(path) => {
                let file = std::fs::File::open(path)
                    .map_err(|e| Refusal::File { path: path.display().to_string(), why: e.to_string() })?;
                command.stdin(Stdio::from(file));
            }
            None => {
                command.stdin(Stdio::null());
            }
        }
        let out = command.output().map_err(|e| unstarted(what, &e))?;
        answer(what, &out)
    }

    /// The loop refuses to run at all until the rule is on the machine.
    pub fn require_sudo(&self) -> Result<(), Refusal> {
        let probe = self.target.probe()?;
        self.ssh("sudo -n", &probe.remote())
            .map(|_| ())
            .map_err(|e| Refusal::Sudo(e.to_string()))
    }

    /// The lid policy the track depends on, asserted rather than assumed.
    pub fn check_lid(&self) -> Result<(), Refusal> {
        let text = self.ssh("reading the lid policy", &format!("cat {LID_CONF}"))?;
        for key in LID_KEYS {
            let got = text
                .lines()
                .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix('='))
                .map(|v| v.trim().to_string())
                .unwrap_or_default();
            if got != "ignore" {
                return Err(Refusal::Lid { key, got });
            }
        }
        Ok(())
    }

    pub fn identity(&self) -> Result<Identity, Refusal> {
        let text = self.ssh("reading the disk's identity", &self.target.identity_query())?;
        Identity::parse(&text)
    }

    /// The boot entry for the stick's ESP: the one already carrying this
    /// loop's label, or a new one.
    pub fn boot_entry(&self) -> Result<String, Refusal> {
        let listing = self.ssh("listing boot entries", EFIBOOTMGR)?;
        if let Some(id) = entry_labelled(&listing, &self.target.label) {
            return Ok(id);
        }
        let created = self.as_root("creating the boot entry", &self.target.create_entry()?, None)?;
        entry_labelled(&created, &self.target.label)
            .ok_or_else(|| Refusal::BootEntry(created.trim().to_string()))
    }

    /// Poll until the machine answers again, or the bound is spent.
    pub fn wait_for_return(&self, secs: u64) -> Result<u64, Refusal> {
        let began = std::time::Instant::now();
        while began.elapsed().as_secs() < secs {
            std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
            if self.ssh("waking check", "true").is_ok() {
                return Ok(began.elapsed().as_secs());
            }
        }
        Err(Refusal::Silent { secs })
    }

    /// Everything the stick's log partition carries, in name order. A freshly
    /// flashed volume holds one boot's files and nothing else, so this is that
    /// boot — and its newest file alone would miss the continuations `logd`
    /// rotates into.
    pub fn read_log(&self) -> Result<String, Refusal> {
        let at = shell_word(&self.target.mount);
        self.ssh("making the mount point", &format!("mkdir -p {at}"))?;
        self.as_root("mounting the log partition", &self.target.mount_log()?, None)?;
        let listing = self.ssh("listing the log", &format!("ls -1 {at}"))?;
        let mut names: Vec<&str> =
            listing.lines().map(str::trim).filter(|n| n.ends_with(".log")).collect();
        names.sort_unstable();
        let mut text = String::new();
        for name in names {
            let file = shell_word(&format!("{}/{name}", self.target.mount));
            text.push_str(&self.ssh("reading a log file", &format!("cat {file}"))?);
        }
        self.as_root("unmounting the log partition", &self.target.umount_log()?, None)?;
        Ok(text)
    }
}

fn unstarted(what: &str, e: &std::io::Error) -> Refusal {
    Refusal::Remote {
        what: what.to_string(),
        status: format!("could not be started: {e}"),
        stderr: String::new(),
    }
}

fn answer(what: &str, out: &std::process::Output) -> Result<String, Refusal> {
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).to_string());
    }
    Err(Refusal::Remote {
        what: what.to_string(),
        status: format!("exited {}", out.status),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// What the binary was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub image: PathBuf,
    pub target: Target,
    pub dry_run: bool,
    pub test: Option<String>,
    /// Where the account's password is read from, once, to install the rule.
    pub install_sudoers: Option<PathBuf>,
    pub wait_secs: u64,
}

impl Args {
    pub fn parse(args: &[String]) -> Result<Self, Refusal> {
        let mut out = Args {
            image: PathBuf::from("target/bootable.img"),
            target: Target::t14(),
            dry_run: false,
            test: None,
            install_sudoers: None,
            wait_secs: RETURN_SECS,
        };
        let mut at = 0;
        while at < args.len() {
            let flag = args[at].as_str();
            let value = || {
                args.get(at + 1)
                    .cloned()
                    .ok_or_else(|| Refusal::Usage(format!("{flag} needs a value")))
            };
            let took = match flag {
                "--dry-run" => {
                    out.dry_run = true;
                    1
                }
                "--image" => {
                    out.image = PathBuf::from(value()?);
                    2
                }
                "--host" => {
                    let host = value()?;
                    let (user, machine) = host.split_once('@').ok_or_else(|| {
                        Refusal::Usage(format!("--host wants <user>@<machine>, not {host:?}"))
                    })?;
                    out.target.user = user.to_string();
                    out.target.host = machine.to_string();
                    2
                }
                "--key" => {
                    out.target.key = PathBuf::from(value()?);
                    2
                }
                "--device" => {
                    out.target.node = Node::parse(&value()?)?;
                    2
                }
                "--test" => {
                    out.test = Some(value()?);
                    2
                }
                "--install-sudoers" => {
                    out.install_sudoers = Some(PathBuf::from(value()?));
                    2
                }
                "--wait-secs" => {
                    let secs = value()?;
                    out.wait_secs =
                        secs.parse().map_err(|_| Refusal::Usage(format!("--wait-secs: {secs:?}")))?;
                    2
                }
                other => return Err(Refusal::Usage(format!("unknown argument {other:?}"))),
            };
            at += took;
        }
        Ok(out)
    }
}

/// Put the rule on the machine, with the account's password on this one
/// command's stdin and nowhere else — never in an argument, never in output.
pub fn install_sudoers(target: &Target, password: &Path) -> Result<(), Refusal> {
    const STAGED: &str = "/tmp/toyos-metal.sudoers";
    let driver = Driver { target: target.clone(), dry_run: false };
    let rule = target.sudoers()?;

    let mut stage = Command::new("ssh")
        .args(target.ssh_argv(&format!("cat > {STAGED}")))
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| unstarted("staging the rule", &e))?;
    stage
        .stdin
        .take()
        .expect("the pipe was just asked for")
        .write_all(rule.as_bytes())
        .map_err(|e| unstarted("staging the rule", &e))?;
    let staged = stage.wait().map_err(|e| unstarted("staging the rule", &e))?;
    if !staged.success() {
        return Err(Refusal::Remote {
            what: "staging the rule".to_string(),
            status: format!("exited {staged}"),
            stderr: String::new(),
        });
    }
    driver.ssh("checking the rule", &format!("/usr/sbin/visudo -c -f {STAGED}"))?;

    let secret = std::fs::File::open(password)
        .map_err(|e| Refusal::File { path: password.display().to_string(), why: e.to_string() })?;
    let command = format!(
        "sudo -S -p '' /usr/bin/install -m 0440 -o root -g root {STAGED} \
         /etc/sudoers.d/toyos-metal"
    );
    let out = Command::new("ssh")
        .args(target.ssh_argv(&command))
        .stdin(Stdio::from(secret))
        .output()
        .map_err(|e| unstarted("installing the rule", &e))?;
    answer("installing the rule", &out)?;
    driver.ssh("clearing the staged rule", &format!("rm -f {STAGED}"))?;
    println!("installed /etc/sudoers.d/toyos-metal");
    Ok(())
}

/// The whole loop. `None` is a dry run, which reaches no verdict because it
/// wrote nothing.
pub fn run(args: &Args) -> Result<Option<Verdict>, Refusal> {
    if let Some(password) = &args.install_sudoers {
        install_sudoers(&args.target, password)?;
    }
    let driver = Driver { target: args.target.clone(), dry_run: args.dry_run };

    let image = admit(&args.image, &args.target)?;
    println!(
        "image {}: {} bytes, {} sectors, ESP p{}, log p{}",
        image.path.display(),
        image.bytes,
        image.sectors,
        image.esp_part,
        image.log_part
    );

    driver.require_sudo()?;
    driver.check_lid()?;
    let identity = driver.identity()?;
    identity.check(&STICK)?;
    println!(
        "disk {}: {} bytes, {} {}, removable",
        args.target.node.whole(),
        identity.bytes(),
        identity.vendor,
        identity.model
    );

    driver.as_root("flashing the stick", &args.target.flash()?, Some(&image.path))?;
    if driver.dry_run {
        println!("  would run: {}", args.target.create_entry()?.remote());
        println!("  would run: {}", args.target.bootnext("NNNN")?.remote());
        println!("  would run: {}", args.target.reboot()?.remote());
        println!("  would run: {}", args.target.mount_log()?.remote());
        println!("  would run: {}", args.target.umount_log()?.remote());
        println!("dry run: nothing was written and the machine was not rebooted");
        return Ok(None);
    }

    let entry = driver.boot_entry()?;
    driver.as_root("setting bootnext", &args.target.bootnext(&entry)?, None)?;
    driver.as_root("rebooting", &args.target.reboot()?, None)?;
    let back = driver.wait_for_return(args.wait_secs)?;
    println!("the machine answered ssh again after {back} s");
    let log = driver.read_log()?;
    print!("{log}");
    Ok(Some(verdict(&log, args.test.as_deref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target { key: PathBuf::from("/home/dev/.ssh/id_ed25519_toyos_runner"), ..Target::t14() }
    }

    #[test]
    fn an_nvme_node_cannot_be_written_down() {
        for name in ["/dev/nvme0n1", "/dev/nvme0n1p3", "/dev/sda1", "/dev/sdaa", "/dev/SDA", "sda"]
        {
            assert_eq!(Node::parse(name), Err(Refusal::Node(name.to_string())), "{name}");
        }
        assert_eq!(Node::parse("/dev/sda").unwrap().whole(), "/dev/sda");
        assert_eq!(Node::parse("/dev/sdb").unwrap().partition(3), "/dev/sdb3");
    }

    #[test]
    fn the_identity_is_the_stick_or_it_is_refused() {
        // Verbatim from `cat /sys/class/block/sda/{size,device/vendor,device/model,removable}`.
        let stick = Identity::parse("60062500\nSanDisk \nUltra           \n1\n").unwrap();
        assert_eq!(stick.sectors, 60_062_500);
        assert_eq!(stick.bytes(), 30_752_000_000);
        assert_eq!(stick.vendor, "SanDisk");
        assert_eq!(stick.model, "Ultra");
        assert!(stick.removable);
        assert_eq!(stick.check(&STICK), Ok(()));

        let bigger = Identity::parse("500118192\nSanDisk \nUltra           \n1\n").unwrap();
        assert_eq!(
            bigger.check(&STICK),
            Err(Refusal::Identity {
                field: "size",
                want: "60062500".to_string(),
                got: "500118192".to_string(),
            })
        );
        let other = Identity::parse("60062500\nKingston\nUltra           \n1\n").unwrap();
        assert!(matches!(other.check(&STICK), Err(Refusal::Identity { field: "vendor", .. })));
        let fixed = Identity::parse("60062500\nSanDisk \nUltra           \n0\n").unwrap();
        assert!(matches!(fixed.check(&STICK), Err(Refusal::Identity { field: "removable", .. })));
        assert!(Identity::parse("60062500\nSanDisk \n").is_err());
    }

    #[test]
    fn every_command_the_loop_builds_is_one_the_rule_permits() {
        let t = target();
        let built = [
            t.flash().unwrap(),
            t.create_entry().unwrap(),
            t.bootnext("001D").unwrap(),
            t.mount_log().unwrap(),
            t.umount_log().unwrap(),
            t.reboot().unwrap(),
            t.probe().unwrap(),
        ];
        let permitted = t.permitted().unwrap();
        assert_eq!(built.len(), permitted.len());
        for root in &built {
            assert!(
                permitted.iter().any(|p| p.admits(root.argv())),
                "nothing permits {:?}",
                root.argv()
            );
        }
    }

    #[test]
    fn a_command_the_rule_does_not_name_cannot_be_built() {
        let t = target();
        for argv in [
            vec![DD, "of=/dev/nvme0n1", "bs=4M", "conv=fsync", "status=none"],
            vec![DD, "of=/dev/sda", "bs=1M", "conv=fsync", "status=none"],
            vec![DD, "of=/dev/sda", "bs=4M", "conv=fsync"],
            vec![DD, "of=/dev/sda", "bs=4M", "conv=fsync", "status=none", "seek=1"],
            vec![MOUNT, "-o", "rw", "/dev/sda3", "/home/t14/toyos-log"],
            vec![MOUNT, "-o", "ro", "/dev/sda3", "/etc"],
            vec![MOUNT, "-o", "ro", "/dev/nvme0n1p1", "/home/t14/toyos-log"],
            vec![UMOUNT, "/"],
            vec!["/usr/bin/sh"],
            vec![EFIBOOTMGR, "--bootnext", "12345"],
            vec![EFIBOOTMGR, "--bootnext", "00g1"],
            vec![EFIBOOTMGR, "--delete-bootnum"],
        ] {
            assert!(
                matches!(Root::new(&t, &argv), Err(Refusal::Unpermitted(_))),
                "the rule must not admit {argv:?}"
            );
        }
        assert!(t.bootnext("001D").is_ok());
        assert!(t.bootnext("ffff").is_ok());
    }

    #[test]
    fn the_rule_is_the_command_list_and_nothing_else() {
        assert_eq!(
            target().sudoers().unwrap(),
            "# toyos-metal: the whole of what the metal loop runs as root.\n\
             t14 ALL=(root) NOPASSWD: /usr/bin/dd of\\=/dev/sda bs\\=4M conv\\=fsync status\\=none\n\
             t14 ALL=(root) NOPASSWD: /usr/bin/efibootmgr --create --disk /dev/sda --part 1 \
             --label ToyOS --loader \\\\EFI\\\\BOOT\\\\BOOTX64.EFI\n\
             t14 ALL=(root) NOPASSWD: /usr/bin/efibootmgr --bootnext \
             [0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f]\n\
             t14 ALL=(root) NOPASSWD: /usr/bin/mount -o ro /dev/sda3 /home/t14/toyos-log\n\
             t14 ALL=(root) NOPASSWD: /usr/bin/umount /home/t14/toyos-log\n\
             t14 ALL=(root) NOPASSWD: /usr/sbin/reboot\n\
             t14 ALL=(root) NOPASSWD: /usr/bin/true\n"
        );
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
                "t14@t14",
                "true",
            ]
        );
        assert_eq!(
            t.identity_query(),
            "cat /sys/class/block/sda/size /sys/class/block/sda/device/vendor \
             /sys/class/block/sda/device/model /sys/class/block/sda/removable"
        );
        assert_eq!(
            t.flash().unwrap().remote(),
            "sudo -n '/usr/bin/dd' 'of=/dev/sda' 'bs=4M' 'conv=fsync' 'status=none'"
        );
        assert_eq!(
            t.create_entry().unwrap().remote(),
            "sudo -n '/usr/bin/efibootmgr' '--create' '--disk' '/dev/sda' '--part' '1' \
             '--label' 'ToyOS' '--loader' '\\EFI\\BOOT\\BOOTX64.EFI'"
        );
    }

    #[test]
    fn the_boot_entry_is_read_out_of_the_firmwares_own_listing() {
        // Four rows verbatim from the T14's `efibootmgr`, plus the row this loop creates.
        let listing = "BootCurrent: 0001\nTimeout: 2 seconds\nBootOrder: 0001,001D\n\
             Boot0000* Windows Boot Manager\tHD(1,GPT,9e92de56,0x800,0x64000)/File(\\bootmgfw.efi)\n\
             Boot0001* Ubuntu\tHD(1,GPT,16c1f60f,0x800,0x219800)/File(\\EFI\\ubuntu\\shimx64.efi)\n\
             Boot0010  Setup\tFvFile(721c8b66)\n\
             Boot0027* ToyOS\tHD(1,GPT,00000000,0x800,0x800)/File(\\EFI\\BOOT\\BOOTX64.EFI)\n";
        assert_eq!(entry_labelled(listing, "ToyOS").as_deref(), Some("0027"));
        assert_eq!(entry_labelled(listing, "Ubuntu").as_deref(), Some("0001"));
        assert_eq!(entry_labelled(listing, "Setup").as_deref(), Some("0010"));
        assert_eq!(entry_labelled(listing, "ToyO"), None);
        assert_eq!(entry_labelled("BootOrder: 0001,001D\n", "ToyOS"), None);
    }

    #[test]
    fn a_verdict_needs_the_kernels_own_boot_record() {
        let booted = "[kernel 1.151 cpu0] Boot: complete (1151ms)\n";
        assert_eq!(boot_millis(booted), Some(1151));
        assert_eq!(verdict(booted, None), Verdict::Pass { boot_ms: 1151, test: None });

        let half = "[kernel 0.084 cpu0] Boot: storage ready (84ms)\n";
        assert_eq!(boot_millis(half), None);
        assert!(matches!(verdict(half, None), Verdict::Fail(_)));
    }

    #[test]
    fn a_test_runner_marker_decides_when_one_is_asked_for() {
        let head = "[kernel 1.151 cpu0] Boot: complete (1151ms)\n";
        let passed = format!("{head}===TEST_START boot===\n===TEST_END boot exit=0===\n");
        assert_eq!(
            verdict(&passed, Some("boot")),
            Verdict::Pass { boot_ms: 1151, test: Some("boot".to_string()) }
        );

        let failed = format!("{head}===TEST_END boot exit=3===\n");
        assert_eq!(verdict(&failed, Some("boot")), Verdict::Fail("boot ended exit=3".to_string()));

        let errored = format!("{head}===TEST_END boot error=no such file===\n");
        assert_eq!(
            verdict(&errored, Some("boot")),
            Verdict::Fail("boot ended error=no such file".to_string())
        );

        assert!(matches!(verdict(head, Some("boot")), Verdict::Fail(_)));
        let other = format!("{head}===TEST_END shutdown exit=0===\n");
        assert!(matches!(verdict(&other, Some("boot")), Verdict::Fail(_)));

        let twice = format!("{head}===TEST_END boot exit=0===\n===TEST_END boot exit=1===\n");
        assert_eq!(verdict(&twice, Some("boot")), Verdict::Fail("boot ended exit=1".to_string()));
    }

    #[test]
    fn the_arguments_name_the_t14_by_default() {
        let args = Args::parse(&[]).unwrap();
        assert_eq!(args.target.user, "t14");
        assert_eq!(args.target.host, "t14");
        assert_eq!(args.target.node.whole(), "/dev/sda");
        assert_eq!(args.wait_secs, RETURN_SECS);
        assert!(!args.dry_run);

        let words: Vec<String> =
            ["--dry-run", "--device", "/dev/sdb", "--host", "runner@box", "--test", "boot"]
                .iter()
                .map(|w| (*w).to_string())
                .collect();
        let args = Args::parse(&words).unwrap();
        assert!(args.dry_run);
        assert_eq!(args.target.node.whole(), "/dev/sdb");
        assert_eq!(args.target.user, "runner");
        assert_eq!(args.target.host, "box");
        assert_eq!(args.test.as_deref(), Some("boot"));

        let nvme = ["--device".to_string(), "/dev/nvme0n1".to_string()];
        assert_eq!(Args::parse(&nvme), Err(Refusal::Node("/dev/nvme0n1".to_string())));
        let bare = ["--image".to_string()];
        assert!(matches!(Args::parse(&bare), Err(Refusal::Usage(_))));
    }

    /// The pre-flash gate against the image this tree builds, when one is
    /// there. `cargo run -- --build-only` writes it; a checkout that has not
    /// built has nothing to admit and the synthetic cases carry the judgement.
    #[test]
    fn the_built_image_passes_the_pre_flash_gate() {
        let image = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/bootable.img");
        let Ok(admitted) = admit(&image, &Target::t14()) else {
            return;
        };
        assert_eq!(admitted.bytes % SECTOR, 0);
        assert_eq!(admitted.sectors, admitted.bytes / SECTOR);
        assert_eq!(admitted.esp_part, 1);
        assert_eq!(admitted.log_part, 3);
        eprintln!(
            "admitted {}: {} bytes, {} sectors",
            admitted.path.display(),
            admitted.bytes,
            admitted.sectors
        );
    }

    #[test]
    fn an_image_that_is_not_whole_sectors_or_has_no_backup_header_is_refused() {
        let dir = std::env::temp_dir().join(format!("toyos-metal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let t = Target::t14();

        let ragged = dir.join("ragged.img");
        std::fs::write(&ragged, vec![0u8; 1000]).expect("write");
        assert_eq!(admit(&ragged, &t), Err(Refusal::Sectors { bytes: 1000 }));

        let empty = dir.join("empty.img");
        std::fs::write(&empty, []).expect("write");
        assert_eq!(admit(&empty, &t), Err(Refusal::Sectors { bytes: 0 }));

        let headless = dir.join("headless.img");
        std::fs::write(&headless, vec![0u8; 1024]).expect("write");
        assert_eq!(
            admit(&headless, &t),
            Err(Refusal::BackupHeader { at: 512, saw: "\0\0\0\0\0\0\0\0".to_string() })
        );

        // The signature in the *primary* header alone, which is the false pass
        // the gate's item 2 names: a healthy front hides a missing backup.
        let front = dir.join("front-only.img");
        let mut bytes = vec![0u8; 1536];
        bytes[512..520].copy_from_slice(b"EFI PART");
        std::fs::write(&front, &bytes).expect("write");
        assert!(matches!(admit(&front, &t), Err(Refusal::BackupHeader { .. })));

        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
