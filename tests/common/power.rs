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

/// A process holding `POWER` runs `shutdown` and the machine powers off: the
/// kernel declared q35's PM1a block, `acpiserver` evaluated the DSDT's `\_S5`
/// and handed the kernel its `SLP_TYPa`, which the kernel has from nowhere
/// else, and QEMU stops for `guest-shutdown`, which neither a reset nor a
/// halt is.
pub fn machine_shutdown(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);

    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    // The values, so a wrong one fails here and not as a machine that stayed
    // up; and the event the shutdown waits on, since one asked before it is
    // refused.
    let mut supplied = boot.text().to_string();
    qemu::await_marker(&mut qemu, &mut supplied, Q35_S5_SUPPLIED, "the ACPI server to hand the kernel \\_S5")?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let mut console = String::new();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    serial::Serial::named("shutdown drain", console).must_be_clean()?;

    eprintln!("  [power] shutdown: QEMU stopped the guest for guest-shutdown");
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
        &super::compile::repo_root().join("tests/toyos-rust-tests"),
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

/// The decimal digit printed straight after `label`, for the decoded bits the
/// read-back names one by one.
fn decimal_field(line: &str, label: &str) -> Result<u64, String> {
    let rest = line.split_once(label).ok_or_else(|| format!("no {label:?} in {line:?}"))?.1;
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().map_err(|e| format!("{label:?} in {line:?}: {e}"))
}

/// The armed boot's half: the loader wrote the register block and the kernel
/// found the timer already running.
///
/// **`TCO1_CNT.TCO_LOCK` is not judged.** `toyos_tco`'s own header names it as
/// the one bit a read-back may not be held to — firmware that set it leaves it
/// set through every write this tree makes, and what it gates is `SMI_EN.TCO_EN`
/// rather than the countdown or the reboot. It is reported and passed over.
pub fn watchdog_armed(
    loader: &serial::Serial,
    kernel: &serial::Serial,
) -> Result<(), String> {
    let line = loader.must_say(&loader_armed())?.to_string();
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
    // The words of it, not the line: this is the register block's own account of
    // whether an expiry reaches the machine, and a machine that has just armed
    // the timer has not expired once.
    for (field, want) in [("no_reboot=", 0), ("timeout=", 0)] {
        let seen = decimal_field(&gates, field)?;
        if seen != want {
            return Err(format!(
                "the loader read {field}{seen} back and this machine is {want}\n{gates}"
            ));
        }
    }
    kernel.must_say(&armed_on_arrival())?;

    eprintln!("  [power] the loader armed it and the kernel found it running: {}", line.trim());
    eprintln!("  [power] {}", gates.trim());
    eprintln!("  [power] tco_lock={}, which no read-back is judged on", decimal_field(&gates, "tco_lock=")?);
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
    // through `must_not_say`: on the stick `loader.log` is a file of its own and
    // carries no kernel record for `Serial::alive` to find.
    loader.must_say(bootlog::LOADER_FIRST_LINE)?;
    says_nothing_of(loader, &loader_armed())?;
    kernel.must_not_say(ARMED_ON_ARRIVAL)?;
    kernel.must_not_say(UNARMED_ON_ARRIVAL)?;
    Ok(())
}

/// `must_not_say` on a channel whose liveness the caller has already
/// established, because `Serial::alive` asks for a kernel record and the
/// loader's own file on the stick has none.
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
    // What its transport went through is on every record, in one line where
    // nothing broke.
    after.must_say_after(bootlog::HANDED_BACK, toyos_blackbox::RECOVERY_OPENS_WITH)?;
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

/// The metal half of the load arm: a T14 boot that never stopped writing, ended
/// by the boot deadline with its controller mid-transfer, and the stick still
/// there afterwards.
///
/// **The bound is the boot deadline's and not the hard-lockup detector's.** The
/// sweep keeps its CPU taking interrupts for exactly that reason
/// (`usb_gate::sweep_under_load`): a CPU that takes none is ended half a bound
/// earlier by `kernel/src/hardlockup`, which is a different mechanism reported
/// under this arm's name, so this judge names the bound it demands.
pub fn usb_load_chain(after: &serial::Serial) -> Result<(), String> {
    after.must_say(bootlog::PREVIOUS_PANIC)?;
    death_took_the_census(after)?;
    after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::DEADLINE_EXPIRED)?;
    // The sweep starts inside the stop, after the supervisor had the file made whole, so
    // its records cross only in the page's tail.
    after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::USB_LOAD_RUNNING)?;
    // A sweep that refused, one the disk stopped answering, and one that swept
    // its whole span before the reset: each is a boot that measured the idle
    // case again under this arm's name.
    says_nothing_of(after, bootlog::USB_LOAD_REFUSED)?;
    says_nothing_of(after, bootlog::USB_LOAD_STOPPED)?;
    says_nothing_of(after, bootlog::USB_LOAD_SWEPT)?;
    says_nothing_of(after, REBOOTING)?;
    // A page that names the other bound is this arm measuring a hard lockup.
    says_nothing_of(after, bootlog::LOCKED_UP)?;
    // **The account reaching the page is itself under test**: it is made a line
    // at a time from the reset path, under the reserve
    // `toyos_blackbox::ACCOUNT_BYTES` keeps for it.
    after.must_say(toyos_build::metaldevices::QUIESCE_HEAD)?;
    after.must_say(bootlog::CHAIN_ENDS_LINE)?;
    // **Reported and not judged.** Which state the reset found the controller
    // in is the open question this arm gathers answers to; a predicate over it
    // would be the suite deciding it. A reset that found no command open writes
    // no such line, and that is a fact about the boot rather than a failure of
    // it.
    let endpoint = toyos_build::metaldevices::QUIESCE_ENDPOINT;
    match after.text().lines().find(|line| line.contains(endpoint)) {
        Some(said) => eprintln!("  [power] {}", said.trim()),
        None => eprintln!("  [power] the reset found no command open on the device"),
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
/// kernel log to the stick, and a wedged boot's `logkeeper` never runs again — so
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
/// 420 s, and left the stick with no kernel log at all — the deadline never
/// fired, because nothing was taking the interrupt that polls it.
pub fn hard_lockup_chain(
    kernel: &serial::Serial,
    after: &serial::Serial,
) -> Result<(), String> {
    // **Nothing this judge reads was written after the wedge.** `logkeeper` stops
    // where the scheduler does, so the stick's kernel log ends at the last
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

const TOOK_THE_LOCK: &str = "the controller lock was held from before the log volume's";

/// The T14's judge for [`usb_reset_hands_devices_back`].
///
/// **The machine is the judge of the device, and nothing else is.** QEMU cannot
/// wedge a stick. What is left for this to read is the reset's own account,
/// which on this machine is in `loader.log`'s pass after the reset rather than
/// in any file the kernel wrote.
///
/// Both arms are orderly reboots, so both owe the barrier.
pub fn usb_reset_on_metal(arms: &[&super::metal::Readback]) -> Result<(), String> {
    let mut bad = Vec::new();
    for back in arms {
        let text = back.loader();
        let head = toyos_build::metaldevices::QUIESCE_HEAD;
        let Some(account) = toyos_build::metaldevices::quiesced(text.text()) else {
            bad.push(format!(
                "{}: loader.log carries no readable {head:?} summary, so nothing stopped this \
                 machine's USB before it reset",
                back.label
            ));
            continue;
        };
        if !account.complete() {
            bad.push(format!("{}: {account:?}", back.label));
        }
        if !text.text().contains(TOOK_THE_LOCK) {
            bad.push(format!(
                "{}: this is an orderly reboot and its account does not say {TOOK_THE_LOCK:?}",
                back.label
            ));
        }
        eprintln!("  [power] {}: {account:?}", back.label);
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!("{} finding(s):\n  {}", bad.len(), bad.join("\n  ")))
}

/// What `/system/bin/acpiserver` says once it serves q35's power button,
/// whole, before it loads the tables.
const ACPI_ARMED: &str = "acpiserver: armed: power button served";

/// The server's line for the press, naming the SCI it came on.
const ACPI_PRESSED: &str =
    "acpiserver: the power button was pressed, on SCI 1 of this boot; asking the supervisor to power off";

/// What `/system/bin/acpiserver` says of each definition block, after
/// `acpiserver: `: its place, how many there are, which it is, and what
/// became of it.
const ACPI_TABLE: &str = "acpiserver: table ";

/// What it says once the kernel has kept `\_S5`'s `SLP_TYPa`, ahead of it.
const ACPI_S5_HANDED: &str = "acpiserver: \\_S5 handed to the kernel: ";

/// The number after `SLP_TYPa=` on a line.
fn slp_typ_a(line: &str) -> Result<u64, String> {
    line.split_once("SLP_TYPa=")
        .and_then(|(_, after)| after.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .ok_or_else(|| format!("no SLP_TYPa on {line:?}"))
}

/// The server loaded every definition block it found, the DSDT first, each
/// said on a line in its place; and the `SLP_TYPa` its `\_S5` evaluates to,
/// which it handed the kernel, and the PM1a control block the kernel says it
/// powers off on with it, are the ones `supplied` holds for this machine: the
/// kernel's line whole to the value, read some way that is not this server's
/// evaluation. Answers how many blocks there were.
pub fn acpi_tables_loaded(log: &serial::Serial, kernel: &serial::Serial, supplied: &str) -> Result<usize, String> {
    let said: Vec<&str> = log.text().lines().filter_map(|line| line.split_once(ACPI_TABLE).map(|(_, said)| said.trim())).collect();
    let Some(count) = said.first().and_then(|first| first.split_once(" of ")?.1.split_once(' ')?.0.parse::<usize>().ok()) else {
        return Err(format!("the server said nothing of a first table ({said:?}):\n{}", log.text()));
    };
    let expected: Vec<String> =
        (1..=count).map(|place| format!("{place} of {count} ({}) loaded", if place == 1 { "DSDT" } else { "SSDT" })).collect();
    if said != expected {
        return Err(format!("the server's tables are {said:#?}, where {count} loaded ones are {expected:#?}"));
    }
    let (handed, kept) = (log.must_say(ACPI_S5_HANDED)?, kernel.must_say(supplied)?);
    if slp_typ_a(handed)? != slp_typ_a(supplied)? {
        return Err(format!("the server's \\_S5 is not this machine's: {:?} beside {supplied:?}", handed.trim()));
    }
    eprintln!("  [power] {count} table(s) loaded; {} beside {}", handed.trim(), kept.trim());
    Ok(count)
}

/// A press of q35's power button stops the machine through ToyOS's own path:
/// the kernel put the machine in ACPI mode for the server's claim, the press
/// arrives as the server's first SCI and the only one, since the kernel masks
/// the level line until the server has served it, and the server has the
/// supervisor stop the machine, which QEMU reports as the guest's own
/// power-off. The press is sent as soon as the server has armed, which is
/// before it loads the machine's tables: the press is latched across the
/// load, whose lines and `\_S5` are read here, on the one firmware's tables a
/// guest has, and powers the machine off with the sleep type the load handed
/// the kernel.
pub fn acpi_power_button(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    let mut console = boot.text().to_string();
    // A wait each, as in `machine_shutdown_short_stop`: the server's line and
    // the kernel's record reach the console by different roads.
    qemu::await_marker(&mut qemu, &mut console, ACPI_ARMED, "the ACPI server arming")?;
    qemu::await_marker(&mut qemu, &mut console, HANDED_OVER_IN_ACPI_MODE, "the kernel's record of the mode it was handed over in")?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    stop.power_button();
    let pressed_at = console.len();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    let after = serial::Serial::named("the press", console[pressed_at..].to_string());
    after.must_say(ACPI_PRESSED)?;
    if !toyos_build::bootlog::asked_to_power_off(after.text()) {
        return Err(format!("the supervisor's last word after the press is not a power-off:\n{}", after.text()));
    }
    after.must_be_clean()?;
    let whole = serial::Serial::named("the boot and the press", console);
    acpi_tables_loaded(&whole, &whole, Q35_S5_SUPPLIED)?;
    whole.must_say_after(ACPI_S5_HANDED, ACPI_PRESSED)?;
    eprintln!("  [power] the press: {ACPI_PRESSED}");
    Ok(())
}

/// How long [`ssdt_acquiring_the_global_lock`] sleeps as it loads, in ms,
/// before its Acquire: the window the press is sent in, wide beside a host's
/// answer to a line.
const PRESS_WINDOW_MS: u16 = 5000;

/// The Acquire's TimeoutValue, in ms; the two together are within the
/// 10 s one evaluation may spend asleep.
const ACQUIRE_MS: u16 = 2000;

/// What the server says of the table's Notify, which runs only where the
/// Acquire before it timed out.
const ACQUIRE_TIMED_OUT: &str = "Notify(\\_SB_.GLW_, 0x80) for the first time";

/// What it says of the load's takes: one, and it found the firmware holding
/// the lock.
const ONE_TAKE_CONTENDED: &str = "; took the Global Lock 1 times, 1 of them from the firmware;";

/// PM1 status and enable (ACPI 6.5 Tables 4.13, 4.14): `PWRBTN_STS` and
/// `PWRBTN_EN`.
const PWRBTN: u16 = 1 << 8;

/// The FACS's lock word's owned bit (Table 5.11), with the pending bit a
/// take that finds it owned sets clear.
const OWNED: u32 = 2;

/// An SSDT (ACPI 6.5 §5.2.11.2) whose definition block declares the device
/// `\_SB.GLW` and, as it loads, sleeps `window` ms, then Acquires `\_GL`
/// within `ms`, giving it back where that took it and Notifying `\_SB.GLW`
/// with 0x80 where it timed out:
///
/// ```text
/// Device (\_SB.GLW) {}
/// Sleep (window)
/// If (Acquire (\_GL, ms)) { Notify (\_SB.GLW, 0x80) } Else { Release (\_GL) }
/// ```
fn ssdt_acquiring_the_global_lock(window: u16, ms: u16) -> Vec<u8> {
    const GL: &[u8] = b"\\_GL_";
    // `\` and a DualNamePrefix (§20.2.2).
    const GLW: &[u8] = b"\\\x2E_SB_GLW_";
    // A PkgLength of one byte (§20.2.4): itself and what follows, under 64.
    let package = |op: &[u8], body: &[u8]| [op, &[u8::try_from(body.len() + 1).expect("a short package")], body].concat();
    let sleep = [&[0x5B, 0x22, 0x0B], &window.to_le_bytes()[..]].concat();
    let acquire = [&[0x5B, 0x23], GL, &ms.to_le_bytes()].concat();
    let notify = [&[0x86], GLW, &[0x0A, 0x80]].concat();
    let release = [&[0x5B, 0x27], GL].concat();
    let aml = [
        package(&[0x5B, 0x82], GLW),
        sleep,
        package(&[0xA0], &[acquire, notify].concat()),
        package(&[0xA1], &release),
    ]
    .concat();
    let length = u32::try_from(36 + aml.len()).expect("a short table");
    let mut table = [b"SSDT".as_slice(), &length.to_le_bytes(), &[2, 0], b"TOYOS ", b"GLWAIT  ", &1u32.to_le_bytes(), b"TOYS", &1u32.to_le_bytes(), &aml].concat();
    // The byte at 9 makes the whole table sum to zero.
    table[9] = table.iter().fold(0u8, |sum, &b| sum.wrapping_sub(b));
    table
}

/// The number after `after` on a line, in hex.
fn hex_after(line: &str, after: &str) -> Result<u64, String> {
    line.split_once(after)
        .map(|(_, rest)| rest.trim_start_matches("0x"))
        .and_then(|rest| u64::from_str_radix(&rest[..rest.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(rest.len())], 16).ok())
        .ok_or_else(|| format!("no number after {after:?} on {line:?}"))
}

/// A press latched before the server's wait for the firmware's release of
/// the Global Lock is served once the wait ends at its Acquire's timeout: the
/// wait clears every enable but `GBL_EN`, takes the SCI the press raised,
/// and puts every enable back after, so the press's status raises the SCI
/// again for `serve`. `tests/acpicase` with the test kernel's actuator for
/// the firmware's side of the lock, which `acpi_mediated`'s `held` arm stages
/// owned before it hands the server the claim; the table QEMU adds sleeps
/// [`PRESS_WINDOW_MS`] as it loads, then Acquires the lock within
/// [`ACQUIRE_MS`].
///
/// The press is sent once the server has armed, and QEMU's monitor reads
/// right after it `PWRBTN_STS` latched under `PWRBTN_EN` and the lock word
/// owned with no pending bit: the server had not yet taken, so its wait came
/// after the press. A press inside the wait is QEMU's to drop, which sets
/// `PWRBTN_STS` only under `PWRBTN_EN`. Then the press must power the machine
/// off, after the Acquire said it timed out.
pub fn acpi_press_across_a_lock_wait(probe: (String, Vec<u8>)) -> Result<(), String> {
    let case = super::compile::repo_root().join("tests/acpicase");
    let mut qemu = QemuInstance::boot_with_options(
        &case,
        &[],
        &[],
        BootOptions {
            // The test kernel, for the Global Lock's actuator, as `acpi_mediated_access` boots it.
            kernel_params: &["i8042-withheld"],
            ready_marker: "acpi: the Global Lock is the FACS's at ",
            extra_root_files: vec![
                probe,
                // The arm's name is the file's: the probe asks whether it is there.
                ("share/acpi_mediated_held".to_string(), b"held\n".to_vec()),
            ],
            acpi_tables: vec![ssdt_acquiring_the_global_lock(PRESS_WINDOW_MS, ACQUIRE_MS)],
            qmp: true,
            ..Default::default()
        },
    );
    let boot = serial::Serial::named("the boot", qemu.boot_log().to_string());
    let pm1 = hex_after(boot.must_say("acpi: the ACPI row: PM1a events ")?, "PM1a events ")?;
    let pm1 = u16::try_from(pm1).map_err(|_| format!("a PM1a event block at {pm1:#x}"))?;
    let lock_word = hex_after(boot.must_say("acpi: the Global Lock is the FACS's at ")?, "the FACS's at ")?;
    // Opened before the press: QMP delivers no event emitted before its
    // client connected.
    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    let mut console = format!("{}\n", qemu.boot_log());
    qemu::await_marker(&mut qemu, &mut console, ACPI_ARMED, "the ACPI server arming")?;
    stop.power_button();
    // Table 4.12: PM1 enable follows PM1 status in the block.
    let (status, enable, word) = (stop.port_word(pm1), stop.port_word(pm1 + 2), stop.memory_word(lock_word));
    if status & enable & PWRBTN == 0 || word != OWNED {
        return Err(format!(
            "right after the press PM1 read status {status:#06x} under enable {enable:#06x} and the lock word {word:#x}, where a \
             press latched before the server's take reads PWRBTN_STS under PWRBTN_EN ({PWRBTN:#06x}) and the word owned with no \
             pending bit ({OWNED:#x})"
        ));
    }
    let pressed_at = console.len();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    let after = serial::Serial::named("the press", console[pressed_at..].to_string());
    after.must_say(ONE_TAKE_CONTENDED)?;
    after.must_say_after(ACQUIRE_TIMED_OUT, ACPI_PRESSED)?;
    after.must_be_clean()?;
    let whole = serial::Serial::named("the boot and the press", console);
    if acpi_tables_loaded(&whole, &whole, Q35_S5_SUPPLIED)? < 2 {
        return Err(format!("the server loaded no table beside the DSDT, so not the one QEMU was handed:\n{}", whole.text()));
    }
    eprintln!("  [power] PM1 status {status:#06x} under enable {enable:#06x} and the lock word {word:#x} after the press");
    eprintln!("  [power] {}", whole.must_say(ONE_TAKE_CONTENDED)?.trim());
    eprintln!("  [power] {}", whole.must_say_after(ACQUIRE_TIMED_OUT, ACPI_PRESSED)?.trim());
    Ok(())
}
