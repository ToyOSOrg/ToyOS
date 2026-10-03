//! The metal profile: which registrations run on the T14, how they batch into
//! images, and what each one's verdict is over the log the stick came back with.
//!
//! **A registration's metal declaration is a separate table**, so a test that
//! runs only under QEMU simply has no row. What a row says is: the boots this
//! test needs — each a boot config, the parameters its image is armed with, and
//! the jobs it needs in that boot's job list — and one predicate over the
//! readbacks those boots produced, in the order the row names them.
//!
//! Two tests naming the same boot share one image and one flash; their job
//! lists are unioned. That is what makes the suite cost boots rather than
//! tests, and one boot is about a minute of the machine's time — so the boot is
//! something an arm names rather than something derived, because sharing is not
//! always safe and only the author knows.

use std::cell::RefCell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use toyos_build::bootlog;
use toyos_build::metalimage;
use toyos_build::metaltimings::{self, Machine, Reading, Record};
use toyos_build::testargs::MetalMode;

use super::serial::Serial;

/// One boot a metal test needs.
pub struct Arm {
    /// **The boot this test rides, named.** Two arms naming one boot share an
    /// image, one flash and one minute of the machine; they must agree on its
    /// config and parameters, and [`batches`] refuses a pair that does not.
    ///
    /// Named rather than derived from (config, parameters), because sharing is
    /// not always safe and only the author knows: `mkdir_cap` fills the
    /// machine-wide directory cap and leaves it there, so `readdir_bound`'s own
    /// `create_dir` on that boot is refused with `OutOfMemory` and it panics. A
    /// test that must not share names its own; it costs a minute and it says so.
    pub boot: &'static str,
    /// The boot config's directory, relative to the repository root.
    pub config: &'static str,
    /// What the image is armed with. Every name here is judged by the
    /// pre-flash gate (`toyos_build::metal::FLASHABLE`) before it is written.
    pub params: &'static [&'static str],
    /// Binary names and runner builtins this test needs in the job list, in the
    /// order it needs them. `reboot` is appended to every list by the
    /// derivation and is never written here.
    pub jobs: &'static [&'static str],
    /// The kernel build this boot needs, empty for the one an image ships.
    ///
    /// **The metal profile flashes test images** (the track's ruling), so a boot
    /// may ask for `build::TEST_KERNEL` — which is the only way the eight
    /// `SYS_DEBUG` binaries reach the machine at all. Empty is the shipping
    /// kernel, and that is what most of the suite wants: it is the artifact the
    /// owner flashes.
    pub features: &'static [&'static str],
    /// The PCI function this boot's image claims: the loop refuses, before the
    /// flash, a machine holding no address on it, and a judge holds the MAC the
    /// boot's driver read to the one the operating system before the flash
    /// read off it. **`None` on every boot that does not ask**: a boot that
    /// needs no cable would be refused for one that is out.
    pub nic: Option<&'static str>,
    /// **The boot is talked to over its own cable.** Its image authorizes a
    /// key minted beside it, and the loop — told `--talk` — reads the log the
    /// machine serves under its name, pings the address the name answers with,
    /// runs a command there and tells it to reboot. `false` on every boot whose
    /// judge reads the stick alone.
    pub talk: bool,
}

/// The ordinary arm: one boot, and the fields a caller must still say.
///
/// A constructor rather than a `Default`, because `boot`, `config` and `jobs`
/// have no sensible default and a partially-defaulted arm is how one ends up
/// riding a machine nobody chose.
pub const fn once(
    boot: &'static str,
    config: &'static str,
    params: &'static [&'static str],
    jobs: &'static [&'static str],
) -> Arm {
    Arm { boot, config, params, jobs, features: &[], nic: None, talk: false }
}

/// One boot carrying members that are **discovered rather than registered**.
///
/// The shared block's binaries are files under `tests/toyos-rust-tests/src/bin/`
/// and its C cases are files under `tests/testcases/tinycc/`; no `&'static`
/// table can name them, and there is nothing to say about each one that is not
/// said about all of them — every member is judged the same way, by the kernel's
/// exit record for it. So they are a boot with a list rather than a row each,
/// and each member is still reported under its own name.
///
/// A chunk rides one flash. A member that takes the machine down takes every
/// member after it *in its chunk* with it, and that is the honest price: on the
/// T14 there is no `MAX_SHARED_REBOOTS` to answer a dead guest with a new one.
#[derive(Clone)]
pub struct SharedBoot {
    pub boot: String,
    pub config: &'static str,
    pub params: &'static [&'static str],
    /// The kernel build, empty for the one an image ships.
    pub features: &'static [&'static str],
    pub members: NonZeroUsize,
    /// What the runner spawns, in order — the whole binary name, `test_rs_`
    /// prefix and all, because that is what the kernel records it under.
    pub jobs: Vec<String>,
    /// Files this boot needs on ROOT beside the binaries: a corpus's committed
    /// expectations, which the guest compares against because on this machine
    /// no host can read what a case printed.
    pub files: Vec<(String, Vec<u8>)>,
    /// Names in `bin/` that reach one binary, so the kernel records each run
    /// under a name of its own. `(from, to)` as the manifest spells them.
    pub links: Vec<(String, String)>,
}

/// How many members allowed `allowance_ms` each one shared boot holds: the
/// runner's [`toyos_tco::JOB_BOUND_MS`] less a tenth of it, shared among them.
pub const fn members_fitting(allowance_ms: u64) -> NonZeroUsize {
    let members = (toyos_tco::JOB_BOUND_MS - toyos_tco::JOB_BOUND_MS / 10) / allowance_ms;
    NonZeroUsize::new(members as usize).expect("a chunk holds a member")
}

/// The name of one chunk of a boot that had to be cut in two.
fn chunk_name(boot: &str, index: usize) -> String {
    if index == 0 {
        boot.to_string()
    } else {
        format!("{boot}-{}", index + 1)
    }
}

/// **A chunk carries only the files and links its own members name.** The C
/// corpus stages a binary and an expectation per case; putting all of both on
/// every chunk would double a flash that is already written over `ssh`.
fn sized(shared: &[SharedBoot]) -> Vec<SharedBoot> {
    let mut out = Vec::new();
    for boot in shared {
        for (index, jobs) in boot.jobs.chunks(boot.members.get()).enumerate() {
            let named: BTreeSet<&str> = jobs.iter().map(String::as_str).collect();
            let mine = |path: &str| {
                let last = path.rsplit('/').next().unwrap_or(path);
                named.contains(last)
                    || last.strip_prefix("test_c_").is_some_and(|c| named.contains(c))
            };
            out.push(SharedBoot {
                boot: chunk_name(&boot.boot, index),
                config: boot.config,
                params: boot.params,
                features: boot.features,
                members: boot.members,
                jobs: jobs.to_vec(),
                files: boot
                    .files
                    .iter()
                    .filter(|(path, _)| mine(path))
                    .cloned()
                    .collect(),
                links: boot.links.iter().filter(|(from, _)| mine(from)).cloned().collect(),
            });
        }
    }
    out
}

/// How a registration runs on the T14.
pub struct Metal {
    pub arms: &'static [Arm],
    /// The readbacks in `arms` order.
    pub judge: fn(&[&Readback]) -> Result<(), String>,
}

/// What one boot left on the stick, and what the host clock saw of it.
pub struct Readback {
    pub label: String,
    /// The directory the loop wrote this boot's files into.
    home: PathBuf,
    loader: String,
    /// The kernel's own records of every `logkeeper` file this boot wrote.
    kernel: String,
    /// Every line of those files, the programs' included.
    log: String,
    /// `Boot: complete (Nms)`, or `None` on a boot that never got there.
    pub boot_ms: Option<u64>,
    /// What the machine spent getting back to `sshserver`.
    pub back_secs: u64,
    /// How long after that the boot stick's own partition was there again.
    pub stick_secs: u64,
    /// The MAC of the function this boot's image claims, as the operating
    /// system before the flash read it, and `None` on every boot that named no
    /// function.
    pub wire_mac: Option<String>,
    /// The machine the loop read before the flash.
    pub machine: Result<Machine, String>,
    /// What this boot's judges measured, for the machine's record to judge.
    numbers: RefCell<BTreeMap<String, u64>>,
}

impl Readback {
    /// One boot's readback out of the three files the loop wrote for it:
    /// `loader.log`, the `logkeeper` files as one text, and the boot file.
    pub fn new(label: &str, home: PathBuf, loader: String, log: String, boot: &str) -> Result<Self, String> {
        let kernel = bootlog::kernel_records(&log);
        let back_secs = toyos_build::metal::back_secs(boot)
            .ok_or_else(|| format!("{label}'s boot file names no `back_secs`: {boot:?}"))?;
        let stick_secs = toyos_build::metal::stick_secs(boot)
            .ok_or_else(|| format!("{label}'s boot file names no `stick_secs`: {boot:?}"))?;
        Ok(Readback {
            label: label.to_string(),
            home,
            boot_ms: bootlog::boot_millis(&kernel),
            loader,
            kernel,
            log,
            back_secs,
            stick_secs,
            wire_mac: toyos_build::metal::wire_mac(boot),
            machine: toyos_build::metal::machine(boot)
                .map_err(|why| format!("{label}'s boot file {why}")),
            numbers: RefCell::new(BTreeMap::new()),
        })
    }

    /// What the loop heard over the boot's own cable, and the log the machine
    /// served it — or why a boot that was to be talked to has neither.
    ///
    /// **Absent is a finding here, never an empty answer**: the loop writes
    /// the conversation before anything can refuse, so a talking boot's
    /// readback without one is a loop that was not told `--talk`.
    pub fn talk(&self) -> Result<(toyos_build::metaltalk::Heard, Vec<String>), String> {
        let at = self.home.join(toyos_build::metal::READBACK_TALK);
        let text = std::fs::read_to_string(&at).map_err(|e| {
            format!("{}: {e} — this boot's loop was not told --talk", at.display())
        })?;
        let heard = toyos_build::metaltalk::Conversation::parse(&text)?.ok_or_else(|| {
            format!("{}'s loop heard nothing over the cable:\n{text}", self.label)
        })?;
        let at = self.home.join(toyos_build::metal::READBACK_STREAM);
        let stream = std::fs::read_to_string(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        Ok((heard, stream.split_inclusive('\n').map(str::to_string).collect()))
    }

    /// One file off the log volume that is neither the loader's nor `logkeeper`'s,
    /// read out of the partition's own bytes; `None` where the volume has no
    /// such file.
    ///
    /// **The loop copies two kinds of file off the mount and this is neither**,
    /// so it comes out of `metal::READBACK_VOLUME` — which the loop keeps on
    /// every boot that came back, because the outside judge runs on every one.
    pub fn log_volume_file(&self, name: &str) -> Result<Option<String>, String> {
        let at = self.home.join(toyos_build::metal::READBACK_VOLUME);
        let volume = std::fs::read(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        let found = super::volumes::read_files(&volume, &[name])?.pop().flatten();
        found
            .map(|bytes| {
                String::from_utf8(bytes).map_err(|e| format!("{}'s {name}: {e}", self.label))
            })
            .transpose()
    }

    /// Every `logkeeper` file this boot wrote, as one text, less every program's
    /// line ([`bootlog::kernel_records`]): no program's line is read as the kernel's.
    /// It ends where the supervisor had `logkeeper` make it whole, so what the kernel writes
    /// inside the stop or a wedge is only on the page ([`Self::after_the_reset`]).
    pub fn kernel(&self) -> Serial {
        Serial::named(&format!("{}'s kernel log", self.label), self.kernel.as_str())
    }

    /// The same files whole, the programs' lines included.
    pub fn log(&self) -> Serial {
        Serial::named(&format!("{}'s log", self.label), self.log.as_str())
    }

    /// `loader.log`, both passes: the one before the kernel handoff and, under
    /// `loaderlog::SEPARATOR`, the one that read the black box afterwards.
    pub fn loader(&self) -> Serial {
        Serial::named(&format!("{}'s loader.log", self.label), self.loader.as_str())
    }

    /// When the last record this boot left was written, in milliseconds since
    /// boot. `None` on a log with no record at all.
    pub fn last_record_ms(&self) -> Option<u64> {
        bootlog::last_record_millis(&self.kernel)
    }

    /// Whether this boot's log is whole to its stop, and the stop's own tail
    /// is on the page.
    ///
    /// **The file ends where the supervisor had `logkeeper` make it whole.** The stop stops
    /// `logkeeper` with every other thread, so what the kernel says from there on
    /// goes on the black-box page under the boot's `DONE` seal and comes back
    /// in the next loader pass, and this reads it there — for every boot,
    /// because the suite's whole verdict is read out of those two files.
    pub fn log_reached_the_stick(&self) -> Result<(), String> {
        let after = self.after_the_reset()?;
        let text = after.text();
        // A boot its own deadline or the lockup detector ended never finished
        // `quiesce`, so it seals no tail. Owed by the boots that handed the
        // machine back, and only by them.
        if !text.contains(bootlog::HANDED_BACK) {
            return Ok(());
        }
        if bootlog::stopping_line(&self.log).is_none() {
            return Err(format!(
                "{}'s pass after the reset read DONE, and its log carries no {:?} from the supervisor: \
                 nothing made the file whole before the stop",
                self.label,
                bootlog::STOPPING,
            ));
        }
        if !text.contains(bootlog::LOG_TAIL_HEAD) || bootlog::handed_back(text).is_err() {
            return Err(format!(
                "{}'s pass after the reset carries no {:?} ending in {:?}: this boot's kernel \
                 sealed no tail of its stop, so nothing says what it did after the file",
                self.label,
                bootlog::LOG_TAIL_HEAD,
                bootlog::REBOOTING,
            ));
        }
        Ok(())
    }

    /// Whether this boot's deadline, where it expired, fired within one timer
    /// period of its bound: every CPU a staged wedge holds re-arms a one-shot of
    /// one scheduler quantum, and the timer entry is what polls. Read out of the
    /// record the pass after the reset printed, because that is the only
    /// channel a wedged boot has.
    pub fn deadline_on_time(&self) -> Result<(), String> {
        let Some(late) = toyos_build::metal::deadline_lateness_ms(&self.loader, &self.kernel)
        else {
            return Ok(());
        };
        within_one_period(late?, toyos_sched::fair::QUANTUM_NS)
            .map_err(|why| format!("{}'s boot deadline {why}", self.label))
    }

    /// What the on-screen panel cost this boot, off the kernel's own census.
    ///
    /// **Off the page, because no file carries it.** A boot that hands the
    /// machine back writes the census inside its stop, after `logkeeper` has
    /// stopped, and seals it among its tail; a boot a bound ended seals it with
    /// its record. The page from *this* boot is the one after the separator:
    /// an earlier chain's report can sit in the pass before it.
    pub fn panel(&self) -> Option<bootlog::Panel> {
        let after = self.after_the_reset().ok()?;
        bootlog::panel_census(after.text())
    }

    /// The same for the other bound: a hard-lockup sample finds a stuck cpu
    /// within one of its sample periods past its bound. Read out of the same
    /// channel and for the same reason.
    pub fn lockup_on_time(&self) -> Result<(), String> {
        let Some(late) = toyos_build::metal::lockup_lateness_ms(&self.loader) else {
            return Ok(());
        };
        within_one_period(late?, toyos_tco::HARD_LOCKUP_SAMPLE_NS)
            .map_err(|why| format!("{}'s hard-lockup detector {why}", self.label))
    }

    /// The stop's own record, off the page its tail is sealed on: the stop
    /// writes it after `logkeeper` has stopped, so no file carries it.
    fn stop_record(&self) -> Option<toyos_quiesce::Record> {
        toyos_build::metal::park(self.after_the_reset().ok()?.text())
    }

    pub fn stop_completed(&self) -> Result<(), String> {
        let handed_back =
            self.after_the_reset().is_ok_and(|after| after.text().contains(bootlog::HANDED_BACK));
        match self.stop_record() {
            None if handed_back => Err(format!(
                "{} handed the machine back and its page carries no record of the stop",
                self.label
            )),
            None => Ok(()),
            Some(park) if !park.stopped_the_machine() => Err(format!(
                "{}'s stop gave up on {} userland thread(s) that never reached a safe point, so \
                 this boot's sync and its last word are claims about a machine that was still \
                 running:\n    {park}",
                self.label,
                park.sweep.running,
            )),
            Some(park) if park.in_flight != 0 => Err(format!(
                "{}'s stop ended with {} block operation(s) open:\n    {park}",
                self.label, park.in_flight,
            )),
            Some(_) => Ok(()),
        }
    }

    /// The loader pass **after** the kernel's reset, which is where a chain
    /// report is. `None` where the chain did not go round — which for a boot
    /// that ended itself is a finding, not an absence.
    pub fn after_the_reset(&self) -> Result<Serial, String> {
        let after = bootlog::after_the_reset(&self.loader).ok_or_else(|| {
            format!(
                "{}'s loader.log carries no pass after the reset: the loader did not point \
                 `BootNext` at itself, or the machine went back to the boot manager\n{}",
                self.label, self.loader
            )
        })?;
        Ok(Serial::named(&format!("{}'s loader pass after the reset", self.label), after))
    }

    /// What the kernel recorded about the process the *runner* spawned as
    /// `binary`, or why there is no such record.
    ///
    /// **The name is the file's, truncated the way the kernel truncates it.**
    /// `ProcessEntry`'s name is `THREAD_NAME_LEN` bytes and the loader fills it
    /// from the path's last component, so a binary whose name is longer than
    /// that is recorded under a prefix: the staged `null_sink_client_exits`
    /// ends `…_client_ex` on the wire, and a predicate looking for the whole
    /// name would find nothing on a perfectly good boot.
    ///
    /// **And the name alone does not identify one process.** A guest binary that
    /// cannot ask what a handle it does not hold does re-executes *itself*, one
    /// child per fault, and every child is recorded under that same name with whatever
    /// exit the fault gave it. Measured on a metal-shaped guest that binary left
    /// forty-two records reading `code=139` and the job's own reading zero, and
    /// the last of them is a child. The job's is the one with the **lowest
    /// pid**: the runner spawned it before it spawned anything.
    ///
    /// A name is written here without the staged `test_rs_` prefix on purpose:
    /// `suite_split` reads that spelling as a machine test *driving* the
    /// binary, and this only says what the kernel recorded about one.
    pub fn exit_code(&self, binary: &str) -> Result<i32, String> {
        let name = bootlog::recorded_name(binary);
        let head = format!("{}{name} pid=", bootlog::EXIT);
        let mut family: Vec<(u64, i32)> = Vec::new();
        for line in self.kernel.lines().filter_map(bootlog::message).filter(|m| m.starts_with(&head)) {
            let field = |label: &str| -> Option<&str> {
                line.split_once(label).and_then(|(_, rest)| rest.split_whitespace().next())
            };
            let (Some(pid), Some(code)) = (field(" pid="), field(" code=")) else { continue };
            let (Ok(pid), Ok(code)) = (pid.parse(), code.parse()) else {
                return Err(format!("unreadable exit record: {line:?}"));
            };
            family.push((pid, code));
        }
        family
            .into_iter()
            .min_by_key(|(pid, _)| *pid)
            .map(|(_, code)| code)
            .ok_or_else(|| {
                format!(
                    "no `{head}` record in {}'s log: {binary} never ran, or never ended",
                    self.label
                )
            })
    }

    /// One number this boot measured, judged by [`run`] against this machine's
    /// record once every judge has spoken. A second value under one name is
    /// refused: which of the two a record kept would be nobody's reading.
    pub fn measured(&self, name: &str, value: u64) -> Result<(), String> {
        match self.numbers.borrow_mut().entry(name.to_string()) {
            Entry::Vacant(slot) => {
                slot.insert(value);
                Ok(())
            }
            Entry::Occupied(was) => Err(format!(
                "{} measured {name} twice, {} and then {value}",
                self.label,
                was.get()
            )),
        }
    }

    /// The job ran and the kernel recorded it exiting cleanly.
    pub fn job_passed(&self, binary: &str) -> Result<(), String> {
        match self.exit_code(binary)? {
            0 => Ok(()),
            code => Err(format!(
                "{binary} exited {code} on the T14; its own lines are in the boot's log under \
                 the name of whoever ran it"
            )),
        }
    }

    /// How many CPUs this machine brought up, off the SMP bring-up records —
    /// which is a different source from the `control_regs:` lines a caller
    /// then holds to it.
    pub fn cpus(&self) -> Result<u32, String> {
        let log = self.kernel();
        if let Some(line) = log.text().lines().find(|l| l.contains("failed to start")) {
            return Err(format!("an AP did not come up on this machine: {line}"));
        }
        let aps = log
            .text()
            .lines()
            .filter(|l| l.contains(bootlog::AP_BRINGUP) && l.contains(" online"))
            .count();
        if aps == 0 {
            return Err(format!(
                "no `{}` record in {}'s log, so this machine reports one CPU and every \
                 SMP assertion over it would be vacuous\n{}",
                bootlog::AP_BRINGUP,
                self.label,
                log.text()
            ));
        }
        Ok(u32::try_from(aps).expect("a CPU count") + 1)
    }
}

/// A bound's lateness against the period of what polls it, and a millisecond
/// either way for the two floored readings the lateness is the difference of.
fn within_one_period(late_ms: i64, period_ns: u64) -> Result<(), String> {
    let period_ms = i64::try_from(period_ns / 1_000_000).expect("a period in milliseconds");
    if (-1..=period_ms + 1).contains(&late_ms) {
        Ok(())
    } else {
        Err(format!("fired {late_ms} ms past its bound, and what polls it runs every {period_ms} ms"))
    }
}

/// One image, and every test that rides it.
struct Batch {
    config: &'static str,
    params: Vec<&'static str>,
    features: &'static [&'static str],
    jobs: Vec<String>,
    files: Vec<(String, Vec<u8>)>,
    links: Vec<(String, String)>,
    /// [`Arm::nic`], carried to the invocation that drives this boot.
    nic: Option<&'static str>,
    /// [`Arm::talk`], carried to the image and to the invocation.
    talk: bool,
}

impl Batch {
    fn add(&mut self, jobs: impl IntoIterator<Item = String>) {
        for job in jobs {
            if !self.jobs.contains(&job) {
                self.jobs.push(job);
            }
        }
    }
}

/// Where a batch's derived config, its image and its readback live.
pub fn at(dir: &Path, label: &str) -> PathBuf {
    dir.join(label)
}

/// The boots `tests` need, keyed by the name their arms give them.
///
/// **Two arms naming one boot are refused where they disagree about it**: an
/// image is one config armed one way, and a silent winner would give one of the
/// two tests a machine it did not ask for.
fn batches(
    tests: &[(&str, &'static Metal)],
    shared: &[SharedBoot],
) -> Result<BTreeMap<String, Batch>, String> {
    let mut out: BTreeMap<String, Batch> = BTreeMap::new();
    // First, so a registration naming a shared boot rides it rather than
    // minting a second one under the same name with a different list.
    for boot in shared {
        // A boot nothing selected is a boot nothing has to flash.
        if boot.jobs.is_empty() {
            continue;
        }
        let was = out.insert(
            boot.boot.clone(),
            Batch {
                config: boot.config,
                params: boot.params.to_vec(),
                features: boot.features,
                jobs: boot.jobs.clone(),
                files: boot.files.clone(),
                links: boot.links.clone(),
                nic: None,
                talk: false,
            },
        );
        if was.is_some() {
            return Err(format!("two shared boots are both named {:?}", boot.boot));
        }
    }
    for (name, decl) in tests {
        let Metal { arms, .. } = decl;
        for arm in *arms {
            let batch = out.entry(arm.boot.to_string()).or_insert_with(|| Batch {
                config: arm.config,
                params: arm.params.to_vec(),
                features: arm.features,
                jobs: Vec::new(),
                files: Vec::new(),
                links: Vec::new(),
                nic: arm.nic,
                talk: arm.talk,
            });
            if batch.config != arm.config
                || batch.params != arm.params
                || batch.features != arm.features
                || batch.nic != arm.nic
                || batch.talk != arm.talk
            {
                return Err(format!(
                    "{name} rides the boot {:?} as ({}, {:?}, {:?}, {:?}, talk={}) and another row \
                     rides it as ({}, {:?}, {:?}, {:?}, talk={}); one boot is one image",
                    arm.boot,
                    arm.config,
                    arm.params,
                    arm.features,
                    arm.nic,
                    arm.talk,
                    batch.config,
                    batch.params,
                    batch.features,
                    batch.nic,
                    batch.talk
                ));
            }
            batch.add(arm.jobs.iter().map(|j| (*j).to_string()));
        }
    }
    Ok(out)
}

/// What `reachable`, a batch's text, names beside its `jobs`: every binary it
/// spells `test_rs_<name>` that is not one of them, and every shared library.
pub fn reached(
    reachable: &str,
    jobs: &[String],
    rust_bins: &[(String, Vec<u8>)],
) -> Vec<(String, Vec<u8>)> {
    // Whole: a longer name that opens with this one is another binary's.
    let names = |staged: &str| {
        reachable.match_indices(staged).any(|(at, _)| {
            !reachable[at + staged.len()..]
                .starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
    };
    let mut out = Vec::new();
    for (name, data) in rust_bins {
        let staged = format!("test_rs_{name}");
        // The shared libraries go on whole: a `dlopen` names a path and never a
        // `test_rs_` literal, so no text could find one — and all of them
        // together are a fraction of one helper binary.
        if name.ends_with(".so") {
            out.push((format!("lib/{name}"), data.clone()));
        } else if !jobs.contains(&staged) && names(&staged) {
            out.push((format!("bin/{staged}"), data.clone()));
        }
    }
    out
}

/// Build one batch's image, and answer where it landed.
fn build(
    root: &Path,
    dir: &Path,
    label: &str,
    batch: &Batch,
    rust_bins: &[(String, Vec<u8>)],
    quiet: bool,
) -> Result<PathBuf, String> {
    let home = at(dir, label);
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;

    let committed = root.join(batch.config).join("system.toml");
    let text = std::fs::read_to_string(&committed)
        .map_err(|e| format!("{}: {e}", committed.display()))?;
    let jobs: Vec<&str> = batch.jobs.iter().map(String::as_str).collect();
    let derived = metalimage::derive(&text, &jobs, &batch.links)
        .map_err(|why| format!("{}: {why}", committed.display()))?;
    // **The job list is in the file name, not only in the file.**
    // `build_test_image` memoizes ROOT on the config's *path*, so two runs whose
    // selection differs — a filtered one and the whole profile — would otherwise
    // share one cached ROOT and the second would boot the first one's job list.
    let config = home.join(format!("system-{:016x}.toml", fingerprint(&derived)));
    std::fs::write(&config, &derived).map_err(|e| format!("{}: {e}", config.display()))?;

    // Only what this batch's jobs name: a stick is written over `ssh`, so an
    // image carrying two hundred binaries it never runs is a minute of flash.
    let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
    for job in &batch.jobs {
        let Some(name) = job.strip_prefix("test_rs_") else { continue };
        let (_, data) = rust_bins
            .iter()
            .find(|(n, _)| n == name)
            .ok_or_else(|| format!("the job {job:?} names no binary under tests/toyos-rust-tests"))?;
        extra.push((format!("bin/{job}"), data.clone()));
    }
    extra.extend(batch.files.iter().cloned());
    // **What a job spawns comes with it, and it is not optional.** A binary that
    // cannot find its `.so` or its helper child does not fail an assertion — it
    // fails to *spawn*. Measured on a metal-shaped guest: `std_tls` did not run
    // at all and took the rest of the job list with it, and `disk_backtrace`
    // and `fault_gates` each panicked on `entity not found`
    // looking for a child nothing had staged.
    //
    // **Which binary, though, is read rather than assumed.** Staging all of them
    // on every image cost 150 MB a boot, and a stick is written over `ssh`. A
    // driver reaches a binary as the literal `test_rs_<name>` — the same
    // spelling `suite_split` reads the harness for — so the text a boot could
    // possibly name it in is its job list, its symlink targets, and the source
    // of every Rust job on it. A binary named nowhere in that is one this boot
    // cannot reach.
    if !batch.jobs.is_empty() {
        let bin = root.join("tests/toyos-rust-tests/src/bin");
        let mut reachable = batch.jobs.join(" ");
        for (from, to) in &batch.links {
            reachable.push(' ');
            reachable.push_str(from);
            reachable.push(' ');
            reachable.push_str(to);
        }
        for job in &batch.jobs {
            let Some(name) = job.strip_prefix("test_rs_") else { continue };
            if let Ok(source) = std::fs::read_to_string(bin.join(format!("{name}.rs"))) {
                reachable.push('\n');
                reachable.push_str(&source);
            }
        }
        extra.extend(reached(&reachable, &batch.jobs, rust_bins));
    }

    // The build a boot asked for, or — where it asked for none — the one its
    // arms imply: an actuator outside `kernel/src/params.rs` needs the kernel
    // that carries them, and nothing else does.
    let declared = toyos_build::build::declared_params(root);
    let implied: &[&str] = if batch.params.iter().all(|p| declared.iter().any(|d| d == p)) {
        &[]
    } else {
        toyos_build::build::TEST_KERNEL
    };
    let features = if batch.features.is_empty() { implied } else { batch.features };
    // **Every metal image, and it is not a field an arm may set.** A boot that
    // wedges after the scheduler is up ends nothing on this machine — the
    // chipset watchdog does not count on this PCH, the runner's own bound
    // reboots through the shutdown syscall the wedge may be inside, and the
    // loop then waits `metal::return_secs` and needs a hand on the power
    // button. Armed after the kernel build is decided above, because a
    // parameter carrying a value is not an actuator and must not pull the test
    // kernel in behind it.
    if let Some(own) = batch.params.iter().find(|p| p.starts_with(toyos_tco::DEADLINE_PARAM))
    {
        // Stated as a refusal rather than as a comment: two tokens leave the
        // kernel taking the first and a reader taking whichever they saw.
        return Err(format!(
            "{label} arms {own:?} of its own, and every metal image is armed with one \
             already — a boot that wants a different bound is a change here and not a \
             field on an arm"
        ));
    }
    let deadline = format!(
        "{}{}",
        toyos_tco::DEADLINE_PARAM,
        toyos_build::metal::bound_for(&batch.params)
    );
    let mut params: Vec<&str> = batch.params.clone();
    params.push(&deadline);
    // **A talking boot carries the key the loop will offer**,
    // minted beside the image so the loop finds it there. Nothing about this
    // host is in it: the loop finds the machine by its name.
    if batch.talk {
        let identity = super::ssh::Identity::mint_in(&talk_home(&home))?;
        extra.push((super::ssh::KEYS_ON_ROOT.to_string(), identity.authorized_line().into_bytes()));
    }
    let plan = toyos_build::build::Plan::new(toyos_build::arch::Arch::X86_64, &config, features, &params);
    let bytes = toyos_build::build::build_test_image(root, &plan, quiet, &extra);
    let image = home.join("image.img");
    std::fs::write(&image, &bytes).map_err(|e| format!("{}: {e}", image.display()))?;
    Ok(image)
}

fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Where a talking boot's key lives, beside its image.
fn talk_home(home: &Path) -> PathBuf {
    home.join("ssh")
}

fn invocation(image: &Path, home: &Path, nic: Option<&str>, talk: bool) -> Vec<String> {
    let mut words = vec![
        "run".to_string(),
        "--bin".to_string(),
        "toyos-metal".to_string(),
        "--".to_string(),
        "--image".to_string(),
        image.display().to_string(),
        "--readback".to_string(),
        home.display().to_string(),
        // Always: the outside judge on the volume the boot left costs one
        // read of the partition, and a suite that only ever reads a mounted
        // `/log` has no reader of those bytes that is not the family of code
        // that wrote them.
        "--fat32-check".to_string(),
    ];
    if let Some(nic) = nic {
        words.push("--nic".to_string());
        words.push(nic.to_string());
    }
    if talk {
        words.push("--talk".to_string());
        words.push(talk_home(home).join("id_ed25519").display().to_string());
    }
    words
}

/// One `toyos-metal` run. Its stderr is echoed here as it comes and kept, so a
/// failed run's verdict names the refusal it ended on beside its exit.
fn drive(root: &Path, words: &[String]) -> Result<(), String> {
    let mut child = Command::new("cargo")
        .args(words)
        .current_dir(root)
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("toyos-metal could not be started: {e}"))?;
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr was asked for piped"));
    let (mut said, mut line) = (String::new(), Vec::new());
    while stderr.read_until(b'\n', &mut line).map_err(|e| format!("toyos-metal's stderr: {e}"))? > 0 {
        let text = String::from_utf8_lossy(&line);
        eprint!("{text}");
        said.push_str(&text);
        line.clear();
    }
    let status = child.wait().map_err(|e| format!("toyos-metal: {e}"))?;
    if status.success() {
        return Ok(());
    }
    Err(match toyos_build::metal::said_refusal(&said) {
        Some(refusal) => format!("toyos-metal exited {status}: {refusal}"),
        None => format!("toyos-metal exited {status} and said no refusal"),
    })
}

pub fn read_readback(dir: &Path, label: &str) -> Result<Readback, String> {
    let home = at(dir, label);
    let read = |name: &str| -> Result<String, String> {
        let at = home.join(name);
        std::fs::read_to_string(&at).map_err(|e| {
            format!("{}: {e} — no readback for {label}; run the driver on its image first", at.display())
        })
    };
    toyos_build::metal::loop_verdict(&read(toyos_build::metal::READBACK_VERDICT)?)
        .map_err(|why| format!("toyos-metal refused {label}: {why}"))?;
    let loader = read(toyos_build::metal::READBACK_LOADER)?;
    let log = read(toyos_build::metal::READBACK_KERNEL)?;
    let boot = read(toyos_build::metal::READBACK_BOOT)?;
    Readback::new(label, home, loader, log, &boot)
}

/// The machine every boot that came back names, or why there is not one.
fn one_machine(readbacks: &BTreeMap<String, Result<Readback, String>>) -> Result<Machine, String> {
    let mut named: Option<Machine> = None;
    for back in readbacks.values().filter_map(|back| back.as_ref().ok()) {
        let machine = back.machine.clone()?;
        match &named {
            Some(first) if *first != machine => {
                return Err(format!("one run names two machines: {first:?} and {machine:?}"))
            }
            Some(_) => {}
            None => named = Some(machine),
        }
    }
    named.ok_or_else(|| "no boot came back to name the machine".to_string())
}

/// What a metal run established.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Every selected test passed on the machine, or — for [`MetalMode::List`]
    /// — the selection batched cleanly and nothing was touched.
    Green,
    /// One did not. **A red on the T14 is a red.**
    Red,
    /// The images were built and nothing was judged, because nothing has been
    /// run on the machine yet. Neither of the other two: a run that reached no
    /// hardware may not report on any.
    Staged,
}

/// The whole metal profile: batch, build, drive, judge, report.
// Each argument is one of the suite's own flags or tables, passed through
// once; a struct holding them would be a second name for the command line.
#[allow(clippy::too_many_arguments)]
pub fn run(
    mode: MetalMode,
    tests: &[(&str, &'static Metal)],
    shared: &[SharedBoot],
    rust_bins: &[(String, Vec<u8>)],
    quiet: bool,
) -> Verdict {
    let root = super::compile::repo_root();
    let shared = sized(shared);
    let shared = shared.as_slice();
    let batches = match batches(tests, shared) {
        Ok(batches) => batches,
        Err(why) => {
            eprintln!("[metal] {why}");
            return Verdict::Red;
        }
    };
    let runs: Vec<&(&str, &'static Metal)> = tests.iter().collect();
    eprintln!(
        "[metal] {} registration(s) and {} shared member(s) over {} boot(s)",
        runs.len(),
        shared.iter().map(|b| b.jobs.len()).sum::<usize>(),
        batches.len(),
    );
    if runs.is_empty() && shared.iter().all(|b| b.jobs.is_empty()) {
        eprintln!("[metal] nothing to run");
        return Verdict::Red;
    }

    let (dir, offline): (PathBuf, bool) = match &mode {
        MetalMode::List => {
            for (label, batch) in &batches {
                println!("{label}: {} job(s) — {:?}", batch.jobs.len(), batch.jobs);
            }
            return Verdict::Green;
        }
        MetalMode::Offline(dir) => (dir.clone(), true),
        MetalMode::Drive => (root.join("target/metal"), false),
    };
    let dir = dir.as_path();

    // A directory holding a readback for every boot is a run the machine has
    // already answered, and the only thing left is the judging.
    let answered = batches.keys().all(|label| {
        at(dir, label).join(toyos_build::metal::READBACK_KERNEL).is_file()
    });
    let judging = offline && answered;

    let mut images: BTreeMap<&str, PathBuf> = BTreeMap::new();
    if !judging {
        // The key a talking boot authorizes is minted by the harness's own ssh
        // client, which the suite builds only on its QEMU path.
        if batches.values().any(|b| b.talk) {
            toyos_build::build::build_host_judges(&root, quiet);
        }
        for (label, batch) in &batches {
            match build(&root, dir, label, batch, rust_bins, quiet) {
                Ok(image) => {
                    eprintln!(
                        "[metal] {label}: {} job(s), armed with {:?} — {}",
                        batch.jobs.len(),
                        batch.params,
                        image.display()
                    );
                    images.insert(label.as_str(), image);
                }
                Err(why) => {
                    eprintln!("[metal] {label}: {why}");
                    return Verdict::Red;
                }
            }
        }
    }

    if offline && !judging {
        let mut request = String::from(
            "# One boot per image. Each invocation is `cargo <words>` from this worktree.\n",
        );
        for (label, image) in &images {
            request.push_str(&format!(
                "\n{label}\n  image: {}\n  cargo {}\n",
                image.display(),
                invocation(image, &at(dir, label), batches[*label].nic, batches[*label].talk).join(" ")
            ));
        }
        let path = dir.join("request.txt");
        if let Err(e) = std::fs::write(&path, &request) {
            eprintln!("[metal] {}: {e}", path.display());
            return Verdict::Red;
        }
        print!("{request}");
        eprintln!(
            "[metal] staged {} image(s); {} lists them. The machine was not touched, so this \
             run establishes nothing about it.",
            images.len(),
            path.display()
        );
        // Not a pass: nothing was judged. A staging run that exited 0 would be
        // a green suite about hardware it never reached.
        return Verdict::Staged;
    }

    // **The driver's exit is the boot's verdict, and nothing below reads a file
    // instead of it.** `toyos-metal` exits 1 for the machine and 2 for the loop.
    let mut refused: BTreeMap<&str, String> = BTreeMap::new();
    if !offline {
        for (label, image) in &images {
            let words = invocation(image, &at(dir, label), batches[*label].nic, batches[*label].talk);
            eprintln!("[metal] {label}: cargo {}", words.join(" "));
            if let Err(why) = drive(&root, &words) {
                refused.insert(label, why);
            }
        }
    }

    // The readbacks, and the boot facts each one carries.
    let mut readbacks: BTreeMap<String, Result<Readback, String>> = BTreeMap::new();
    for label in batches.keys() {
        let back = match refused.get(label.as_str()) {
            Some(why) => Err(why.clone()),
            None => read_readback(dir, label),
        };
        readbacks.insert(label.clone(), back);
    }
    if judge_readbacks(&root, &readbacks, &runs, shared) {
        Verdict::Red
    } else {
        Verdict::Green
    }
}

/// Every verdict a run's readbacks carry, and this machine's record judged by
/// them and added to off the boots that passed: one function of the readbacks,
/// whether the loop wrote them a moment ago or a run long past did. Answers
/// whether anything was red.
pub fn judge_readbacks(
    root: &Path,
    readbacks: &BTreeMap<String, Result<Readback, String>>,
    runs: &[&(&str, &'static Metal)],
    shared: &[SharedBoot],
) -> bool {
    let mut red = false;
    // **A boot with any failure of its own adds no row**, whether the loop,
    // the boot's own checks, or a test or member riding it failed.
    let mut failed: BTreeSet<&str> = BTreeSet::new();
    eprintln!("\n[metal] the boots");
    for (label, back) in readbacks {
        let back = match back {
            Err(why) => {
                eprintln!("  FAIL {label}: {why}");
                failed.insert(label);
                continue;
            }
            Ok(back) => back,
        };
        let ms = back.boot_ms.map_or_else(|| "-".to_string(), |ms| ms.to_string());
        eprintln!(
            "  {label}: Boot: complete {ms} ms, back in {} s, the stick enumerated {} s after that",
            back.back_secs, back.stick_secs
        );
        let panel = back.panel();
        if let Some(panel) = panel {
            eprintln!(
                "    the panel painted {} time(s) and put {} px on the glass",
                panel.paints, panel.pixels
            );
        }
        let mut findings: Vec<String> = Vec::new();
        // The census crosses only on the page, and a page the pass after the
        // reset cleared as another image's carries none.
        let owes_a_panel = bootlog::foreign_done(&back.loader).is_err();
        for (field, value, owed) in [
            ("complete_ms", back.boot_ms, true),
            ("panel_max_us", panel.map(|panel| panel.max_micros), owes_a_panel),
            ("panel_us", panel.map(|panel| panel.micros), owes_a_panel),
        ] {
            let name = format!("boot.{label}.{field}");
            match value {
                Some(value) => findings.extend(back.measured(&name, value).err()),
                None if owed => findings.push(format!("{name}: this boot recorded none")),
                None => {}
            }
        }
        // **Every boot, and before any verdict is read out of its log.** A
        // test's judge reads the file the stick came back with, so a file that
        // stops before the boot does turns a machine fact into a missing line —
        // and the missing line is what a reader would have to guess about.
        findings.extend(back.log_reached_the_stick().err());
        findings.extend(back.stop_completed().err());
        findings.extend(back.deadline_on_time().err());
        findings.extend(back.lockup_on_time().err());
        for why in &findings {
            eprintln!("    FAIL {why}");
        }
        if !findings.is_empty() {
            failed.insert(label);
        }
    }

    eprintln!("\n[metal] the tests");
    let mut passed = 0usize;
    for (name, decl) in runs {
        let Metal { arms, judge } = decl;
        let mut owed: Vec<&Readback> = Vec::new();
        let mut missing: Option<String> = None;
        for arm in *arms {
            match readbacks.get(arm.boot).expect("every arm was batched") {
                Ok(back) => owed.push(back),
                Err(why) => missing = Some(why.clone()),
            }
        }
        let verdict = match missing {
            Some(why) => Err(why),
            None => judge(&owed),
        };
        match verdict {
            Ok(()) => {
                eprintln!("  PASS {name}");
                passed += 1;
            }
            Err(why) => {
                eprintln!("  FAIL {name}: {why}");
                failed.extend(arms.iter().map(|arm| arm.boot));
            }
        }
    }
    let mut members = 0usize;
    for boot in shared {
        if boot.jobs.is_empty() {
            continue;
        }
        eprintln!("\n[metal] {}: {} member(s)", boot.boot, boot.jobs.len());
        let back = readbacks.get(&boot.boot).expect("every shared boot was batched");
        let mut ran = 0usize;
        for job in &boot.jobs {
            members += 1;
            let verdict = match back {
                Err(why) => Err(why.clone()),
                Ok(back) => back.job_passed(job),
            };
            match verdict {
                Ok(()) => {
                    passed += 1;
                    ran += 1;
                }
                // One line per red and none per pass: two hundred `PASS` lines
                // bury the four that matter.
                Err(why) => {
                    eprintln!("  FAIL {job}: {}", why.lines().next().unwrap_or(&why));
                    failed.insert(&boot.boot);
                }
            }
        }
        if let (Ok(back), true) = (back, ran > 0) {
            if let (Some(complete), Some(last)) = (back.boot_ms, back.last_record_ms()) {
                let each = last.saturating_sub(complete) / ran as u64;
                eprintln!("  {} ms per member over the {ran} that ran", each);
            }
        }
    }
    red |= !failed.is_empty();

    eprintln!("\n[metal] the timings");
    let mut measured: BTreeMap<String, Reading> = BTreeMap::new();
    for (label, back) in readbacks {
        let Ok(back) = back else { continue };
        let passed = !failed.contains(label.as_str());
        for (name, &value) in back.numbers.borrow().iter() {
            match measured.entry(name.clone()) {
                Entry::Vacant(slot) => {
                    slot.insert(Reading { value, passed });
                }
                // Judged on the first, and recorded off neither.
                Entry::Occupied(mut first) => {
                    eprintln!("  FAIL {name} is measured by two boots, and {label} is the second");
                    first.get_mut().passed = false;
                    red = true;
                }
            }
        }
    }
    let machine = one_machine(readbacks);
    let record =
        machine.as_ref().map_err(Clone::clone).and_then(|machine| Record::load(root, machine));
    match (machine, record) {
        (Ok(machine), Ok(record)) => {
            let judged = metaltimings::judge(&machine, record, &measured);
            if let Some(firmware) = &judged.firmware {
                eprintln!("  FAIL {firmware}");
                red = true;
            }
            for over in &judged.over {
                eprintln!("  FAIL {over}");
                red = true;
            }
            if !judged.unmeasured.is_empty() {
                eprintln!(
                    "  {} recorded number(s) this run measured nothing for: {}",
                    judged.unmeasured.len(),
                    judged.unmeasured.join(", ")
                );
            }
            eprintln!(
                "  {} number(s) on {} {}, BIOS {}; {} past its record, {} off a boot that failed",
                measured.len(),
                machine.vendor,
                machine.product,
                machine.bios,
                judged.over.len(),
                measured.values().filter(|reading| !reading.passed).count()
            );
            if judged.changed {
                match judged.record.save(root) {
                    Ok(at) => eprintln!(
                        "  {} now records {} number(s) for this machine: commit it",
                        at.display(),
                        judged.record.measured.len()
                    ),
                    Err(why) => {
                        eprintln!("  FAIL {why}");
                        red = true;
                    }
                }
            }
        }
        (Err(why), _) | (_, Err(why)) => {
            eprintln!("  FAIL {} number(s) and no record to judge them by: {why}", measured.len());
            red = true;
        }
    }
    eprintln!(
        "\n[metal] {passed} passed, {} failed, {} boot(s)",
        runs.len() + members - passed,
        readbacks.len()
    );
    red
}
