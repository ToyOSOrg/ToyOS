//! The two ways the machine stops, told apart by QEMU rather than by the guest:
//! a reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
//! status 0, so what is asserted is the cause its `SHUTDOWN` event names, and a
//! reboot implemented as a power-off reds on `guest-shutdown`.
//!
//! Two names here judge the boot *after* a reset instead, and pay for it:
//! `blackbox_panic_chain` and `blackbox_done_chain` set
//! `BootOptions::takes_the_reset`, which gives up the stop reason every other
//! test in this file judges by, because a page crossing a reset cannot be
//! observed from a QEMU that exits on one.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use toyos_blackbox::{PHYS, State};
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
    drain.must_say(&format!("{} spin", bootlog::JOB_DEADLINE_SAID))?;
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

/// The `0x…` word printed straight after `label`, so a register is compared as
/// a number and not as the spelling the loader happened to use for it.
fn hex_field(line: &str, label: &str) -> Result<u64, String> {
    let rest = line
        .split_once(label)
        .ok_or_else(|| format!("no {label:?} in {line:?}"))?
        .1
        .trim_start();
    let digits = rest
        .strip_prefix("0x")
        .ok_or_else(|| format!("{label:?} is not followed by a hex word in {line:?}"))?;
    let end = digits.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(digits.len());
    u64::from_str_radix(&digits[..end], 16).map_err(|e| format!("{label:?} in {line:?}: {e}"))
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
    // The words, not the line: a read-back that printed a register the loader
    // never wrote would satisfy the line and is the failure worth catching.
    let read_back = boot.must_say("watchdog: read back TCO_RLD=")?.to_string();
    let tmr = hex_field(&read_back, "TCO_TMR=")?;
    if tmr != u64::from(toyos_tco::TIMER) {
        return Err(format!(
            "the loader read TCO_TMR={tmr} back from the chipset and wrote {}\n{read_back}",
            toyos_tco::TIMER
        ));
    }
    // The reset gate is inside this block on the generations the table names, so
    // a guest whose armed timer could not reset it is one the bound is a lie on.
    boot.must_say("so a second expiry can reset this machine")?;
    // q35 is the positive control for the question a single read cannot answer:
    // an armed timer counts down. A T14 printing the other branch is a chipset
    // that never counts, not a loader that never armed.
    let counts = boot.must_say("so the timer counts")?.to_string();
    let from = hex_field(&counts, "TCO_RLD went ")?;
    let to = hex_field(&counts, "-> ")?;
    if to >= from {
        return Err(format!("an armed TCO went {from} -> {to} over one tick\n{counts}"));
    }
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
    eprintln!("  [power] {}", counts.trim());
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

/// The kernel's fast panic bound in seconds — `kernel/src/panic_reboot.rs`'s
/// `FAST_BOUND`, which `panic-reboot-fast` swaps in for the shipped minute.
///
/// A kernel constant does not cross into the harness, so it is written here and
/// then *read back*: [`panic_armed`] is the whole arm line including this
/// number, and both tests demand it before they judge anything. A bound that
/// moved in the kernel and not here reds on that line rather than on a stop
/// reason nobody could attribute.
const PANIC_FAST_SECS: u64 = 5;

/// The panic path's arm line, which is also this boot's ready marker: the guest
/// has stopped scheduling by the time it is printed, and it is the instant the
/// bound starts running.
const PANIC_ARMED_HEAD: &str = "panic: rebooting in";

fn panic_armed() -> String {
    format!("{PANIC_ARMED_HEAD} {PANIC_FAST_SECS} s unless a key is pressed, timed by ")
}

/// A guest whose kernel panicked and armed the bound. `Profile::Metal` for the
/// same reason `screen_pager_keys` needs it: QEMU routes injected keys to one
/// handler per device class, and this is the only GOP profile with an i8042 and
/// no `usb-kbd` to send them to instead.
fn panicked() -> BootOptions {
    BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        kernel_params: &["test-late-panic", "panic-reboot-fast"],
        ready_marker: PANIC_ARMED_HEAD,
        ..Default::default()
    }
}

/// What a panicked guest that never stopped means where the bound should have
/// ended it.
const PANICKED_AND_STAYED_UP: &str =
    "QEMU never reported stopping: nobody pressed a key and the panicked guest held its panel \
     anyway";

/// A panicked kernel nobody is at returns the machine to firmware itself.
/// [`panic_key_holds`] is the same guest with a key pressed inside the bound.
///
/// The verdict is QEMU's stop reason arriving inside the bound the arm line
/// names, which is what the budget below is: a reset that had to wait longer
/// than the bound plus what a reset costs is not this bound firing.
pub fn panic_reboots(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, panicked());
    let boot = serial::Serial::boot(&qemu);
    // Not `must_be_clean`: this boot panics on purpose, and the arm line is
    // what says the panic path — not something else — is holding the machine.
    let line = boot.must_say(&panic_armed())?.to_string();

    let budget = qemu.budget(Duration::from_secs(PANIC_FAST_SECS) + RESET_ALLOWANCE);
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), budget);
    let reason = stop.reason();
    // A guest that came back to firmware pays none of this: `-no-reboot` exits and the reader disconnects.
    let tail = qemu.drain_serial(WAIT);
    returned_to_firmware(reason, PANICKED_AND_STAYED_UP, &tail)?;

    let drain = serial::Serial::named("panic reboot drain", tail.as_str());
    drain.must_say(PANIC_REBOOTING)?;

    eprintln!("  [power] the panicked guest reset itself inside {budget:?} of: {}", line.trim());
    Ok(())
}

/// A panic inside `percpu::init_bsp`, one statement after it loads the IDT,
/// finds a reset register already decoded.
///
/// That is the window the owner's T14 stops in and the earliest point a panic is
/// reportable at all. The FADT's reset register used to be decoded at
/// `acpi::init_power`, hundreds of statements later, so a panic here said it had
/// "decoded no reset register to hand the machine back to firmware with" and
/// held the panel for a hand.
///
/// **The reset itself is not asserted here, and cannot be on this guest**: the
/// bound is carried in TSC cycles, and before `clock::init` those come from
/// CPUID leaves 15H/16H, which QEMU's model answers with zeros. So this guest
/// reaches the *other* held branch — no clock — and the machine the bound was
/// written for is the judge of the reset. What is asserted is the half QEMU can
/// see, which is the half that was broken: the register is decoded before the
/// panic, and the panic does not name it as missing.
/// `panic_reboots` covers the reset once the calibrated clock exists.
pub fn panic_before_peripherals_reboots(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions {
        kernel_params: &["test-panic-after-idt", "panic-reboot-fast"],
        ready_marker: PANIC_HELD_HEAD,
        ..panicked()
    };
    let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let boot = serial::Serial::boot(&qemu);

    // Ordering is the whole assertion: this line is what `init_power` used to
    // print long after the panic below.
    let decoded = boot.must_say("ACPI: reset register SystemIO")?.to_string();
    boot.must_say("EARLY PANIC: panicked at")?;
    let held = boot.must_say(PANIC_HELD_HEAD)?.to_string();
    // The one thing this branch removed. A guest reaching the other held branch
    // for the other reason must not be read as this one passing.
    boot.must_not_say("decoded no reset register")?;
    if !held.contains("states no TSC frequency") {
        return Err(format!(
            "the panel held for a reason this guest was not expected to reach\n{held}"
        ));
    }

    eprintln!("  [power] a panic inside init_bsp found {}", decoded.trim());
    Ok(())
}

/// The head of [`panic_reboot::arm`]'s other line — the machine holds. Kept
/// apart from [`PANIC_ARMED_HEAD`] there and here for the same reason.
///
/// [`panic_reboot::arm`]: kernel/src/panic_reboot.rs
const PANIC_HELD_HEAD: &str = "panic: holding this panel";

/// What the reset itself is allowed to cost on top of the bound: the flush the
/// reset path makes before it writes the register, and the host seeing QEMU's
/// event. Scaled by [`QemuInstance::budget`] at the call site.
const RESET_ALLOWANCE: Duration = Duration::from_secs(20);

/// The panic path's second line, written raw because the log is already drained
/// by then (`kernel/src/panic_reboot.rs`'s `reboot_now`).
const PANIC_REBOOTING: &str = "panic: no key inside the bound, so nobody is here";

/// The control on [`panic_reboots`]: the same guest with one key pressed inside
/// the bound holds its panel and is still there several bounds later.
///
/// The key is `a`, not a page key: what retires the bound is that somebody is at
/// the machine, and `screen_pager_keys` is where the pager's own two keys are judged.
pub fn panic_key_holds(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, panicked());
    let boot = serial::Serial::boot(&qemu);
    // Demanded before the key, so a control that never armed a bound cannot
    // pass by holding a panel nothing was counting down.
    boot.must_say(&panic_armed())?;

    let socket = qemu.qmp_socket().to_path_buf();
    qemu::qmp_send_keys(&socket, &[("a", true), ("a", false)]);

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(PANEL_HELD_FOR));
    if let Some(seen) = stop.reason() {
        let tail = qemu.drain_serial(WAIT);
        return Err(format!(
            "a key was pressed inside the bound and QEMU stopped this guest anyway, for \
             {seen:?}\n{tail}"
        ));
    }

    // One monitor per `-qmp` socket, so the shutdown watch is given up before
    // the screendump connects.
    drop(stop);
    // QEMU not exiting is not the claim in this test's name; the panel still
    // carrying the report is. The fill and not a line of it, because a key that
    // is not a page key leaves the pager unsteered and which page is up when the
    // dump is taken is nobody's to say.
    let fill = qemu.screendump().fill();
    if fill != FILL_FATAL {
        return Err(format!(
            "the guest is still up {PANEL_HELD_FOR:?} after the key, but its panel fills \
             {fill:?} and not the fatal {FILL_FATAL:?}: whatever it holds is not the report"
        ));
    }

    eprintln!("  [power] a key retired the bound and the report held the panel {PANEL_HELD_FOR:?}");
    Ok(())
}

/// What `panic_console`'s `Fill::Fatal` paints behind a report — the one thing
/// on that panel which does not depend on the page the pager has up.
const FILL_FATAL: [u8; 3] = [0x60, 0x00, 0x00];

/// Several bounds, so the control is not a race the guest won once.
const PANEL_HELD_FOR: Duration = Duration::from_secs(PANIC_FAST_SECS * 4);

/// The head the loader writes every line about the page under
/// (`bootloader/src/blackbox.rs`), and the line a harvested report goes under.
const BLACKBOX_HEAD: &str = "Black box:";
const PREVIOUS_PANIC: &str = "Previous boot's panic:";

/// The loader's last line on a pass that reads the page and boots no kernel
/// (`bootloader/src/loaderlog.rs`), which is also this test's drain predicate.
const ENDS_THE_CHAIN: &str =
    "Loader log: the last boot is accounted for, so this pass resets the machine";

/// The loader's last line on a pass that *does* boot one, which is what tells
/// a chain that ended from one that went round again.
const HANDS_OFF: &str = "Loader log: the kernel handoff begins, so this file ends here";

/// A line of the first boot's own report, which has to come back out of DRAM on
/// the boot after it: the panic's message, so what is recovered is the crash
/// and not merely a page that checksummed.
const BLACKBOX_WITNESS: &str = "test-late-panic: on-screen console check";

/// The earliest panic this tree can stage, inside `percpu::init_bsp` one
/// statement after the IDT is loaded — which is before `params::init`, and so
/// before everything the kernel used to learn the page's address from.
///
/// **It cannot drive the chain and that is not a choice**: the reboot bound is
/// carried in TSC cycles, and before `clock::init` those come off CPUID leaves
/// this guest's CPU answers with zeros, so the panic path holds the panel rather
/// than resetting (`issues/panic-path/the-panic-bounds-cpuid-clock-runs-on-no-guest-this-tree-boots.md`).
/// The seal is read off the page itself instead, which needs no reset at all.
const EARLY_WITNESS: &str = "test-panic-after-idt: the IDT is loaded and nothing else is up";

/// The first record `serial::init` writes, which is the first thing the kernel
/// does after taking the page.
const SERIAL_IS_UP: &str = "serial: 16550 loopback read";

/// The page armed and its address handed to the kernel, as the two sides say it.
fn armed_line() -> String {
    format!("{BLACKBOX_HEAD} {PHYS:#x} armed")
}

fn kernel_took_it() -> String {
    format!("black box: {PHYS:#x} is this boot's")
}

/// What the loader writes about a page that still read ARMED, which is a kernel
/// that reached neither of the two paths that write one.
fn armed_and_nothing_else() -> String {
    format!("{PREVIOUS_PANIC} the page still reads {}", State::Armed.named())
}

/// What it writes about a kernel that handed the machine back on purpose.
fn done_line() -> String {
    format!("the last boot read {}", State::Done.named())
}

/// The two-boot shape both chain judges use: a guest that takes its own reset,
/// so the loader pass after it is observable.
fn chained(params: &'static [&'static str]) -> BootOptions {
    BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        kernel_params: params,
        takes_the_reset: true,
        ready_marker: HANDS_OFF,
        ..Default::default()
    }
}

/// Resets a chain leaves behind: the kernel's own, and the pass that read the
/// page ending itself rather than returning to the boot manager.
const CHAIN_RESETS: usize = 2;

/// Both chain judges' second half: the pass after the reset said its piece and
/// then reset the machine itself.
///
/// **The reset is not decoration.** A UEFI application that returns leaves its
/// `SIGNAL_EXIT_BOOT_SERVICES` callback registered and is then unloaded, and the
/// next operating system's own `ExitBootServices` calls into that freed image —
/// measured on the owner's T14 as Ubuntu freezing in its EFI stub. Asked of QEMU
/// and not of the guest, because a guest that says it is about to reset is not a
/// guest that did.
fn ended_in_a_reset(resets: &mut qemu::QmpResets) -> Result<(), String> {
    let seen = resets.seen(CHAIN_RESETS);
    if seen < CHAIN_RESETS {
        return Err(format!(
            "QEMU reported {seen} guest reset(s) and this chain is {CHAIN_RESETS}: the pass that              read the page returned to the boot manager instead of resetting, which leaves this              image's exit-boot-services callback registered for the next operating system to              call into"
        ));
    }
    Ok(())
}

/// Every line the guest said after its first boot handed off, up to the second
/// pass's own last line.
fn after_the_reset(qemu: &mut QemuInstance, until: &str) -> serial::Serial {
    let until = until.to_string();
    let tail = qemu.drain_until(CHAIN_WAIT, move |line| line.contains(&until));
    serial::Serial::named("boot after the reset", tail.as_str())
}

/// What the boot after a reset has to arrive inside: the bound the first boot
/// counts down, plus firmware and a loader. Scaled by `drain_until`, and the
/// predicate is what ends the drain.
const CHAIN_WAIT: Duration = Duration::from_secs(PANIC_FAST_SECS + 60);

/// The chain closes on a panic: the kernel seals what the panel rendered, the
/// machine resets itself, and the boot after it is this loader again — which
/// reads the page, writes the report, and hands the machine back to firmware
/// rather than starting the same loop over.
///
/// The A/B is this guest's own two passes. QEMU zeroes a machine's RAM, so the
/// first pass must find no page at all, which is what stops a green run meaning
/// "the loader says that about every boot".
pub fn blackbox_panic_chain(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let params: &[&str] = &["test-late-panic", "panic-reboot-fast"];
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        chained(params),
    );
    // The capture ends at the loader's handoff line, so what it can carry is
    // the loader's own account; that the *kernel* took the page is
    // `blackbox_unclaimed_page`'s to say and is not restated here.
    let first = serial::Serial::boot(&qemu);
    // Opened before either reset: events queue on the socket from here.
    let mut resets = qemu::QmpResets::open(qemu.qmp_socket(), qemu.budget(CHAIN_WAIT));
    first.must_say(&armed_line())?;
    // Nothing was harvested on a machine whose RAM QEMU zeroed, so the pass
    // below is reading this boot's page and not a claim about every boot.
    if let Some(line) = first.text().lines().find(|l| l.contains(PREVIOUS_PANIC)) {
        return Err(format!(
            "the first pass of a machine with zeroed RAM reported a previous boot ({line:?}), so              the pass after the reset would say nothing\n{}",
            first.text()
        ));
    }

    let second = after_the_reset(&mut qemu, ENDS_THE_CHAIN);
    second.must_say(PREVIOUS_PANIC)?;
    // **After the harvest line, and that is the whole of the assertion.** This
    // capture begins at the first boot's handoff, so it carries that boot's own
    // panic on the console too — and a whole-capture scan for the witness was
    // satisfied by it, which let a kernel that sealed nothing pass. Only the
    // loader's `| ` lines come after the harvest line.
    second.must_say_after(PREVIOUS_PANIC, BLACKBOX_WITNESS)?;
    // The page read PANIC and not the state the loader itself put there, which
    // is what tells a report that crossed the reset from a kernel that vanished.
    second.must_not_say(&armed_and_nothing_else())?;
    second.must_say(ENDS_THE_CHAIN)?;
    // The chain ends rather than going round: a pass that booted a kernel would
    // have said so, and this one must not have.
    second.must_not_say(HANDS_OFF)?;
    ended_in_a_reset(&mut resets)?;
    drop(qemu);

    eprintln!(
        "  [power] the panic crossed the reset, and the pass that read it ended in a reset of \
         its own"
    );
    Ok(())
}

/// The other way a kernel ends, and the control on the name above: a boot that
/// hands the machine back on purpose seals DONE, and the loader pass after it
/// reports a deliberate stop rather than a death — then ends the chain too, so
/// the machine goes back to the firmware's own boot order.
pub fn blackbox_done_chain(
    _test_config: &Path,
    _c_bins: &[(String, Vec<u8>)],
    _rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let config = super::compile::repo_root().join("tests/jobcase/system.toml");
    let case = config.parent().expect("system.toml has a directory");
    let mut qemu = QemuInstance::boot_with_options(case, &[], &[], chained(&[]));
    let first = serial::Serial::boot(&qemu);
    let mut resets = qemu::QmpResets::open(qemu.qmp_socket(), qemu.budget(CHAIN_WAIT));
    first.must_say(&armed_line())?;

    let second = after_the_reset(&mut qemu, ENDS_THE_CHAIN);
    second.must_say(&done_line())?;
    // The distinction the whole state machine exists for: a deliberate stop is
    // not a panic and not a kernel that vanished.
    second.must_not_say(&armed_and_nothing_else())?;
    second.must_not_say(BLACKBOX_WITNESS)?;
    second.must_not_say(HANDS_OFF)?;
    ended_in_a_reset(&mut resets)?;
    drop(qemu);

    eprintln!("  [power] a deliberate reboot sealed DONE and the chain ended in a reset");
    Ok(())
}

/// The kernel side with no page under it: a boot whose loader claimed none says
/// so by name and writes nowhere.
///
/// **A control on the loader's claim, not on the feature.** Without it a green
/// chain says only that a claimed page works, never that a kernel handed no page
/// declines to write one — and a kernel that wrote anyway would be writing into
/// memory nothing reserved.
pub fn blackbox_unclaimed_page(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::Metal, ..Default::default() },
    );
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    // The loader claimed one on this guest, so what is judged here is the
    // *kernel's* reading of its own parameter line: the address it was given is
    // the address the loader printed, and nothing else in the line is a page.
    let claimed = boot.must_say(&armed_line())?.to_string();
    boot.must_say(&kernel_took_it())?;
    boot.must_say(&format!("blackbox={PHYS:#x}"))?;
    // **Before `serial::init`, and that ordering is the assertion.** The page
    // used to be taken after the console, the parameter line's UTF-8 check and
    // `params::init`, and the owner's laptop panicked before all three: it
    // rendered a panel and reset itself with the page still holding the loader's
    // `ARMED`. No staged panic can land in that window — arming one needs the
    // line parsed first — so what is judged is where the kernel says it took
    // the page, which moves the moment the reading moves.
    boot.must_say_after(&kernel_took_it(), SERIAL_IS_UP)?;
    drop(qemu);

    eprintln!("  [power] the loader named the page and the kernel took it: {}", claimed.trim());
    Ok(())
}

/// A panic earlier than everything the kernel used to learn the page's address
/// from seals it anyway — read out of the page's own bytes, by QEMU.
///
/// **What it judges is the seal, not the ordering.** No staged panic can land
/// before `params::init` — arming one requires that line to have been parsed —
/// so the earliest this tree can produce is inside `percpu::init_bsp`, which is
/// after it, and a kernel that took the page late would still seal here. That
/// the page is taken *before* `serial::init` is `blackbox_unclaimed_page`'s to
/// say, off the order of two records.
///
/// The oracle is `pmemsave`, which is QEMU reading its own guest's physical
/// memory — not the guest reporting on itself, and not a reset the guest cannot
/// perform here anyway. The bytes are then handed to the same `recover` the next
/// boot's loader would call, so what is judged is a page that loader would read.
pub fn blackbox_early_panic_sealed(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            qmp: true,
            kernel_params: &["test-panic-after-idt"],
            ready_marker: EARLY_WITNESS,
            ..Default::default()
        },
    );
    let boot = serial::Serial::boot(&qemu);
    // The kernel took the page before it panicked, which is the whole claim;
    // without this line the seal below could only have come from the loader.
    boot.must_say(&kernel_took_it())?;

    // **Polled, because the marker above precedes the seal rather than
    // following it.** The console line is the panic's *first* act and the seal
    // is one of its last — `render` seals inside the same call that paints — so
    // a single read here is a race the guest wins only while the host is quiet,
    // and a busy host loses it. Nothing else in this boot can write the page, so
    // waiting for `Panic` cannot pass for the wrong reason; a page that stays
    // `ARMED` for the whole bound is the defect this test is for.
    let (state, text) = sealed_state(&mut qemu, Duration::from_secs(10))?;
    if state != State::Panic {
        return Err(format!(
            "the page reads {} after a panic, so the panic path did not reach it and the next \
             boot would report a kernel that vanished",
            state.named()
        ));
    }
    let text = String::from_utf8_lossy(&text);
    if !text.contains(EARLY_WITNESS) {
        return Err(format!(
            "the page is sealed PANIC and does not carry {EARLY_WITNESS:?}, so what crossed is \
             not this crash\n{text}"
        ));
    }
    drop(qemu);

    eprintln!(
        "  [power] a panic before `params::init` sealed {} bytes, read off the page by QEMU",
        text.len()
    );
    Ok(())
}

/// Every exception seals its registers at the entry, and on a healthy boot the
/// page carries the last one — read off the page's own bytes by QEMU.
///
/// **This is the only judge of the entry seal, and it is a positive one.** A
/// fault whose report never runs is what the seal exists for, and no actuator
/// stages that: every fault this tree can arm reaches `halt_all_cpus`, which
/// paints and seals PANIC over the record. What is left is the ordinary case —
/// a boot takes demand page faults, the entry seals each, and the newest is on
/// the page until something overwrites it. That the record is a real one, with
/// this machine's vector and a canonical `rip`, is what says the entry wrote it.
///
/// The cache write-back the seal also does cannot be judged here at all: this
/// guest is TCG and has no caches for `CLFLUSH` to have anything to do
/// (`issues/panic-path/the-seals-cache-writeback-runs-on-no-guest-this-tree-boots.md`).
pub fn blackbox_fault_sealed(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions { profile: qemu::Profile::Metal, qmp: true, ..Default::default() },
    );
    serial::Serial::boot(&qemu).must_be_clean()?;

    let page = qemu.guest_page(PHYS)?;
    let page: &[u8; toyos_blackbox::BYTES] =
        page.as_slice().try_into().map_err(|_| "pmemsave returned the wrong length".to_string())?;
    let Some((state, text)) = toyos_blackbox::recover(page) else {
        return Err(format!("the page at {PHYS:#x} carries nothing: {:02x?}", &page[..32]))
    };
    if state != State::Fault {
        return Err(format!(
            "the page reads {} on a boot that took exceptions and neither panicked nor stopped, \
             so its exception entry sealed nothing",
            state.named()
        ));
    }
    let Some(fault) = toyos_blackbox::Fault::from_bytes(text) else {
        return Err(format!("the page is sealed FAULT and its {} bytes are not a record", text.len()))
    };
    // The vector this machine's boot ends on, named rather than ranged: a
    // record whose vector is whatever happened to be there says nothing about
    // whether the entry read the frame or a zeroed one.
    if fault.vector != PAGE_FAULT_VECTOR {
        return Err(format!(
            "the newest sealed fault is vector {} and a boot of this shape ends on \
             {PAGE_FAULT_VECTOR}: {fault:?}",
            fault.vector
        ));
    }
    // A `rip` of zero is a record that was never filled in, and one outside
    // both halves is a frame that was not read where the stub pushed it.
    if fault.rip == 0 || fault.cr3 == 0 {
        return Err(format!("the sealed record has an empty rip or cr3: {fault:?}"));
    }
    drop(qemu);

    eprintln!(
        "  [power] the exception entry sealed vector {} err={:#x} rip={:#018x} on the page",
        fault.vector, fault.error_code, fault.rip
    );
    Ok(())
}

/// `#PF`, which is the vector a boot of this shape ends its exceptions on:
/// demand paging is what a running machine faults for.
const PAGE_FAULT_VECTOR: u64 = 14;

/// The same early panic on a machine with no serial port at all: the panel and
/// the page are the only two channels there are, and both must carry it.
///
/// **The combination nothing else covers.** `blackbox_early_panic_sealed` is
/// this crash with a 16550 to report it on, and `screen_panic_muted` is a muted
/// guest whose panic is late — clock calibrated, `logd` running, the machine
/// released. The owner's laptop is neither: no serial port and a crash before
/// any of that, which is the arm where `halt_all_cpus`' waits have nothing left
/// to wait for and where `!has_console()` sends the panic path down branches no
/// other guest executes.
pub fn blackbox_early_panic_sealed_muted(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions {
        profile: qemu::Profile::Metal,
        qmp: true,
        mute: true,
        kernel_params: &["test-panic-after-idt"],
        ..Default::default()
    };
    // The muted profile is this test's whole premise, so it is checked and not assumed.
    let argv = qemu::profile_argv(&options);
    match argv.iter().position(|a| a == "-serial") {
        Some(i) if argv.get(i + 1).is_some_and(|v| v == "none") => {}
        _ => return Err(format!("the muted profile still has a 16550: {argv:?}")),
    }

    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    // Nothing announces it — there is no console for a marker to arrive on — so
    // the screen is polled. The bound covers firmware plus the root read off USB.
    let dump = qemu.screendump_until("PANIC:", Duration::from_secs(30));
    let text = dump.text();
    if !text.contains(EARLY_WITNESS) {
        return Err(format!(
            "the panel of a guest with no serial port does not carry {EARLY_WITNESS:?}, so the \
             fatal text reached neither channel\ndecoded screen:\n{text}"
        ));
    }
    // Nothing on this path may wait for a drainer that cannot run: before the
    // machine is released there is no `logd` and no scheduler to carry one, so
    // the budget must never be entered rather than entered and spent.
    if text.contains(LOG_DRAIN_EXPIRED) {
        return Err(format!(
            "the panel carries {LOG_DRAIN_EXPIRED:?} on a boot that crashed before the machine \
             was released, so the panic path spent a budget waiting for a drainer that could not \
             exist\ndecoded screen:\n{text}"
        ));
    }

    let (state, sealed) = sealed_state(&mut qemu, Duration::from_secs(10))?;
    if state != State::Panic {
        return Err(format!(
            "the page reads {} after a panic on a machine whose only other channel is the \
             panel\ndecoded screen:\n{text}",
            state.named()
        ));
    }
    let sealed = String::from_utf8_lossy(&sealed);
    if !sealed.contains(EARLY_WITNESS) {
        return Err(format!("the page is sealed PANIC without {EARLY_WITNESS:?}\n{sealed}"));
    }
    drop(qemu);

    eprintln!(
        "  [power] no serial port and a crash before the clock: the panel carries the report and \
         the page carries {} sealed bytes",
        sealed.len()
    );
    Ok(())
}

/// `kernel/src/arch/apic.rs`'s `LOG_DRAIN_EXPIRED`, which a boot that never had
/// a drainer may not print.
const LOG_DRAIN_EXPIRED: &str = "the report did not reach /log";

/// The black-box page's state once the guest has finished writing it, or what
/// it still read at the deadline.
///
/// The seal is one of the panic path's last acts and every console or panel
/// marker a test can wait on comes before it, so the page is polled rather than
/// read once. Only the panicking guest writes it, so a poll cannot observe a
/// state some other writer put there.
fn sealed_state(qemu: &mut QemuInstance, within: Duration) -> Result<(State, Vec<u8>), String> {
    let deadline = std::time::Instant::now() + within;
    let mut last = None;
    loop {
        let page = qemu.guest_page(PHYS)?;
        let page: &[u8; toyos_blackbox::BYTES] = page
            .as_slice()
            .try_into()
            .map_err(|_| "pmemsave returned the wrong length".to_string())?;
        match toyos_blackbox::recover(page) {
            Some((State::Panic, text)) => return Ok((State::Panic, text.to_vec())),
            Some((state, text)) => last = Some((state, text.to_vec())),
            None if last.is_none() => {
                last = None;
            }
            None => {}
        }
        if std::time::Instant::now() >= deadline {
            return match last {
                Some(seen) => Ok(seen),
                None => Err(format!(
                    "the page at {PHYS:#x} carried nothing the next boot's loader would read for \
                     {within:?} after a panic this kernel rendered"
                )),
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
