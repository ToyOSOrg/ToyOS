//! Which console lines say something died, and whose death each is: the one
//! vocabulary the test harness's waits and the launch's console relay
//! (`build/vmconsole.rs`) both read.

use crate::is_kernel_line;

/// Whose death a console line reports.
///
/// The distinction is not decoration — it is the whole of what the harness's
/// `QemuInstance::run_test` was missing. A machine that has halted answers
/// nothing else the run asks; a process that died is what half the suite is
/// *for*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Died {
    /// The kernel itself. Every path that writes one of these words ends at
    /// `panic::halt_all_cpus`.
    Kernel,
    /// A process the kernel killed: a Ring 3 fault, reported by name in
    /// `kernel/src/arch/x86_64/idt/exceptions.rs`. The machine is fine — a test whose
    /// whole subject is a process dying produces these deliberately. Before a boot's ready
    /// marker it still ends the boot: whatever died was the supervisor or one of its
    /// children, and nothing left is going to reach the marker.
    Faulted,
    /// A process that ended itself — its own panic handler wrote the line
    /// (`sdk/libc/src/lib.rs`, or the std fork's). Never the machine's
    /// business, and not even always the boot's: `sshserver` lost a race with
    /// `netstack`'s teardown on a NIC-less machine and panicked across four
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
/// somebody else*. So who is speaking picks the column —
/// [`crate::is_kernel_line`] — and a spelling read out of the wrong column is
/// exactly the bug this table exists to make unwriteable. Nothing
/// else in `tests/common/qemu.rs` knows these words; `one_vocabulary` in
/// `tests/checks.rs` is what keeps it that way.
///
/// The two columns are equal for every spelling no program in this tree writes,
/// and that is deliberate rather than lazy: the console is not line-atomic, so
/// a program's unterminated write can be spliced ahead of a kernel record and
/// take the kernel's head off the front of the assembled line. A word
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
pub const DEATHS: &[(&str, Died, Died)] = &[
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
    // kernel/src/iommu/fault.rs — a fault on a function this kernel drives has
    // nobody to hand it to, so the handler halts. One a *process* drives says
    // `owner=process`, and one naming no enumerated function `owner=none`, and
    // the machine goes on, which is why the needle is the owner rather than
    // the fault.
    ("iommu: DMA FAULT owner=kernel", Died::Kernel, Died::Kernel),
    // kernel/src/main.rs — a panic that landed on a CPU already inside a fault
    // or a report. The rest of the line is `panic::last_words`: which of the
    // four states it found, what that first crash was, and where the second
    // one is. It goes out the UART port first and then as a record, so a
    // capture can carry it twice.
    ("DOUBLE PANIC", Died::Kernel, Died::Kernel),
    // kernel/src/main.rs — the reentry guard, written straight out the UART
    // port with no prefix. It reaches the 16550 log
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
/// The one answer. The harness's `wait_for_ready` ends a boot on a death of
/// any kind the machine cannot come back from; `run_test` and `await_guest`
/// end a run on [`Died::Kernel`] alone, because a program is allowed to die
/// without taking the machine with it; `Serial::must_be_clean` refuses a
/// capture carrying one; and the console relay shows a kernel's death however
/// early it came. One vocabulary, and no way for them to disagree about a
/// spelling.
pub fn died(line: &str) -> Option<Died> {
    let (_, by_kernel, by_anyone) = DEATHS.iter().find(|(word, _, _)| line.contains(word))?;
    Some(if is_kernel_line(line) { *by_kernel } else { *by_anyone })
}
