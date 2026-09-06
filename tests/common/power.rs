//! The two ways the machine stops, told apart by QEMU rather than by the guest:
//! a reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
//! status 0, so what is asserted is the cause its `SHUTDOWN` event names, and a
//! reboot implemented as a power-off reds on `guest-shutdown`.
//!
//! One name here judges the boot *after* the reset instead, and pays for it:
//! `panic_blackbox_survives` sets `BootOptions::takes_the_reset`, which gives up
//! the stop reason every other test in this file judges by, because a page
//! crossing a reset cannot be observed from a QEMU that exits on one.

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

/// The two lines the loader writes about the black-box page
/// (`bootloader/src/blackbox.rs`), which are also the two this pair judges.
const PREVIOUS_PANIC: &str = "Previous boot's panic:";
const BLACKBOX_RESERVED: &str = "Black box: reserved";

/// A line of the first boot's own report, which has to come back out of DRAM
/// on the boot after it. The panic's message, so what is recovered is the
/// crash and not merely a page that checksummed.
const BLACKBOX_WITNESS: &str = "test-late-panic: on-screen console check";

/// The guest that panics, resets, and comes back: the same arming as
/// [`panic_reboots`], because it is that reset this rides on.
const BLACKBOX_PARAMS: &[&str] = &["test-late-panic", "panic-reboot-fast"];

/// What the second boot has to arrive inside: the bound the first boot counts
/// down, plus firmware and a loader. Scaled at the call site — it is a liveness
/// ceiling on a guest, and the predicate is what ends the drain.
const REBOOT_WAIT: Duration = Duration::from_secs(PANIC_FAST_SECS + 60);

/// A panic survives the reset and reaches the stick: the kernel seals what the
/// panel rendered into a page of DRAM, the machine returns itself to firmware,
/// and the next boot's loader finds it there and writes it into `loader.log`.
///
/// The A/B is this guest's own two boots. QEMU starts a machine with its RAM
/// zeroed, so the first boot must find no page — which is what stops a green
/// run meaning "the loader says that about every boot".
pub fn panic_blackbox_survives(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    // Its own image, because the verdict is read off the stick once the guest
    // is gone, and the shared one is a lane's to reuse.
    let image_path = super::lane::dir().join("blackbox-boot.img");
    let image = qemu::build_boot_image(test_config, c_bins, rust_bins, BLACKBOX_PARAMS);
    std::fs::write(&image_path, &image).map_err(|e| format!("write the boot image: {e}"))?;
    let (start, len) = super::volumes::log_extent(&image, &image_path)?;

    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: qemu::Profile::Metal,
            boot_image: Some(image_path.clone()),
            kernel_params: BLACKBOX_PARAMS,
            // The one test in the tree that lets a guest take its own reset:
            // nothing about a page crossing one can be judged from a QEMU that
            // exits on it.
            takes_the_reset: true,
            ready_marker: PANIC_ARMED_HEAD,
            ..Default::default()
        },
    );

    let first = serial::Serial::boot(&qemu);
    first.must_say(BLACKBOX_RESERVED)?;
    // Not `must_not_say`: that one demands a live capture first, and this one
    // is a boot that panicked on purpose. The claim is only about this line.
    if let Some(line) = first.text().lines().find(|l| l.contains(PREVIOUS_PANIC)) {
        return Err(format!(
            "the first boot of a machine whose RAM QEMU zeroed reported a previous panic \
             ({line:?}), so the second boot's report would say nothing\n{}",
            first.text()
        ));
    }
    first.must_say(&panic_armed())?;

    // Ends on the *kernel* line of the boot after the reset, not on the loader
    // line this test is about. `println!` reaches the console before it reaches
    // the file, and the loader closes `loader.log` at the handoff — so a drain
    // that stopped at its own needle would kill the guest inside the window
    // between the two writes, and did: `loader.log` came back holding the
    // loader's first line and nothing else.
    let armed = blackbox_armed();
    let tail = qemu.drain_until(REBOOT_WAIT, |line| line.contains(&armed));
    let second = serial::Serial::named("boot after the reset", tail.as_str());
    second.must_say(PREVIOUS_PANIC)?;
    second.must_say(&armed)?;
    // Killed once that boot's loader has closed the file, and the stick read after.
    drop(qemu);

    let lines = super::volumes::loader_log_lines(&image_path, start, len)?;
    let log = lines.join("\n");
    for want in [PREVIOUS_PANIC, BLACKBOX_WITNESS] {
        if !log.contains(want) {
            return Err(format!(
                "{} on the stick does not carry {want:?}, so the machine with no console has \
                 no account of the boot that died\n{log}",
                toyos_build::bootlog::LOADER_LOG
            ));
        }
    }
    // The loader renders the recovered bytes in the panel's own alphabet, and
    // this file is compared byte for byte against a console elsewhere.
    if let Some(line) = lines.iter().find(|line| !line.is_ascii()) {
        return Err(format!("the recovered report put {line:?} in the log, which is not ASCII"));
    }

    let _ = std::fs::remove_file(&image_path);
    eprintln!(
        "  [power] the panic crossed the reset: {} carries {} lines of it",
        toyos_build::bootlog::LOADER_LOG,
        lines.iter().filter(|l| l.starts_with("| ")).count()
    );
    Ok(())
}

/// The control: an ordinary boot claims the page and reports it empty.
///
/// Without it a green judge says only that the loader can print a report it
/// found, never that a boot which crashed nowhere prints none — and a loader
/// that reported one unconditionally would satisfy the judge and be a lie on
/// every machine that never crashed.
pub fn panic_blackbox_clean_boot(
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
    let line = boot.must_say(BLACKBOX_RESERVED)?.to_string();
    boot.must_not_say(PREVIOUS_PANIC)?;
    // The other half of the claim, and the one the kernel makes: a page the
    // loader claimed is a page the kernel will write, said in the kernel's own
    // line rather than inferred from the loader's.
    boot.must_say(&blackbox_armed())?;
    drop(qemu);

    eprintln!("  [power] a boot that crashed nowhere reports no previous panic: {}", line.trim());
    Ok(())
}

/// The kernel's own line about the page (`kernel/src/blackbox.rs`), which says
/// the memory map carried the loader's claim into this boot. Derived from the
/// one constant all three binaries name, so a moved address reds here.
fn blackbox_armed() -> String {
    format!("black box: {:#x} is this boot's", toyos_blackbox::PHYS)
}
