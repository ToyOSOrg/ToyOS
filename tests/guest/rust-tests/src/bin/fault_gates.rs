//! A CPU exception raised by a Ring 3 process ends that process as its
//! `<signal.h>` class of fault and leaves the machine running.
//!
//! A vector with no IDT gate does not fault the process. The CPU takes the
//! absent gate as a second, contributory fault and escalates to #DF, and
//! `double_fault_handler` halts every CPU — so before the gates went in, the
//! first arm below took the whole guest down and this test timed out.
//!
//! **The loop is the assertion.** Arm N+1 can only run because the machine
//! survived arm N, and the `echo` at the end proves a fresh process still
//! starts. That is why the arms this environment cannot raise are still worth
//! spawning: they cost 10 ms each, and the day one of them starts faulting is
//! the day its gate has to be there.

use std::os::toyos::process::ExitStatusExt;
use std::process::Command;

use toyos_abi::end::{End, Fault};

enum Expect {
    /// Measured wide and alone: the CPU raises this from Ring 3 every time, so
    /// the child must end as this fault.
    Ends(Fault),
    /// It does not, and `fault_gate_child` prints the register readback that
    /// says why. The exit code is deliberately not asserted: what these arms
    /// contribute is the machine still being here for the next one, and that is
    /// the half a missing gate would take away.
    MachineLives,
}

const ARMS: &[(&str, Expect)] = &[
    ("de", Expect::Ends(Fault::Arithmetic)),
    ("de_overflow", Expect::Ends(Fault::Arithmetic)),
    ("ud", Expect::Ends(Fault::Instruction)),
    // Both #SS routes arrive as #GP under TCG; see the child.
    ("ss", Expect::Ends(Fault::Memory)),
    ("ss_rsp", Expect::Ends(Fault::Memory)),
    // Trappable at all only because `CR0.NE` is in the declaration every CPU
    // is held to (`arch/x86_64/control_regs.rs`): with it clear the exception is
    // signalled on FERR#, which nothing in a modern machine listens to.
    ("mf", Expect::Ends(Fault::Arithmetic)),
    // TCG raises no #XM whatever MXCSR says. `CR4.OSXMMEXCPT` is declared set,
    // so this arm is `Ends(Fault::Arithmetic)` on metal and cannot be here.
    ("xm", Expect::MachineLives),
    // `CR0.AM` is declared clear, so `RFLAGS.AC` buys a Ring 3 process nothing
    // on any machine this kernel runs on — emulated or not.
    ("ac", Expect::MachineLives),
    ("pf", Expect::Ends(Fault::Memory)),
    // Each dies with the kernel's direct map in a register or an operand its
    // crash report would follow; `crash_report_reads_no_kernel_memory` reads
    // what the report said. `kernel_stack` is `#UD` with its stack there.
    ("kernel_stack", Expect::Ends(Fault::Instruction)),
    ("kernel_page", Expect::Ends(Fault::Memory)),
];

fn main() {
    for (kind, expect) in ARMS {
        let status = Command::new("/system/bin/test_rs_fault_gate_child")
            .arg(kind)
            .status()
            .unwrap_or_else(|e| panic!("failed to spawn fault_gate_child {kind}: {e}"));
        match expect {
            Expect::Ends(fault) => assert_eq!(
                End::from_word(status.into_raw()),
                Some(End::Fault(*fault)),
                "{kind} did not end as a {} fault: the CPU did not raise it, the kernel resumed \
                 it, or it read the fault as another ({status})",
                fault.name(),
            ),
            Expect::MachineLives => {}
        }
        println!("  {kind}: {status}");
    }

    let out = Command::new("/system/bin/echo")
        .arg("still alive")
        .output()
        .expect("failed to spawn a process after the fault arms");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "still alive",
        "the machine survived every fault but can no longer start a process",
    );
    println!("every Ring 3 fault left the machine up, and every raised one killed its process");
}
