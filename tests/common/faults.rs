//! The double fault path, which is the one that has to survive being the
//! thing that reports on itself.
//!
//! #DF is the only vector with an IST, so it is the only stack in the kernel
//! whose overflow is invisible: it is heap memory, it is written while the
//! crash report is being produced, and the corruption lands under whatever
//! the allocator handed out next. A test that only asserted "the report
//! appeared" would have passed throughout -- the report *did* appear, and it
//! scribbled on the heap on its way out.
//!
//! So the assertion is the kernel's own high-water measurement, taken after
//! `panic_flush` (the deepest point) and written straight to the UART rather
//! than through the log ring, which is one of the things an overflow may have
//! corrupted.

use super::serial::Serial;

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
