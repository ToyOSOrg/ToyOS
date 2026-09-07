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
//!
//! **What reaches the stick is not what reaches a QEMU console.** A userland
//! `println!` ends at `Backend::None` on a machine with no serial port, so
//! `===TEST_END <name> exit=N===` does not exist here: a job's verdict crosses
//! as the kernel's own `exit: <name> pid=N code=N cpu=Nms` record. Every
//! predicate below reads records, never console text.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use toyos_build::bootlog;
use toyos_build::metalimage;
use toyos_build::metalprofile::Profile;

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
    /// `create_dir` on that boot is refused with `OutOfMemory` and it panics —
    /// which is why each has a boot of its own in QEMU too. A test that must
    /// not share names its own; it costs a minute and it says so.
    ///
    /// It is also the label: the image directory, the readback directory and
    /// every `tests/metal-profile.toml` row for that boot are named after it.
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
    /// **How many kernel boots this one flash buys**, each closed by the
    /// loader's own reporting pass. One, unless a test's subject is what
    /// survives a reset: boot 1 writes and syncs, the machine goes all the way
    /// back to firmware, boot 2 reads what is still there. The readback is the
    /// last boot's, and every boot costs another `return_secs`.
    ///
    /// **N boots *without* the reporting pass between them is out of scope, and
    /// deliberately.** `bootloader/src/blackbox.rs` sets `ends_the_chain`
    /// unconditionally, and that is the mechanism that stops a machine which
    /// panics on every boot from looping on a desk with no power switch to stop
    /// it. Disabling it is a loader policy change with its own argument to
    /// make, never a field a test declaration may set — and nothing a
    /// write-sync-readback needs is on the far side of it, because a boot that
    /// went all the way back to firmware is the *stronger* readback.
    pub boots: u32,
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
    Arm { boot, config, params, jobs, features: &[], boots: 1 }
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
/// The whole list rides one flash. A member that takes the machine down takes
/// every member after it with it, and that is the honest price: on the T14 there
/// is no `MAX_SHARED_REBOOTS` to answer a dead guest with a new one.
pub struct SharedBoot {
    pub boot: &'static str,
    pub config: &'static str,
    pub params: &'static [&'static str],
    /// The kernel build, empty for the one an image ships.
    pub features: &'static [&'static str],
    /// What the runner spawns, in order — the whole binary name, `test_rs_`
    /// prefix and all, because that is what the kernel records it under.
    pub jobs: Vec<String>,
}

/// Whether a registration runs on the T14, and how.
pub enum Metal {
    /// It does not, and why — a row rather than a silence, because "no metal
    /// declaration" is the answer for the hundred tests nobody has looked at
    /// and this is the answer for one somebody has.
    QemuOnly(&'static str),
    Runs {
        arms: &'static [Arm],
        /// The readbacks in `arms` order.
        judge: fn(&[&Readback]) -> Result<(), String>,
    },
}

/// What one boot left on the stick, and what the host clock saw of it.
pub struct Readback {
    pub label: String,
    loader: String,
    kernel: String,
    /// `Boot: complete (Nms)`, or `None` on a boot that never got there.
    pub boot_ms: Option<u64>,
    /// What the machine spent getting back to `sshd`.
    pub back_secs: u64,
}

impl Readback {
    /// Every `logd` file this boot wrote, as one text.
    pub fn kernel(&self) -> Serial {
        Serial::named(&format!("{}'s kernel log", self.label), self.kernel.as_str())
    }

    /// `loader.log`, both passes: the one before the kernel handoff and, under
    /// `loaderlog::SEPARATOR`, the one that read the black box afterwards.
    pub fn loader(&self) -> Serial {
        Serial::named(&format!("{}'s loader.log", self.label), self.loader.as_str())
    }

    /// The loader pass **after** the kernel's reset, which is where a chain
    /// report is. `None` where the chain did not go round — which for a boot
    /// that ended itself is a finding, not an absence.
    pub fn after_the_reset(&self) -> Result<Serial, String> {
        let at = self.loader.find(bootlog::SEPARATOR).ok_or_else(|| {
            format!(
                "{}'s loader.log carries no pass after the reset: the loader did not point \
                 `BootNext` at itself, or the machine went back to the boot manager\n{}",
                self.label, self.loader
            )
        })?;
        Ok(Serial::named(
            &format!("{}'s loader pass after the reset", self.label),
            &self.loader[at..],
        ))
    }

    /// What the kernel recorded about the process the *runner* spawned as
    /// `binary`, or why there is no such record.
    ///
    /// **The name is the file's, truncated the way the kernel truncates it.**
    /// `ProcessEntry`'s name is `THREAD_NAME_LEN` bytes and the loader fills it
    /// from the path's last component, so a binary whose name is longer than
    /// that is recorded under a prefix — `test_rs_null_sink_client_exits` is
    /// `test_rs_null_sink_client_ex` on the wire, and a predicate looking for
    /// the whole name would find nothing on a perfectly good boot.
    ///
    /// **And the name alone does not identify one process.** A guest binary that
    /// cannot ask what a handle it does not hold does — the pattern
    /// `handle_kill_policy` is built on — re-executes *itself*, one child per
    /// fault, and every child is recorded under that same name with whatever
    /// exit the fault gave it. Measured on a metal-shaped guest,
    /// `test_rs_handle_kill_policy` left forty-two records reading `code=139`
    /// and the job's own reading zero, and the last of them is a child. The
    /// job's is the one with the **lowest pid**: the runner spawned it before it
    /// spawned anything.
    pub fn exit_code(&self, binary: &str) -> Result<i32, String> {
        let name = bootlog::recorded_name(binary);
        let head = format!("{}{name} pid=", bootlog::EXIT);
        let mut family: Vec<(u64, i32)> = Vec::new();
        for line in self.kernel.lines().filter(|l| l.contains(&head)) {
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

    /// One number this boot measured, against the ceiling
    /// `tests/metal-profile.toml` holds for it.
    ///
    /// **The gate fails closed on a name with no row**, which is the profile's
    /// own rule: a measurement nobody has priced must not pass by having no
    /// ceiling. The file is read once and kept, because every judge that asks
    /// asks inside one process and [`run`] has already read it to judge the
    /// boots.
    pub fn number(&self, name: &str, value: u64) -> Result<(), String> {
        static PROFILE: std::sync::OnceLock<Result<Profile, String>> = std::sync::OnceLock::new();
        PROFILE
            .get_or_init(|| {
                Profile::load(&super::compile::repo_root()).map_err(|why| why.to_string())
            })
            .as_ref()
            .map_err(Clone::clone)?
            .judge(name, value)
            .map_err(|why| why.to_string())
    }

    /// The job ran and the kernel recorded it exiting cleanly.
    pub fn job_passed(&self, binary: &str) -> Result<(), String> {
        match self.exit_code(binary)? {
            0 => Ok(()),
            code => Err(format!(
                "{binary} exited {code} on the T14; its output reaches no channel on this \
                 machine, so the code is the whole verdict"
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

/// What a `--metal` invocation was asked to do with the machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Build every image, hand each to `toyos-metal`, judge what came back.
    Drive,
    /// Do not touch the machine. Judge the readbacks in the directory if it
    /// holds one for every boot; otherwise build the images into it and write
    /// down what to run. **The completeness of the directory decides**, so a
    /// half-answered run stages the rest instead of reporting on the half.
    Offline,
}

/// One image, and every test that rides it.
struct Batch {
    config: &'static str,
    params: Vec<&'static str>,
    features: &'static [&'static str],
    jobs: Vec<String>,
    boots: u32,
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
fn at(dir: &Path, label: &str) -> PathBuf {
    dir.join(label)
}

/// The boots `tests` need, keyed by the name their arms give them.
///
/// **Two arms naming one boot are refused where they disagree about it**: an
/// image is one config armed one way, and a silent winner would give one of the
/// two tests a machine it did not ask for.
fn batches<'a>(
    tests: &[(&'a str, &'static Metal)],
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
            boot.boot.to_string(),
            Batch {
                config: boot.config,
                params: boot.params.to_vec(),
                features: boot.features,
                jobs: boot.jobs.clone(),
                boots: 1,
            },
        );
        if was.is_some() {
            return Err(format!("two shared boots are both named {:?}", boot.boot));
        }
    }
    for (name, decl) in tests {
        let Metal::Runs { arms, .. } = decl else { continue };
        for arm in *arms {
            let batch = out.entry(arm.boot.to_string()).or_insert_with(|| Batch {
                config: arm.config,
                params: arm.params.to_vec(),
                features: arm.features,
                jobs: Vec::new(),
                boots: arm.boots,
            });
            if batch.config != arm.config
                || batch.params != arm.params
                || batch.features != arm.features
            {
                return Err(format!(
                    "{name} rides the boot {:?} as ({}, {:?}, {:?}) and another row rides it \
                     as ({}, {:?}, {:?}); one boot is one image",
                    arm.boot,
                    arm.config,
                    arm.params,
                    arm.features,
                    batch.config,
                    batch.params,
                    batch.features
                ));
            }
            // The most any rider asks for: a boot that answers a two-boot
            // question also answers every one-boot question on it.
            batch.boots = batch.boots.max(arm.boots);
            batch.add(arm.jobs.iter().map(|j| (*j).to_string()));
        }
    }
    Ok(out)
}

/// Build one batch's image, and answer where it landed.
fn build(
    root: &Path,
    dir: &Path,
    label: &str,
    batch: &Batch,
    rust_bins: &[(String, Vec<u8>)],
    helpers: &[&str],
    quiet: bool,
) -> Result<PathBuf, String> {
    let home = at(dir, label);
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;

    let committed = root.join(batch.config).join("system.toml");
    let text = std::fs::read_to_string(&committed)
        .map_err(|e| format!("{}: {e}", committed.display()))?;
    let jobs: Vec<&str> = batch.jobs.iter().map(String::as_str).collect();
    let derived = metalimage::derive(&text, &jobs)
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
    // **What a job spawns comes with it, and it is not optional.** A binary
    // that cannot find its `.so` or its helper does not fail an assertion — it
    // fails to *spawn*. Measured on a metal-shaped guest: `test_rs_std_tls` did
    // not run at all and took the rest of the job list with it, and
    // `disk_backtrace`, `fault_gates` and `panic_recovery` each panicked on
    // `entity not found` looking for a child that was never staged. Which job
    // needs which is the job's business and not this function's, so both sets
    // go on whole the moment any job does.
    if extra.iter().any(|(name, _)| name.starts_with("bin/test_rs_")) {
        for (name, data) in rust_bins {
            if name.ends_with(".so") {
                extra.push((format!("lib/{name}"), data.clone()));
            } else if helpers.contains(&name.as_str()) {
                extra.push((format!("bin/test_rs_{name}"), data.clone()));
            }
        }
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
    let plan = toyos_build::build::Plan::new(&config, features, &batch.params);
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

/// The invocation that turns one image into one readback. Written down in the
/// staged request and run by [`Mode::Drive`], so the two cannot differ.
fn invocation(image: &Path, home: &Path, boots: u32) -> Vec<String> {
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
    if boots > 1 {
        words.push("--boots".to_string());
        words.push(boots.to_string());
    }
    words
}

fn read_readback(dir: &Path, label: &str) -> Result<Readback, String> {
    let home = at(dir, label);
    let read = |name: &str| -> Result<String, String> {
        let at = home.join(name);
        std::fs::read_to_string(&at).map_err(|e| {
            format!("{}: {e} — no readback for {label}; run the driver on its image first", at.display())
        })
    };
    let loader = read(toyos_build::metal::READBACK_LOADER)?;
    let kernel = read(toyos_build::metal::READBACK_KERNEL)?;
    let boot = read(toyos_build::metal::READBACK_BOOT)?;
    let back_secs = toyos_build::metal::back_secs(&boot)
        .ok_or_else(|| format!("{label}'s boot file names no `back_secs`: {boot:?}"))?;
    Ok(Readback {
        label: label.to_string(),
        boot_ms: bootlog::boot_millis(&kernel),
        loader,
        kernel,
        back_secs,
    })
}

/// What a metal run established.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Every selected test passed on the machine.
    Green,
    /// One did not. **A red on the T14 is a red.**
    Red,
    /// The images were built and nothing was judged, because nothing has been
    /// run on the machine yet. Neither of the other two: a run that reached no
    /// hardware may not report on any.
    Staged,
}

/// The whole metal profile: batch, build, drive, judge, report.
pub fn run(
    mode: Mode,
    dir: &Path,
    tests: &[(&str, &'static Metal)],
    shared: &[SharedBoot],
    rust_bins: &[(String, Vec<u8>)],
    // Binaries a job spawns that are not jobs themselves: the shared block's
    // helper children, which discovery leaves out and which nothing else would
    // then put on the image.
    helpers: &[&str],
    quiet: bool,
) -> Verdict {
    let root = super::compile::repo_root();
    let profile = match Profile::load(&root) {
        Ok(profile) => profile,
        Err(why) => {
            eprintln!("[metal] {why}");
            return Verdict::Red;
        }
    };
    let batches = match batches(tests, shared) {
        Ok(batches) => batches,
        Err(why) => {
            eprintln!("[metal] {why}");
            return Verdict::Red;
        }
    };
    let declared: Vec<&str> = tests
        .iter()
        .filter_map(|(name, decl)| match decl {
            Metal::QemuOnly(why) => Some((*name, *why)),
            Metal::Runs { .. } => None,
        })
        .map(|(name, why)| {
            eprintln!("[metal] QEMU-only: {name} — {why}");
            name
        })
        .collect();
    let runs: Vec<&(&str, &'static Metal)> =
        tests.iter().filter(|(_, d)| matches!(d, Metal::Runs { .. })).collect();
    eprintln!(
        "[metal] {} registration(s) and {} shared member(s) over {} boot(s); {} declared \
         QEMU-only",
        runs.len(),
        shared.iter().map(|b| b.jobs.len()).sum::<usize>(),
        batches.len(),
        declared.len(),
    );
    if runs.is_empty() && shared.iter().all(|b| b.jobs.is_empty()) {
        eprintln!("[metal] nothing to run");
        return Verdict::Red;
    }

    // A directory holding a readback for every boot is a run the machine has
    // already answered, and the only thing left is the judging.
    let answered = batches.keys().all(|label| {
        at(dir, label).join(toyos_build::metal::READBACK_KERNEL).is_file()
    });
    let judging = mode == Mode::Offline && answered;

    let mut images: BTreeMap<&str, PathBuf> = BTreeMap::new();
    if !judging {
        for (label, batch) in &batches {
            match build(&root, dir, label, batch, rust_bins, helpers, quiet) {
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

    if mode == Mode::Offline && !judging {
        let mut request = String::from(
            "# One boot per image. Each invocation is `cargo <words>` from this worktree.\n",
        );
        for (label, image) in &images {
            request.push_str(&format!(
                "\n{label}\n  image: {}\n  cargo {}\n",
                image.display(),
                invocation(image, &at(dir, label), batches[*label].boots).join(" ")
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

    if mode == Mode::Drive {
        for (label, image) in &images {
            let words = invocation(image, &at(dir, label), batches[*label].boots);
            eprintln!("[metal] {label}: cargo {}", words.join(" "));
            match Command::new("cargo").args(&words).current_dir(&root).status() {
                Ok(status) if status.success() => {}
                Ok(status) => {
                    // The driver's own exit codes: 1 is the machine, 2 is the
                    // loop. Either way the readback below says what came back,
                    // and a boot with no readback reds every test on it.
                    eprintln!("[metal] {label}: toyos-metal exited {status}");
                }
                Err(e) => {
                    eprintln!("[metal] {label}: toyos-metal could not be started: {e}");
                    return Verdict::Red;
                }
            }
        }
    }

    // The readbacks, and the boot facts each one carries.
    let mut readbacks: BTreeMap<String, Result<Readback, String>> = BTreeMap::new();
    for label in batches.keys() {
        readbacks.insert(label.clone(), read_readback(dir, label));
    }

    let mut red = false;
    eprintln!("\n[metal] the boots");
    for (label, back) in &readbacks {
        match back {
            Err(why) => {
                eprintln!("  FAIL {label}: {why}");
                red = true;
            }
            Ok(back) => {
                let ms = back.boot_ms.map_or_else(|| "-".to_string(), |ms| ms.to_string());
                eprintln!("  {label}: Boot: complete {ms} ms, back in {} s", back.back_secs);
                for (field, value) in [
                    ("complete_ms", back.boot_ms),
                    ("back_secs", Some(back.back_secs)),
                ] {
                    let Some(value) = value else {
                        eprintln!("    FAIL boot.{label}.complete_ms: this boot recorded none");
                        red = true;
                        continue;
                    };
                    if let Err(why) = profile.judge(&format!("boot.{label}.{field}"), value) {
                        eprintln!("    FAIL {why}");
                        red = true;
                    }
                }
            }
        }
    }

    eprintln!("\n[metal] the tests");
    let mut passed = 0usize;
    for (name, decl) in &runs {
        let Metal::Runs { arms, judge } = decl else { continue };
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
                red = true;
            }
        }
    }
    let mut members = 0usize;
    for boot in shared {
        if boot.jobs.is_empty() {
            continue;
        }
        eprintln!("\n[metal] {}: {} member(s)", boot.boot, boot.jobs.len());
        let back = readbacks.get(boot.boot).expect("every shared boot was batched");
        for job in &boot.jobs {
            members += 1;
            let verdict = match back {
                Err(why) => Err(why.clone()),
                Ok(back) => back.job_passed(job),
            };
            match verdict {
                Ok(()) => passed += 1,
                // One line per red and none per pass: two hundred `PASS` lines
                // bury the four that matter.
                Err(why) => {
                    eprintln!("  FAIL {job}: {}", why.lines().next().unwrap_or(&why));
                    red = true;
                }
            }
        }
    }
    eprintln!(
        "\n[metal] {passed} passed, {} failed, {} boot(s)",
        runs.len() + members - passed,
        batches.len()
    );
    if red {
        Verdict::Red
    } else {
        Verdict::Green
    }
}

