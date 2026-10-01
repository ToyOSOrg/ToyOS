//! The USB mass-storage gate.
//!
//! Ground truth is the backing file on the *host*: the harness writes bytes
//! into the image
//! before the boot and the guest has to find them, and the guest writes bytes
//! the harness finds afterwards. Neither half of the driver certifies the
//! other, which a read-back-what-you-wrote test would have let it do.
//!
//! Lives here rather than in `toyos.rs` so the registration hunk in that shared
//! file stays two lines: every agent edits it, and a wide diff there is how
//! work gets swept into somebody else's commit.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::qemu::{self, BootOptions, Profile, QemuInstance};
use super::serial;

fn test_dir() -> PathBuf {
    super::lane::dir()
}

fn sparse(path: &Path, bytes: u64) -> std::fs::File {
    let file = std::fs::File::create(path).expect("create the USB image");
    file.set_len(bytes).expect("size the USB image");
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("reopen the USB image")
}

/// Ask test-runner to run `name`, a binary no image carries, and wait for its
/// answer. The spawn's refusal is a kernel record
/// (`spawn: /system/bin/<name>: not found`), which is the probe's load; any other
/// answer ends the wait red, naming it.
fn absent_probe(
    qemu: &mut QemuInstance,
    console: &mut String,
    name: &str,
    after: &str,
) -> Result<(), String> {
    let asked = console.len();
    writeln!(qemu.stdin_mut(), "run {name}").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let answer = format!("===TEST_END {name} ");
    let refused = format!("===TEST_END {name} error=entity not found===");
    qemu::await_guest(qemu, console, &format!("test-runner's answer to {name} {after}"), |c| {
        c[asked..].contains(&answer)
    })?;
    match console[asked..].lines().find(|line| line.contains(&answer)) {
        Some(line) if line.contains(&refused) => Ok(()),
        line => Err(format!(
            "test-runner answered {name} {after} with {line:?}, and a name no image carries is \
             answered {refused:?}"
        )),
    }
}

/// The staged break on a real stick: the transfer abandoned on the boot stick's
/// first WRITE(10) is recovered, the write completes, the disk keeps its
/// number, and the boot goes on to the deliberate reboot that ends its chain.
pub fn transport_break_on_metal(
    kernel: &serial::Serial,
    after: &serial::Serial,
) -> Result<(), String> {
    transport_break_recovered(kernel)?;
    super::power::done_chain(after)
}

/// The kernel log's half of [`transport_break_on_metal`].
///
/// **The ladder enters at the port reset**: the break leaves the stick owed a
/// WRITE's data, across which no class reset may be asked. The stick decides
/// the rest. It answers the rung's TEST UNIT READY on its port, and the write
/// goes out again there; or it leaves its port under the reset — a SuperSpeed
/// stick enumerated on the USB2 half of its receptacle trains on the USB3 half
/// — is held, comes back as the same device, and the write goes out again on
/// it. **Either way no rung takes it offline.**
pub fn transport_break_recovered(kernel: &serial::Serial) -> Result<(), String> {
    let staged = kernel.must_say(
        "transport broke on SCSI 0x2a: a staged break skipped the data phase wait; break 1 of ",
    )?;
    let under_test = broke_on(staged)?;
    let entered = kernel.must_say_after(
        staged,
        &format!("usb-storage: {under_test} is owed the data of the command that broke"),
    )?;
    kernel.must_not_say(&format!("usb-storage: {under_test} is offline"))?;
    if let Ok(left) = kernel.must_say_after(entered, " after this driver reset it; it is held ") {
        let back = kernel.must_say_after(left, " as the same device (USB ")?;
        kernel.must_say_after(
            back,
            "is back, and the operation it was asked went out again on it: it completed",
        )?;
        eprintln!("  [usb] {left}");
        eprintln!("  [usb] {back}");
        return Ok(());
    }
    let took = kernel.must_say_after(entered, &format!("usb-storage: {under_test} the port reset took"))?;
    kernel.must_say_after(took, &format!("usb-storage: {under_test} SCSI 0x2a completed after "))?;
    eprintln!("  [usb] {took}");
    Ok(())
}

/// Which device a `usb-storage: <bdf> slot <n> transport broke …` line is about.
///
/// **Refused rather than widened if the line stops naming one.** A count of
/// broken transports is evidence about a disk, and a machine that boots off USB
/// always has at least two: the answer to "how many times did *this* disk's
/// transport break" is not recoverable from a line that does not say which disk
/// it was, and matching every disk's line instead is how this test came to red
/// on a boot stick's own clean recovery.
fn broke_on(line: &str) -> Result<&str, String> {
    line.split_once("usb-storage: ")
        .and_then(|(_, rest)| rest.split_once(" transport broke"))
        .map(|(who, _)| who)
        .ok_or_else(|| {
            format!("{line:?} does not name the device whose transport broke, so nothing can \
                    count that device's breaks apart from another's")
        })
}

/// The stick the machine booted from, pulled while the desktop is up.
///
/// **The instrument for #152, and the reason it exists is that the failure has
/// no other witness.** `/log` is on the stick, so the recording of the event
/// dies with the event; the machine has no serial port; it is not a panic, so
/// the on-screen console never paints; and Ctrl+Alt+D answers nothing. Three
/// investigations ran on the owner's description alone.
///
/// The pull is `device_del` on the boot stick, which had no device id until
/// this gate needed one — every earlier unplug test names a *data* disk, and a
/// data disk carries neither `/boot` nor `/log` nor the mount the log sink
/// writes through. That difference is the whole scenario.
///
/// The liveness signal is `compositor: frames=`: it comes from a composited frame, so
/// its absence is a desktop that stopped drawing rather than an instrument that
/// stopped counting — which is exactly what the owner reports, a clock that
/// stops advancing. The second probe is the serial console, which reaches
/// userland through a different path: `run` makes test-runner print a line and
/// then walk the VFS looking for a binary.
///
/// **A green run does not certify the machine survives an unplug.** It
/// certifies that this shape of unplug, on this emulated controller, leaves the
/// guest drawing and answering. What it *is* good for is red: a red here is the
/// first reproduction of the owner's freeze anywhere but his desk.
pub fn usb_boot_stick_pulled() -> Result<(), String> {
    /// Probes sent before the pull, and after it, each once the one before it
    /// was answered. The drumbeat is the liveness signal as well as the load:
    /// each one is a userland `println!` into the ring the log sink drains to
    /// the stick, and a VFS walk for a binary that is not there. A machine that
    /// pauses while the driver tears the port down and then carries on answers
    /// every one; one that never carries on is what the ceiling reds.
    const BEFORE: usize = 12;
    const AFTER: usize = 40;

    // metalcase's machine shape with `/system/bin/logd` rotating at 256 bytes rather
    // than a mebibyte, so the log writer is not just appending when the device
    // goes: every few probes it creates a file, sweeps the volume, deletes the
    // oldest and syncs the mount. That is FAT allocation and directory writes
    // in flight at the moment of the pull, which is the state the owner's
    // machine is in and the one a quiet idle desktop never reaches. It was a
    // kernel parameter until L6 and is a manifest row now, because the writer
    // is a userland program.
    let config = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/logrotatecase");
    let options = BootOptions {
        profile: Profile::Metal,
        qmp: true,
        // The T14's core count. How many CPUs are in the idle loop when the
        // device goes is the whole question on one hypothesis.
        smp: 8,
        ..Default::default()
    };
    let argv = qemu::profile_argv(&options);
    if !argv.iter().any(|a| a.contains(&format!("id={}", qemu::BOOT_STICK_ID))) {
        return Err(format!("the boot stick has no device id, so it cannot be pulled: {argv:?}"));
    }

    let mut qemu = QemuInstance::boot_with_options(&config, &[], &[], options);
    let socket = qemu.qmp_socket().to_path_buf();
    let mut console = qemu.boot_log().to_string();
    let frames = |text: &str| text.matches("compositor: frames=").count();

    // The machine has to be drawing before it is asked to survive anything, or
    // a green run is a boot that never started.
    qemu::await_guest(&mut qemu, &mut console, "the compositor's first frame", |c| frames(c) >= 1)
        .map_err(|why| format!("{why}\n{console}"))?;

    // And it has to be writing to the stick, or the pull is a disconnect with
    // nothing in flight — which is not the state the owner's machine is in.
    // `/system/bin/logd` names the file it opened on `/log`, and that volume is on the
    // device this test is about to take away.
    if !console.contains("logd: this boot's kernel log is /log/") {
        return Err(format!(
            "logd opened no file on /log, so the stick is not being written to and this gate \
             stages nothing:\n{console}"
        ));
    }

    /// Every probe answered, and the compositor two frame batches on, or the
    /// ceiling's red with QEMU's account of the vCPUs.
    fn drumbeat(
        qemu: &mut QemuInstance,
        console: &mut String,
        probes: std::ops::Range<usize>,
        after: &str,
    ) -> Result<(), String> {
        let frames = |text: &str| text.matches("compositor: frames=").count();
        let from = console.len();
        for i in probes {
            if let Err(why) = absent_probe(qemu, console, &format!("pull-probe-{i}"), after) {
                let report = crate::freeze_report(qemu, console);
                return Err(format!("{why}\n{console}\n{report}"));
            }
        }
        if let Err(why) = qemu::await_guest(qemu, console, &format!("two frame batches {after}"), |c| {
            frames(&c[from..]) >= 2
        }) {
            let report = crate::freeze_report(qemu, console);
            return Err(format!("{why}\n{console}\n{report}"));
        }
        Ok(())
    }

    drumbeat(&mut qemu, &mut console, 0..BEFORE, "before the pull")?;
    // The rotation actually ran, so "the writer was busy" is a fact rather than
    // a manifest row that might have been dropped.
    if !console.contains("logd: /log/") || !console.contains("and this boot continues in") {
        return Err(format!(
            "logd never rotated, so the pull below lands on a writer that is only \
             appending:\n{console}"
        ));
    }

    let mut devices = qemu::QmpDevices::open(&socket);
    devices.del(qemu::BOOT_STICK_ID);
    drop(devices);
    drumbeat(&mut qemu, &mut console, BEFORE..BEFORE + AFTER, "after the boot stick was pulled")?;

    // And the same stick put back. The owner reports the freeze from a replug
    // as well as from a pull, and the two are different states: a replug binds
    // a new disk under mounts that still name the old one.
    let replug = test_dir().join("usb-replug.img");
    drop(sparse(&replug, 512 * 1024 * 1024));
    let mut devices = qemu::QmpDevices::open(&socket);
    devices.blockdev_add("replug", &replug);
    devices.add("usb-storage", "xhci.0", "replug0", &[("drive", "replug")]);
    drop(devices);
    drumbeat(
        &mut qemu,
        &mut console,
        BEFORE + AFTER..BEFORE + 2 * AFTER,
        "after a stick went back into the port the boot stick was pulled from",
    )?;

    for bad in ["PANIC:", "panicked at"] {
        if console.contains(bad) {
            return Err(format!("{bad:?} after the boot stick was pulled\n{console}"));
        }
    }
    let _ = std::fs::remove_file(&replug);

    eprintln!(
        "  [usb] the boot stick was pulled out from under a running desktop with the log sink \
         rotating: all {AFTER} console probes answered and the desktop drew on after the pull, \
         and again after a stick went back in"
    );
    Ok(())
}
