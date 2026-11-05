use std::io::Write;
use std::path::Path;

use toyos_build::bootlog;

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The kernel's last word when it powers the machine off, in
/// `kernel/src/syscall/machine.rs`.
pub const SHUTTING_DOWN: &str = "Shutting down.";

/// What the kernel logs once the `acpi` claim's holder has supplied the
/// power-off's sleep type (`kernel/src/arch/x86_64/power.rs`), whole to the
/// value, on q35: QEMU's PM1a control block, and the `SLP_TYPa` its DSDT's
/// `\_S5` names, which is the one its ICH9 powers off on.
const Q35_S5_SUPPLIED: &str = "power: S5 is PM1a 0x604 with SLP_TYPa=0,";

/// What the kernel logs at the `acpi` claim on a machine its firmware handed
/// over in ACPI mode (`kernel/src/arch/x86_64/acpi_mode.rs`), as OVMF does
/// q35: the enable and its wait are the T14's to exercise (`acpi_mode`).
const HANDED_OVER_IN_ACPI_MODE: &str = "acpi: the firmware handed this machine over in ACPI mode, so nothing is written";

/// Wait for the boot's `last` word on a guest asked to end, then for QEMU to
/// stop for `reason` and exit; `console` gains everything said on the way.
///
/// A reset, a power-off and a triple fault all end a `-no-reboot` QEMU with
/// status 0, so the cause is the one its `SHUTDOWN` event names. `stop` is
/// opened before the guest is asked: QMP delivers no event emitted before its
/// client connected.
pub fn ended(
    qemu: &mut QemuInstance,
    stop: &mut qemu::QmpShutdown,
    console: &mut String,
    last: &str,
    reason: &str,
) -> Result<(), String> {
    qemu::await_marker(qemu, console, last, "the boot's last word")?;
    let stopped = stop.reason();
    let by = qemu.budget(qemu::GUEST_QUIET);
    console.push_str(&qemu.await_exit(by)?);
    if stopped.as_deref() != Some(reason) {
        return Err(format!("QEMU stopped this guest for {stopped:?}, not {reason:?}\n{console}"));
    }
    Ok(())
}

/// A stop that ends with a userland thread still running is followed by a
/// power-off all the same, with the sleep type `acpiserver` handed the
/// kernel: q35 hands over in ACPI mode, so the power-off
/// quiets the events `acpiserver` enabled, and QEMU stops for
/// `guest-shutdown`. One CPU and `stop-budget-spent`, so the stop's one sweep
/// finds `stop_short`'s spinner queued behind the stop's caller.
pub fn machine_shutdown_short_stop(test_config: &Path) -> Result<(), String> {
    let short = qemu::build_toyos_bin(
        qemu::SUITE_ARCH,
        &super::compile::repo_root().join("tests/guest/rust-tests"),
        "stop_short",
    );
    let options = BootOptions {
        qmp: true,
        smp: 1,
        kernel_params: &["stop-budget-spent"],
        extra_root_files: vec![("bin/test_rs_stop_short".to_string(), short)],
        ..Default::default()
    };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    let mut console = boot.text().to_string();
    // A wait each: the server's line reaches the console through `logkeeper`
    // and the kernel's record does not, so a capture that ends at either need
    // not hold the other.
    qemu::await_marker(&mut qemu, &mut console, ACPI_ARMED, "the ACPI server arming")?;
    qemu::await_marker(&mut qemu, &mut console, Q35_S5_SUPPLIED, "the ACPI server to hand the kernel \\_S5")?;
    // The kernel's own record, committed before the one just waited for.
    serial::Serial::named("boot", console.clone()).must_say(HANDED_OVER_IN_ACPI_MODE)?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    writeln!(qemu.stdin_mut(), "run test_rs_stop_short").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let asked_at = console.len();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    let after = serial::Serial::named("the short stop", console[asked_at..].to_string());
    after.must_be_clean()?;
    let record = after
        .text()
        .lines()
        .find_map(toyos_quiesce::Record::parse)
        .ok_or_else(|| format!("no stop record:\n{}", after.text()))?;
    if record.stopped_the_machine() {
        return Err(format!("the stop left no thread running, so this boot staged no short stop: {record}"));
    }
    eprintln!("  [power] a short stop, then the power-off: {record}");
    Ok(())
}

/// The loader's lines about the proof, in `loader/src/proof.rs`.
const NO_STOP_MARK: &str = "Proof: the last boot left no stop mark, so it proves no image";
const STOPPED_CLEANLY: &str = "stopped cleanly";
const FLOOR_RAISED: &str = "by the boot that proved it";

/// A boot that stops cleanly proves its image: `logkeeper` marks the log
/// partition at the stop's flush, and the next loader pass takes the mark and
/// raises the anti-rollback floor to the image's signed version. The first
/// pass, on a fresh image, has no mark and raises nothing.
pub fn a_clean_stop_proves_its_image(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { profile: qemu::Profile::Headless, qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let first = serial::Serial::named("the first loader pass", qemu.loader_log());
    first.must_say(NO_STOP_MARK)?;
    says_nothing_of(&first, FLOOR_RAISED)?;
    let mut console = qemu.boot_log().to_string();
    qemu.reset_on_reboot();
    let asked_at = console.len();
    writeln!(qemu.stdin_mut(), "run reboot").expect("write to QEMU stdin");
    qemu.flush_stdin();
    qemu::await_marker_new(&mut qemu, &mut console, bootlog::REBOOTING, asked_at, "the clean stop")?;
    let rebooted_at = console.len();
    qemu::await_marker_new(&mut qemu, &mut console, bootlog::COMPLETE, rebooted_at, "the next boot")?;
    // The second pass is on the console where the loader writes the console,
    // and otherwise the 16550's file's second.
    let after = &console[asked_at..];
    let uart = qemu.loader_log();
    let first_in_uart = uart.find(bootlog::LOADER_FIRST_LINE);
    let next = match after.find(bootlog::LOADER_FIRST_LINE) {
        Some(at) => after[at..].to_string(),
        None => match uart.rfind(bootlog::LOADER_FIRST_LINE).filter(|at| Some(*at) != first_in_uart) {
            Some(at) => uart[at..].to_string(),
            None => return Err(format!("no loader pass after the reboot:\n{uart}\n{after}")),
        },
    };
    let next = serial::Serial::named("the loader pass after the reboot", next);
    let proved = next.must_say(STOPPED_CLEANLY)?.to_string();
    let raised = next.must_say(FLOOR_RAISED)?;
    eprintln!("  [power] {}", proved.trim());
    eprintln!("  [power] {}", raised.trim());
    Ok(())
}

/// The kernel's read-back above its own arm, in
/// `kernel/src/arch/x86_64/watchdog.rs`: whole clauses, one per branch.
const ARMED_ON_ARRIVAL: &str = "so the bootloader had already armed the timer";
/// Unreachable from this suite: every guest that reaches the kernel's arm
/// passed the parameter, and the loader read the same one first.
const UNARMED_ON_ARRIVAL: &str = "so nothing had armed the timer";

/// The tail of the loader's own arm line, which names the shipped bound and so
/// tells it from the kernel's, whatever the chipset and its PCI ids turn out to be.
fn loader_armed() -> String {
    format!("armed for {}ms, and the kernel takes it over", toyos_tco::BOUND_MS)
}

/// The kernel's line once it feeds the timer, whichever chipset it is.
fn kernel_feeds() -> String {
    format!("this machine resets if no scheduler pass runs for {}ms", toyos_tco::BOUND_MS)
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

/// The decimal digit printed straight after `label`, for the decoded bits the
/// read-back names one by one.
fn decimal_field(line: &str, label: &str) -> Result<u64, String> {
    let rest = line.split_once(label).ok_or_else(|| format!("no {label:?} in {line:?}"))?.1;
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().map_err(|e| format!("{label:?} in {line:?}: {e}"))
}

/// Each decoded bit `fields` names reads its value in `line`.
fn bits_are(line: &str, fields: &[(&str, u64)]) -> Result<(), String> {
    for (field, want) in fields {
        let seen = decimal_field(line, field)?;
        if seen != *want {
            return Err(format!("the loader read {field}{seen} back and this machine is {want}\n{line}"));
        }
    }
    Ok(())
}

/// The armed boot's half: the loader wrote the chipset's watchdog and the
/// kernel found it running and feeds it.
pub fn watchdog_armed(
    loader: &serial::Serial,
    kernel: &serial::Serial,
) -> Result<(), String> {
    let line = loader.must_say(&loader_armed())?.to_string();
    if line.contains(" FCH watchdog at ") {
        fch_armed(loader)?;
    } else {
        tco_armed(loader)?;
    }
    let arrival = kernel.must_say(ARMED_ON_ARRIVAL)?;
    eprintln!("  [power] {}", arrival.trim());
    kernel.must_say(&kernel_feeds())?;
    eprintln!("  [power] the loader armed it and the kernel found it running: {}", line.trim());
    Ok(())
}

/// **`TCO1_CNT.TCO_LOCK` is not judged.** `toyos_tco`'s own header names it as
/// the one bit a read-back may not be held to — firmware that set it leaves it
/// set through every write this tree makes, and what it gates is `SMI_EN.TCO_EN`
/// rather than the countdown or the reboot. It is reported and passed over.
fn tco_armed(loader: &serial::Serial) -> Result<(), String> {
    // The words, not the line: a read-back that printed a register the loader
    // never wrote would satisfy the line and is the failure worth catching.
    let read_back = loader.must_say("watchdog: read back TCO_RLD=")?.to_string();
    let tmr = hex_field(&read_back, "TCO_TMR=")?;
    if tmr != u64::from(toyos_tco::TIMER) {
        return Err(format!(
            "the loader read TCO_TMR={tmr} back from the chipset and wrote {}\n{read_back}",
            toyos_tco::TIMER
        ));
    }
    // The reset gate is inside this block on the generations the table names, so
    // a machine whose armed timer could not reset it is one the bound is a lie on.
    let gates = loader.must_say("so a second expiry can reset this machine")?.to_string();
    // This is the register block's own account of whether an expiry reaches
    // the machine, and a machine that has just armed the timer has not expired once.
    bits_are(&gates, &[("no_reboot=", 0), ("timeout=", 0)])?;
    eprintln!("  [power] {}", gates.trim());
    eprintln!("  [power] tco_lock={}, which no read-back is judged on", decimal_field(&gates, "tco_lock=")?);
    Ok(())
}

/// The FCH's half: the decode and the tick took, the timer is counting the
/// bound to a reset of the CPUs, and nothing the PPR documents gates that.
/// `cold=`, `clock_gating_dis=` and `action_en=` are reported and not judged:
/// the first is firmware's policy over every reset, the second's effect on the
/// count is undocumented, and the third is reserved before Family 1Ah.
fn fch_armed(loader: &serial::Serial) -> Result<(), String> {
    use toyos_tco::fch;
    let read_back = loader.must_say("watchdog: read back PMx000=")?.to_string();
    let decode = hex_field(&read_back, "PMx000=")?;
    let tick = (decode >> 24) & 0xf;
    if decode & u64::from(fch::WATCHDOG_TMR_EN) == 0 || tick != fch::TICK as u64 {
        return Err(format!("the loader read PMx000={decode:#010x} back: no decode, or not the {} ms tick\n{read_back}", fch::TICK_MS));
    }
    let control = fch::Control(hex_field(&read_back, "WatchdogControl=")? as u32);
    if !control.running() || control.powers_off() {
        return Err(format!("the loader read {control:?} back, which is not counting to a reset\n{read_back}"));
    }
    // Read straight after the trigger: the count written, or one tick under it.
    let count = decimal_field(&read_back, "WatchdogCount=")?;
    let timer = u64::from(fch::TIMER);
    if !(timer - 1..=timer).contains(&count) {
        return Err(format!("the loader read WatchdogCount={count} back and wrote {timer}\n{read_back}"));
    }
    let gates = loader.must_say("so an expiry can reset this machine")?.to_string();
    bits_are(&gates, &[("power_off=", 0), ("use_cpu_rst=", 1)])?;
    eprintln!("  [power] {}", read_back.trim());
    eprintln!("  [power] {}", gates.trim());
    Ok(())
}

/// The control: a boot that did not name the parameter arms nothing, and the
/// kernel says nothing about a timer either way.
pub fn watchdog_quiet(
    loader: &serial::Serial,
    kernel: &serial::Serial,
) -> Result<(), String> {
    // The loader's channel said *something*, which is what makes the absence
    // below mean anything. Asked of the loader's own first line rather than
    // through `must_not_say`: on the log volume `loader.log` is a file of its own and
    // carries no kernel record for `Serial::alive` to find.
    loader.must_say(bootlog::LOADER_FIRST_LINE)?;
    says_nothing_of(loader, &loader_armed())?;
    kernel.must_not_say(ARMED_ON_ARRIVAL)?;
    kernel.must_not_say(UNARMED_ON_ARRIVAL)?;
    Ok(())
}

/// `must_not_say` on a channel whose liveness the caller has already
/// established, because `Serial::alive` asks for a kernel record and the
/// loader's own file on the log volume has none.
pub fn says_nothing_of(channel: &serial::Serial, needle: &str) -> Result<(), String> {
    match channel.text().lines().find(|l| l.contains(needle)) {
        Some(line) => Err(format!(
            "{needle:?} on a channel that should not have it: {line:?}\n{}",
            channel.text()
        )),
        None => Ok(()),
    }
}

/// The kernel decoded a reset register out of this machine's FADT and wrote it.
///
/// **The port is the machine's, not q35's.** What a boot on real hardware can
/// be held to is that the decode happened and named a register the kernel
/// writes — `ACPI: no reset register this kernel writes` is the other branch and
/// is a machine that cannot return itself to firmware at all.
pub fn reset_register_decoded(kernel: &serial::Serial) -> Result<(), String> {
    says_nothing_of(kernel, "ACPI: no reset register this kernel writes")?;
    let line = kernel.must_say("ACPI: reset register ")?;
    eprintln!("  [power] {}", line.trim());
    Ok(())
}

/// The loader found the firmware's memory-overwrite request and set it, so the
/// firmware overwrites memory before the boot after this one.
pub fn memory_overwrite_requested(loader: &serial::Serial) -> Result<(), String> {
    let line = loader.must_say("Memory overwrite: MemoryOverwriteRequestControl read ")?;
    if !line.contains("and is now [01]") {
        return Err(format!("the loader did not set the request: {}", line.trim()));
    }
    eprintln!("  [power] {}", line.trim());
    Ok(())
}

/// What `/system/bin/acpiserver` says once it serves q35's power button,
/// whole, before it loads the tables.
const ACPI_ARMED: &str = "acpiserver: armed: power button served";

/// [`STICK_LOAD_JOB`](super::metal::STICK_LOAD_JOB)'s boot: the job wrote the
/// log disk, and the boot's own deadline reset the machine while it did — the
/// driver held the volume to a boot record, no stop mark and the outside
/// judge's reading (`toyos_metal::ended_by_its_deadline`). The host booting
/// again off that disk is the loop's own half, and a readback is only there
/// once it did.
pub fn the_disk_survives_a_reset_under_load(back: &super::metal::Readback) -> Result<(), String> {
    if let Some(line) = bootlog::stopping_line(back.log().text()) {
        return Err(format!("the boot stopped, so no reset landed under the load: {line}"));
    }
    // What reached the medium: the load's lines in the file read back off the
    // log volume.
    let reached = super::metal::bytes_of_lines(back.log().text(), "stick_load: line ");
    if reached < 1 << 20 {
        return Err(format!("{reached} bytes of the load's lines reached the disk, under a mebibyte"));
    }
    eprintln!("  [power] {} KiB of the load's lines reached the disk before the reset", reached >> 10);
    Ok(())
}
