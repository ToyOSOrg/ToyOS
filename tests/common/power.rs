use std::io::Write;
use std::path::Path;

use toyos_blackbox::{PHYS, State};
use toyos_build::bootlog::{self, REBOOTING};

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial;

/// The kernel's last word when it powers the machine off, in
/// `kernel/src/syscall/machine.rs`.
pub const SHUTTING_DOWN: &str = "Shutting down.";

/// What the kernel logs once it has decoded S5 soft-off, ahead of the PM1a
/// control block's port and the `SLP_TYPa` the DSDT's `\_S5_` names.
const SOFT_OFF_DECODED: &str = "ACPI: PM1a=";

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
/// boot decoded q35's PM1a block and its `\_S5_`, and QEMU stops for
/// `guest-shutdown`, which neither a reset nor a halt is.
pub fn machine_shutdown(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { qmp: true, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);

    let boot = serial::Serial::boot(&qemu);
    boot.must_be_clean()?;
    // The values this kernel read out of q35's tables, so a decode it got
    // wrong fails here and not as a machine that stayed up.
    boot.must_say(&format!("{SOFT_OFF_DECODED}0x604 SLP_TYPa=0"))?;

    let mut stop = qemu::QmpShutdown::open(qemu.qmp_socket(), qemu.budget(qemu::GUEST_QUIET));
    writeln!(qemu.stdin_mut(), "run shutdown").expect("write to QEMU stdin");
    qemu.flush_stdin();
    let mut console = String::new();
    ended(&mut qemu, &mut stop, &mut console, SHUTTING_DOWN, "guest-shutdown")?;
    serial::Serial::named("shutdown drain", console).must_be_clean()?;

    eprintln!("  [power] shutdown: QEMU stopped the guest for guest-shutdown");
    Ok(())
}

/// The kernel decoded S5 soft-off out of this machine's FADT and DSDT. Every
/// other branch of `arch::power::init_off` says `no soft-off` and not this.
pub fn soft_off_decoded(kernel: &serial::Serial) -> Result<(), String> {
    let line = kernel.must_say(SOFT_OFF_DECODED)?;
    eprintln!("  [power] {}", line.trim());
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
/// kernel said after `logd` stopped — its stop record, the panel's census, and
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
    after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::DEADLINE_EXPIRED)?;
    // The sweep starts inside the stop, after init had the file made whole, so
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

/// The metal half of [`boot_deadline_ends_a_wedge`]: a T14 boot that wedged on
/// purpose ended itself, and the pass after the reset read why off the page.
///
/// **The only evidence a wedge can leave on this machine.** `logd` writes the
/// kernel log to the stick, and a wedged boot's `logd` never runs again — so
/// everything after the wedge exists only in the record ring, and the black box
/// is the one channel that carries a copy of it across the reset.
pub fn deadline_wedge_chain(after: &serial::Serial) -> Result<(), String> {
    after.must_say(bootlog::PREVIOUS_PANIC)?;
    let said = after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::DEADLINE_EXPIRED)?.to_string();
    // The control: the machine reached the staged wedge, and then never reached
    // the reset it was one statement away from.
    after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::WEDGE_STAGED)?;
    // And the CPU that asked for it took interrupts again: it comes through the
    // syscall gate with `IF` masked, and one left deaf is what the lockup
    // detector ends a machine for. On this machine the assertion has a counter
    // behind it, which is what the guest's has not.
    after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::WEDGE_ARRIVED_DEAF)?;
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
    // **Nothing this judge reads was written after the wedge.** `logd` stops
    // where the scheduler does, so the stick's kernel log ends at the last
    // spawn, and the sealed page can fill with a boot's *older* records before
    // it reaches the lines the control writes about itself. What crosses is the
    // arm line and the record itself.
    kernel.must_say("by each cpu's own performance counter")?;
    says_nothing_of(kernel, "CPUID states no architectural performance counter")?;

    after.must_say(bootlog::PREVIOUS_PANIC)?;
    let said = after.must_say_after(bootlog::PREVIOUS_PANIC, bootlog::LOCKED_UP)?.to_string();
    let stuck = after.must_say_after(bootlog::PREVIOUS_PANIC, "spinning on the lock at 0x")?;
    sp_is_a_kernel_stack(stuck)?;
    // The staged control's own witness, carried by the mechanism rather than by
    // a log line that may not survive: the lock the stuck cpu is inside was
    // taken at the control's own source line, which no other boot can say.
    after.must_say_after(bootlog::PREVIOUS_PANIC, "taken at src/hardlockup/probe.rs")?;
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
