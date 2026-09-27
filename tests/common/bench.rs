//! The bench, rehearsed in QEMU: `toyos-metal` drives a machine that runs
//! ToyOS and nothing else — no Ubuntu anywhere — through one whole metal boot:
//! the image delivered with `update --once`, the reboot asked over ssh, the
//! boot's service swapped while it runs and the machine handed back, the
//! machine back as the bench, the boot's files read over sftp and judged by
//! the judges the T14's old path runs.
//!
//! **The machine is the bench image on a stick** (`tests/benchvirtiocase`),
//! its sshd authorizing a runner key minted here; **the boot is staged as the
//! metal profile stages it** (`metal::stage`): `tests/swapcase` with the swap
//! rehearsal's hold job, its bound, its own key and the netd binary the swap
//! sends. The loop is the library call `toyos-metal` makes, with the machine
//! reached through QEMU's forwards instead of its name.
//!
//! **The oracles are the machine's, not the loop's**: the kernel's own record
//! that the boot was slot B booted once, the loader's line that it asked for
//! once, the table still marking the bench after, and init's words on the
//! swap — each read where the machine left it, beside the loop's verdict.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;

use toyos_build::metal::{self, Args, Machine};
use toyos_build::metaltalk::Peer;

use super::qemu::{self, BootOptions, QemuInstance, Staged};
use super::ssh::{self, Identity, HOST};

/// The bench's config in front of QEMU's virtio-net.
const BENCH: &str = "tests/benchvirtiocase";

/// The bench's own version: under every staged boot's, which is its build's
/// second, so `update` takes each as newer than what runs.
const BENCH_VERSION: u64 = 100;

/// The boot the loop delivers: the swap rehearsal's machine, held until the
/// swap hands it back.
const SWAPPING: super::metal::Arm = super::metal::Arm {
    swap: Some("netd"),
    ..super::metal::once("benchswap", "tests/swapcase", &[], super::swap::HOLD_JOBS)
};

/// The kernel's record of a slot booted once (`kernel/src/main.rs`).
const ONCE_RECORD: &str = "boot: slot B, once, as the running system asked; the slot table marks A";

/// The last line of the `loader-previous.log` staged before the bench boots.
const STALE_CHAIN: &str = "the staged chain's last line\n";

/// **The exit**: the loop drives the whole bench cycle against a machine
/// running ToyOS alone, and a tampered upload is refused before it.
pub fn bench_loop_drives_a_toyos_machine(
    _: &Path,
    _: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let root = super::compile::repo_root();
    let scratch = super::lane::dir().join("bench-loop");
    std::fs::create_dir_all(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;

    let (boot, key) = super::metal::stage(&scratch, &SWAPPING, rust_bins)?;
    let key = key.ok_or("a swapping boot authorizes a key of its own")?;
    let home = boot.parent().ok_or("the staged image has a directory")?.to_path_buf();
    let update = toyos_build::image::update_of(&boot)?;

    let runner = Identity::mint_in(&scratch.join("runner"))?;
    let bench = toyos_build::build::bench_image(
        &root,
        &root.join(BENCH),
        &runner.authorized_line(),
        BENCH_VERSION,
        2 * update.bytes.len() as u64,
        true,
    )?;
    let disk = scratch.join("bench.img");
    std::fs::write(&disk, bench).map_err(|e| format!("write {}: {e}", disk.display()))?;
    // An earlier chain's file, longer than any chain and ending in a line no
    // pass writes: a pass that writes over it rather than deleting it first
    // leaves that tail in every readback after.
    let mut file = std::fs::File::open(&disk).map_err(|e| format!("{}: {e}", disk.display()))?;
    let log_guid = toyos_build::image::unique_guid_of(&mut file, toyos_gpt::Guid::MICROSOFT_BASIC)?;
    drop(file);
    let mut stale = "an earlier chain's line\n".repeat(16 << 10).into_bytes();
    stale.extend_from_slice(STALE_CHAIN.as_bytes());
    toyos_build::image::create_file_on(&disk, log_guid, toyos_build::bootlog::LOADER_PREVIOUS_LOG, &stale)?;
    let vars = scratch.join("OVMF_VARS.fd");
    std::fs::copy(root.join("ovmf/OVMF_VARS-pure-efi.fd"), &vars).map_err(|e| format!("the variable store: {e}"))?;
    let data = scratch.join("data.img");
    toyos_build::build::create_sparse(&data, qemu::NVME_SMALL);

    let (ssh_port, log_port) = (qemu::free_host_port(), qemu::free_host_port());
    let options = BootOptions {
        profile: qemu::Profile::Headless,
        boot_image: Some(Staged::Written(disk.clone())),
        nvme_image: Some(data),
        ssh_port: Some(ssh_port),
        log_port: Some(log_port),
        takes_the_reset: true,
        firmware_vars: Some(vars),
        ..Default::default()
    };
    let mut guest = QemuInstance::boot_with_options(&root.join(BENCH), &[], &[], options);
    let mut console = guest.boot_log().to_string();
    qemu::await_marker(&mut guest, &mut console, "sshd: listening on port 22", "the bench's sshd")?;
    if !console.contains("boot: slot A, the one the slot table marks") {
        return Err("the bench did not boot its own marked slot".to_string());
    }

    // **A tampered upload is refused**, by `update` and before the loop: one
    // byte of the kernel past the signed header, whose signature still holds.
    let mut bent = update.bytes;
    bent[toyos_update::image::SIGNED_BYTES + 100] ^= 0x01;
    let tampered = scratch.join("tampered.update");
    std::fs::write(&tampered, &bent).map_err(|e| format!("write {}: {e}", tampered.display()))?;
    let refused = ssh::ssh_pipe(HOST, ssh_port, &runner, "update --once", &tampered)?;
    let said = format!("{}{}", refused.stdout_text(), refused.stderr_text());
    if refused.status != Some(1) || !said.contains("the kernel is not the bytes its signed header names") {
        return Err(format!("a tampered upload ended {:?} saying {said:?}", refused.status));
    }
    eprintln!("  [bench] a kernel byte flipped under its signature: {}", said.trim());

    // **The clock the loop reads a `--nic` boot's cable against**, from the
    // bench itself: `date -u +%s` answers a second.
    let clock = ssh::ssh_exec(HOST, ssh_port, &runner, "date -u +%s")?;
    let said = clock.stdout_text();
    if clock.status != Some(0) || said.trim().parse::<u64>().is_err() {
        return Err(format!("`date -u +%s` ended {:?} saying {said:?}", clock.status));
    }
    let other = ssh::ssh_exec(HOST, ssh_port, &runner, "date +%Y")?;
    if other.status != Some(2) {
        return Err(format!("`date +%Y` ended {:?} saying {:?}, where every other form is refused as 2", other.status, other.stdout_text()));
    }
    eprintln!("  [bench] the bench's clock reads {}", said.trim());

    // **The loop**: the library call `toyos-metal` makes, the machine reached
    // through the forwards.
    let readback = scratch.join("readback");
    let words: Vec<String> = [
        "--image",
        &boot.display().to_string(),
        "--readback",
        &readback.display().to_string(),
        "--swap",
        "netd",
        "--binary",
        &home.join("netd").display().to_string(),
        "--talk",
        &key.display().to_string(),
        "--hand-back",
        "--key",
        &runner.private().display().to_string(),
    ]
    .iter()
    .map(|w| w.to_string())
    .collect();
    let mut args = Args::parse(&words).map_err(|refusal| refusal.to_string())?;
    args.machine = Machine {
        log: Peer::At(SocketAddr::from((Ipv4Addr::LOCALHOST, log_port))),
        ssh: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, ssh_port))),
    };
    // The loop reads nothing off this guest's console, and QEMU drops what
    // nobody reads; a thread takes it so the machine's account stays whole.
    let driven = std::thread::scope(|scope| {
        let driving = scope.spawn(|| metal::run(&args));
        while !driving.is_finished() {
            console.push_str(&guest.drain_serial(std::time::Duration::from_millis(500)));
        }
        driving.join().map_err(|_| "the loop panicked".to_string())
    })?;
    console.push_str(&guest.drain_serial(std::time::Duration::from_millis(500)));
    let ms = driven.map_err(|refusal| format!("the loop refused: {refusal}"))?;
    eprintln!("  [bench] the loop's verdict: Boot: complete in {ms:?} ms");

    // What the machine itself says about the cycle.
    let kernel = std::fs::read_to_string(readback.join(metal::READBACK_KERNEL)).map_err(|e| format!("kernel.log: {e}"))?;
    if !kernel.contains(ONCE_RECORD) {
        return Err(format!("the boot's own log never says {ONCE_RECORD:?}"));
    }
    let loader = std::fs::read_to_string(readback.join(metal::READBACK_LOADER)).map_err(|e| format!("loader.log: {e}"))?;
    for owed in [
        "Slot B: asked for once; the table marks A",
        "Request: a boot of slot B once is taken off the slot table",
        "Anti-rollback floor: not raised, because slot B's image was booted once",
    ] {
        if !loader.contains(owed) {
            return Err(format!("the boot's loader passes never say {owed:?}"));
        }
    }
    if loader.contains(STALE_CHAIN) {
        return Err(format!("the boot's loader passes carry {STALE_CHAIN:?}, the tail of a file staged before the bench's first pass"));
    }
    let raised = format!("Anti-rollback floor: {}, raised", update.version);
    if loader.contains(&raised) {
        return Err(format!("a boot of slot B once raised the floor: {raised:?}"));
    }
    let swapped = std::fs::read_to_string(readback.join(metal::READBACK_SWAP)).map_err(|e| format!("swap.txt: {e}"))?;
    eprintln!("  [bench] swap.txt: {}", swapped.lines().next().unwrap_or_default());
    // And the bench is the bench again: its own slot, still marked.
    let after = &console[console.rfind("boot: slot").ok_or("no slot record at all")?..];
    if !after.starts_with("boot: slot A, the one the slot table marks") {
        return Err(format!("the machine came back as {:?}, and the bench is slot A as marked", after.lines().next()));
    }
    let mut file = std::fs::File::open(&disk).map_err(|e| format!("{}: {e}", disk.display()))?;
    let table = toyos_build::image::slot_table_of(&mut file)?;
    if table.marked != toyos_update::slots::Which::A || !table.request.is_empty() {
        return Err(format!("the slot table after the cycle is {table:?}: slot A marked and nothing asked is owed"));
    }
    eprintln!("  [bench] update --once, reboot, readback, judge, swap and hand-back, with no Ubuntu: the bench is slot A again");
    drop(guest);
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(())
}
