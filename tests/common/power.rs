//! The two ways the machine stops, told apart by QEMU rather than by the guest:
//! a reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
//! status 0, so what is asserted is the cause its `SHUTDOWN` event names, and a
//! reboot implemented as a power-off reds on `guest-shutdown`. Nothing here
//! judges the boot after the reset: `-no-reboot` exits instead of taking it.

use std::io::Write;
use std::path::Path;
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

/// A boot with no host on the console runs its manifest's jobs and ends
/// itself, and the loader's own account of it is on the stick beside `logd`'s.
pub fn metal_job_reboot(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join("tests/jobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");

    // Built here, because a boot deletes the image it built and this one is read after the guest is gone.
    let image_path = super::lane::dir().join("jobcase-boot.img");
    let mut image = qemu::build_boot_image(case, &[], &[], &[]);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = super::volumes::log_extent(&image, &image_path)?;
    // A file under the loader's name that the last boot could have left: a
    // loader that opens without truncating ends in this one's tail.
    let stale = (bootlog::LOADER_LOG.to_string(), vec![b'x'; 64 * 1024]);
    super::volumes::stage_files(&mut image[start..start + len], &[stale])?;
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;

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
    // The control for `job_deadline_reboots`: a list that finishes inside the
    // bound is ended by its own last job and never by the deadline.
    drain.must_not_say(bootlog::JOB_DEADLINE_SAID)?;
    drain.must_say(REBOOTING)?;
    returned_to_firmware(reason, ASKED_AND_STAYED_UP, &tail)?;
    drop(qemu);

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

    let printed = loader_window(&console)?;
    let written = super::volumes::loader_log_lines(&image_path, start, len)?;
    // Compared byte for byte against a console the firmware rendered: a
    // character it has no glyph for is a line the two channels disagree about
    // on one machine and not on the next.
    if let Some(line) = written.iter().find(|line| !line.is_ascii()) {
        return Err(format!("the loader wrote {line:?}, which is not ASCII"));
    }
    if written != printed {
        return Err(format!(
            "{} carries {} line(s) and the loader printed {}\n--- on the stick\n{}\n--- on the \
             console\n{}",
            bootlog::LOADER_LOG,
            written.len(),
            printed.len(),
            written.join("\n"),
            printed.join("\n"),
        ));
    }

    let _ = std::fs::remove_file(&image_path);
    eprintln!(
        "  [power] {name} carries Boot: complete ({boot_ms}ms) and this boot's last line, and \
         {} carries the loader's {} lines beside it",
        bootlog::LOADER_LOG,
        written.len()
    );
    Ok(())
}

/// A job list that never finishes ends the boot anyway, on the runner's own
/// deadline: the kernel is alive and its scheduler passes keep feeding the
/// chipset, so no watchdog is what fires here.
pub fn job_deadline_reboots(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join("tests/jobdeadlinecase/system.toml");
    let case = config.parent().expect("system.toml has a directory");

    let mut qemu = QemuInstance::boot_with_options(
        case,
        &[],
        &[],
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() },
    );
    serial::Serial::boot(&qemu).must_be_clean()?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(WAIT));
    let reason = stop.reason();
    let tail = qemu.drain_serial(WAIT);

    let drain = serial::Serial::named("deadline drain", tail.as_str());
    drain.must_be_clean()?;
    drain.must_say("===TEST_START spin===")?;
    // A deadline that fired without naming the job it was inside answers
    // nothing to whoever reads the console afterwards.
    drain.must_say(bootlog::JOB_DEADLINE_SAID)?;
    drain.must_say("spin was running")?;
    drain.must_say(REBOOTING)?;
    returned_to_firmware(reason, ASKED_AND_STAYED_UP, &tail)?;

    eprintln!("  [power] the job list did not finish and the runner ended the boot itself");
    Ok(())
}

/// Everything the loader printed on the console, its first line to its last.
fn loader_window(console: &str) -> Result<Vec<String>, String> {
    let lines: Vec<&str> = console.lines().collect();
    let at = |line: &str| {
        lines
            .iter()
            .position(|seen| seen.contains(line))
            .ok_or_else(|| format!("the loader never printed {line:?} on the console"))
    };
    let (first, last) = (at(bootlog::LOADER_FIRST_LINE)?, at(bootlog::LOADER_LAST_LINE)?);
    if last < first {
        return Err(format!(
            "the console carries {:?} before {:?}, so there is no window between them",
            bootlog::LOADER_LAST_LINE,
            bootlog::LOADER_FIRST_LINE
        ));
    }
    Ok(lines[first..=last].iter().map(|line| (*line).to_string()).collect())
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

/// The kernel's read-back above its own arm, in
/// `kernel/src/drivers/watchdog.rs`: whole clauses, one per branch.
const ARMED_ON_ARRIVAL: &str = "so the bootloader had already armed the timer";
/// Unreachable from this suite: every guest that reaches the kernel's arm
/// passed the parameter, and the loader read the same one first.
const UNARMED_ON_ARRIVAL: &str = "so nothing had armed the timer";

/// The tail of the loader's own arm line, which names the shipped bound and so
/// tells it from the kernel's, whatever the port and the PCI ids turn out to be.
fn loader_armed() -> String {
    format!(
        "armed for {}ms, and the kernel takes it over",
        toyos_tco::bound_of(toyos_tco::TIMER)
    )
}

/// The register value the loader wrote, as the kernel reports finding it.
fn armed_on_arrival() -> String {
    format!("TCO_TMR={} on arrival, {ARMED_ON_ARRIVAL}", toyos_tco::TIMER)
}

/// The loader arms the chipset's watchdog before it jumps, so the handoff is
/// inside the bound.
///
/// What the kernel's read-back must report is the register value the loader
/// wrote, never merely a running timer: `TCO_TMR_HLT` is clear out of reset on
/// q35.
pub fn loader_watchdog_arms(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let armed = BootOptions {
        profile: qemu::Profile::Metal,
        kernel_params: &[toyos_tco::PARAM],
        ..Default::default()
    };
    let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, armed);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    let line = boot.must_say(&loader_armed())?.to_string();
    boot.must_say(&armed_on_arrival())?;
    drop(qemu);

    let idle = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::Metal, ..Default::default() },
    );
    let quiet = serial::Serial::boot(&idle);
    quiet.must_be_clean()?;
    quiet.must_not_say(&loader_armed())?;
    quiet.must_not_say(ARMED_ON_ARRIVAL)?;
    quiet.must_not_say(UNARMED_ON_ARRIVAL)?;
    drop(idle);

    eprintln!("  [power] the loader armed it and the kernel found it running: {}", line.trim());
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
///
/// The tail is what makes it the kernel's: on every guest that passes the
/// parameter the loader prints the same port and a `TCO_TMR=` of its own, which
/// the head of this line cannot be told from.
const ARMED: &str =
    "watchdog: 8086:2918 TCO at 0x660 TCO_TMR=2 — this machine resets if no scheduler pass runs \
     for 2400ms";

const FED_FOR: Duration = Duration::from_secs(20);
