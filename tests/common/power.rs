//! The two ways the machine stops, told apart by QEMU rather than by the guest:
//! a reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
//! status 0, so what is asserted is the cause its `SHUTDOWN` event names, and a
//! reboot implemented as a power-off reds on `guest-shutdown`. Nothing here
//! judges the boot after the reset: `-no-reboot` exits instead of taking it.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

const WAIT: Duration = Duration::from_secs(20);

pub const REBOOTING: &str = "Rebooting.";

/// QEMU calls a reset-register write `guest-reset` and ACPI S5 `guest-shutdown`, which the console cannot tell apart.
fn returned_to_firmware(reason: Option<String>, tail: &str) -> Result<(), String> {
    match reason.as_deref() {
        Some("guest-reset") => Ok(()),
        Some(seen) => Err(format!(
            "QEMU stopped this guest for {seen:?}, not a guest reset: the machine was not \
             returned to firmware\n{tail}"
        )),
        None => Err(format!(
            "QEMU never reported stopping: the guest asked for a reboot and stayed up\n{tail}"
        )),
    }
}

/// The machine returns to firmware when a process holding `POWER` asks it to.
pub fn machine_reboot(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions { qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);

    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    // A decode this kernel got wrong, never one it bypassed: a kernel writing
    // 0xcf9 without reading the FADT satisfies this and the stop reason both.
    boot.must_say("ACPI: reset register SystemIO 0xcf9 <- 0x0f")?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));

    writeln!(qemu.stdin_mut(), "run reboot").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let reason = stop.reason();
    // Ends when QEMU exits and the reader disconnects, so a guest that came back to firmware pays none of this.
    let tail = qemu.drain_serial(WAIT);

    let drain = serial::Serial::named("reboot drain", tail.as_str());
    drain.must_be_clean()?;
    drain.must_say(REBOOTING)?;
    returned_to_firmware(reason, &tail)?;

    eprintln!("  [power] QEMU stopped the guest for guest-reset");
    Ok(())
}

/// A boot with no host on the console runs its manifest's jobs and ends itself.
/// `Rebooting.` reaching the log partition is the assertion the T14 rests on.
pub fn metal_job_reboot(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join("tests/jobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");

    // Built here, because a boot deletes the image it built and this one is read after the guest is gone.
    let image_path = super::lane::dir().join("jobcase-boot.img");
    let image = qemu::build_boot_image(case, &[], &[], &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = super::volumes::log_extent(&image, &image_path)?;

    let mut qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions {
            profile: qemu::Profile::Metal,
            qmp: true,
            boot_image: Some(image_path.clone()),
            ..Default::default()
        },
    );
    serial::Serial::boot(&qemu).must_be_clean()?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));
    let reason = stop.reason();
    let tail = qemu.drain_serial(WAIT);
    let drain = serial::Serial::named("job drain", tail.as_str());
    drain.must_be_clean()?;
    drain.must_say("===TEST_START reboot===")?;
    drain.must_say(REBOOTING)?;
    returned_to_firmware(reason, &tail)?;
    drop(qemu);

    let (name, log) = super::volumes::newest_log(&image_path, start, len)?;
    let text = String::from_utf8_lossy(&log);
    // The volume is born clean in an image built moments ago, so every record in it is this boot's.
    for record in ["Boot: complete", REBOOTING] {
        if !text.contains(record) {
            return Err(format!(
                "{record:?} is not in {name} on the log partition: the reset outran logd, so a \
                 machine with no console would have no account of this boot\n{text}"
            ));
        }
    }

    let _ = std::fs::remove_file(&image_path);
    eprintln!("  [power] {name} carries this boot's last line, written before the reset");
    Ok(())
}
