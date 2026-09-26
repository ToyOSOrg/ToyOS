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

use std::path::{Path, PathBuf};
use std::time::Instant;

use toyos_build::bootlog;
use toyos_build::build::{self, Plan};
use toyos_build::image::{self, SecondSlot, Signing};
use toyos_build::signing::{self, Key};
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
            ..Default::default()
        };
        let config = super::compile::repo_root().join(CONFIG);
        let mut guest = QemuInstance::boot_with_options(&config, &[], &[], options);
        let mut console = guest.boot_log().to_string();
        qemu::await_marker(&mut guest, &mut console, "sshd: listening on port 22", "sshd to open its port")?;
        Ok((guest, console))
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
        qemu::await_marker_new(guest, console, marker, from, &format!("{marker:?} after the reboot")).map_err(|why| {
            let all = guest.uart_log();
            format!("{why}\nthe 16550 since the reboot:\n{}", &all[uart.min(all.len())..])
        })?;
        Ok((from, uart))
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
    qemu::await_marker_new(&mut guest, &mut console, DEFAULT_READY, from, "the new slot's ready marker")?;
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
    drop(guest);
    let _ = std::fs::remove_dir_all(&rig.scratch);
    Ok(())
}

/// **Every slot the loader must refuse is refused by name, and the other boots**
/// — a flipped byte, no signed header, another key's signature, a version
/// under the floor — and `update` itself refuses the two it can see before it
/// writes anything: another key, and an image older than what runs.
pub fn update_refusals_boot_the_other_slot(_: &Path, _: &[(String, Vec<u8>)], _: &[(String, Vec<u8>)]) -> Result<(), String> {
    let rig = Rig::stage("update-refusals")?;
    let stranger = Key::mint();
    let (next, _) = rig.update("next", &[], &[], NEXT, signing::key())?;
    let (foreign, _) = rig.update("foreign", &[], &[], NEXT, &stranger)?;
    let (older, _) = rig.update("older", &[], &[], OLDER, signing::key())?;

    // The machine's first boot: `update` refuses both before it writes, and a
    // reboot proves the base image, which raises the floor to its version.
    let (mut guest, mut console) = rig.boot()?;
    for (file, word) in [(&foreign, "the signature is not this machine's key's"), (&older, &*format!("its version {OLDER} is older than {BASE}"))] {
        let (status, said) = rig.install(file)?;
        if status != Some(1) || !said.contains(word) {
            return Err(format!("`update < {}` ended {status:?} saying {said:?}, where {word:?} is owed", file.display()));
        }
    }
    let (from, uart) = rig.reboot_until(&mut guest, &mut console, DEFAULT_READY)?;
    loader_said(&guest, uart, &format!("Anti-rollback floor: {BASE}, raised from 0 by the boot that proved it"))?;
    owed(&console, from, &format!("{SLOT_RECORD} A, the one the slot table marks"))?;
    drop(guest);

    let bytes = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
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
    let (from, uart) = rig.reboot_until(&mut guest, &mut console, &fell_back)?;
    qemu::await_marker_new(&mut guest, &mut console, DEFAULT_READY, from, "slot A's ready marker")?;
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
