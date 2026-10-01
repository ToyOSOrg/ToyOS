//! Assertions over what the guest said, that cannot pass on a dead channel.
//!
//! The capture itself is not new: [`QemuInstance::boot_log`] has always held
//! every console line up to the ready marker, and nineteen call sites read it.
//! What was missing is a vocabulary — every one of those sites hand-rolls
//! `contains` against a `String`, and seven of them do it in the shape
//!
//! ```ignore
//! for bad in ["PANIC:", "panicked at"] {
//!     if log.contains(bad) { return Err(..) }
//! }
//! ```
//!
//! which is a claim about nothing if `log` is empty. A capture that silently
//! comes back empty turns every such scan green. That is the failure this type
//! exists to make impossible: a negative assertion first has to prove the
//! channel carried anything at all.
//!
//! Liveness is "the kernel wrote at least one line". Every configuration that
//! has a text channel logs before anything a test asserts on can happen —
//! including a guest that dies at 0.068 s, which is the earliest failure in
//! the suite — so zero kernel lines means the channel broke, never that the
//! boot was clean.
//!
//! This is the text channel. The framebuffer is `screen.rs`, deliberately the
//! only thing in the suite that reads pixels.

use super::qemu::is_kernel_line;

/// Whose death a console line reports.
///
/// The distinction is not decoration — it is the whole of what
/// [`QemuInstance::run_test_paced`] was missing. A machine that has halted
/// answers nothing else the run asks; a process that died is what half this
/// suite is *for*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Died {
    /// The kernel itself. Every path that writes one of these words ends at
    /// `panic::halt_all_cpus`.
    Kernel,
    /// A process the kernel killed: a Ring 3 fault, reported by name in
    /// `kernel/src/arch/x86_64/idt/exceptions.rs`. The machine is fine — a test whose
    /// whole subject is a process dying (`handle_kill_policy` and every
    /// `faults.rs` probe) produces these deliberately. Before a boot's ready
    /// marker it still ends the boot: whatever died was `init` or one of its
    /// children, and nothing left is going to reach the marker.
    Faulted,
    /// A process that ended itself — its own panic handler wrote the line
    /// (`userland/libc/src/lib.rs`, or the std fork's). Never the machine's
    /// business, and not even always the boot's: `sshd` lost a race with
    /// `netd`'s teardown on a NIC-less machine and panicked across four
    /// recorded boots that then came up perfectly, which is why a boot wait
    /// must not end on one.
    Panicked,
}

/// Every spelling of a death this tree produces, and what it means from each of
/// the two speakers a console carries.
///
/// **One table, two columns, because the spelling is only half the answer.**
/// `PANIC:` is the header `crash_report_panic` writes and it is also whatever a
/// program chooses to print; `SEGFAULT` is written by the kernel *about
/// somebody else*. So who is speaking picks the column — [`is_kernel_line`],
/// the harness's one definition of that — and a spelling read out of the wrong
/// column is exactly the bug this table exists to make unwriteable. Nothing
/// else in `tests/common/qemu.rs` knows these words; `one_vocabulary` in
/// `tests/toyos.rs` is what keeps it that way.
///
/// The two columns are equal for every spelling no program in this tree writes,
/// and that is deliberate rather than lazy: the console is not line-atomic, so
/// a program's unterminated write can be spliced ahead of a kernel record and
/// take the `[kernel …]` prefix off the front of the assembled line. A word
/// only the kernel says is still the kernel's however the line was built.
///
/// Order matters where one spelling contains another: `PANIC:` is looked for
/// before `panicked at`, so the header of a kernel crash report is read as the
/// header and not as its own second line — and `EARLY PANIC:` needs no row,
/// because it carries `PANIC:` inside it.
///
/// What this table does **not** claim, because the console cannot say it:
/// `fatal_exception`'s recursive arm writes `FAULT rip=… RECURSIVE` and then
/// halts if the fault was the kernel's and kills the process if it was a
/// program's — and it is the only line either case produces, because that arm
/// skips `crash_report`. The non-recursive arm prints the same `FAULT rip=…`
/// before every ordinary Ring 3 segfault, of which this suite stages many
/// deliberately. So the spelling is ambiguous both ways and is left out; a
/// recursive kernel fault is still found by the guard, one silent ceiling later.
pub(crate) const DEATHS: &[(&str, Died, Died)] = &[
    // spelling             the kernel wrote it   anybody else wrote it
    // kernel/src/arch/x86_64/idt/exceptions.rs — a Ring 0 exception. Always fatal.
    ("KERNEL PANIC", Died::Kernel, Died::Kernel),
    // `double_fault_handler`, which is `-> !` and ends at `halt_all_cpus`. It
    // writes none of the words above it, which is how a staged `#DF` inside a
    // `run_test` was still reported as a stall after the rest of this table
    // existed — the measurement that put this row here.
    ("DOUBLE FAULT", Died::Kernel, Died::Kernel),
    // `machine_check_handler`, the one exception a Ring 3 frame does not make
    // the process's fault. Also `-> !`.
    ("MACHINE CHECK", Died::Kernel, Died::Kernel),
    // kernel/src/arch/x86_64/vtd/fault.rs — a fault on a stream this kernel drives
    // has nobody to hand it to, so the handler halts. One a *process* drives
    // says `owner=slot<N>` and the machine goes on, which is why the needle is
    // the owner rather than the fault.
    ("iommu: DMA FAULT owner=kernel", Died::Kernel, Died::Kernel),
    // kernel/src/main.rs — a panic that landed on a CPU already inside a fault
    // or a report. The rest of the line is `panic::last_words`: which of the
    // four states it found, what that first crash was, and where the second
    // one is. It goes out the UART port first and then as a record, so a
    // capture can carry it twice.
    ("DOUBLE PANIC", Died::Kernel, Died::Kernel),
    // kernel/src/main.rs — the reentry guard, written straight out the UART
    // port with no lock and therefore with no prefix. It reaches the 16550 log
    // rather than the console, and is here so that a capture carrying it is
    // never read as anything else.
    ("PANIC REENTRY", Died::Kernel, Died::Kernel),
    // kernel/src/arch/x86_64/idt/exceptions.rs `crash_report_panic` — a Rust `panic!`.
    ("PANIC:", Died::Kernel, Died::Panicked),
    // `PanicInfo`'s `Display` newlines this out of the record above, so the
    // kernel writes it too — and so does every program's panic handler.
    ("panicked at", Died::Kernel, Died::Panicked),
    ("libc panic:", Died::Panicked, Died::Panicked),
    // kernel/src/arch/x86_64/idt/exceptions.rs — a Ring 3 fault, by name.
    ("SEGFAULT", Died::Faulted, Died::Faulted),
    ("SIGILL tid=", Died::Faulted, Died::Faulted),
    ("SIGFPE tid=", Died::Faulted, Died::Faulted),
    ("SIGBUS tid=", Died::Faulted, Died::Faulted),
    ("FATAL tid=", Died::Faulted, Died::Faulted),
];

/// What this console line says died, if anything.
///
/// The one answer. `wait_for_ready` ends a boot on a death of any kind the
/// machine cannot come back from; `run_test_paced` and `await_guest` end a run
/// on [`Died::Kernel`] alone, because a program is allowed to die without
/// taking the machine with it; and [`Serial::must_be_clean`] refuses a capture
/// carrying one. Four questions, one vocabulary, and no way for them to
/// disagree about a spelling.
pub fn died(line: &str) -> Option<Died> {
    let (_, by_kernel, by_anyone) = DEATHS.iter().find(|(word, _, _)| line.contains(word))?;
    Some(if is_kernel_line(line) { *by_kernel } else { *by_anyone })
}

/// The first line of a capture on which the kernel said it was dying.
///
/// For a wait that holds the whole capture rather than reading a line at a time
/// — [`super::qemu::await_guest`] is the one — and for the same reason: a guest
/// that has halted every CPU has stopped for a reason that is written down, and
/// a verdict of "it went quiet" throws that reason away.
pub fn kernel_death(capture: &str) -> Option<&str> {
    capture.lines().find(|l| died(l) == Some(Died::Kernel))
}

/// How much of a dying kernel's own account a verdict carries.
///
/// A fatal report is a header, a register dump, a page walk and a bounded
/// backtrace, and every CPU is being halted around it, so very little else
/// reaches the console after it. Eighty lines holds one whole report with room
/// to spare. The *first* eighty, where `kernel_account` in `tests/toyos.rs`
/// keeps the *last* sixty of what a killed process left behind: that one reads
/// a machine that is still running and its tail says how the process ended,
/// this one starts at the end and the head is the whole of what says why.
pub(crate) const REPORT_LINES: usize = 80;

/// Everything the guest said from the line the kernel announced its own death.
///
/// **The artefact, and a verdict that named the death used to throw it away.**
/// On 2026-08-18 a `DOUBLE FAULT on CPU 1` took a twelve-wide suite's guest
/// down; `double_fault_handler` writes its whole report on IST1 — the header,
/// `cr2`, the `#DF` frame, a page walk, a kernel backtrace and a scan of the
/// original stack for the frame that started the chain — and the failing test's
/// arm printed `result.stdout`, which is the *userland* half of the capture and
/// carried two daemon lines. The report was in `result.serial` and nothing read
/// it (`issues/kernel/a-double-fault-on-cpu-1-under-a-wide-suite.md`).
///
/// The death line is where it starts, because everything before it is the run
/// going normally and the point of a bound is that the report survives it.
/// Truncation says how much it dropped rather than dropping it in silence.
///
/// One capture, taken in the order the guest wrote it: [`super::qemu::WaitVerdict`]
/// hands the halves of a test's window over in that order, so the first kernel
/// death in the window is the one reported on.
pub fn death_report(capture: &str) -> Option<String> {
    // `split_inclusive` rather than `lines`, because the offset of the line is
    // what the report starts at and `lines` throws it away.
    let mut at = 0;
    for line in capture.split_inclusive('\n') {
        if died(line) == Some(Died::Kernel) {
            let all: Vec<&str> = capture[at..].lines().collect();
            let kept = all.len().min(REPORT_LINES);
            let head = if all.len() > kept {
                format!("(the first {kept} of the {} lines that followed)\n", all.len())
            } else {
                String::new()
            };
            return Some(format!("{head}{}", all[..kept].join("\n")));
        }
        at += line.len();
    }
    None
}

pub struct Serial {
    text: String,
    /// What produced it, for error messages that name the channel.
    source: String,
}

impl Serial {
    /// For text a test collected itself — a `drain_serial` window, the 16550
    /// file of a guest that died early.
    pub fn named(source: &str, text: impl Into<String>) -> Self {
        Self { text: text.into(), source: source.to_string() }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn kernel_lines(&self) -> usize {
        self.text.lines().filter(|l| is_kernel_line(l)).count()
    }

    /// A line carrying a kernel prefix somewhere other than its start, which
    /// is the virtio-console's missing line atomicity showing up in the
    /// capture: `log!` and a userspace `println!` interleave mid-word (see
    /// `issues/`). Reported rather than repaired — a needle that went
    /// missing because it was split in half should say so instead of looking
    /// like the guest never said it.
    pub fn interleaved(&self) -> Option<&str> {
        self.text
            .lines()
            .find(|l| !is_kernel_line(l) && l.contains(toyos_build::kernelconsole::HEAD))
    }

    /// The channel carried something the kernel wrote.
    pub fn alive(&self) -> Result<(), String> {
        if self.kernel_lines() == 0 {
            return Err(format!(
                "the {} carried no kernel output at all ({} bytes): every assertion \
                 below it would be a claim about nothing",
                self.source,
                self.text.len()
            ));
        }
        Ok(())
    }

    /// The guest said this. Returns the whole line, so a caller that needs a
    /// field out of it parses from the line rather than re-scanning the blob.
    pub fn must_say(&self, needle: &str) -> Result<&str, String> {
        if let Some(line) = self.text.lines().find(|l| l.contains(needle)) {
            return Ok(line);
        }
        let note = match self.interleaved() {
            Some(l) => format!(
                "\nnote: the {} has interleaved lines, so this needle may have been \
                 split across one — first: {l:?}",
                self.source
            ),
            None => String::new(),
        };
        Err(format!("{needle:?} never reached the {}:{note}\n{}", self.source, self.text))
    }

    /// The guest said this **after** it said `marker`.
    ///
    /// A whole-capture scan answers with the earliest line of that shape,
    /// whoever wrote it and whenever — and for a test that *stages* the event it
    /// is looking for, the earliest line is the wrong one whenever anything else
    /// on the machine can produce the same shape. `i8042_undecoded_bytes`
    /// injects an undecodable key once the guest prints `===I8042_READY===` and
    /// then read the first `nothing decoded` line in its capture as the answer;
    /// the driver's own bring-up can produce one before that marker, and on a
    /// laptop a real spurious interrupt can too.
    ///
    /// The marker is what the injection was timed off, so it is the boundary the
    /// test actually knows — no host clock is involved, and a stranger line
    /// before it can no longer be read as the test's own. A missing marker is a
    /// failure rather than a fallback to the whole capture: the anchor going
    /// missing is exactly when the loose scan would look like it worked.
    pub fn must_say_after(&self, marker: &str, needle: &str) -> Result<&str, String> {
        let Some(at) = self.text.find(marker) else {
            return Err(format!(
                "{marker:?} — the line {needle:?} would have to follow — never reached the {}:\n{}",
                self.source, self.text
            ));
        };
        // From the end of the marker's own line, so a marker and a needle that
        // share one line (the console splices them when a writer left no
        // newline) is not read as the needle arriving first.
        let after = self.text[at..].find('\n').map_or(self.text.len(), |n| at + n + 1);
        if let Some(line) = self.text[after..].lines().find(|l| l.contains(needle)) {
            return Ok(line);
        }
        let earlier = match self.text[..after].lines().find(|l| l.contains(needle)) {
            Some(l) => format!(
                "\nnote: one arrived *before* {marker:?} and is not this test's — first: {l:?}"
            ),
            None => String::new(),
        };
        Err(format!(
            "{needle:?} never reached the {} after {marker:?}:{earlier}\n{}",
            self.source, self.text
        ))
    }

    /// The guest did not say this — and the channel was working, so the
    /// absence means something.
    pub fn must_not_say(&self, needle: &str) -> Result<(), String> {
        self.alive()?;
        match self.text.lines().find(|l| l.contains(needle)) {
            Some(line) => Err(format!(
                "{needle:?} on a {} that should not have it: {line:?}\n{}",
                self.source, self.text
            )),
            None => Ok(()),
        }
    }

    /// Nothing panicked.
    ///
    /// Read straight off [`DEATHS`] rather than out of a second list beside it:
    /// what this refuses is every spelling the *kernel* uses about itself,
    /// wherever on the line it appears. A process dying is not this assertion's
    /// business — `must_not_say("SEGFAULT")` is a thing a caller says when it
    /// means it.
    pub fn must_be_clean(&self) -> Result<(), String> {
        for (bad, by_kernel, _) in DEATHS {
            if *by_kernel != Died::Kernel {
                continue;
            }
            self.must_not_say(bad)?;
        }
        for bad in NEVER_CLEAN {
            self.must_not_say(bad)?;
        }
        Ok(())
    }
}

/// Lines a boot survives and still must not print.
///
/// [`DEATHS`] is what reads a line that took the machine down. These took
/// nothing down and are records of something that should not have happened at
/// all — so the only capture allowed to hold one is the capture of the test
/// that staged it, and every other boot in the estate reds.
const NEVER_CLEAN: &[&str] = &[
    // kernel/src/arch/x86_64/vtd/fault.rs — a function a *process* drives reached an
    // address its own domain does not map. The machine goes on and the claim
    // refuses every later call, so this is not a death; it is a driver whose
    // descriptors are wrong, and a netd that did it on every boot would
    // otherwise pass everywhere. `userdev_dma_fault` stages exactly one on
    // purpose and reads it with `must_say`.
    "iommu: DMA FAULT owner=slot",
];
