//! The machine updates itself, rehearsed in QEMU: an image goes over ssh into
//! `update`'s standard input, is written to the slot the machine is not
//! running, and boots after a reboot; a slot the loader must refuse, and one
//! whose kernel dies, each leave the machine on the other slot.
//!
//! **The oracles are the loader's and the kernel's own lines**, read off the
//! 16550 the loader speaks on and the console the kernel does: which slot the table marked, each refusal by its name, the slot
//! the kernel says it came from, and the kernel's length the loader loaded —
//! against the sections this host built and signed, which it holds. A dead
//! boot's report is read back out of `/log` too, which is where the owner
//! finds it on a machine with no console.
//!
//! One machine per test, and it keeps its firmware variables in a copy of
//! its own (`BootOptions::firmware_vars`), because the anti-rollback floor a
//! proven boot raises is what the next boot of the same machine is held to.
//! What the running system can write and the loader must not trust — the
//! slots' record, the slot table — the host writes into the image between or
//! beneath boots, and the variable store it reads and plants in by EDK2's own
//! layout ([`vars`]).

use std::path::{Path, PathBuf};
use std::time::Instant;

use toyos_build::bootlog;
use toyos_build::build::{self, Plan};
use toyos_build::image::{self, SecondSlot, Signing};
use toyos_build::signing::{self, Key};
use toyos_update::floor::{self as floors, Scope};
use toyos_update::record::{Booted, Record};
use toyos_update::slots::Which;

use super::qemu::{self, BootOptions, QemuInstance, Staged, DEFAULT_READY};
use super::ssh::{self, Identity, HOST};

/// The boot config every image here is built from.
const CONFIG: &str = "tests/updatecase";

/// The versions the three images carry: the machine's own, the update, and an
/// image older than the floor the first proves.
const BASE: u64 = 100;
const NEXT: u64 = 200;
const OLDER: u64 = 50;

/// The kernel record naming the slot a boot came from (`kernel/src/params.rs`).
const SLOT_RECORD: &str = "boot: slot";

/// What the loader says of a slot whose every byte its signature vouched for.
const VERIFIED: &str = "kernel, cmdline and ROOT are the bytes the signed header names";

/// A version no image here carries, which a forged record names.
const FORGED: u64 = 1 << 63;

/// The loader's refusal of a stored floor it did not write
/// (`bootloader/src/floor.rs`), which boots nothing.
const FLOOR_REFUSED: &str = "it is refused rather than read as no floor";

/// The loader's slots' record, on the log partition beside `loader.log`.
const RECORD_FILE: &str = "attempts";

/// One machine: its disk, its firmware variables, the key the host logs in
/// with, and where the host reaches its sshd.
struct Rig {
    scratch: PathBuf,
    image: PathBuf,
    vars: PathBuf,
    identity: Identity,
    port: u16,
    /// The base image's parts, for what the loader is held to.
    base_kernel: usize,
    /// A second stick behind the machine's in `BootOrder`, where the test
    /// stages one ([`Rig::with_recovery`]).
    recovery: Option<PathBuf>,
}

/// The plan for this config's image: `features` is the kernel build, `params`
/// the actuators its slot arms, `version` its signed header's.
fn plan(features: &[&str], params: &[&str], version: u64, second: Option<SecondSlot>) -> Plan {
    let mut plan = Plan::new(toyos_build::arch::Arch::X86_64, &super::compile::repo_root().join(CONFIG).join("system.toml"), features, params);
    plan.version = version;
    plan.second = second;
    plan
}

/// What every image here carries on ROOT beside the config's own: the key the
/// host logs in with.
fn staged(identity: &Identity) -> Vec<(String, Vec<u8>)> {
    vec![(toyos_build::build::AUTHORIZED_ON_ROOT.to_string(), identity.authorized_line().into_bytes())]
}

impl Rig {
    /// The machine's disk: slot A holding this config's image at [`BASE`] on
    /// the shipping kernel, marked, and an empty slot B with room for the same
    /// ROOT again.
    fn stage(name: &str) -> Result<Self, String> {
        let scratch = super::lane::dir().join(name);
        std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
        let identity = Identity::mint(&format!("{name}-key"))?;
        let root = super::compile::repo_root();
        let files = staged(&identity);
        let base = plan(&[], &[], BASE, None);
        let parts = build::build_test_parts(&root, &base, true, &files);
        let room = SecondSlot { root_bytes: 2 * parts.root.len() as u64 };
        let disk = image::create_boot_image(
            toyos_build::arch::Arch::X86_64,
            &parts.kernel,
            &parts.bootloader,
            &parts.root,
            "",
            Signing { key: signing::key(), version: BASE },
            Some(room),
        );
        let image = scratch.join("machine.img");
        std::fs::write(&image, disk).map_err(|e| format!("write {}: {e}", image.display()))?;
        let vars = scratch.join("OVMF_VARS.fd");
        std::fs::copy(root.join("ovmf/OVMF_VARS-pure-efi.fd"), &vars)
            .map_err(|e| format!("copy the firmware's variable store: {e}"))?;
        Ok(Self { scratch, image, vars, identity, port: qemu::free_host_port(), base_kernel: parts.kernel.len(), recovery: None })
    }

    /// This machine with a second stick behind its own in `BootOrder`: another
    /// image of the same config, one slot, its own GUIDs — the recovery stick
    /// a machine falls to, and the ESP a request names.
    fn with_recovery(mut self) -> Result<Self, String> {
        let root = super::compile::repo_root();
        let parts = build::build_test_parts(&root, &plan(&[], &[], BASE, None), true, &staged(&self.identity));
        let disk = image::create_boot_image(
            toyos_build::arch::Arch::X86_64,
            &parts.kernel,
            &parts.bootloader,
            &parts.root,
            "",
            Signing { key: signing::key(), version: BASE },
            None,
        );
        let path = self.scratch.join("recovery.img");
        std::fs::write(&path, disk).map_err(|e| format!("write {}: {e}", path.display()))?;
        self.recovery = Some(path);
        Ok(self)
    }

    /// The recovery stick's image, which [`Rig::with_recovery`] staged.
    fn recovery(&self) -> Result<&Path, String> {
        self.recovery.as_deref().ok_or_else(|| "this rig stages no recovery stick".to_string())
    }

    /// The unique GUID of the one partition of type `kind` on the image at `path`.
    fn guid_on(path: &Path, kind: toyos_gpt::Guid) -> Result<[u8; 16], String> {
        let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        image::unique_guid_of(&mut file, kind)
    }

    /// The line the loader says of the log partition the image at `path` names,
    /// which tells one stick's passes from the other's on the one 16550.
    fn loader_of(path: &Path) -> Result<String, String> {
        Ok(format!("Log partition: signature {:02x?}", Self::guid_on(path, toyos_gpt::Guid::MICROSOFT_BASIC)?))
    }

    /// The kernel's line naming the log partition of the image at `path`, which
    /// tells one stick's kernels from the other's on the one console.
    fn kernel_of(path: &Path) -> Result<String, String> {
        Ok(format!("boot: log partition guid {:02x?}", Self::guid_on(path, toyos_gpt::Guid::MICROSOFT_BASIC)?))
    }

    /// Run `command` over ssh and hold it to status 0 and `owed` on its output.
    fn asks(&self, command: &str, owed: &str) -> Result<String, String> {
        let exec = ssh::ssh_exec(HOST, self.port, &self.identity, command)?;
        let said = format!("{}{}", exec.stdout_text(), exec.stderr_text());
        eprintln!("  [update] `{command}` ended {:?}: {}", exec.status, said.trim());
        if exec.status != Some(0) || !said.contains(owed) {
            return Err(format!("`{command}` ended {:?} saying {said:?}, where {owed:?} is owed", exec.status));
        }
        Ok(said)
    }

    /// An update for this machine, written to a file: `features` and `params`
    /// as [`plan`] takes them, signed by `key` at `version`.
    fn update(&self, name: &str, features: &[&str], params: &[&str], version: u64, key: &Key) -> Result<(PathBuf, usize), String> {
        let root = super::compile::repo_root();
        let parts = build::build_test_parts(&root, &plan(features, params, version, None), true, &staged(&self.identity));
        let bytes = image::update_image(&parts.kernel, &parts.root, &params.join(","), Signing { key, version });
        let path = self.scratch.join(format!("{name}.update"));
        std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok((path, parts.kernel.len()))
    }

    /// Boot the machine once, taking every reset it makes, to its ready
    /// marker: the guest and its console so far.
    fn boot(&self) -> Result<(QemuInstance, String), String> {
        let options = BootOptions {
            profile: qemu::Profile::Headless,
            boot_image: Some(Staged::Written(self.image.clone())),
            ssh_port: Some(self.port),
            takes_the_reset: true,
            firmware_vars: Some(self.vars.clone()),
            recovery_stick: self.recovery.clone(),
            qmp: true,
            ..Default::default()
        };
        let config = super::compile::repo_root().join(CONFIG);
        let mut guest = QemuInstance::boot_with_options(&config, &[], &[], options);
        let mut console = guest.boot_log().to_string();
        qemu::await_marker(&mut guest, &mut console, "sshd: listening on port 22", "sshd to open its port")?;
        Ok((guest, console))
    }

    /// Boot the machine on the T14's shape, whose 16550 is its console, to
    /// the loader's line `marker`: for a boot that never reaches a kernel, or
    /// is cut at the loader's handoff.
    fn launch(&self, marker: &'static str) -> QemuInstance {
        let options = BootOptions {
            profile: qemu::Profile::Metal,
            boot_image: Some(Staged::Written(self.image.clone())),
            takes_the_reset: true,
            firmware_vars: Some(self.vars.clone()),
            recovery_stick: self.recovery.clone(),
            ready_marker: marker,
            ..Default::default()
        };
        QemuInstance::boot_with_options(&super::compile::repo_root().join(CONFIG), &[], &[], options)
    }

    /// The log partition's unique GUID, which the slots' record carries.
    fn log_guid(&self) -> Result<[u8; 16], String> {
        let mut file = std::fs::File::open(&self.image).map_err(|e| format!("{}: {e}", self.image.display()))?;
        image::unique_guid_of(&mut file, toyos_gpt::Guid::MICROSOFT_BASIC)
    }

    /// The slots' record the loader last wrote.
    fn record(&self) -> Result<Record, String> {
        let guid = self.log_guid()?;
        let mut file = std::fs::File::open(&self.image).map_err(|e| format!("{}: {e}", self.image.display()))?;
        let bytes = image::read_file_on(&mut file, guid, RECORD_FILE)?;
        Record::decode(&bytes, &guid).map_err(|why| format!("the slots' record {why}"))
    }

    /// `reboot` over ssh, with `edit` made to the slots' record while the
    /// machine is held at the reset its kernel makes — **the record as the
    /// running system could have left it**, written after that kernel's last
    /// write (its cache writes back whole blocks, the record's among them) and
    /// before the loader's first read — then the console until `marker`: where
    /// on the console and on the 16550 the boots after the reboot begin.
    fn reboot_forging(
        &self,
        guest: &mut QemuInstance,
        console: &mut String,
        edit: impl FnOnce(&mut Record) -> Result<(), String>,
        marker: &str,
    ) -> Result<(usize, usize), String> {
        let (from, uart) = (console.len(), guest.uart_log().len());
        let mut hold = qemu::QmpHold::arm(guest.qmp_socket());
        let asked = ssh::ssh_fire(HOST, self.port, &self.identity, "reboot")?;
        eprintln!("  [update] `reboot` answered {asked:?}");
        hold.held(qemu::GUEST_WEDGED)?;
        let guid = self.log_guid()?;
        let mut record = self.record()?;
        edit(&mut record)?;
        image::overwrite_file_on(&self.image, guid, RECORD_FILE, &record.encode(&guid))?;
        if self.record()? != record {
            return Err("the forged record did not read back".into());
        }
        hold.release();
        await_machine(guest, console, &format!("{marker:?} after the forged reboot"), |c| c[from.min(c.len())..].contains(marker))?;
        Ok((from, uart))
    }

    /// The slots' record as a clean hand-back leaves it, so the next pass is no retry.
    fn powered_off_cleanly(&self) -> Result<(), String> {
        let guid = self.log_guid()?;
        let record = Record { count: 0, booted: None, ..self.record()? };
        image::overwrite_file_on(&self.image, guid, RECORD_FILE, &record.encode(&guid))
    }

    /// Slot `which`'s signed header, off its volume.
    fn signed_header(&self, which: Which) -> Result<Vec<u8>, String> {
        let mut file = std::fs::File::open(&self.image).map_err(|e| format!("{}: {e}", self.image.display()))?;
        let slot = image::slot_table_of(&mut file)?.slot(which).ok_or_else(|| format!("no slot {}", which.letter()))?;
        image::read_file_on(&mut file, slot.boot, toyos_update::slots::SIGNED_FILE)
    }

    /// Flip a byte of slot `which`'s kernel, past its signed header: the
    /// signature still verifies and the hash does not.
    fn bend_kernel(&self, which: Which) -> Result<(), String> {
        let mut file = std::fs::File::open(&self.image).map_err(|e| format!("{}: {e}", self.image.display()))?;
        let slot = image::slot_table_of(&mut file)?.slot(which).ok_or_else(|| format!("no slot {}", which.letter()))?;
        let mut kernel = image::read_file_on(&mut file, slot.boot, toyos_update::slots::KERNEL_FILE)?;
        drop(file);
        kernel[100] ^= 0x01;
        image::overwrite_file_on(&self.image, slot.boot, toyos_update::slots::KERNEL_FILE, &kernel)
    }

    /// The name of the floor this machine's loader keeps.
    fn floor_name(&self) -> Result<String, String> {
        let key = signing::key();
        Ok(floors::name(key.floor_scope(), &key.public(), &self.log_guid()?).as_str().to_string())
    }

    /// `update < file` over ssh: its status and what it said.
    fn install(&self, file: &Path) -> Result<(Option<u32>, String), String> {
        let exec = ssh::ssh_pipe(HOST, self.port, &self.identity, "update", file)?;
        let said = format!("{}{}", exec.stdout_text(), exec.stderr_text());
        eprintln!("  [update] `update < {}` ended {:?}: {}", file.display(), exec.status, said.trim());
        Ok((exec.status, said))
    }

    /// `reboot` over ssh, and the console until `marker`: where on the console
    /// and on the loader's 16550 the boots after the reboot begin.
    fn reboot_until(&self, guest: &mut QemuInstance, console: &mut String, marker: &str) -> Result<(usize, usize), String> {
        let (from, uart) = (console.len(), guest.uart_log().len());
        let asked = ssh::ssh_fire(HOST, self.port, &self.identity, "reboot")?;
        eprintln!("  [update] `reboot` answered {asked:?}");
        await_machine(guest, console, &format!("{marker:?} after the reboot"), |c| c[from.min(c.len())..].contains(marker))
            .map_err(|why| {
                let all = guest.uart_log();
                format!("{why}\nthe 16550 since the reboot:\n{}", &all[uart.min(all.len())..])
            })?;
        Ok((from, uart))
    }
}

/// Wait until `done` holds of the console, while the machine is talking on
/// either of its channels.
///
/// **Not the harness's own wait**: that one hears the console alone, and
/// between a kernel's reset and the next kernel's first line the machine
/// talks only on the 16550 — the loader's passes, one of which hashes ROOT —
/// so a machine working through two of them reads as one gone quiet. Its
/// bounds are the harness's, [`qemu::GUEST_QUIET`] of silence on both and
/// [`qemu::GUEST_WEDGED`] in all.
fn await_machine(guest: &mut QemuInstance, console: &mut String, doing: &str, done: impl Fn(&str) -> bool) -> Result<(), String> {
    let began = Instant::now();
    let (mut heard, mut grew) = (0usize, Instant::now());
    loop {
        if done(console) {
            return Ok(());
        }
        let more = guest.drain_serial(std::time::Duration::from_millis(200));
        console.push_str(&more);
        let now = console.len() + guest.uart_log().len();
        if now != heard {
            (heard, grew) = (now, Instant::now());
        }
        if grew.elapsed() >= qemu::GUEST_QUIET {
            return Err(format!(
                "{} waiting for {doing}: the console and the 16550 both went quiet for {} s",
                qemu::STALLED,
                qemu::GUEST_QUIET.as_secs()
            ));
        }
        if began.elapsed() >= qemu::GUEST_WEDGED {
            return Err(format!("{} waiting for {doing}: it never stopped talking and never got there", qemu::STALLED));
        }
    }
}

/// `what` is in `console` from `from` on, or the finding that it is not.
fn owed(console: &str, from: usize, what: &str) -> Result<(), String> {
    if console[from.min(console.len())..].contains(what) {
        return Ok(());
    }
    Err(format!("{what:?} is not on the console after byte {from}:\n{}", &console[from.min(console.len())..]))
}

/// `what` is among the loader's lines from byte `from` of its 16550 on.
fn loader_said(guest: &QemuInstance, from: usize, what: &str) -> Result<(), String> {
    let uart = guest.uart_log();
    let since = &uart[from.min(uart.len())..];
    if since.contains(what) {
        return Ok(());
    }
    let loader: Vec<&str> = since.lines().filter(|l| !l.starts_with("[kernel ")).collect();
    Err(format!("the loader never said {what:?}; it said:\n{}", loader.join("\n")))
}

/// **The exit**: a kernel change reaches the running machine as `ssh … update
/// < image`, is written to the idle slot, and is the kernel the next boot
/// runs; the boot that proved the old image raised the floor, and the one
/// that proves the new image raises it past the old one.
pub fn update_boots_the_new_kernel(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-boots")?;
    // The actuator kernel, and no actuator armed: a different kernel binary
    // that boots the same machine.
    let (next, next_kernel) = rig.update("next", build::TEST_KERNEL, &[], NEXT, signing::key())?;
    if next_kernel == rig.base_kernel {
        return Err(format!("the update's kernel is {next_kernel} bytes, the base's too: no change to carry"));
    }
    let (mut guest, mut console) = rig.boot()?;
    owed(&console, 0, &format!("{SLOT_RECORD} A, the one the slot table marks"))?;

    let asked = Instant::now();
    let (status, said) = rig.install(&next)?;
    let installed = asked.elapsed();
    if status != Some(0) || !said.contains(&format!("update: installed version {NEXT} in slot B")) {
        return Err(format!("`update` ended {status:?} saying {said:?}"));
    }
    let (from, uart) = rig.reboot_until(&mut guest, &mut console, &format!("{SLOT_RECORD} B, the one the slot table marks"))?;
    await_machine(&mut guest, &mut console, "the new slot's ready marker", |c| c[from..].contains(DEFAULT_READY))?;
    let booted = asked.elapsed();
    loader_said(&guest, uart, &format!("Anti-rollback floor: {BASE}, raised from 0 by the boot that proved it"))?;
    loader_said(&guest, uart, &format!("Slot B: {VERIFIED}"))?;
    loader_said(&guest, uart, &format!("Kernel: {next_kernel} bytes"))?;
    for line in guest.uart_log()[uart..].lines().filter(|l| l.contains("TSC cycles") || l.contains("Loader TSC")) {
        eprintln!("  [update] loader: {line}");
    }
    eprintln!(
        "  [update] {} bytes installed in {} ms; slot B's kernel ({next_kernel} bytes, the base's {}) \
         at its ready marker {} ms after `update` was asked",
        std::fs::metadata(&next).map(|m| m.len()).unwrap_or(0),
        installed.as_millis(),
        rig.base_kernel,
        booted.as_millis()
    );

    // **A record the running system forged proves nothing**: slot B's own
    // entry, a digest that is not its header's and a version no image
    // carries. The pass that reads the clean reboot verifies B's header,
    // finds another digest, and leaves the floor at the base's version.
    let forge = |record: &mut Record| {
        let booted = record.booted.filter(|b| b.slot == Which::B).ok_or("the loader wrote down no boot of slot B")?;
        let mut digest = booted.digest;
        digest[0] ^= 1;
        record.booted = Some(Booted { version: FORGED, digest, ..booted });
        Ok(())
    };
    let (from, uart) =
        rig.reboot_forging(&mut guest, &mut console, forge, &format!("{SLOT_RECORD} B, the one the slot table marks"))?;
    await_machine(&mut guest, &mut console, "slot B's ready marker again", |c| c[from..].contains(DEFAULT_READY))?;
    loader_said(&guest, uart, "Anti-rollback floor: not raised, because the proven image is not verified: slot B's signed header is")?;
    loader_said(&guest, uart, &format!("{} (image scope) holds {BASE}", rig.floor_name()?))?;
    let since = guest.uart_log()[uart..].to_string();
    for raised in [format!("Anti-rollback floor: {NEXT}"), format!("Anti-rollback floor: {FORGED}")] {
        if since.contains(&raised) {
            return Err(format!("a forged record raised the floor: the loader said {raised:?}"));
        }
    }
    eprintln!("  [update] a record naming slot B under another digest and version {FORGED} raised nothing");

    // **What anti-rollback is for**: the plain reboot proves slot B's own
    // image, the floor rises to the update's version, and the slot the machine
    // updated from is below it.
    let (from, uart) = rig.reboot_until(&mut guest, &mut console, &format!("{SLOT_RECORD} B, the one the slot table marks"))?;
    await_machine(&mut guest, &mut console, "slot B's ready marker a third time", |c| c[from..].contains(DEFAULT_READY))?;
    loader_said(&guest, uart, &format!("Anti-rollback floor: {NEXT}, raised from {BASE} by the boot that proved it"))?;
    drop(guest);
    image::restage_table(&rig.image, |t| t.marked = Which::A)?;
    let (guest, console) = rig.boot()?;
    loader_said(&guest, 0, &format!("Slot A: REFUSED, its version {BASE} is below {NEXT}, the highest a boot has proven"))?;
    owed(&console, 0, &format!("{SLOT_RECORD} B, because the marked slot A was refused: version"))?;
    eprintln!("  [update] the boot of slot B raised the floor to {NEXT}, and slot A at {BASE} is refused under it");
    drop(guest);
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **Every slot the loader must refuse is refused by name, and the other boots**
/// — a flipped byte, no signed header, another key's signature, a version
/// under the floor — and `update` itself refuses what it can see: another
/// key and an image older than what runs, before it writes anything, and a
/// kernel or ROOT that is not the bytes the header names, or bytes past the
/// last section, before it moves the mark. And the floor the reboot raises is
/// the version the loader verified, whatever the record on the disk says.
pub fn update_refusals_boot_the_other_slot(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-refusals")?;
    let stranger = Key::mint();
    let (next, _) = rig.update("next", &[], &[], NEXT, signing::key())?;
    let (foreign, _) = rig.update("foreign", &[], &[], NEXT, &stranger)?;
    let (older, _) = rig.update("older", &[], &[], OLDER, signing::key())?;
    let bytes = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let whole = bytes(&next)?;
    let header = toyos_update::image::Header::parse(&whole).map_err(|why| why.to_string())?;
    let root_at = toyos_update::image::SIGNED_BYTES + (header.kernel().len + header.cmdline().len) as usize;
    let bent = |name: &str, bend: &dyn Fn(&mut Vec<u8>)| -> Result<PathBuf, String> {
        let mut bytes = whole.clone();
        bend(&mut bytes);
        let path = rig.scratch.join(format!("{name}.update"));
        std::fs::write(&path, bytes).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(path)
    };
    let kernel_flipped = bent("kernel-flipped", &|b| b[toyos_update::image::SIGNED_BYTES + 100] ^= 0x01)?;
    let root_flipped = bent("root-flipped", &|b| b[root_at + 100] ^= 0x01)?;
    let appended = bent("appended", &|b| b.push(0))?;

    // The machine's first boot: `update` refuses each, and a reboot proves the
    // base image, which raises the floor to its version.
    let (mut guest, mut console) = rig.boot()?;
    let older_word = format!("its version {OLDER} is older than {BASE}");
    for (file, word) in [
        (&foreign, "the signature is not this machine's key's"),
        (&older, older_word.as_str()),
        (&kernel_flipped, "the kernel is not the bytes its signed header names"),
        (&root_flipped, "ROOT is not the bytes its signed header names"),
        (&appended, "the input carries more bytes than its signed header names"),
    ] {
        let (status, said) = rig.install(file)?;
        if status != Some(1) || !said.contains(word) {
            return Err(format!("`update < {}` ended {status:?} saying {said:?}, where {word:?} is owed", file.display()));
        }
    }
    // **What the running system writes, the loader does not believe**: the
    // record names slot A and its own digest, and a version no image carries.
    let forge = |record: &mut Record| {
        let booted = record.booted.filter(|b| b.slot == Which::A).ok_or("the loader wrote down no boot of slot A")?;
        record.booted = Some(Booted { version: FORGED, ..booted });
        Ok(())
    };
    let (from, uart) = rig.reboot_forging(&mut guest, &mut console, forge, DEFAULT_READY)?;
    loader_said(&guest, uart, &format!("Anti-rollback floor: {BASE}, raised from 0 by the boot that proved it"))?;
    if guest.uart_log()[uart..].contains(&FORGED.to_string()) {
        return Err(format!("the loader said the forged version {FORGED}"));
    }
    owed(&console, from, &format!("{SLOT_RECORD} A, the one the slot table marks"))?;
    drop(guest);

    let mut flipped = bytes(&next)?;
    // A byte of the kernel, past the signed header: the signature still
    // verifies and the hash does not.
    flipped[toyos_update::image::SIGNED_BYTES + 100] ^= 0x01;
    let cases: [(&str, Vec<u8>, bool, &str); 4] = [
        ("a flipped byte", flipped, true, "hash"),
        ("no signed header", bytes(&next)?, false, "unsigned"),
        ("another key's signature", bytes(&foreign)?, true, "signature"),
        ("a version under the floor", bytes(&older)?, true, "version"),
    ];
    for (what, update, signed, word) in cases {
        image::stage_slot(&rig.image, Which::B, &update, signed)?;
        let (guest, console) = rig.boot()?;
        loader_said(&guest, 0, "Slot B: REFUSED")?;
        owed(&console, 0, &format!("{SLOT_RECORD} A, because the marked slot B was refused: {word}"))?;
        loader_said(&guest, 0, &format!("Slot A: {VERIFIED}"))?;
        eprintln!("  [update] slot B with {what}: refused as {word:?}, and slot A booted");
        drop(guest);
    }
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **A boot that dies falls back on its own**: an update whose kernel panics
/// is installed and marked, the reboot boots it, it panics, the loader reads
/// the panic and marks that image dead, and the pass after boots slot A —
/// whose kernel says why, and whose `/log` carries the saying.
pub fn update_falls_back_from_a_dying_kernel(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-dies")?;
    // A panic once the boot is complete, and the panicked kernel's own reset
    // bound shortened so it hands the machine back inside the test: the
    // reset is what brings the loader round to read the panic.
    let (dying, _) = rig.update("dying", build::TEST_KERNEL, &["test-late-panic", "panic-reboot-fast"], NEXT, signing::key())?;
    let (mut guest, mut console) = rig.boot()?;
    let (status, said) = rig.install(&dying)?;
    if status != Some(0) || !said.contains(&format!("update: installed version {NEXT} in slot B")) {
        return Err(format!("`update` ended {status:?} saying {said:?}"));
    }
    let fell_back = format!("{SLOT_RECORD} A, because the marked slot B was refused: died");
    // Until slot A says it fell back, or slot B has booted a second time: a
    // loader that does not fall back boots the dead slot again, and that is the
    // answer, not a wait for one that never comes.
    let again = format!("{SLOT_RECORD} B, ");
    let (from, uart) = (console.len(), guest.uart_log().len());
    ssh::ssh_fire(HOST, rig.port, &rig.identity, "reboot")?;
    await_machine(&mut guest, &mut console, "slot A to fall back, or slot B to boot again", |c| {
        let since = &c[from.min(c.len())..];
        since.contains(&fell_back) || since.matches(&again).count() >= 2
    })?;
    let booted_b = console[from..].matches(&again).count();
    if !console[from..].contains(&fell_back) {
        return Err(format!("slot B booted {booted_b} times after the update and slot A never did"));
    }
    await_machine(&mut guest, &mut console, "slot A's ready marker", |c| c[from..].contains(DEFAULT_READY))?;
    loader_said(&guest, uart, "Previous boot's panic:")?;
    loader_said(&guest, uart, "died on its last boot, so no pass boots it again until an update replaces it")?;

    // The owner's channel on a machine with no console: the fallback boot's
    // own `/log`, whole once that boot has ended itself.
    let at = console.len();
    ssh::ssh_fire(HOST, rig.port, &rig.identity, "reboot")?;
    qemu::await_marker_new(&mut guest, &mut console, bootlog::REBOOTING, at, "the fallback boot to end")?;
    drop(guest);
    let bytes = std::fs::read(&rig.image).map_err(|e| format!("{}: {e}", rig.image.display()))?;
    let (start, len) = super::volumes::log_extent(&bytes, &rig.image)?;
    let log = super::volumes::whole_log(&rig.image, start, len)?;
    if !log.iter().any(|line| line.contains(&fell_back)) {
        return Err(format!("/log never says {fell_back:?}; it holds {} lines", log.len()));
    }
    eprintln!("  [update] slot B's kernel panicked, slot A booted on its own, and /log says {fell_back:?}");
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// Each of `whats` is among `log`'s lines, or the finding that one is not.
fn said(log: &str, whats: &[&str]) -> Result<(), String> {
    for what in whats {
        if !log.contains(what) {
            return Err(format!("{what:?} is not in what the machine said:\n{log}"));
        }
    }
    Ok(())
}

/// **A hang of an image no boot has proven is a death**: slot B is handed the
/// machine and cut at the loader's handoff, which is a hang or a power cut
/// as far as any pass can tell; the retry boots nothing and marks B's image
/// dead, since no floor stands at or above its version; and the pass after
/// boots slot A.
pub fn update_hang_kills_an_unproven_image(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-hang")?;
    let (next, _) = rig.update("next", &[], &[], NEXT, signing::key())?;
    let update = std::fs::read(&next).map_err(|e| format!("{}: {e}", next.display()))?;
    image::stage_slot(&rig.image, Which::B, &update, true)?;

    let handed = rig.launch(bootlog::LOADER_LAST_LINE);
    said(handed.boot_log(), &[&format!("Slot B: {VERIFIED}")])?;
    drop(handed);
    let retry = rig.launch(bootlog::CHAIN_ENDS_LINE);
    said(retry.boot_log(), &[bootlog::HUNG_WITHOUT_A_RECORD, "died on its last boot, so no pass boots it again"])?;
    drop(retry);
    let after = rig.launch(bootlog::LOADER_LAST_LINE);
    said(after.boot_log(), &["Slot B: REFUSED, its image died on its last boot", &format!("Slot A: {VERIFIED}")])?;
    drop(after);
    eprintln!("  [update] slot B cut at its handoff was a death: the retry marked it, and slot A booted");
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **The record a pass writes before it has chosen names no booted image**: a
/// pass whose every slot is refused panics after that first write, and the
/// record it leaves must not still credit the image the last pass booted —
/// whose clean end the next pass would take as that image's proof.
pub fn update_refused_pass_credits_no_image(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-uncredited")?;
    let handed = rig.launch(bootlog::LOADER_LAST_LINE);
    said(handed.boot_log(), &[&format!("Slot A: {VERIFIED}")])?;
    drop(handed);
    if rig.record()?.booted.map(|b| b.slot) != Some(Which::A) {
        return Err(format!("the pass that booted slot A wrote down {:?}", rig.record()?.booted));
    }
    // A byte of slot A's kernel, and slot B holds no image: every slot is
    // refused, so the pass panics after its first write and before its second.
    rig.bend_kernel(Which::A)?;
    let refused = rig.launch("Slots: no slot verifies");
    said(refused.boot_log(), &["Slot A: REFUSED, its kernel is not the bytes its signed header names", "Slot B: REFUSED"])?;
    drop(refused);
    if let Some(booted) = rig.record()?.booted {
        return Err(format!("a pass that booted nothing left a record naming slot {}'s image", booted.slot.letter()));
    }
    eprintln!("  [update] a pass that refused every slot left a record naming no booted image");
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **init claims nothing the slot table names but an idle slot's partition on
/// the running disk**: a table naming the ESP, the log partition, or either
/// of the running slot's partitions as the idle slot's is refused by name,
/// and `update` holds nothing.
pub fn update_grant_refuses_a_stray_partition(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    use toyos_update::slots::Slot;
    let rig = Rig::stage("update-grant")?;
    let (next, _) = rig.update("next", &[], &[], NEXT, signing::key())?;
    let mut file = std::fs::File::open(&rig.image).map_err(|e| format!("{}: {e}", rig.image.display()))?;
    let esp = image::unique_guid_of(&mut file, toyos_gpt::Guid::EFI_SYSTEM)?;
    let table = image::slot_table_of(&mut file)?;
    drop(file);
    let (a, b) = (table.slot(Which::A).ok_or("no slot A")?, table.slot(Which::B).ok_or("no slot B")?);
    let log = rig.log_guid()?;
    let not_a_slot = "the idle slot's volume is not of the type a slot's partition of that kind carries";
    let cases = [
        ("the ESP", esp, b.root, not_a_slot),
        ("the log partition", log, b.root, not_a_slot),
        ("the running slot's volume", a.boot, b.root, "the idle slot's volume is one of the running slot's"),
        ("the running slot's ROOT", b.boot, a.root, "the idle slot's ROOT is one of the running slot's"),
    ];
    for (what, boot, root, why) in cases {
        image::restage_table(&rig.image, |t| t.slots[Which::B.index()] = Some(Slot { boot, root, version: 0 }))?;
        let (mut guest, mut console) = rig.boot()?;
        let (status, said) = rig.install(&next)?;
        if status != Some(1) || !said.contains("this process holds no `slots:table`") {
            return Err(format!("with slot B naming {what}, `update` ended {status:?} saying {said:?}"));
        }
        let refused = format!("init: update: no slot to grant: {why}");
        await_machine(&mut guest, &mut console, &format!("init to refuse {what}"), |c| c.contains(&refused))?;
        eprintln!("  [update] slot B naming {what}: init granted nothing, and `update` held nothing");
        drop(guest);
    }
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **A floor is its key's and its image's**: the owner's floor and another
/// image's, both at the highest version there is, hold this image to nothing
/// — the other image's is deleted, the owner's never — and the clean reboot
/// raises this image's own. And this image's own floor, stored in a shape its
/// loader never writes, is refused and boots nothing.
pub fn update_floor_is_the_images_own(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-floor")?;
    let key = signing::key();
    let own = rig.floor_name()?;
    let owner = floors::name(Scope::Machine, &key.public(), &[0; 16]).as_str().to_string();
    let other = floors::name(Scope::Image, &key.public(), &[0x55; 16]).as_str().to_string();
    let template = super::compile::repo_root().join("ovmf/OVMF_VARS-pure-efi.fd");
    let fresh = || std::fs::copy(&template, &rig.vars).map(|_| ()).map_err(|e| format!("{}: {e}", rig.vars.display()));

    fresh()?;
    vars::plant(&rig.vars, &owner, floors::ATTRIBUTES, &u64::MAX.to_le_bytes())?;
    vars::plant(&rig.vars, &other, floors::ATTRIBUTES, &u64::MAX.to_le_bytes())?;
    let (mut guest, mut console) = rig.boot()?;
    owed(&console, 0, &format!("{SLOT_RECORD} A, the one the slot table marks"))?;
    loader_said(&guest, 0, &format!("Anti-rollback floor: {other} is no floor this image loader keeps; deleted"))?;
    loader_said(&guest, 0, &format!("Anti-rollback floor: {own} (image scope) holds 0"))?;
    let (_, uart) = rig.reboot_until(&mut guest, &mut console, DEFAULT_READY)?;
    loader_said(&guest, uart, &format!("Anti-rollback floor: {BASE}, raised from 0 by the boot that proved it"))?;
    drop(guest);

    let stored = vars::live(&rig.vars)?;
    let value = |name: &str| stored.iter().filter(|v| v.name == name).map(|v| v.data.clone()).collect::<Vec<_>>();
    let want = [(&owner, vec![u64::MAX.to_le_bytes().to_vec()]), (&own, vec![BASE.to_le_bytes().to_vec()]), (&other, vec![])];
    for (name, holds) in want {
        if value(name) != holds {
            return Err(format!("the variable store holds {name} as {:?}, where {holds:?} is owed", value(name)));
        }
    }
    eprintln!("  [update] the owner's floor and another image's held this one to nothing; the other's went, the owner's stayed");

    fresh()?;
    vars::plant(&rig.vars, &own, floors::ATTRIBUTES, &[1; 9])?;
    let refused = rig.launch(FLOOR_REFUSED);
    said(refused.boot_log(), &[&format!("Anti-rollback floor: {own}: it holds 9 bytes where this loader writes 8")])?;
    drop(refused);
    eprintln!("  [update] this image's own floor in nine bytes was refused, and nothing booted");    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// The `BootNext` a request asks for: the one line the loader writes it on.
const BOOT_NEXT_SET: &str = "(written now): the firmware boots it once, at the reset this pass ends with";

/// What a pass says of a request it could not take off a read-only stick.
const STANDS: &str = "Request: a boot of another ESP stands, and is not acted on, because taking it off the slot table failed";

/// **A request for another ESP boots that ESP once, and the order resumes**:
/// the machine asks for its recovery stick with `update --boot-next`; the pass
/// takes the request off the slot table, writes an entry for
/// that stick and points `BootNext` at it; the recovery stick's kernel boots;
/// its reboot hands the machine to the firmware's order, which is the
/// machine's own stick; and the machine's next reboot is its own again.
pub fn update_boot_next_boots_the_entry_once(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-boot-next")?.with_recovery()?;
    let recovery = rig.recovery()?.to_path_buf();
    let esp = toyos_gpt::Guid(Rig::guid_on(&recovery, toyos_gpt::Guid::EFI_SYSTEM)?);
    let (ours, theirs) = (Rig::kernel_of(&rig.image)?, Rig::kernel_of(&recovery)?);
    let (guest, console) = rig.boot()?;
    owed(&console, 0, &ours)?;
    rig.asks(&format!("update --boot-next {esp}"), &format!("the loader boots EFI system partition {esp} once"))?;
    drop(guest);
    rig.powered_off_cleanly()?;

    // A pass that cannot take the request off the table sets nothing.
    let unwritable = BootOptions {
        profile: qemu::Profile::Metal,
        boot_image: Some(Staged::Written(rig.image.clone())),
        stick_readonly: true,
        firmware_vars: Some(rig.vars.clone()),
        recovery_stick: rig.recovery.clone(),
        ready_marker: STANDS,
        ..Default::default()
    };
    let pass = QemuInstance::boot_with_options(&super::compile::repo_root().join(CONFIG), &[], &[], unwritable);
    let said = pass.boot_log().to_string();
    drop(pass);
    let upto = &said[..said.find(STANDS).ok_or("the pass never said its request stands")?];
    if upto.contains("BootNext=Boot") {
        return Err(format!("a pass that could not take the request off the slot table set BootNext:\n{upto}"));
    }
    // Past the marker the pass goes on and may point `BootNext` at its own
    // entry, and never at the recovery stick's.
    if let Ok(next) = vars::global(&rig.vars, "BootNext") {
        let number = u16::from_le_bytes(next.get(..2).ok_or("a BootNext shorter than a number")?.try_into().expect("two bytes"));
        let option = vars::global(&rig.vars, &format!("Boot{number:04X}"))?;
        if vars::load_option(&option).is_ok_and(|(guid, _)| guid == esp.0) {
            return Err(format!("a pass that could not take the request off the slot table set BootNext to Boot{number:04X}, the recovery stick's"));
        }
    }
    eprintln!("  [update] a pass that could not write the slot table set no BootNext");

    let (mut guest, mut console) = rig.boot()?;
    owed(&console, 0, &theirs)?;
    loader_said(&guest, 0, "Request: a boot of another ESP is taken off the slot table")?;
    loader_said(&guest, 0, &format!("ESP {esp} {BOOT_NEXT_SET}"))?;
    eprintln!("  [update] `update --boot-next {esp}` booted the recovery stick at the next boot");

    // Twice more, each until the machine's own kernel or the recovery stick's
    // a second time: a request never taken away boots the recovery stick at
    // every pass, and that is the answer, not a wait for one that never comes.
    for doing in ["the recovery stick hands the machine back", "the machine reboots itself"] {
        let (from, uart) = (console.len(), guest.uart_log().len());
        ssh::ssh_fire(HOST, rig.port, &rig.identity, "reboot")?;
        await_machine(&mut guest, &mut console, doing, |c| {
            let since = &c[from.min(c.len())..];
            since.contains(&ours) || since.contains(&theirs)
        })?;
        if console[from..].contains(&theirs) {
            return Err(format!("{doing}, and the recovery stick booted again: the request asked for it once"));
        }
        await_machine(&mut guest, &mut console, "the machine's sshd", |c| c[from..].contains(SSHD_LISTENING))?;
        let since = guest.uart_log()[uart..].to_string();
        if since.contains("BootNext=Boot") {
            return Err(format!("{doing}, and a pass set BootNext again:\n{since}"));
        }
    }
    let booted = console.matches(&theirs).count();
    if booted != 1 {
        return Err(format!("the recovery stick's kernel booted {booted} times, where the request asked for once"));
    }
    eprintln!("  [update] the recovery stick booted once, and the machine's own stick at every boot after");
    drop(guest);
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// A trial writes nothing of the image the machine keeps, and a refused trial
/// boots the marked slot with no refusal told.
pub fn update_trial_writes_nothing_of_the_kept_slot(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-trial")?;
    let (next, _) = rig.update("next", &[], &[], NEXT, signing::key())?;
    let kept = rig.signed_header(Which::A)?;
    let (mut guest, mut console) = rig.boot()?;
    let asked = ssh::ssh_pipe(HOST, rig.port, &rig.identity, "update --once", &next)?;
    let said = format!("{}{}", asked.stdout_text(), asked.stderr_text());
    if asked.status != Some(0) || !said.contains(&format!("update: installed version {NEXT} in slot B")) {
        return Err(format!("`update --once` ended {:?} saying {said:?}", asked.status));
    }
    let trial = format!("{SLOT_RECORD} B, once, as the running system asked; the slot table marks A");
    let (from, _) = rig.reboot_until(&mut guest, &mut console, &trial)?;
    await_machine(&mut guest, &mut console, "the trial's sshd", |c| c[from..].contains(SSHD_LISTENING))?;

    // Newer than the trial's: nothing but the grant stands before slot A.
    let (later, _) = rig.update("later", &[], &[], NEXT + 1, signing::key())?;
    let (status, said) = rig.install(&later)?;
    if rig.signed_header(Which::A)? != kept {
        return Err(format!("on the trial, `update` wrote slot A, the image the machine keeps, and ended {status:?} saying {said:?}"));
    }
    if status != Some(1) || !said.contains("this process holds no `slots:table`") {
        return Err(format!("on the trial, `update` ended {status:?} saying {said:?}"));
    }
    let refused = "init: update: no slot to grant: slot B runs on trial, and the idle slot is A, the image the machine keeps";
    await_machine(&mut guest, &mut console, "init to refuse the trial a grant", |c| c[from..].contains(refused))?;
    let (_, uart) = rig.reboot_until(&mut guest, &mut console, &format!("{SLOT_RECORD} A, the one the slot table marks"))?;
    loader_said(&guest, uart, "Request: slot B's trial, which is over, is taken off the slot table")?;
    drop(guest);
    rig.powered_off_cleanly()?;
    let mut file = std::fs::File::open(&rig.image).map_err(|e| format!("{}: {e}", rig.image.display()))?;
    let table = image::slot_table_of(&mut file)?;
    if table.marked != Which::A || !table.request.is_empty() {
        return Err(format!("after the trial the slot table is {table:?}: slot A marked and nothing asked is owed"));
    }
    eprintln!("  [update] the trial held no grant, slot A kept its image, and the boot after was slot A's");

    // A trial the loader refuses: slot B's kernel bent, and asked for once.
    drop(file);
    rig.bend_kernel(Which::B)?;
    image::restage_table(&rig.image, |t| t.request.next = Some(toyos_update::slots::Next::Slot(Which::B)))?;
    let (guest, console) = rig.boot()?;
    loader_said(&guest, 0, "Slot B: REFUSED, its kernel is not the bytes its signed header names")?;
    owed(&console, 0, &format!("{SLOT_RECORD} A, the one the slot table marks"))?;
    eprintln!("  [update] a refused trial booted slot A as marked, with no refusal told");
    drop(guest);
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// What sshd says once it serves, on every image here.
const SSHD_LISTENING: &str = "sshd: listening on port 22";

/// **`update --boot-first` makes this loader's entry the firmware's first**:
/// the pass after the reboot writes an entry for its own ESP and puts it at
/// the head of `BootOrder`; the firmware boots by it at every reset after —
/// the pass after that was booted as that entry — and the variable store,
/// read by EDK2's layout and not by the loader's, holds the order and an
/// entry naming this image's ESP by `HD(…)/\EFI\BOOT\BOOTX64.EFI`.
pub fn update_boot_first_puts_the_loader_first(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-boot-first")?;
    let esp = Rig::guid_on(&rig.image, toyos_gpt::Guid::EFI_SYSTEM)?;
    let (mut guest, mut console) = rig.boot()?;
    rig.asks("update --boot-first", "first in the firmware's BootOrder")?;
    let (_, uart) = rig.reboot_until(&mut guest, &mut console, DEFAULT_READY)?;
    loader_said(&guest, uart, "Request: the boot order is taken off the slot table")?;
    let since = guest.uart_log()[uart..].to_string();
    let line = since
        .lines()
        .find(|l| l.contains("this loader's ESP (written now), is first"))
        .ok_or_else(|| format!("the loader never put its entry first:\n{since}"))?;
    eprintln!("  [update] {line}");
    let number = line
        .split("Boot entries: Boot")
        .nth(1)
        .and_then(|rest| rest.get(..4))
        .and_then(|hex| u16::from_str_radix(hex, 16).ok())
        .ok_or_else(|| format!("no entry number in {line:?}"))?;
    let (_, uart) = rig.reboot_until(&mut guest, &mut console, DEFAULT_READY)?;
    loader_said(&guest, uart, &format!("this pass was booted as Boot{number:04X}"))?;
    // Asked once, written once.
    let since = guest.uart_log()[uart..].to_string();
    if let Some(again) = since.lines().find(|l| l.contains("Request:") || l.contains("is first:")) {
        return Err(format!("a pass after the one that wrote the order said {again:?}"));
    }
    drop(guest);

    let order = vars::global(&rig.vars, "BootOrder")?;
    let first = order.get(..2).map(|w| u16::from_le_bytes([w[0], w[1]]));
    if first != Some(number) {
        return Err(format!("the variable store's BootOrder is {order:02x?}, whose head is not Boot{number:04X}"));
    }
    let option = vars::global(&rig.vars, &format!("Boot{number:04X}"))?;
    let (guid, path) = vars::load_option(&option)?;
    if guid != esp || path != r"\EFI\BOOT\BOOTX64.EFI" {
        return Err(format!("Boot{number:04X} names partition {guid:02x?} and {path:?}, where {esp:02x?} and the removable path are owed"));
    }
    eprintln!("  [update] BootOrder begins Boot{number:04X}, which names this image's ESP, and the firmware booted by it");
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **A machine whose every slot is refused boots its recovery stick**: slot
/// A's kernel carries a flipped byte and slot B holds no image, so no slot
/// verifies; the loader sets `BootNext` to the entry after its own in
/// `BootOrder` and resets, and the recovery stick behind it boots — past an
/// entry for the machine's own ESP, planted right behind the entry that boots
/// it, which would boot the same failure again.
pub fn update_no_slot_boots_the_recovery_stick(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-recovery")?.with_recovery()?;
    // The firmware's own entries, which its first boot writes.
    drop(rig.launch("Slots: the table marks"));
    rig.powered_off_cleanly()?;
    let order = vars::global(&rig.vars, "BootOrder")?;
    let order: Vec<u16> = order.chunks(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect();
    let planted = (0x100u16..).find(|n| vars::global(&rig.vars, &format!("Boot{n:04X}")).is_err()).expect("a free number");
    vars::plant_global(&rig.vars, &format!("Boot{planted:04X}"), &own_esp_option(&rig.image)?)?;
    let mut planted_order = order.clone();
    planted_order.insert(1, planted);
    vars::plant_global(&rig.vars, "BootOrder", &planted_order.iter().flat_map(|n| n.to_le_bytes()).collect::<Vec<u8>>())?;

    rig.bend_kernel(Which::A)?;
    // The recovery stick's own loader, naming its log partition: the pass
    // that follows the fall.
    let theirs: &'static str = Box::leak(Rig::loader_of(rig.recovery()?)?.into_boxed_str());
    let fell = rig.launch(theirs);
    said(
        fell.boot_log(),
        &["Slot A: REFUSED, its kernel is not the bytes its signed header names", "Slots: no slot verifies", "this pass failed, so BootNext=Boot"],
    )?;
    let state = fell.boot_log().lines().find(|l| l.contains("this pass was booted as")).unwrap_or_default().to_string();
    let current = order[0];
    if !state.contains(&format!("booted as Boot{current:04X}; BootOrder is {current:04X},{planted:04X},")) {
        return Err(format!("the pass was not booted by Boot{current:04X} with the planted Boot{planted:04X} behind it: {state:?}"));
    }
    let falls: Vec<&str> = fell.boot_log().lines().filter(|l| l.contains("this pass failed")).collect();
    let recovery = planted_order.get(2).ok_or("the firmware's order holds no entry behind its first")?;
    let owed = format!("BootNext=Boot{recovery:04X}, the entry after Boot{current:04X}");
    if falls.len() != 1 || !falls[0].contains(&owed) {
        return Err(format!("the pass fell by {falls:?}, where once, past Boot{planted:04X} to the recovery stick's entry ({owed:?}), is owed"));
    }
    eprintln!("  [update] {}; the recovery stick's loader took the machine", falls[0]);
    drop(fell);
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// What the loader's panic handler says of a failed pass with nothing behind
/// its own entry.
const NO_ENTRY: &str = "this pass failed, and there is no entry to fall to";

/// **A failed pass with no entry behind its own powers the machine off**,
/// never resetting into the same failure: slot A's kernel carries a flipped
/// byte and slot B holds no image, so no slot verifies, and every entry behind
/// the stick's in `BootOrder` is made inactive (`LOAD_OPTION_ACTIVE`, UEFI 2.10
/// §3.1.3, cleared). The pass says so, and QEMU, which takes every reset here,
/// exits after that one pass.
pub fn update_no_entry_powers_off(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-no-entry")?;
    // The firmware's own entries, which its first boot writes.
    drop(rig.launch("Slots: the table marks"));
    rig.powered_off_cleanly()?;
    let order = vars::global(&rig.vars, "BootOrder")?;
    let order: Vec<u16> = order.chunks(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect();
    let (current, behind) = order.split_first().ok_or("the firmware wrote an empty BootOrder")?;
    for n in behind {
        let name = format!("Boot{n:04X}");
        let mut option = vars::global(&rig.vars, &name)?;
        option[0] &= !1;
        vars::plant_global(&rig.vars, &name, &option)?;
    }
    rig.bend_kernel(Which::A)?;
    let mut fell = rig.launch("Boot entries: this pass failed");
    let fall = fell.boot_log().lines().find(|l| l.contains("this pass failed")).unwrap_or_default().to_string();
    if !fall.contains(NO_ENTRY) {
        let state = fell.boot_log().lines().find(|l| l.contains("this pass was booted as")).unwrap_or_default().to_string();
        return Err(format!(
            "with Boot{current:04X}'s followers {behind:04X?} made inactive, the pass fell by {fall:?} ({state:?}), where {NO_ENTRY:?} is owed"
        ));
    }
    let after = fell.await_exit(std::time::Duration::from_secs(30)).map_err(|why| format!("the machine did not power off after {fall:?}: {why}"))?;
    let all = format!("{}{after}", fell.boot_log());
    said(&all, &["Slot A: REFUSED, its kernel is not the bytes its signed header names", "Slots: no slot verifies"])?;
    let passes = all.matches(bootlog::LOADER_FIRST_LINE).count();
    if passes != 1 {
        return Err(format!("the loader ran {passes} passes, where one that powers the machine off is owed:\n{all}"));
    }
    eprintln!("  [update] {fall}");
    drop(fell);
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// An active `EFI_LOAD_OPTION` for `HD(<the ESP of the image at path>)/
/// \EFI\BOOT\BOOTX64.EFI`, its bytes written from UEFI 2.10 §3.1.3 and §10.3.6
/// and the partition read out of the GPT entry array by §5.3.3's layout.
fn own_esp_option(path: &Path) -> Result<Vec<u8>, String> {
    let guid = Rig::guid_on(path, toyos_gpt::Guid::EFI_SYSTEM)?;
    let mut disk = vec![0u8; 64 << 10];
    std::fs::File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut disk)).map_err(|e| format!("{}: {e}", path.display()))?;
    let word = |at: usize, n: usize| disk[at..at + n].iter().rev().fold(0u64, |v, &b| v << 8 | u64::from(b));
    let (array, count, size) = (word(512 + 72, 8) as usize * 512, word(512 + 80, 4) as usize, word(512 + 84, 4) as usize);
    let index = (0..count).find(|i| disk[array + i * size + 16..array + i * size + 32] == guid).ok_or("no GPT entry names the ESP")?;
    let (first, last) = (word(array + index * size + 32, 8), word(array + index * size + 40, 8));
    let mut node = vec![4, 1, 42, 0];
    node.extend((index as u32 + 1).to_le_bytes());
    node.extend(first.to_le_bytes());
    node.extend((last - first + 1).to_le_bytes());
    node.extend(guid);
    node.extend([2, 2]);
    let file: Vec<u8> = r"\EFI\BOOT\BOOTX64.EFI".encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    node.extend([4, 4]);
    node.extend((4 + file.len() as u16).to_le_bytes());
    node.extend(file);
    node.extend([0x7f, 0xff, 4, 0]);
    let mut option = 1u32.to_le_bytes().to_vec();
    option.extend((node.len() as u16).to_le_bytes());
    option.extend("Planted".encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
    option.extend(node);
    Ok(option)
}

/// The firmware's variable store, as OVMF keeps it in its `VARS` file: a
/// firmware volume holding an authenticated variable store, read and written
/// by the layout EDK2 declares for it (`MdeModulePkg/Include/Guid/
/// VariableFormat.h`) and not by anything the loader shares — only the
/// floor's vendor GUID, which the store is asked for.
mod vars {
    use std::path::Path;

    /// The floor's vendor, `33BE3D4A-30E6-49F5-8050-F169D93A20FB`, in the
    /// byte order `EFI_GUID` stores.
    const VENDOR: [u8; 16] = [0x4a, 0x3d, 0xbe, 0x33, 0xe6, 0x30, 0xf5, 0x49, 0x80, 0x50, 0xf1, 0x69, 0xd9, 0x3a, 0x20, 0xfb];
    /// `EFI_FIRMWARE_VOLUME_HEADER`: its signature and its header's length.
    const FV_SIGNATURE: (usize, &[u8]) = (0x28, b"_FVH");
    const FV_HEADER_LEN_AT: usize = 0x30;
    /// `VARIABLE_STORE_HEADER`: signature GUID, size, format, state, reserved.
    const STORE_HEADER: usize = 16 + 4 + 1 + 1 + 2 + 4;
    /// `AUTHENTICATED_VARIABLE_HEADER`: start id, state, reserved, attributes,
    /// monotonic count, time stamp, public key index, name size, data size,
    /// vendor GUID.
    const HEADER: usize = 2 + 1 + 1 + 4 + 8 + 16 + 4 + 4 + 4 + 16;
    const START_ID: u16 = 0x55AA;
    const VAR_ADDED: u8 = 0x3F;
    /// `VAR_ADDED & VAR_IN_DELETED_TRANSITION`: still the variable until the
    /// copy replacing it is added.
    const IN_TRANSITION: u8 = 0x3E;
    /// What a retired copy's state is ANDed with.
    const VAR_DELETED: u8 = 0xFD;
    /// `EFI_VARIABLE_NON_VOLATILE | BOOTSERVICE_ACCESS | RUNTIME_ACCESS`.
    const NV_BS_RT: u32 = 0x7;

    pub struct Var {
        pub name: String,
        pub data: Vec<u8>,
    }

    /// Where the variables begin and where the store ends.
    fn store(bytes: &[u8]) -> Result<(usize, usize), String> {
        let (at, sig) = FV_SIGNATURE;
        if bytes.get(at..at + sig.len()) != Some(sig) {
            return Err("the variable file is no firmware volume".into());
        }
        let header = u16::from_le_bytes([bytes[FV_HEADER_LEN_AT], bytes[FV_HEADER_LEN_AT + 1]]) as usize;
        let size = u32::from_le_bytes(bytes[header + 16..header + 20].try_into().expect("four bytes")) as usize;
        Ok((header + STORE_HEADER, header + size))
    }

    /// One variable header in the store.
    struct Found {
        /// Where its header begins.
        at: usize,
        state: u8,
        vendor: [u8; 16],
        var: Var,
    }

    /// Every variable header in the store, and where the erased space after
    /// them begins.
    fn walk(bytes: &[u8]) -> Result<(Vec<Found>, usize), String> {
        let (mut at, end) = store(bytes)?;
        let mut out = Vec::new();
        while at + HEADER <= end && u16::from_le_bytes([bytes[at], bytes[at + 1]]) == START_ID {
            let word = |off: usize| u32::from_le_bytes(bytes[at + off..at + off + 4].try_into().expect("four bytes")) as usize;
            let (name_len, data_len) = (word(36), word(40));
            let vendor: [u8; 16] = bytes[at + 44..at + 60].try_into().expect("sixteen bytes");
            let name_at = at + HEADER;
            let units: Vec<u16> = bytes[name_at..name_at + name_len]
                .chunks(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&u| u != 0)
                .collect();
            let data = bytes[name_at + name_len..name_at + name_len + data_len].to_vec();
            out.push(Found { at, state: bytes[at + 2], vendor, var: Var { name: String::from_utf16_lossy(&units), data } });
            at = (name_at + name_len + data_len).next_multiple_of(4);
        }
        Ok((out, at))
    }

    /// The live variables under the floor's vendor.
    pub fn live(path: &Path) -> Result<Vec<Var>, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(walk(&bytes)?
            .0
            .into_iter()
            .filter(|found| found.vendor == VENDOR && (found.state == VAR_ADDED || found.state == IN_TRANSITION))
            .map(|found| found.var)
            .collect())
    }

    /// `EFI_GLOBAL_VARIABLE`, `8BE4DF61-93CA-11D2-AA0D-00E098032B8C`, in the
    /// byte order `EFI_GUID` stores: `BootOrder`'s and every `Boot####`'s.
    const GLOBAL: [u8; 16] = [0x61, 0xdf, 0xe4, 0x8b, 0xca, 0x93, 0xd2, 0x11, 0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c];

    /// The one live global variable called `name`: the one the store added
    /// last, since a rewrite adds a copy before it retires the old.
    pub fn global(path: &Path, name: &str) -> Result<Vec<u8>, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        walk(&bytes)?
            .0
            .into_iter()
            .rfind(|found| found.vendor == GLOBAL && found.state == VAR_ADDED && found.var.name == name)
            .map(|found| found.var.data)
            .ok_or_else(|| format!("the variable store holds no live {name}"))
    }

    /// What an `EFI_LOAD_OPTION` boots: the GPT signature of its HARDDRIVE
    /// node and the path of its FILE_PATH node — read by the tables of UEFI
    /// 2.10 §3.1.3 and §10.3.6 here, and not by `toyos_update::entry`, which
    /// wrote it.
    pub fn load_option(option: &[u8]) -> Result<([u8; 16], String), String> {
        let path_len = u16::from_le_bytes([option[4], option[5]]) as usize;
        let mut at = 6;
        while option.get(at..at + 2).ok_or("the description runs off the option")? != [0, 0] {
            at += 2;
        }
        at += 2;
        let mut path = option.get(at..at + path_len).ok_or("the device path runs off the option")?;
        let (mut guid, mut file) = (None, None);
        while path.len() >= 4 {
            let len = u16::from_le_bytes([path[2], path[3]]) as usize;
            let node = path.get(..len).filter(|_| len >= 4).ok_or("a node that cannot be stepped over")?;
            match (node[0], node[1]) {
                (4, 1) if node.len() == 42 && node[40] == 2 && node[41] == 2 => {
                    guid = Some(<[u8; 16]>::try_from(&node[24..40]).expect("sixteen bytes"))
                }
                (4, 4) => {
                    let units: Vec<u16> =
                        node[4..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
                    file = Some(String::from_utf16_lossy(&units));
                }
                (0x7f, 0xff) => break,
                _ => {}
            }
            path = &path[len..];
        }
        Ok((guid.ok_or("no GPT HARDDRIVE node")?, file.ok_or("no FILE_PATH node")?))
    }

    /// Add `name` under the floor's vendor with `attributes` and `data`, as
    /// the firmware would have added it.
    pub fn plant(path: &Path, name: &str, attributes: u32, data: &[u8]) -> Result<(), String> {
        add(path, VENDOR, name, attributes, data)
    }

    /// Make the global variable `name` hold `data`, non-volatile and readable
    /// at boot and at runtime, as the firmware rewrites one: its live copy
    /// retired, and the new one added.
    pub fn plant_global(path: &Path, name: &str, data: &[u8]) -> Result<(), String> {
        let mut bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        for found in walk(&bytes)?.0.iter().filter(|f| f.vendor == GLOBAL && f.state == VAR_ADDED && f.var.name == name) {
            bytes[found.at + 2] &= VAR_DELETED;
        }
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        add(path, GLOBAL, name, NV_BS_RT, data)
    }

    fn add(path: &Path, vendor: [u8; 16], name: &str, attributes: u32, data: &[u8]) -> Result<(), String> {
        let mut bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (_, end) = store(&bytes)?;
        let (_, at) = walk(&bytes)?;
        let mut units: Vec<u8> = name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        let mut var = vec![0u8; HEADER];
        var[..2].copy_from_slice(&START_ID.to_le_bytes());
        var[2] = VAR_ADDED;
        var[4..8].copy_from_slice(&attributes.to_le_bytes());
        var[36..40].copy_from_slice(&(units.len() as u32).to_le_bytes());
        var[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
        var[44..60].copy_from_slice(&vendor);
        var.append(&mut units);
        var.extend_from_slice(data);
        if at + var.len() > end || bytes[at..at + var.len()].iter().any(|&b| b != 0xFF) {
            return Err(format!("no erased room for {name} at byte {at} of the variable store"));
        }
        bytes[at..at + var.len()].copy_from_slice(&var);
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    }
}
