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

/// The blocked-task dump's NMI probe: a CPU that ignores a kick is named, and
/// then asked where it is with the one interrupt it cannot mask.
///
/// The verdict `no answer: it did not reach a scheduler pass` has three causes
/// — spinning with `IF` clear, halted with a lost kick, wedged below the
/// interrupt layer — and on the owner's T14 it named three CPUs without saying
/// which. The NMI separates them, so what this asserts is the separation: the
/// kick goes unanswered, the NMI is answered, and the `rip` it brings back
/// lands in the spin the actuator is executing.
///
/// The last assertion is the one that keeps the instrument honest. A probe that
/// reported *some* address would satisfy every other line here; only resolving
/// it against the kernel's own symbols says the report points at where the CPU
/// actually was.
///
/// **Judged on the T14 and in no QEMU guest.** The deafness is a window of the
/// actuator's own clock and the dump's kick and NMI budgets are the kernel's,
/// so a guest the host starves misses the window and reads exactly like the
/// defect this hunts.
pub fn dump_nmi_probe_on_metal(kernel: &Serial) -> Result<(), String> {
    let log = kernel.text();
    if !log.contains("=== blocked-task dump:") {
        return Err(format!("the dump never ran — is `dump-deaf-cpu` on?\n{log}"));
    }
    let silent: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("no answer: it did not reach a scheduler pass"))
        .collect();
    if silent.len() != 1 {
        return Err(format!(
            "expected exactly the deafened CPU to miss its kick, got {}:\n{}\n{log}",
            silent.len(),
            silent.join("\n"),
        ));
    }
    if log.contains("no NMI answer either") {
        return Err(format!(
            "the NMI went unanswered too. The victim spins with IF clear and an NMI is not \
             maskable by IF, so this says the NMI never reached it at all — vector 2, the ICR \
             delivery mode, or the handler.\n{log}"
        ));
    }
    let Some(rest) = log.split("NMI answered, it is here:\n").nth(1) else {
        return Err(format!("the probe reported no rip for the silent CPU\n{log}"));
    };
    let rip_line = rest.lines().next().unwrap_or("");
    if !rip_line.contains("deaf_window") {
        return Err(format!(
            "the rip resolved to `{}`, not to the spin the CPU was executing — a probe that \
             names the wrong instruction is worse than one that names none\n{log}",
            rip_line.trim(),
        ));
    }
    // And it comes back: an NMI interrupts, it does not kill. The witness has
    // to be the victim's own line, printed after it re-enables interrupts.
    // `Boot: complete` was the first attempt and is no witness at all — it is
    // printed at 225 ms, ten seconds before this window opens, and by cpu0 into
    // the boot log this drain does not even contain.
    if !log.contains("rejoined after") {
        return Err(format!(
            "the deafened CPU never said it was back — an NMI must interrupt a CPU, not kill \
             it\n{log}"
        ));
    }
    Ok(())
}
