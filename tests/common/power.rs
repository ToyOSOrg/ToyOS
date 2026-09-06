//! The two ways the machine stops, told apart by QEMU rather than by the guest:
//! a reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
//! status 0, so what is asserted is the cause its `SHUTDOWN` event names, and a
//! reboot implemented as a power-off reds on `guest-shutdown`. Nothing here
//! judges the boot after the reset: `-no-reboot` exits instead of taking it.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use toyos_build::bootlog::{self, REBOOTING};

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

const WAIT: Duration = Duration::from_secs(20);

/// What a guest that never stopped means where something asked it to.
const ASKED_AND_STAYED_UP: &str =
    "QEMU never reported stopping: the guest asked for a reboot and stayed up";

/// QEMU calls a reset-register write `guest-reset` and ACPI S5 `guest-shutdown`,
/// which the console cannot tell apart. `never` is what a guest that did not
/// stop at all means to the caller, which is not the same thing twice.
fn returned_to_firmware(reason: Option<String>, never: &str, tail: &str) -> Result<(), String> {
    match reason.as_deref() {
        Some("guest-reset") => Ok(()),
        Some(seen) => Err(format!(
            "QEMU stopped this guest for {seen:?}, not a guest reset: the machine was not \
             returned to firmware\n{tail}"
        )),
        None => Err(format!("{never}\n{tail}")),
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
    returned_to_firmware(reason, ASKED_AND_STAYED_UP, &tail)?;

    eprintln!("  [power] QEMU stopped the guest for guest-reset");
    Ok(())
}

/// What one jobcase boot leaves behind for a host that reads it afterwards.
pub struct Jobcase {
    pub image: PathBuf,
    /// The log partition's byte range inside [`Self::image`].
    pub log_at: (usize, usize),
    /// Everything the machine put on the serial line, firmware and loader
    /// included, up to the kernel's ready marker.
    pub console: String,
}

/// The jobcase on the metal profile, run until it hands the machine back to
/// firmware. `name` is the image's, because two guests may not share one;
/// `stage` is put on the log partition before the machine that reads it exists.
///
/// Built here, because a boot deletes the image it built and this one is read
/// after the guest is gone.
pub fn jobcase_reboot(name: &str, stage: &[(String, Vec<u8>)]) -> Result<Jobcase, String> {
    let config = super::compile::repo_root().join("tests/jobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");

    let image_path = super::lane::dir().join(name);
    let mut image = qemu::build_boot_image(case, &[], &[], &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let log_at = super::volumes::log_extent(&image, &image_path)?;
    if !stage.is_empty() {
        let (start, len) = log_at;
        super::volumes::stage_files(&mut image[start..start + len], stage)?;
        std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    }

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
    let console = qemu.boot_log().to_string();

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));
    let reason = stop.reason();
    let tail = qemu.drain_serial(WAIT);

    let drain = serial::Serial::named("job drain", tail.as_str());
    drain.must_be_clean()?;
    drain.must_say("===TEST_START reboot===")?;
    drain.must_say(REBOOTING)?;
    returned_to_firmware(reason, ASKED_AND_STAYED_UP, &tail)?;
    drop(qemu);
    Ok(Jobcase { image: image_path, log_at, console })
}

/// A boot with no host on the console runs its manifest's jobs and ends itself.
pub fn metal_job_reboot(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let boot = jobcase_reboot("jobcase-boot.img", &[])?;
    let image_path = boot.image;
    let (start, len) = boot.log_at;

    let (name, log) = super::volumes::newest_log(&image_path, start, len)?;
    let text = String::from_utf8_lossy(&log);
    // The volume is born clean in an image built moments ago, so every record in it is this boot's.
    // Judged by `bootlog`, because a T14 run judges the same volume by it.
    let boot_ms = bootlog::verdict(&text).map_err(|unfit| {
        format!(
            "{name}: {unfit}. A machine with no console would have no account of this \
             boot\n{text}"
        )
    })?;

    let _ = std::fs::remove_file(&image_path);
    eprintln!("  [power] {name} carries Boot: complete ({boot_ms}ms) and this boot's last line");
    Ok(())
}

/// The chipset resets a machine whose kernel stops feeding its watchdog, and
/// `watchdog_fed` is the same guest with the feed on. Starvation begins well
/// after boot, so what is measured is a reset inside this guest's own scaled
/// ceiling once it has, never an arm-to-ready race.
pub fn watchdog_resets(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, starved());
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    boot.must_say(ARMED)?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));
    let reason = stop.reason();
    let tail = qemu.drain_serial(WAIT);

    returned_to_firmware(reason, "the chipset never reset a guest that stopped feeding it", &tail)?;

    eprintln!("  [power] the chipset reset a guest that stopped feeding it");
    Ok(())
}

/// The control: the same guest, feeding, runs past the bound and is still there.
pub fn watchdog_fed(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions { kernel_params: &["watchdog", "tco-fast"], ..starved() };
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    // Without this the control cannot tell a fed watchdog from none at all.
    boot.must_say(ARMED)?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), FED_FOR);
    if let Some(seen) = stop.reason() {
        let tail = qemu.drain_serial(WAIT);
        return Err(format!(
            "QEMU stopped a guest that was feeding its watchdog, for {seen:?}\n{tail}"
        ));
    }

    let result = qemu.run_test("pwd", Duration::from_secs(30));
    if result.exit_code != Some(0) {
        return Err(format!("the guest stopped answering after {FED_FOR:?}: {result:?}"));
    }

    eprintln!("  [power] a fed guest ran {FED_FOR:?}, several bounds, and still answers");
    Ok(())
}

fn starved() -> BootOptions {
    BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        kernel_params: &["watchdog", "tco-fast", "tco-starve"],
        ..Default::default()
    }
}

/// The line `arm` logs on q35 at the fast bound; both tests demand it first.
const ARMED: &str = "watchdog: 8086:2918 TCO at 0x660 TCO_TMR=2";

const FED_FOR: Duration = Duration::from_secs(20);
