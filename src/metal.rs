//! `toyos-metal` — the loop that flashes ToyOS to the T14's stick, boots it
//! once, and answers with [`crate::bootlog`]'s verdict on what that boot wrote
//! to the log partition.
//!
//! It runs on the development host and reaches the machine over `ssh`.
//! [`Target::words`] is the one place a root command line is written — the
//! installed `/etc/sudoers.d/toyos-metal` and every argv are rendered from it —
//! so the loop constructs no command outside [`JOBS`] but the one that puts
//! that rule there, [`install_sudoers`], which the rule does not permit either.
//!
//! Two admission checks stand before any write: the image is a whole number of
//! [`LBA`]-byte sectors carrying `EFI PART` in its final one, and the disk
//! answers with the identity this loop was given. A node that is not
//! `/dev/sd<letter>` cannot be written down, so the machine's NVMe is unnameable.

use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::bootlog;
use crate::image::LBA;

const CONNECT_SECS: u64 = 10;

/// How long the machine has to go quiet after `reboot`, and how long it then
/// has to answer again.
const GOING_DOWN_SECS: u64 = 120;
const RETURN_SECS: u64 = 420;

const POLL_SECS: u64 = 5;

/// The absolute paths `which` answered on the machine; sudo matches a path and
/// not a name, so nothing here is spelled relatively.
const DD: &str = "/usr/bin/dd";
const EFIBOOTMGR: &str = "/usr/bin/efibootmgr";
const MOUNT: &str = "/usr/bin/mount";
const UMOUNT: &str = "/usr/bin/umount";
const REBOOT: &str = "/usr/sbin/reboot";
const TRUE: &str = "/usr/bin/true";
const WIPEFS: &str = "/usr/sbin/wipefs";

/// What the firmware loads off the stick's ESP.
const LOADER: &str = r"\EFI\BOOT\BOOTX64.EFI";

/// The three logind keys that keep a closed-lid machine awake.
const LID_KEYS: &[&str] =
    &["HandleLidSwitch", "HandleLidSwitchExternalPower", "HandleLidSwitchDocked"];

/// Every way this loop refuses, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A device node that is not `/dev/sd<letter>`.
    Node(String),
    /// A word that cannot be written into a sudoers rule unambiguously.
    Word(String),
    /// No `HOME`, so the default key path would be rooted at `/`.
    NoHome,
    /// The disk did not answer with the identity this loop was given.
    Identity { field: &'static str, want: String, got: String },
    File { path: String, why: String },
    /// The image's length is not a whole number of sectors.
    Sectors { bytes: u64 },
    /// The final sector does not begin `EFI PART`: the backup table is gone.
    BackupHeader { at: u64, saw: String },
    Table(String),
    /// The table does not hold exactly one partition of a type the loop needs.
    Partitions { what: &'static str, matched: u32 },
    /// The image puts a partition somewhere the installed rule does not name.
    PartitionIndex { what: &'static str, want: u32, got: u32 },
    /// What landed on the disk is not what the image says.
    Landed { what: String, want: u64, got: u64 },
    /// `efibootmgr` named no four-hex-digit boot entry.
    BootEntry(String),
    /// Creating the entry moved the firmware's boot order.
    BootOrder { before: String, now: String },
    Sudo(String),
    /// A lid key that no longer reads `ignore`, which is what keeps the machine up.
    Lid { key: &'static str, got: String },
    Remote { what: String, status: String, stderr: String },
    /// The machine did not go down, or did not come back.
    Silent { what: &'static str, secs: u64 },
    /// The log the boot left is not a passing boot's.
    Log(bootlog::Unfit),
    Usage(String),
}

impl Refusal {
    /// Whether the machine failed rather than the loop: the boot is the subject
    /// only once the stick is written and the reboot asked for.
    pub fn about_the_boot(&self) -> bool {
        matches!(self, Self::Silent { .. } | Self::Log(_))
    }
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
            Self::NoHome => write!(f, "no HOME, so there is no default key path; pass --key"),
            Self::Identity { field, want, got } => {
                write!(f, "the disk's {field} reads {got:?} where this loop was given {want:?}")
            }
            Self::File { path, why } => write!(f, "{path}: {why}"),
            Self::Sectors { bytes } => write!(
                f,
                "the image is {bytes} bytes, which is not a whole number of {LBA}-byte sectors"
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
            Self::Landed { what, want, got } => {
                write!(f, "{what} on the disk is {got} where the image says {want}")
            }
            Self::BootEntry(saw) => {
                write!(f, "efibootmgr named no four-hex-digit boot entry: {saw:?}")
            }
            Self::BootOrder { before, now } => write!(
                f,
                "creating the boot entry moved BootOrder from {before:?} to {now:?}: this loop \
                 buys one boot with --bootnext and leaves the order alone"
            ),
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
            Self::Silent { what, secs } => write!(
                f,
                "the machine did not {what} within {secs} s; it may be sitting in ToyOS with its \
                 one boot already spent, which needs a hand on the power button"
            ),
            Self::Log(unfit) => write!(f, "the log partition came back and {unfit}"),
            Self::Usage(why) => write!(f, "{why}"),
        }
    }
}

/// A SCSI disk node, `/dev/sd` and one lower-case letter.
///
/// **An NVMe node cannot be written down.** The T14's Ubuntu install is on
/// `nvme0n1`, and this type is what keeps that true whatever a caller passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Node(u8);

impl Node {
    fn parse(path: &str) -> Result<Self, Refusal> {
        let letter = path.strip_prefix("/dev/sd").and_then(|rest| {
            let [byte] = rest.as_bytes() else { return None };
            byte.is_ascii_lowercase().then_some(*byte)
        });
        letter.map(Node).ok_or_else(|| Refusal::Node(path.to_string()))
    }

    fn whole(&self) -> String {
        format!("/dev/sd{}", self.0 as char)
    }

    fn partition(&self, index: u32) -> String {
        format!("{}{index}", self.whole())
    }

    /// Where `/sys` answers for the disk, and for one of its partitions.
    fn sysfs(&self) -> String {
        format!("/sys/class/block/sd{}", self.0 as char)
    }

    fn part_sysfs(&self, index: u32) -> String {
        format!("{}/sd{}{index}", self.sysfs(), self.0 as char)
    }
}

/// What a disk has to answer before this loop writes it.
struct Expect {
    sectors: u64,
    vendor: &'static str,
    model: &'static str,
}

/// The stick, as `/sys/class/block/sda` on the T14 answered.
const STICK: Expect = Expect { sectors: 60_062_500, vendor: "SanDisk", model: "Ultra" };

/// What `/sys/class/block/<disk>/` answered, in the order [`Target::identity`]
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

    fn check(&self, want: &Expect) -> Result<(), Refusal> {
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

/// Declares every command this loop runs as root and the table the rule is
/// rendered from out of one list, so a `Job` the rule does not carry cannot be
/// written down.
macro_rules! jobs {
    ($($name:ident),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Job { $($name),+ }
        const JOBS: &[Job] = &[$(Job::$name),+];
    };
}

jobs!(Wipe, Flash, Create, Delete, BootNext, Mount, Umount, Reboot, Probe);

/// One word of a root command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Word {
    Is(String),
    /// The four hex digits `efibootmgr` numbers a boot entry with: the rule
    /// masks it, the argv fills it.
    Hex4,
}

impl Word {
    /// A literal this loop will neither shell-quote wrongly nor let sudo read
    /// as a glob. Whitespace and the metacharacters are refused rather than
    /// escaped: no command here needs one.
    fn literal(text: &str) -> Result<Self, Refusal> {
        let bad = |c: char| {
            c.is_whitespace() || matches!(c, '*' | '?' | '[' | ']' | '#' | '!' | '"' | '\'')
        };
        if text.is_empty() || text.chars().any(bad) {
            return Err(Refusal::Word(text.to_string()));
        }
        Ok(Self::Is(text.to_string()))
    }

    /// The word as `sudoers(5)` reads it: `,`, `:`, `=` and a `^` escaped once,
    /// and a backslash four times — the page's "you must escape the backslash
    /// twice", for the two levels of escaping it names, the sudoers parser's
    /// and `fnmatch(3)`'s.
    fn sudoers(&self) -> String {
        match self {
            Self::Is(text) => {
                let mut out = String::new();
                for c in text.chars() {
                    match c {
                        '\\' => out.push_str(r"\\\\"),
                        ',' | ':' | '=' | '^' => {
                            out.push('\\');
                            out.push(c);
                        }
                        _ => out.push(c),
                    }
                }
                out
            }
            Self::Hex4 => "[0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f]".to_string(),
        }
    }
}

/// The account name as the rule's *user* field, which [`Word::sudoers`] cannot
/// render: every character the two escape differently is refused, along with
/// the four that carry meaning in that field — `,`, `:`, `%` and `+`.
fn user_word(user: &str) -> Result<Word, Refusal> {
    if user.contains([',', ':', '%', '+', '\\', '(', ')']) {
        return Err(Refusal::Word(user.to_string()));
    }
    Word::literal(user)
}

/// One word as the machine's login shell must receive it: `ssh` hands the whole
/// command to that shell, and what `sudo` matches is the argv the shell made.
fn shell_word(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// Where the loop runs and what it may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    user: String,
    host: String,
    key: PathBuf,
    node: Node,
    /// The image's ESP, the partition the firmware is pointed at.
    esp_part: u32,
    /// The image's TOYOS-LOG partition, the one channel out of a metal boot.
    log_part: u32,
    /// Where that partition is mounted on the machine; under the account's own
    /// home, so no root command creates it.
    mount: String,
    /// The boot entry's label in the firmware's list.
    label: String,
}

impl Target {
    /// The T14 as the track names it, with the runner key this host holds.
    fn t14() -> Result<Self, Refusal> {
        let home = std::env::var_os("HOME").ok_or(Refusal::NoHome)?;
        Ok(Self {
            user: "t14".to_string(),
            host: "t14".to_string(),
            key: PathBuf::from(home).join(".ssh/id_ed25519_toyos_runner"),
            node: Node(b'a'),
            esp_part: 1,
            log_part: 3,
            mount: "/home/t14/toyos-log".to_string(),
            label: "ToyOS".to_string(),
        })
    }

    /// **The one place a root command line is written.** The installed rule and
    /// every argv are rendered from what this returns.
    fn words(&self, job: Job) -> Result<Vec<Word>, Refusal> {
        let node = self.node.whole();
        let of = format!("of={node}");
        let log = self.node.partition(self.log_part);
        let esp = self.esp_part.to_string();
        let literal =
            |words: &[&str]| -> Result<Vec<Word>, Refusal> { words.iter().map(|w| Word::literal(w)).collect() };
        let masked = |head: &[&str]| -> Result<Vec<Word>, Refusal> {
            let mut words = literal(head)?;
            words.push(Word::Hex4);
            Ok(words)
        };
        match job {
            Job::Wipe => literal(&[WIPEFS, "--all", &node]),
            Job::Flash => literal(&[DD, &of, "bs=4M", "conv=fsync"]),
            // `--create-only`, never `--create`: the latter "add[s] to
            // bootorder" (efibootmgr(8)) at the top, so the boot after the one
            // `--bootnext` bought picks ToyOS again, and a job list whose one
            // job is a reboot then loops.
            Job::Create => literal(&[
                EFIBOOTMGR,
                "--create-only",
                "--disk",
                &node,
                "--part",
                &esp,
                "--label",
                &self.label,
                "--loader",
                LOADER,
            ]),
            Job::Delete => masked(&[EFIBOOTMGR, "--delete-bootnum", "--bootnum"]),
            Job::BootNext => masked(&[EFIBOOTMGR, "--bootnext"]),
            Job::Mount => literal(&[MOUNT, "-o", "ro", &log, &self.mount]),
            Job::Umount => literal(&[UMOUNT, &self.mount]),
            Job::Reboot => literal(&[REBOOT]),
            Job::Probe => literal(&[TRUE]),
        }
    }

    /// The rule, one line per job.
    ///
    /// **A command written with no arguments permits any arguments**
    /// (`sudoers(5)`, "Command Arguments"), so a one-word job renders `""`.
    fn sudoers(&self) -> Result<String, Refusal> {
        let user = user_word(&self.user)?.sudoers();
        let mut out =
            String::from("# toyos-metal: the whole of what the metal loop runs as root.\n");
        for job in JOBS {
            let rendered: Vec<String> = self.words(*job)?.iter().map(Word::sudoers).collect();
            let line = match rendered.len() {
                1 => format!("{} \"\"", rendered[0]),
                _ => rendered.join(" "),
            };
            out.push_str(&format!("{user} ALL=(root) NOPASSWD: {line}\n"));
        }
        Ok(out)
    }

    /// One job's argv, with `fill` substituted for the masked word: a job's mask
    /// count and its caller must agree, and a `fill` that is not four hex digits
    /// is a bad reading of `efibootmgr` rather than a bug here.
    fn argv(&self, job: Job, fill: Option<&str>) -> Result<Vec<String>, Refusal> {
        let words = self.words(job)?;
        let masked = words.iter().filter(|w| **w == Word::Hex4).count();
        assert_eq!(masked, usize::from(fill.is_some()), "{job:?} takes {masked} filled word(s)");
        words
            .iter()
            .map(|word| match word {
                Word::Is(text) => Ok(text.clone()),
                Word::Hex4 => {
                    let fill = fill.unwrap_or_default();
                    let hex = fill.len() == 4 && fill.bytes().all(|b| b.is_ascii_hexdigit());
                    hex.then(|| fill.to_string())
                        .ok_or_else(|| Refusal::BootEntry(fill.to_string()))
                }
            })
            .collect()
    }

    /// One job as the machine's login shell must receive it.
    fn remote(&self, job: Job, fill: Option<&str>) -> Result<String, Refusal> {
        let words: Vec<String> = self.argv(job, fill)?.iter().map(|w| shell_word(w)).collect();
        Ok(format!("sudo -n {}", words.join(" ")))
    }

    /// The one read that decides whether anything is written.
    fn identity(&self) -> String {
        let at = self.node.sysfs();
        format!("cat {at}/size {at}/device/vendor {at}/device/model {at}/removable")
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

/// One partition of the image, in [`LBA`]-byte sectors — the unit `/sys`
/// reports `start` and `size` for a disk in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Part {
    index: u32,
    start: u64,
    sectors: u64,
    guid: toyos_gpt::Guid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Flashable {
    path: PathBuf,
    bytes: u64,
    esp: Part,
    log: Part,
}

/// Whole sectors, `EFI PART` in the *final* one, and exactly one partition of
/// each of the three types at the numbers the installed rule names.
fn admit(path: &Path, target: &Target) -> Result<Flashable, Refusal> {
    let sector = u64::from(LBA);
    let unreadable = |why: String| Refusal::File { path: path.display().to_string(), why };
    let mut file = std::fs::File::open(path).map_err(|e| unreadable(e.to_string()))?;
    let bytes = file.metadata().map_err(|e| unreadable(e.to_string()))?.len();
    if bytes == 0 || bytes % sector != 0 {
        return Err(Refusal::Sectors { bytes });
    }

    let at = bytes - sector;
    let mut last = vec![0u8; sector as usize];
    file.seek(SeekFrom::Start(at)).map_err(|e| unreadable(e.to_string()))?;
    file.read_exact(&mut last).map_err(|e| unreadable(e.to_string()))?;
    if &last[..8] != b"EFI PART" {
        let saw = String::from_utf8_lossy(&last[..8]).to_string();
        return Err(Refusal::BackupHeader { at, saw });
    }

    let esp = one_partition(&mut file, toyos_gpt::Guid::EFI_SYSTEM, "ESP")?;
    let log = one_partition(&mut file, toyos_gpt::Guid::MICROSOFT_BASIC, "TOYOS-LOG")?;
    one_partition(&mut file, toyos_gpt::Guid::TOYOS_ROOT, "TOYOS-ROOT")?;
    for (what, got, want) in
        [("the ESP", esp.index, target.esp_part), ("TOYOS-LOG", log.index, target.log_part)]
    {
        if got != want {
            return Err(Refusal::PartitionIndex { what, want, got });
        }
    }
    Ok(Flashable { path: path.to_path_buf(), bytes, esp, log })
}

/// The one partition of type `guid`, numbered as Linux numbers it: the entry
/// index plus one.
fn one_partition(
    file: &mut std::fs::File,
    guid: toyos_gpt::Guid,
    what: &'static str,
) -> Result<Part, Refusal> {
    const BLANK: toyos_gpt::Partition = toyos_gpt::Partition {
        index: 0,
        type_guid: toyos_gpt::Guid::ZERO,
        unique_guid: toyos_gpt::Guid::ZERO,
        first_lba: 0,
        last_lba: 0,
    };
    let mut out = [BLANK; 2];
    let scan = toyos_gpt::locate_type(&mut crate::image::FileSectors(file), guid, &mut out)
        .map_err(|e| Refusal::Table(format!("{e:?}")))?;
    if scan.matched != 1 {
        return Err(Refusal::Partitions { what, matched: scan.matched });
    }
    Ok(Part {
        index: out[0].index + 1,
        start: out[0].first_lba,
        sectors: out[0].lba_count(),
        guid: out[0].unique_guid,
    })
}

/// How many bytes `dd` says it copied, out of its own last line.
fn dd_copied(stderr: &str) -> Option<u64> {
    let line = stderr.lines().find(|l| l.contains(" bytes ") && l.contains("copied"))?;
    line.split_whitespace().next()?.parse().ok()
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
        return Err(Refusal::BootOrder { before: was, now });
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
    /// Print each write this run would make instead of making it; every read
    /// still happens, or the rehearsal would be of a different loop.
    dry_run: bool,
}

impl Driver {
    fn ssh(&self, what: &str, command: &str) -> Result<String, Refusal> {
        let out = Command::new("ssh")
            .args(self.target.ssh_argv(command))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| unstarted(what, &e))?;
        answer(what, out).map(|out| String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// One root command, with `stdin` streamed into it. Announced either way,
    /// and run only when this is not a dry run — which is what `None` means.
    fn as_root(
        &self,
        what: &str,
        job: Job,
        fill: Option<&str>,
        stdin: Option<&Path>,
    ) -> Result<Option<Output>, Refusal> {
        let line = self.target.remote(job, fill)?;
        println!("  {} {line}", if self.dry_run { "would run:" } else { "run:" });
        if self.dry_run {
            return Ok(None);
        }
        let mut command = Command::new("ssh");
        command.args(self.target.ssh_argv(&line));
        match stdin {
            Some(path) => {
                let file = std::fs::File::open(path).map_err(|e| Refusal::File {
                    path: path.display().to_string(),
                    why: e.to_string(),
                })?;
                command.stdin(Stdio::from(file));
            }
            None => {
                command.stdin(Stdio::null());
            }
        }
        let out = command.output().map_err(|e| unstarted(what, &e))?;
        answer(what, out).map(Some)
    }

    /// The loop refuses to run at all until the rule is on the machine.
    fn require_sudo(&self) -> Result<(), Refusal> {
        let probe = self.target.remote(Job::Probe, None)?;
        self.ssh("sudo -n", &probe).map(|_| ()).map_err(|e| Refusal::Sudo(e.to_string()))
    }

    /// **The write is verified, not assumed**: a short local read ends `ssh`
    /// and the remote `dd` exits 0 on the prefix it got, so `dd`'s own byte
    /// count and then the table the kernel re-read are both compared. `wipefs`
    /// first, because the stick outlives the image and its old backup GPT would
    /// otherwise leave the firmware two tables.
    fn flash(&self, image: &Flashable) -> Result<(), Refusal> {
        self.as_root("wiping the old signatures", Job::Wipe, None, None)?;
        let out = self.as_root("flashing the stick", Job::Flash, None, Some(&image.path))?;
        let Some(out) = out else { return Ok(()) };
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let copied = dd_copied(&stderr).ok_or_else(|| Refusal::Remote {
            what: "flashing the stick".to_string(),
            status: "printed no byte count".to_string(),
            stderr,
        })?;
        if copied != image.bytes {
            let what = "the byte count dd reported".to_string();
            return Err(Refusal::Landed { what, want: image.bytes, got: copied });
        }
        self.ssh("settling the new table", "udevadm settle")?;
        for (name, part) in [("the ESP", image.esp), ("TOYOS-LOG", image.log)] {
            let at = self.target.node.part_sysfs(part.index);
            let read = self.ssh("reading the new table back", &format!("cat {at}/start {at}/size"))?;
            let mut numbers = read.lines().map(str::trim).map(str::parse::<u64>);
            let start = numbers.next().and_then(Result::ok).unwrap_or_default();
            let sectors = numbers.next().and_then(Result::ok).unwrap_or_default();
            for (what, want, got) in
                [("first sector", part.start, start), ("sector count", part.sectors, sectors)]
            {
                if want != got {
                    return Err(Refusal::Landed { what: format!("{name}'s {what}"), want, got });
                }
            }
        }
        Ok(())
    }

    /// The boot entry for *this* image's ESP. An entry carrying the label but
    /// another partition's GUID names a partition that is gone — every
    /// `--build-only` draws a fresh one — so it is deleted rather than reused.
    fn boot_entry(&self, esp: &Part) -> Result<String, Refusal> {
        let want = esp.guid.to_string().to_ascii_lowercase();
        let ours = |listing: &str| {
            entries_labelled(listing, &self.target.label)
                .into_iter()
                .partition::<Vec<_>, _>(|(_, guid)| *guid == want)
        };
        let listing = self.ssh("listing boot entries", EFIBOOTMGR)?;
        let (mine, stale) = ours(&listing);
        if let Some((id, _)) = mine.first() {
            return Ok(id.clone());
        }
        for (id, _) in stale {
            self.as_root("deleting a stale boot entry", Job::Delete, Some(&id), None)?;
        }
        // Read after those deletions, because a delete takes its entry out of
        // the order too: what the create is judged against is the state the
        // create acts on.
        let before = self.ssh("listing boot entries", EFIBOOTMGR)?;
        self.as_root("creating the boot entry", Job::Create, None, None)?;
        // Read back rather than parsed out of the create's own output: the
        // firmware's list is what the next command names, and one parser reads
        // it.
        let after = self.ssh("listing boot entries", EFIBOOTMGR)?;
        match entry_after_create(&before, &after, &self.target.label, &want) {
            Ok(id) => Ok(id),
            // A dry run created nothing, so `0000` stands for the id the
            // firmware would have given it — the only value in the run that is
            // not the real run's.
            Err(Refusal::BootEntry(_)) if self.dry_run => Ok("0000".to_string()),
            Err(refusal) => {
                // Every entry this create added under the loop's own label,
                // whatever partition it names: one left behind is one the next
                // run inherits. A failure to clear it does not replace the
                // refusal being answered.
                let had: Vec<String> = ids(&before, &self.target.label);
                for id in ids(&after, &self.target.label) {
                    if had.contains(&id) {
                        continue;
                    }
                    let what = "deleting the entry this create made";
                    let _ = self.as_root(what, Job::Delete, Some(&id), None);
                }
                Err(refusal)
            }
        }
    }

    /// **`reboot` is `systemctl` and returns before the machine goes down**, so
    /// the machine is watched down before it is watched back up: a probe that
    /// caught dying Ubuntu would read a stick ToyOS had never booted.
    fn ride_the_reboot(&self, secs: u64) -> Result<u64, Refusal> {
        self.wait(GOING_DOWN_SECS, "go down", false)?;
        self.wait(secs, "come back", true)
    }

    fn wait(&self, secs: u64, what: &'static str, answering: bool) -> Result<u64, Refusal> {
        let began = std::time::Instant::now();
        while began.elapsed().as_secs() < secs {
            std::thread::sleep(std::time::Duration::from_secs(POLL_SECS));
            if self.ssh("probing", "true").is_ok() == answering {
                return Ok(began.elapsed().as_secs());
            }
        }
        Err(Refusal::Silent { what, secs })
    }

    /// The loader's own file, and then everything `logd` wrote, in name order:
    /// a freshly flashed volume holds one boot's files, and its newest file
    /// alone would miss the continuations `logd` rotates into.
    ///
    /// Two strings and not one, because only the second is the boot's log: a
    /// verdict over the loader's lines would read a partition GUID as a boot
    /// record.
    fn read_log(&self) -> Result<(String, String), Refusal> {
        self.ssh("making the mount point", &format!("mkdir -p {}", shell_word(&self.target.mount)))?;
        self.as_root("mounting the log partition", Job::Mount, None, None)?;
        let read = self.read_mounted();
        // Unconditional, because a failure in the read would otherwise leave
        // the partition mounted and wedge the next run — and subordinate,
        // because the read's failure is the one worth answering with.
        let unmounted = self.as_root("unmounting the log partition", Job::Umount, None, None);
        match (read, unmounted) {
            (Ok(both), Ok(_)) => Ok(both),
            (Ok(_), Err(umount)) => Err(umount),
            (Err(read), Ok(_)) => Err(read),
            (Err(read), Err(umount)) => Err(Refusal::Remote {
                what: format!("reading the log ({read}), and then unmounting it"),
                status: "both failed".to_string(),
                stderr: umount.to_string(),
            }),
        }
    }

    fn read_mounted(&self) -> Result<(String, String), Refusal> {
        let at = shell_word(&self.target.mount);
        let listing = self.ssh("listing the log", &format!("ls -1 {at}"))?;
        let present: Vec<&str> = listing.lines().map(str::trim).collect();
        // Absence is an answer and not a failure: the loader says on the
        // machine's screen why it wrote none, and the boot is judged either way.
        let loader = if present.contains(&bootlog::LOADER_LOG) {
            self.cat(bootlog::LOADER_LOG)?
        } else {
            format!("{}: the loader wrote none\n", bootlog::LOADER_LOG)
        };
        let mut names: Vec<&str> =
            present.into_iter().filter(|n| bootlog::is_logd_file(n)).collect();
        names.sort_unstable();
        let mut text = String::new();
        for name in names {
            text.push_str(&self.cat(name)?);
        }
        Ok((loader, text))
    }

    fn cat(&self, name: &str) -> Result<String, Refusal> {
        let file = shell_word(&format!("{}/{name}", self.target.mount));
        self.ssh("reading a log file", &format!("cat {file}"))
    }
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

/// What the binary was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    image: PathBuf,
    target: Target,
    dry_run: bool,
    /// Where the account's password is read from, once, to install the rule.
    install_sudoers: Option<PathBuf>,
    wait_secs: u64,
}

impl Args {
    pub fn parse(args: &[String]) -> Result<Self, Refusal> {
        let mut out = Args {
            image: PathBuf::from("target/bootable.img"),
            target: Target::t14()?,
            dry_run: false,
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
                    user_word(user)?;
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
                "--install-sudoers" => {
                    out.install_sudoers = Some(PathBuf::from(value()?));
                    2
                }
                "--wait-secs" => {
                    let secs = value()?;
                    out.wait_secs = secs
                        .parse()
                        .map_err(|_| Refusal::Usage(format!("--wait-secs: {secs:?}")))?;
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
fn install_sudoers(target: &Target, password: &Path) -> Result<(), Refusal> {
    // Staged in the account's own home rather than in a world-writable /tmp,
    // where another user could win the race between `visudo -c` and `install`.
    let staged = format!("/home/{}/.toyos-metal.sudoers", target.user);
    let driver = Driver { target: target.clone(), dry_run: false };
    // Cleared whichever way the install went: a refused rule left behind is a
    // file the next run would `visudo -c` instead of the one it just wrote.
    let installed = stage_and_install(&driver, target, password, &staged);
    let cleared = driver.ssh("clearing the staged rule", &format!("rm -f {staged}"));
    installed.and(cleared)?;
    println!("installed /etc/sudoers.d/toyos-metal");
    Ok(())
}

fn stage_and_install(
    driver: &Driver,
    target: &Target,
    password: &Path,
    staged: &str,
) -> Result<String, Refusal> {
    let rule = target.sudoers()?;

    let mut stage = Command::new("ssh")
        .args(target.ssh_argv(&format!("umask 077 && cat > {staged}")))
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| unstarted("staging the rule", &e))?;
    stage
        .stdin
        .take()
        .expect("the pipe was just asked for")
        .write_all(rule.as_bytes())
        .map_err(|e| unstarted("staging the rule", &e))?;
    let out = stage.wait_with_output().map_err(|e| unstarted("staging the rule", &e))?;
    answer("staging the rule", out)?;
    driver.ssh("checking the rule", &format!("/usr/sbin/visudo -c -f {staged}"))?;

    // The password crosses this process as bytes on one pipe and reaches no
    // argument, no formatter and no output; `sudo -S` reads one line, so the
    // newline is supplied whether or not the file carries one.
    let mut secret = std::fs::read(password)
        .map_err(|e| Refusal::File { path: password.display().to_string(), why: e.to_string() })?;
    while secret.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
        secret.pop();
    }
    secret.push(b'\n');
    let command = format!(
        "sudo -S -p '' /usr/bin/install -m 0440 -o root -g root {staged} \
         /etc/sudoers.d/toyos-metal"
    );
    let mut install = Command::new("ssh")
        .args(target.ssh_argv(&command))
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| unstarted("installing the rule", &e))?;
    install
        .stdin
        .take()
        .expect("the pipe was just asked for")
        .write_all(&secret)
        .map_err(|e| unstarted("installing the rule", &e))?;
    let out = install.wait_with_output().map_err(|e| unstarted("installing the rule", &e))?;
    answer("installing the rule", out)?;
    Ok(String::new())
}

/// The whole loop, answering the boot's own millisecond count. `None` is a dry
/// run, which reaches no boot record because it wrote nothing.
pub fn run(args: &Args) -> Result<Option<u64>, Refusal> {
    if let Some(password) = &args.install_sudoers {
        install_sudoers(&args.target, password)?;
    }
    let driver = Driver { target: args.target.clone(), dry_run: args.dry_run };

    let image = admit(&args.image, &args.target)?;
    println!(
        "image {}: {} bytes, ESP p{} at {}+{}, log p{} at {}+{}",
        image.path.display(),
        image.bytes,
        image.esp.index,
        image.esp.start,
        image.esp.sectors,
        image.log.index,
        image.log.start,
        image.log.sectors
    );

    driver.require_sudo()?;
    let policy =
        driver.ssh("reading the lid policy", "systemd-analyze cat-config systemd/logind.conf")?;
    lid_policy(&policy)?;
    let identity = Identity::parse(&driver.ssh("reading the disk", &args.target.identity())?)?;
    identity.check(&STICK)?;
    println!(
        "disk {}: {} sectors, {} {}, removable",
        args.target.node.whole(),
        identity.sectors,
        identity.vendor,
        identity.model
    );

    driver.flash(&image)?;
    let entry = driver.boot_entry(&image.esp)?;
    driver.as_root("setting bootnext", Job::BootNext, Some(&entry), None)?;
    driver.as_root("rebooting", Job::Reboot, None, None)?;
    if driver.dry_run {
        driver.as_root("mounting the log partition", Job::Mount, None, None)?;
        driver.as_root("unmounting the log partition", Job::Umount, None, None)?;
        println!("dry run: nothing was written and the machine was not rebooted");
        return Ok(None);
    }

    let back = driver.ride_the_reboot(args.wait_secs)?;
    println!("the machine answered ssh again after {back} s");
    let (loader, log) = driver.read_log()?;
    // The loader's first, because a boot that stopped before the kernel leaves
    // nothing else, and the refusal below is then all this report would carry.
    print!("{loader}{log}");
    bootlog::verdict(&log).map(Some).map_err(Refusal::Log)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            key: PathBuf::from("/home/dev/.ssh/id_ed25519_toyos_runner"),
            ..Target::t14().expect("a test run has HOME")
        }
    }

    #[test]
    fn an_nvme_node_cannot_be_written_down() {
        for name in ["/dev/nvme0n1", "/dev/nvme0n1p3", "/dev/sda1", "/dev/sdaa", "/dev/SDA", "sda"]
        {
            assert_eq!(Node::parse(name), Err(Refusal::Node(name.to_string())), "{name}");
        }
        assert_eq!(Node::parse("/dev/sda").unwrap().whole(), "/dev/sda");
        assert_eq!(Node::parse("/dev/sdb").unwrap().partition(3), "/dev/sdb3");
        assert_eq!(Node::parse("/dev/sda").unwrap().part_sysfs(1), "/sys/class/block/sda/sda1");
    }

    #[test]
    fn the_identity_is_the_stick_or_it_is_refused() {
        let stick = Identity::parse("60062500\nSanDisk \nUltra           \n1\n").unwrap();
        assert_eq!(stick.sectors, 60_062_500);
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
        let model = Identity::parse("60062500\nSanDisk \nCruzer          \n1\n").unwrap();
        assert!(matches!(model.check(&STICK), Err(Refusal::Identity { field: "model", .. })));
        let fixed = Identity::parse("60062500\nSanDisk \nUltra           \n0\n").unwrap();
        assert!(matches!(fixed.check(&STICK), Err(Refusal::Identity { field: "removable", .. })));
        assert!(Identity::parse("60062500\nSanDisk \n").is_err());
        assert!(Identity::parse("many\nSanDisk \nUltra\n1\n").is_err());
    }

    #[test]
    fn the_rule_is_the_command_table_rendered() {
        let hex = "[0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f]";
        assert_eq!(
            target().sudoers().unwrap(),
            format!(
                "# toyos-metal: the whole of what the metal loop runs as root.\n\
                 t14 ALL=(root) NOPASSWD: /usr/sbin/wipefs --all /dev/sda\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/dd of\\=/dev/sda bs\\=4M conv\\=fsync\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/efibootmgr --create-only --disk /dev/sda \
                 --part 1 --label ToyOS --loader \\\\\\\\EFI\\\\\\\\BOOT\\\\\\\\BOOTX64.EFI\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/efibootmgr --delete-bootnum --bootnum {hex}\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/efibootmgr --bootnext {hex}\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/mount -o ro /dev/sda3 /home/t14/toyos-log\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/umount /home/t14/toyos-log\n\
                 t14 ALL=(root) NOPASSWD: /usr/sbin/reboot \"\"\n\
                 t14 ALL=(root) NOPASSWD: /usr/bin/true \"\"\n"
            )
        );
    }

    /// A one-field mutation has to move both renderings at once, which is what
    /// makes them one table rather than two lists that agree today.
    #[test]
    fn one_table_feeds_the_rule_and_the_argv() {
        let mut moved = target();
        moved.node = Node::parse("/dev/sdb").unwrap();
        moved.log_part = 2;
        let rule = moved.sudoers().unwrap();
        assert!(rule.contains("/usr/bin/mount -o ro /dev/sdb2 /home/t14/toyos-log"), "{rule}");
        assert_eq!(
            moved.argv(Job::Mount, None).unwrap(),
            ["/usr/bin/mount", "-o", "ro", "/dev/sdb2", "/home/t14/toyos-log"]
        );
        assert!(rule.contains("/usr/sbin/wipefs --all /dev/sdb"), "{rule}");
        assert_eq!(moved.argv(Job::Wipe, None).unwrap(), ["/usr/sbin/wipefs", "--all", "/dev/sdb"]);
        assert!(!rule.contains("/dev/sda"), "{rule}");
    }

    #[test]
    fn a_boot_entry_the_firmware_did_not_name_cannot_be_filled_in() {
        let t = target();
        assert_eq!(t.argv(Job::BootNext, Some("001D")).unwrap().last().unwrap(), "001D");
        assert_eq!(t.argv(Job::Delete, Some("ffff")).unwrap().last().unwrap(), "ffff");
        for bad in ["12345", "00g1", "1", "", "0 1", "../."] {
            assert_eq!(
                t.argv(Job::BootNext, Some(bad)),
                Err(Refusal::BootEntry(bad.to_string())),
                "{bad}"
            );
        }
    }

    #[test]
    fn an_account_name_that_would_widen_the_rule_is_refused() {
        for user in [
            "t14,root",
            "t14 root",
            "t14:x",
            "%sudo",
            "+netgroup",
            "",
            "t*",
            "t14 ALL=(root) NOPASSWD",
            "t14\t",
            // The three a word escapes differently from an argument, so the
            // argument rendering can never reach the user field.
            r"t14\root",
            "t14(x",
            "t14)x",
        ] {
            let mut t = target();
            t.user = user.to_string();
            assert_eq!(t.sudoers(), Err(Refusal::Word(user.to_string())), "{user}");
            let host = ["--host".to_string(), format!("{user}@box")];
            assert!(
                matches!(Args::parse(&host), Err(Refusal::Word(_) | Refusal::Usage(_))),
                "{user}"
            );
        }
    }

    #[test]
    fn a_sudoers_word_escapes_what_sudoers_reads() {
        let escaped = |text: &str| Word::Is(text.to_string()).sudoers();
        assert_eq!(escaped("of=/dev/sda"), r"of\=/dev/sda");
        assert_eq!(escaped("a,b"), r"a\,b");
        assert_eq!(escaped("a:b"), r"a\:b");
        assert_eq!(escaped("^a"), r"\^a");
        assert_eq!(Word::Hex4.sudoers(), "[0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f][0-9A-Fa-f]");
        assert_eq!(escaped(LOADER), r"\\\\EFI\\\\BOOT\\\\BOOTX64.EFI");
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
            t.identity(),
            "cat /sys/class/block/sda/size /sys/class/block/sda/device/vendor \
             /sys/class/block/sda/device/model /sys/class/block/sda/removable"
        );
        assert_eq!(
            t.remote(Job::Flash, None).unwrap(),
            "sudo -n '/usr/bin/dd' 'of=/dev/sda' 'bs=4M' 'conv=fsync'"
        );
        assert_eq!(
            t.remote(Job::Create, None).unwrap(),
            "sudo -n '/usr/bin/efibootmgr' '--create-only' '--disk' '/dev/sda' '--part' '1' \
             '--label' 'ToyOS' '--loader' '\\EFI\\BOOT\\BOOTX64.EFI'"
        );
    }

    #[test]
    fn a_create_that_moved_the_boot_order_is_refused() {
        let guid = "69ddc8f6-fab2-423f-9818-93bb0ba7349c";
        let before = "BootCurrent: 0001\nBootOrder: 0001,001D\n\
             Boot0001* Ubuntu\tHD(1,GPT,16c1f60f-0f7b-4c3d-ba3f-5d75df1fe7bf,0x800,0x1000)\
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
        assert!(!Refusal::BootOrder { before: String::new(), now: "0002".to_string() }
            .about_the_boot());
    }

    #[test]
    fn a_boot_entry_is_matched_on_the_partition_its_path_names() {
        let listing = "BootCurrent: 0001\nBootOrder: 0001,001D\n\
             Boot0001* Ubuntu\tHD(1,GPT,16c1f60f-0f7b-4c3d-ba3f-5d75df1fe7bf,0x800,0x219800)\
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

    #[test]
    fn a_failed_boot_and_a_loop_that_could_not_run_are_different_answers() {
        assert!(Refusal::Log(bootlog::Unfit::NoBootRecord).about_the_boot());
        assert!(Refusal::Log(bootlog::Unfit::Unfinished("x".to_string())).about_the_boot());
        assert!(Refusal::Silent { what: "come back", secs: RETURN_SECS }.about_the_boot());
        assert!(!Refusal::Node("/dev/nvme0n1".to_string()).about_the_boot());
        assert!(!Refusal::Sudo("a password is required".to_string()).about_the_boot());
        assert!(!Refusal::Landed { what: "dd".to_string(), want: 1, got: 2 }.about_the_boot());
        assert!(!Refusal::NoHome.about_the_boot());
    }

    #[test]
    fn dd_is_believed_only_where_it_states_a_count() {
        let real = "44+0 records in\n44+0 records out\n\
                    184549376 bytes (185 MB, 176 MiB) copied, 12.3169 s, 15.0 MB/s\n";
        assert_eq!(dd_copied(real), Some(184_549_376));
        let short = "10+0 records in\n10+0 records out\n\
                     41943040 bytes (42 MB, 40 MiB) copied, 3.1 s, 13.5 MB/s\n";
        assert_eq!(dd_copied(short), Some(41_943_040));
        assert_eq!(dd_copied("44+0 records in\n44+0 records out\n"), None);
        assert_eq!(dd_copied(""), None);
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
    fn the_arguments_name_the_t14_by_default() {
        let args = Args::parse(&[]).unwrap();
        assert_eq!(args.target.user, "t14");
        assert_eq!(args.target.node.whole(), "/dev/sda");
        assert_eq!(args.wait_secs, RETURN_SECS);
        assert!(!args.dry_run);

        let words: Vec<String> = ["--dry-run", "--device", "/dev/sdb", "--host", "runner@box"]
            .iter()
            .map(|w| (*w).to_string())
            .collect();
        let args = Args::parse(&words).unwrap();
        assert!(args.dry_run);
        assert_eq!(args.target.node.whole(), "/dev/sdb");
        assert_eq!(args.target.user, "runner");
        assert_eq!(args.target.host, "box");

        let nvme = ["--device".to_string(), "/dev/nvme0n1".to_string()];
        assert_eq!(Args::parse(&nvme), Err(Refusal::Node("/dev/nvme0n1".to_string())));
        assert!(matches!(Args::parse(&["--image".to_string()]), Err(Refusal::Usage(_))));
        assert!(matches!(Args::parse(&["--flash".to_string()]), Err(Refusal::Usage(_))));
        assert!(matches!(
            Args::parse(&["--wait-secs".to_string(), "soon".to_string()]),
            Err(Refusal::Usage(_))
        ));
    }

    /// A disk carrying `parts` in order, so `admit`'s refusals are exercised on
    /// tables rather than on prose about tables.
    fn disk(parts: &[(&str, &gpt::partition_types::Type)]) -> Vec<u8> {
        use std::io::Cursor;
        const TOTAL: usize = 16 * 1024 * 1024;
        let mut bytes = vec![0u8; TOTAL];
        let mut cursor = Cursor::new(&mut bytes);
        gpt::mbr::ProtectiveMBR::with_lb_size((TOTAL / 512 - 1) as u32)
            .overwrite_lba0(&mut cursor)
            .expect("the protective MBR");
        let mut gdisk = gpt::GptConfig::default()
            .initialized(false)
            .writable(true)
            .logical_block_size(gpt::disk::LogicalBlockSize::Lb512)
            .create_from_device(Box::new(cursor), None)
            .expect("a table");
        gdisk.update_partitions(std::collections::BTreeMap::new()).expect("an empty table");
        for (name, kind) in parts {
            gdisk
                .add_partition(name, 256 * 1024, (*kind).clone(), 0, Some(2048))
                .expect("a partition");
        }
        let mut device = gdisk.write().expect("the table");
        device.seek(SeekFrom::Start(0)).expect("rewind");
        let mut out = vec![0u8; TOTAL];
        device.read_exact(&mut out).expect("read back");
        out
    }

    const TOYOS_ROOT: gpt::partition_types::Type = gpt::partition_types::Type {
        guid: toyos_gpt::Guid::TOYOS_ROOT_TEXT,
        os: gpt::partition_types::OperatingSystem::None,
    };

    #[test]
    fn an_image_admits_only_the_table_the_installed_rule_names() {
        let dir = std::env::temp_dir().join(format!("toyos-metal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let t = target();
        let write = |name: &str, bytes: &[u8]| {
            let at = dir.join(name);
            std::fs::write(&at, bytes).expect("write");
            at
        };
        let esp = gpt::partition_types::EFI;
        let basic = gpt::partition_types::BASIC;

        assert_eq!(
            admit(&write("ragged.img", &vec![0u8; 1000]), &t),
            Err(Refusal::Sectors { bytes: 1000 })
        );
        assert_eq!(admit(&write("empty.img", &[]), &t), Err(Refusal::Sectors { bytes: 0 }));
        assert_eq!(
            admit(&write("headless.img", &vec![0u8; 1024]), &t),
            Err(Refusal::BackupHeader { at: 512, saw: "\0\0\0\0\0\0\0\0".to_string() })
        );
        // The signature in the *primary* header alone: the false pass a healthy
        // front hides, which is why the check reads the final sector.
        let mut front = vec![0u8; 1536];
        front[512..520].copy_from_slice(b"EFI PART");
        assert!(matches!(
            admit(&write("front-only.img", &front), &t),
            Err(Refusal::BackupHeader { .. })
        ));

        let two = disk(&[("a", &esp), ("b", &esp), ("log", &basic), ("root", &TOYOS_ROOT)]);
        assert_eq!(
            admit(&write("two-esps.img", &two), &t),
            Err(Refusal::Partitions { what: "ESP", matched: 2 })
        );
        let no_log = disk(&[("esp", &esp), ("root", &TOYOS_ROOT)]);
        assert_eq!(
            admit(&write("no-log.img", &no_log), &t),
            Err(Refusal::Partitions { what: "TOYOS-LOG", matched: 0 })
        );
        // The log where the installed rule does not name it: p2, not p3.
        let moved = disk(&[("esp", &esp), ("log", &basic), ("root", &TOYOS_ROOT)]);
        assert_eq!(
            admit(&write("moved-log.img", &moved), &t),
            Err(Refusal::PartitionIndex { what: "TOYOS-LOG", want: 3, got: 2 })
        );
        // And the shape `src/image.rs` writes, which is the one admitted.
        let built = disk(&[("esp", &esp), ("root", &TOYOS_ROOT), ("log", &basic)]);
        let ok = admit(&write("built.img", &built), &t).expect("the built shape is admitted");
        assert_eq!((ok.esp.index, ok.log.index), (1, 3));
        assert_eq!(ok.bytes % u64::from(LBA), 0);
        assert_ne!(ok.esp.guid, toyos_gpt::Guid::ZERO);

        std::fs::remove_dir_all(&dir).expect("clean up");
    }
}
