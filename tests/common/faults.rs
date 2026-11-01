use std::path::Path;

use super::qemu::{self, BootOptions, QemuInstance};
use super::serial::Serial;

/// The first and the last line of the report `nested_nmi` writes to the UART.
const NESTED: &str = "[nmi] NESTED NMI on cpu ";
const STOPS: &str = "[nmi]   the outer handler's frame is gone; the machine stops here.";

/// The halt's flush ends on the reboot's arm line, which is this boot's ready
/// marker.
const ARMED: &str = "panic: rebooting in";

/// What `kernel/src/drivers/serial.rs` writes when the report or the halt's
/// flush could not have the console registers clean.
pub const UNCLEAN: [&str; 2] = [
    "[serial] the console registers stayed held through the bound",
    "[serial] this cpu's own fatal path held the console registers",
];

pub fn nested_nmi_is_loud(test_config: &Path) -> Result<(), String> {
    let qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
        BootOptions {
            kernel_params: &["nmi-nested"],
            // The profile whose 16550 is the console: the nested-NMI report is
            // a raw write — that handler may not reach the log ring at all
            // (`arch::idt::nmi`) — so on any other profile it lands on a UART
            // nothing here is reading.
            profile: qemu::Profile::Metal,
            ready_marker: ARMED,
            ..Default::default()
        },
    );
    let serial = Serial::boot(&qemu);
    let first = report(serial.text())?;
    eprintln!("  [nmi] nested: {first}");
    Ok(())
}

/// `nested_nmi`'s first line, once its three are whole and back to back and
/// nothing in `capture` says the registers were not clean.
pub fn report(capture: &str) -> Result<&str, String> {
    let lines: Vec<&str> = capture.lines().collect();
    let at = lines.iter().position(|l| l.starts_with(NESTED));
    let Some(report) = at.and_then(|at| lines.get(at..at + 3)).filter(|r| whole(r)) else {
        return Err(format!("the nested-NMI report reached the console spliced:\n{capture}"));
    };
    if let Some(unclean) = lines.iter().find(|l| UNCLEAN.iter().any(|said| l.contains(said))) {
        return Err(format!("the report's registers were not clean: {unclean}\n{capture}"));
    }
    Ok(report[0])
}

/// `nested_nmi`'s three lines, each exactly as it writes them: a burst another
/// CPU put on the 16550 inside the report splits one.
fn whole(report: &[&str]) -> bool {
    let [first, registers, last] = report else { return false };
    let hex = |v: &str| v.len() == 18 && v.starts_with("0x") && v[2..].bytes().all(|b| b.is_ascii_hexdigit());
    first
        .strip_prefix(NESTED)
        .and_then(|rest| rest.strip_suffix(": a second NMI entered while IST2 was still in use."))
        .is_some_and(|cpu| cpu.parse::<u32>().is_ok())
        && registers
            .strip_prefix("[nmi]   rip=")
            .and_then(|rest| rest.split_once(" rsp="))
            .is_some_and(|(rip, rsp)| hex(rip) && hex(rsp))
        && *last == STOPS
}

/// Where `test_rs_fault_gate_child`'s kernel arms aim: the direct map's first
/// words, which no process may name.
const KERNEL_RSP: &str = "0xffff800000000000";
const KERNEL_RBP: &str = "0xffff800000000010";
const KERNEL_READ: &str = "0xffff800000000008";

/// **A crash report reads a faulting process's memory only at user
/// addresses.** One of `test_rs_fault_gates`' children dies with its stack and
/// frame pointers aimed at the kernel's direct map, another reading there; the
/// kernel's records name each refusal, and carry neither a word from behind
/// them nor the kernel's page walk for the read.
pub fn crash_report_reads_no_kernel_memory(kernel: &Serial) -> Result<(), String> {
    for refused in [
        format!("Stack (from RSP): {KERNEL_RSP} refused: no user address"),
        format!("rbp {KERNEL_RBP} refused: no user address"),
        format!("Page walk for {KERNEL_READ} refused: no user address"),
    ] {
        kernel.must_say(&refused)?;
    }
    // A stack word the report read is `[address] = value`, and the walk's
    // header is `Page walk for address [PML4=…`.
    let walk = format!("Page walk for {KERNEL_READ} [");
    let leaked = |l: &&str| (l.contains("[0xffff8") && l.contains("] = ")) || l.contains(&walk);
    match kernel.text().lines().find(leaked) {
        Some(line) => Err(format!("the kernel's records carry kernel memory a crash report read: {line:?}")),
        None => Ok(()),
    }
}

/// What `kernel-wx-leaf` says of the leaf it stages (`kernel/src/main.rs`).
const STAGED: &str = "kernel-wx-leaf: the leaf at ";
/// The middle of the boot's audit refusal (`kernel/src/mm/mod.rs`).
const AUDIT_REFUSED: &str = " kernel leaves, writable and executable: ";

/// The boot's audit of the kernel's tables against one direct-map leaf
/// `kernel-wx-leaf` made writable and executable: the refusal names that leaf
/// under both rules and counts no other, so every leaf the boot made itself
/// passed the same walk.
pub fn kernel_wx_leaf_is_refused(test_config: &Path, profile: qemu::Profile) -> Result<(), String> {
    let qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
        BootOptions { profile, kernel_params: &["kernel-wx-leaf"], ready_marker: AUDIT_REFUSED, ..Default::default() },
    );
    let serial = qemu.boot_log();
    let at = serial
        .lines()
        .find_map(|line| line.split(STAGED).nth(1)?.split_whitespace().next())
        .ok_or_else(|| format!("no {STAGED:?} line\n{serial}"))?;
    let refusal = serial
        .lines()
        .find(|line| line.contains(AUDIT_REFUSED))
        .map(str::trim_end)
        .ok_or_else(|| format!("no audit refusal\n{serial}"))?;
    let once = format!(": 1 (the first at {at})");
    let (both, outside) = (format!("writable and executable{once}"), "; executable outside the text ");
    match refusal.split_once(outside) {
        Some((writable, executable)) if writable.ends_with(&both) && executable.ends_with(&once) => {
            eprintln!("  [kernel_wx] {refusal}");
            Ok(())
        }
        _ => Err(format!("the audit did not name {at} alone under both rules: {refusal}\n{serial}")),
    }
}

/// What `tsc-step` says of each step it stages, and the clock's records of
/// slewing the first away and demoting the TSC at the second
/// (`kernel/src/arch/x86_64/clocksource.rs`).
const TSC_STEP_STAGED: &str = "tsc-step: the TSC reads ";
const TSC_STEP_SLEWED: &str = "clock: TSC STEP: the TSC stepped ";
const TSC_SLEWED_AWAY: &str = "clock: the TSC's step is slewed away";
const TSC_STEP_AGAIN: &str = "cycles further ahead from here";
const TSC_DEMOTED: &str = "clock: DEMOTED: ";
const DEMOTED_READS: &str = "clock: a process's reading of now asked the kernel since the TSC was demoted";

/// A TSC `tsc-step` reads 10 ms ahead, and 10 ms further once the clock has
/// slewed the first step away: the first is slewed, the second demotes the
/// TSC, and processes' readings of now reach the kernel after it.
pub fn a_stepped_tsc_is_slewed_then_demoted(test_config: &Path) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
        BootOptions { profile: qemu::Profile::Metal, kernel_params: &["tsc-step"], ready_marker: TSC_STEP_SLEWED, ..Default::default() },
    );
    let mut serial = qemu.boot_log().to_string();
    // A 10 ms step at 500 ppm is 20 s of slewing; the rest is the RTC's edges either side of each step.
    serial.push_str(&qemu.drain_until(std::time::Duration::from_secs(120), |l| l.contains(DEMOTED_READS) || l.contains("PANIC")));
    let lines: Vec<&str> = serial.lines().collect();
    let at = |marker: &str| lines.iter().position(|l| l.contains(marker)).ok_or_else(|| format!("no {marker:?} line\n{serial}"));
    let order = [at(TSC_STEP_STAGED)?, at(TSC_STEP_SLEWED)?, at(TSC_SLEWED_AWAY)?, at(TSC_STEP_AGAIN)?, at(TSC_DEMOTED)?, at(DEMOTED_READS)?];
    if !order.is_sorted() || serial.contains("PANIC") {
        return Err(format!("the steps, the slew and the demotion are out of order: {order:?}\n{serial}"));
    }
    let stepped: Option<i64> = lines[order[1]].split(TSC_STEP_SLEWED).nth(1).and_then(|r| r.split(" ns").next()).and_then(|n| n.parse().ok());
    if !stepped.is_some_and(|ns| (9_000_000..=11_000_000).contains(&ns)) {
        return Err(format!("the slewed step is not the staged 10 ms: {}\n{serial}", lines[order[1]].trim()));
    }
    for i in [1, 4, 5] {
        eprintln!("  [tsc_step] {}", lines[order[i]].trim());
    }
    Ok(())
}

/// What `hpet-stall` says of the stall it stages, the sampler's record of
/// leaving the span out, and a report after it.
const HPET_STALL_STAGED: &str = "hpet-stall: the HPET reads ";
const HPET_MOVED: &str = "clock: the HPET moved and the TSC did not, so this span is left out: ";
const CLOCK_REPORT: &str = "clock: over ";

/// An HPET `hpet-stall` reads 10 ms behind while the TSC and the ACPI PM timer
/// count on: the sampler says the HPET moved, leaves the span out, and the
/// clock goes on reporting rather than panicking.
pub fn a_stalled_hpet_is_left_out(test_config: &Path) -> Result<(), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
        BootOptions { profile: qemu::Profile::Metal, kernel_params: &["hpet-stall"], ready_marker: HPET_MOVED, ..Default::default() },
    );
    let staged_then_moved = {
        let serial = qemu.boot_log();
        let staged = serial.find(HPET_STALL_STAGED);
        let moved = serial.find(HPET_MOVED);
        match (staged, moved) {
            (Some(staged), Some(moved)) if staged < moved => Ok(serial[moved..].lines().next().unwrap_or_default().to_string()),
            _ => Err(format!("no {HPET_STALL_STAGED:?} before {HPET_MOVED:?}\n{serial}")),
        }
    }?;
    let after = qemu.drain_until(std::time::Duration::from_secs(60), |l| l.contains(CLOCK_REPORT) || l.contains("PANIC"));
    if after.contains("PANIC") || !after.contains(CLOCK_REPORT) {
        return Err(format!("the clock did not report again after it left the span out\n{after}"));
    }
    eprintln!("  [hpet_stall] {}", staged_then_moved.trim());
    Ok(())
}
