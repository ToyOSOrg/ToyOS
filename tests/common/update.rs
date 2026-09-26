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
}

/// The plan for this config's image: `features` is the kernel build, `params`
/// the actuators its slot arms, `version` its signed header's.
fn plan(features: &[&str], params: &[&str], version: u64, second: Option<SecondSlot>) -> Plan {
    let mut plan = Plan::new(&super::compile::repo_root().join(CONFIG).join("system.toml"), features, params);
    plan.version = version;
    plan.second = second;
    plan
}

/// What every image here carries on ROOT beside the config's own: the key the
/// host logs in with.
fn staged(identity: &Identity) -> Vec<(String, Vec<u8>)> {
    vec![(ssh::KEYS_ON_ROOT.to_string(), identity.authorized_line().into_bytes())]
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
        Ok(Self { scratch, image, vars, identity, port: qemu::free_host_port(), base_kernel: parts.kernel.len() })
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
/// runs; and the boot that proved the old image raised the floor.
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

/// **A floor is its key's and its image's**: this image's own floor stored in
/// a shape its loader never writes is refused and boots nothing; the owner's
/// floor and another image's, both at the highest version there is, hold
/// this image to nothing — the other image's is deleted, the owner's never —
/// and the clean reboot raises this image's own.
pub fn update_floor_is_the_images_own(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-floor")?;
    let key = signing::key();
    let own = rig.floor_name()?;
    let owner = floors::name(Scope::Machine, &key.public(), &[0; 16]).as_str().to_string();
    let other = floors::name(Scope::Image, &key.public(), &[0x55; 16]).as_str().to_string();
    let template = super::compile::repo_root().join("ovmf/OVMF_VARS-pure-efi.fd");
    let fresh = || std::fs::copy(&template, &rig.vars).map(|_| ()).map_err(|e| format!("{}: {e}", rig.vars.display()));

    fresh()?;
    vars::plant(&rig.vars, &own, floors::ATTRIBUTES, &[1; 9])?;
    let refused = rig.launch(FLOOR_REFUSED);
    said(refused.boot_log(), &[&format!("Anti-rollback floor: {own}: it holds 9 bytes where this loader writes 8")])?;
    drop(refused);

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
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
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

    /// Every variable header in the store: its offset, state, name and data.
    fn walk(bytes: &[u8]) -> Result<(Vec<(usize, u8, [u8; 16], String, Vec<u8>)>, usize), String> {
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
            out.push((at, bytes[at + 2], vendor, String::from_utf16_lossy(&units), data));
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
            .filter(|(_, state, vendor, _, _)| *vendor == VENDOR && (*state == VAR_ADDED || *state == IN_TRANSITION))
            .map(|(_, _, _, name, data)| Var { name, data })
            .collect())
    }

    /// Add `name` under the floor's vendor with `attributes` and `data`, as
    /// the firmware would have added it.
    pub fn plant(path: &Path, name: &str, attributes: u32, data: &[u8]) -> Result<(), String> {
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
        var[44..60].copy_from_slice(&VENDOR);
        var.append(&mut units);
        var.extend_from_slice(data);
        if at + var.len() > end || bytes[at..at + var.len()].iter().any(|&b| b != 0xFF) {
            return Err(format!("no erased room for {name} at byte {at} of the variable store"));
        }
        bytes[at..at + var.len()].copy_from_slice(&var);
        std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
    }
}
