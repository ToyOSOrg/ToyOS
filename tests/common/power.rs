use std::io::Write;
use std::path::Path;

use toyos_blackbox::{PHYS, State};
use toyos_build::bootlog::{self, REBOOTING};

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

/// What the stop alerts where the console's wire was kept through the stop's
/// whole budget, in `kernel/src/log/console.rs`.
const WIRE_KEPT: &str = "kept the wire through the stop's";

/// What both of the stop's alerts say where the wire's holder let it go only
/// past the stop's budget or not at all, in `kernel/src/log/console.rs`.
pub const WIRE_LATE: &str = "the wire through the stop's";

/// What a staging says where the `klogd` it woke had not taken the wire
/// `DEAF_CPU` into the stop's wait, with `klogd`'s state and the run queue, in
/// `kernel/src/log/console.rs`. Every staged test reds on it.
const STAGED_KLOGD_UNRUN: &str = "console: the staged klogd has not taken the wire";

/// What a stop staged by `wire-held-at-the-last-word` logs where it holds the
/// console's wire itself, in `kernel/src/log/console.rs`; where it does not,
/// `klogd` holds it inside its hold from here to the seal.
const STOP_HOLDS_AT_THE_LAST_WORD: &str = "console: the stop holds the wire at the boot's last word, staged";

/// What a stop staged by `wire-held-across-the-stop` logs once `klogd` holds
/// the console's wire, in `kernel/src/log/console.rs`.
const WIRE_HELD: &str = "console: klogd holds the wire as the stop begins, staged";

/// The actuators that stage `klogd` holding the wire across the stop: until
/// the stop asks for it, or, `kept`, for good.
pub fn wire_staged(kept: bool) -> &'static [&'static str] {
    if kept { &["wire-kept-through-the-stop"] } else { &["wire-held-across-the-stop"] }
}

/// A stop that found `klogd` holding the console's wire, judged on the whole
/// `console` of a boot that ended with the `last` word: the staged hold, the
/// stop's record and the last word are all on it, in that order; where `klogd`
/// `kept` the wire the stop said so between the hold and the record, and
/// where it did not the console is clean.
pub fn judge_the_held_wire(console: &str, last: &str, kept: bool) -> Result<(), String> {
    serial::Serial::named("staged stop", console.to_string()).must_not_say(STAGED_KLOGD_UNRUN)?;
    let lines: Vec<&str> = console.lines().collect();
    let at = |what: &str| lines.iter().position(|l| l.contains(what));
    let held = at(WIRE_HELD).ok_or_else(|| format!("the staged klogd never said it held the wire\n{console}"))?;
    let record = lines
        .iter()
        .position(|l| toyos_quiesce::Record::parse(l).is_some())
        .ok_or_else(|| format!("the stop's record is not on the console\n{console}"))?;
    let said = at(last).ok_or_else(|| format!("{last:?} is not on the console\n{console}"))?;
    if !(held < record && record < said) {
        return Err(format!("the hold, the stop's record and {last:?} are on lines {held}, {record} and {said}\n{console}"));
    }
    match (kept, at(WIRE_KEPT)) {
        (true, Some(alert)) if held < alert && alert < record => Ok(()),
        (false, None) => serial::Serial::named("staged stop", console.to_string()).must_be_clean(),
        (_, alert) => Err(format!(
            "klogd {} the wire, and the stop's alert is on line {alert:?}\n{console}",
            if kept { "kept" } else { "was asked for" },
        )),
    }
}

/// `klogd` holds the console's wire as the stop begins, on one CPU, where it
/// runs only when the stop gives the CPU back: the machine powers off with the
/// stop's record and its last word on the console. `kept`, `klogd` keeps the
/// wire through the stop's whole budget, and the stop says so and writes
/// over it.
pub fn machine_shutdown_wire_held(test_config: &Path, kept: bool) -> Result<(), String> {
    let options = BootOptions { qmp: true, smp: 1, kernel_params: wire_staged(kept), ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    let mut console = boot.text().to_string();
    qemu::await_marker(&mut qemu, &mut console, Q35_S5_SUPPLIED, "the ACPI server to hand the kernel \\_S5")?;
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    judge_the_held_wire(&console, SHUTTING_DOWN, kept)?;
    eprintln!("  [power] klogd held the wire across the stop{}; the record and the last word reached it", if kept { " and kept it" } else { "" });
    Ok(())
}

/// `klogd` is staged to hold the console's wire from the boot's last word to
/// past the stop's seal, on one CPU, which from the seal on runs nothing but
/// the stop: a stop that left the wire to `klogd` there powers off with the
/// last word on no console. This one holds the wire itself, says so above the
/// last word, and puts the last word on the console.
pub fn machine_shutdown_wire_at_the_seal(test_config: &Path) -> Result<(), String> {
    let options =
        BootOptions { qmp: true, smp: 1, kernel_params: &["wire-held-at-the-last-word"], ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    let mut console = boot.text().to_string();
    qemu::await_marker(&mut qemu, &mut console, Q35_S5_SUPPLIED, "the ACPI server to hand the kernel \\_S5")?;
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    let lines: Vec<&str> = console.lines().collect();
    let at = |what: &str| lines.iter().position(|l| l.contains(what));
    match (at(STOP_HOLDS_AT_THE_LAST_WORD), at(SHUTTING_DOWN)) {
        (Some(holds), Some(said)) if holds < said => {}
        (holds, said) => {
            return Err(format!(
                "the stop's hold of the wire at the last word is on line {holds:?} and {SHUTTING_DOWN:?} on \
                 {said:?}\n{console}"
            ))
        }
    }
    serial::Serial::named("staged stop", console.clone()).must_be_clean()?;
    eprintln!("  [power] the stop held the wire at the last word and across the seal; the last word reached it");
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

/// A wedge under the chipset watchdog, read off the boot and the loader pass
/// after its reset: the kernel took the loader's arm over and fed it to the
/// stop where the wedge is staged; then the machine reset with nothing in the
/// kernel ending it, and the FCH says its watchdog did.
pub fn watchdog_reset_chain(
    kernel: &serial::Serial,
    log: &serial::Serial,
    after: &serial::Serial,
) -> Result<(), String> {
    kernel.must_say(ARMED_ON_ARRIVAL)?;
    kernel.must_say(&kernel_feeds())?;
    bootlog::stopping_line(log.text()).ok_or_else(|| {
        format!("the armed boot's log carries no {:?}: it never reached the stop the wedge is staged at", bootlog::STOPPING)
    })?;
    // An ARMED page is a kernel that neither its deadline nor its lockup
    // detector ended; one lost with the reset is a black box the FCH's reset
    // does not keep, and fails here.
    after.must_say(&armed_and_nothing_else())?;
    let said = after.must_say(toyos_tco::fch::RESET_SAID)?.to_string();
    after.must_say(bootlog::CHAIN_ENDS_LINE)?;
    says_nothing_of(after, bootlog::LOADER_LAST_LINE)?;
    eprintln!("  [power] {}", said.trim());
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

/// A line of the first boot's own report, which has to come back out of DRAM on
/// the boot after it: the panic's message, so what is recovered is the crash
/// and not merely a page that checksummed.
const BLACKBOX_WITNESS: &str = "test-late-panic: on-screen console check";

/// The first record `serial::init` writes, which is the first thing the kernel
/// does after taking the page.
const SERIAL_IS_UP: &str = "serial: 16550 loopback read";

/// The page armed and its address handed to the kernel, as the two sides say it.
fn armed_line() -> String {
    format!("{} {PHYS:#x} armed", bootlog::BLACKBOX_HEAD)
}

fn kernel_took_it() -> String {
    format!("black box: {PHYS:#x} is this boot's")
}

/// What the loader writes about a page that still read ARMED, which is a kernel
/// that reached neither of the two paths that write one.
fn armed_and_nothing_else() -> String {
    format!("{} the page still reads {}", bootlog::PREVIOUS_PANIC, State::Armed.named())
}

/// The loader pass after a deliberate reboot: it read DONE, said so, and ended
/// the chain rather than booting another kernel.
///
/// On the T14 this pass is already what every metal boot does — the loader
/// points `BootNext` at itself before each handoff and a pass with a finding
/// appends to the same `loader.log` — so the argument is that file's tail.
pub fn done_chain(after: &serial::Serial) -> Result<(), String> {
    after.must_say(bootlog::HANDED_BACK)?;
    the_tail_is_the_stops(after)?;
    // The distinction the whole state machine exists for: a deliberate stop is
    // not a panic and not a kernel that vanished.
    says_nothing_of(after, &armed_and_nothing_else())?;
    says_nothing_of(after, BLACKBOX_WITNESS)?;
    // The chain ends rather than going round: a pass that booted a kernel would
    // have said so, and this one must not have.
    says_nothing_of(after, bootlog::LOADER_LAST_LINE)?;
    after.must_say(bootlog::CHAIN_ENDS_LINE)?;
    eprintln!("  [power] a deliberate reboot sealed DONE and the chain ended in a reset");
    Ok(())
}

/// The tail the stop sealed under the seal: the one channel for what the
/// kernel said after `logkeeper` stopped — its stop record, the panel's census, and
/// the last word. Read after the seal,
/// because the same lines are in the capture on the first boot's console.
fn the_tail_is_the_stops(after: &serial::Serial) -> Result<(), String> {
    let head = after.must_say_after(bootlog::HANDED_BACK, bootlog::LOG_TAIL_HEAD)?.to_string();
    let tail: Vec<&str> = after
        .text()
        .lines()
        .skip_while(|line| !line.contains(&head))
        .filter(|line| line.contains(bootlog::LOG_TAIL))
        .collect();
    if !tail.iter().any(|line| toyos_quiesce::Record::parse(line).is_some()) {
        return Err(format!("the page's tail carries no stop record:\n{}", tail.join("\n")));
    }
    for owed in [bootlog::PANEL_CENSUS, REBOOTING] {
        if !tail.iter().any(|line| line.contains(owed)) {
            return Err(format!("the page's tail carries no {owed:?}:\n{}", tail.join("\n")));
        }
    }
    Ok(())
}

/// A machine that died took its census as one that stops does: the record its
/// death sealed carries every line of `kernel/src/census.rs`, written by the
/// seal itself, since no process's end leaves one in the ring for a tail to
/// carry.
fn death_took_the_census(after: &serial::Serial) -> Result<(), String> {
    for line in ["irq: cpu0 ", "tlb: shootdowns=", "irq: unclaimed vectors", bootlog::PANEL_CENSUS] {
        after.must_say_after(bootlog::PREVIOUS_PANIC, line)?;
    }
    Ok(())
}

/// The `cpuN` the bracket of `line`'s record names, before `needle`.
fn record_cpu<'a>(line: &'a str, needle: &str) -> Option<&'a str> {
    line.split(needle)
        .next()?
        .split(|c: char| c.is_whitespace() || c == '[' || c == ']')
        .rfind(|word| word.strip_prefix("cpu").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())))
}

/// The metal half of [`boot_deadline_ends_a_wedge`]: a T14 boot that wedged on
/// purpose ended itself, and the pass after the reset read why off the page.
///
/// **The only evidence a wedge can leave on this machine.** `logkeeper` writes the
/// kernel log to the log volume, and a wedged boot's `logkeeper` never runs again — so
/// everything after the wedge exists only in the record ring, and the black box
/// is the one channel that carries a copy of it across the reset.
pub fn deadline_wedge_chain(after: &serial::Serial) -> Result<(), String> {
    after.must_say(bootlog::PREVIOUS_PANIC)?;
    death_took_the_census(after)?;
    let said = after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::DEADLINE_EXPIRED)?.to_string();
    // The control: the machine reached the staged wedge, and then never reached
    // the reset it was one statement away from.
    let staged = after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::WEDGE_STAGED)?;
    // And the CPU that asked for it arrived through the syscall gate with
    // interrupts open: one that arrived deaf is the gate masking a syscall's
    // body. Keyed to that CPU, because every other one arrives awake from a
    // pass whatever the gate does.
    let cpu = record_cpu(staged, bootlog::WEDGE_STAGED)
        .ok_or_else(|| format!("no cpu in the record that staged the wedge: {staged:?}"))?;
    after.must_say_after(bootlog::PREVIOUS_PANIC, &format!("wedge: {cpu} {}", bootlog::WEDGE_AWAKE))?;
    says_nothing_of(after, bootlog::WEDGE_ARRIVED_DEAF)?;
    // And the seal names where that CPU stood: its timer's last kernel frame is
    // inside the spin, resolved by the kernel against its own symbols.
    let pc = after.must_say_after(bootlog::PREVIOUS_PANIC, &format!("{cpu}{}", bootlog::SEAL_PC))?;
    if !pc.contains(bootlog::WEDGE_SPIN) {
        return Err(format!("the seal puts {cpu} outside the wedge's spin `{}`: {pc:?}", bootlog::WEDGE_SPIN));
    }
    says_nothing_of(after, bootlog::REBOOTING)?;
    // **The two bounds composing, on the one machine that has both.** This
    // wedge spins with `IF` set, so every CPU still takes its timer interrupt
    // and every CPU's performance counter still samples it — and the
    // hard-lockup detector, whose bound is the earlier of the two, must find
    // nothing. A page reading it here would mean this machine resets a boot
    // that was merely stopped, which on the T14 is a reset loop.
    says_nothing_of(after, bootlog::LOCKED_UP)?;
    says_nothing_of(after, &armed_and_nothing_else())?;
    after.must_say(bootlog::CHAIN_ENDS_LINE)?;
    says_nothing_of(after, bootlog::LOADER_LAST_LINE)?;
    eprintln!("  [power] {}", said.trim());
    Ok(())
}

/// The metal half of [`hard_lockup_ends_a_deaf_cpu`], and **the arm that proves
/// the counter**: on this machine CPUID states an architectural PMU, so the
/// staged CPU's own performance-counter NMI is what samples it and nothing is
/// sent to it. QEMU's TCG guest can prove the handler and the record; only
/// hardware can prove the thing that delivers them.
///
/// The state under it is the one measured on this machine and on no other: run
/// 22's boot hung after its job list with a 120 s deadline armed, sat past
/// 420 s, and left the log volume with no kernel log at all — the deadline never
/// fired, because nothing was taking the interrupt that polls it.
pub fn hard_lockup_chain(
    kernel: &serial::Serial,
    after: &serial::Serial,
) -> Result<(), String> {
    // **Nothing this judge reads was written after the wedge.** `logkeeper` stops
    // where the scheduler does, so the log volume's kernel log ends at the last
    // spawn, and the sealed page can fill with a boot's *older* records before
    // it reaches the lines the control writes about itself. What crosses is the
    // arm line and the record itself.
    kernel.must_say("by each cpu's own performance counter")?;
    says_nothing_of(kernel, "CPUID states no architectural performance counter")?;

    after.must_say(bootlog::PREVIOUS_PANIC)?;
    death_took_the_census(after)?;
    let said = after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::LOCKED_UP)?.to_string();
    let stuck = after.must_say_after(bootlog::PREVIOUS_PANIC, "spinning on the lock at 0x")?;
    sp_is_a_kernel_stack(stuck)?;
    // And the NMI frame's `rip` is where that CPU stood: inside the lock's
    // spin, resolved by the kernel against its own symbols. A `pc` read off
    // any other word of the frame resolves to nothing, or to somewhere else.
    let pc = after.must_say_after(bootlog::LOCKED_UP, bootlog::LOCKUP_PC)?;
    if !bootlog::LOCK_SPIN.iter().all(|part| pc.contains(part)) {
        return Err(format!("the record puts the stuck cpu outside the lock's spin {:?}: {pc:?}", bootlog::LOCK_SPIN));
    }
    // The staged control's own witness, carried by the mechanism rather than by
    // a log line that may not survive: the lock the stuck cpu is inside was
    // taken at the control's own source line, which no other boot can say.
    after.must_say_after(bootlog::PREVIOUS_PANIC, "taken at kernel/src/hardlockup/probe.rs")?;
    // **cpu0's line is what proves the counter rather than a sender.** Nothing
    // sends cpu0 an NMI on this boot — the control's sender is skipped where the
    // cpus have counters — so a sample recorded against cpu0 came from cpu0's
    // own overflow, and a sample against every cpu is the arm working on all of
    // them.
    after.must_say_after(bootlog::PREVIOUS_PANIC, "cpu0 irqs=")?;
    after.must_say_after(bootlog::PREVIOUS_PANIC, "cpu7 irqs=")?;
    // The deadline was armed on this boot too, at twice this bound, and is not
    // what ended the machine.
    says_nothing_of(after, bootlog::DEADLINE_EXPIRED)?;
    says_nothing_of(after, &armed_and_nothing_else())?;
    // **Not [`bootlog::CHAIN_ENDS_LINE`], which the deadline's arm demands.**
    // That pass's own last line is written after the record, and a `loader.log`
    // that stopped inside the record never reaches it — so demanding it here
    // would judge the loader's file capacity and call it a lockup. The pass
    // having booted no kernel is what this asserts instead.
    says_nothing_of(after, bootlog::LOADER_LAST_LINE)?;
    eprintln!("  [power] {}", said.trim());
    Ok(())
}

/// The NMI entry routed the frame's `rsp` to the sample: a kernel stack is above
/// `mm::PHYS_OFFSET`, and the `rflags` a swapped load would put there is below
/// `0x400000`.
fn sp_is_a_kernel_stack(stuck: &str) -> Result<(), String> {
    if !stuck.contains("sp=0xffff") {
        return Err(format!("the stuck cpu's sp is no kernel stack: {stuck}"));
    }
    Ok(())
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

/// The loader named a page, the kernel took *that* page, and it took it before
/// the console existed.
pub fn blackbox_unclaimed(
    loader: &serial::Serial,
    kernel: &serial::Serial,
) -> Result<(), String> {
    // The loader claimed one on this machine, so what is judged here is the
    // *kernel's* reading of its own parameter line: the address it was given is
    // the address the loader printed, and nothing else in the line is a page.
    let claimed = loader.must_say(&armed_line())?.to_string();
    loader.must_say(&format!("blackbox={PHYS:#x}"))?;
    kernel.must_say(&kernel_took_it())?;
    // **Before `serial::init`, and that ordering is the assertion.** The page
    // used to be taken after the console, the parameter line's UTF-8 check and
    // `params::init`, and the owner's laptop panicked before all three: it
    // rendered a panel and reset itself with the page still holding the loader's
    // `ARMED`. No staged panic can land in that window — arming one needs the
    // line parsed first — so what is judged is where the kernel says it took
    // the page, which moves the moment the reading moves.
    kernel.must_say_after(&kernel_took_it(), SERIAL_IS_UP)?;

    eprintln!("  [power] the loader named the page and the kernel took it: {}", claimed.trim());
    Ok(())
}

/// What `/system/bin/acpiserver` says once it serves q35's power button,
/// whole, before it loads the tables.
const ACPI_ARMED: &str = "acpiserver: armed: power button served";

/// [`STICK_LOAD_JOB`](super::metal::STICK_LOAD_JOB)'s boot: the job wrote the
/// log disk, the boot deadline ended the machine while it did, and the pass
/// after the reset read the deadline's record. The host booting again off that
/// disk is the loop's own half, and a readback is only there once it did.
pub fn the_disk_survives_a_reset_under_load(back: &super::metal::Readback) -> Result<(), String> {
    let after = back.after_the_reset()?;
    after.must_say(bootlog::PREVIOUS_PANIC)?;
    after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::DEADLINE_EXPIRED)?;
    says_nothing_of(&after, REBOOTING)?;
    // What reached the medium: the load's lines in the file read back off the
    // log volume.
    let reached = super::metal::bytes_of_lines(back.log().text(), "stick_load: line ");
    if reached < 1 << 20 {
        return Err(format!("{reached} bytes of the load's lines reached the disk, under a mebibyte"));
    }
    eprintln!("  [power] {} KiB of the load's lines reached the disk before the reset", reached >> 10);
    Ok(())
}
