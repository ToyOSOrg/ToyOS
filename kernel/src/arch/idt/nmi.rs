//! Vector 2: probes a CPU that isn't answering an IPI kick by writing its
//! `rip` into a lock-free per-CPU slot for `sched::dump` to read from
//! ordinary context. Never logs, since the interrupted context may hold the
//! log ring's lock, and never reschedules, so no preempt-count or
//! exit-to-user check either.
//! Runs on IST2 for the `#DF` `arch::syscall`'s CPL-0/user-`rsp` window would
//! otherwise take; `PerCpu::nmi_active` guards IST2's non-reentrancy by
//! routing a second NMI to [`nested_nmi`] instead of corrupting the stack.

use core::arch::naked_asm;

use crate::arch::percpu::OFF_NMI_ACTIVE;

/// Ten pushes of eight bytes place the interrupt frame's `rip` here.
const RIP_OFFSET: usize = 80;
/// `cs` and `rsp` say whether the NMI landed in the CPL-0/user-`rsp` window the IST exists for.
const CS_OFFSET: usize = RIP_OFFSET + 8;
const RSP_OFFSET: usize = RIP_OFFSET + 24;

/// Before any push, the CPU's own five words start at `rsp`.
const NESTED_RIP_OFFSET: usize = 0;
const NESTED_RSP_OFFSET: usize = 24;

#[unsafe(naked)]
pub(super) extern "sysv64" fn nmi_entry() {
    naked_asm!(
        // Not routed through `arch::entry::ring3_naked_asm`: cld must run here, since `note`'s sysv64 call requires DF clear.
        "cld",
        // Checked before any push: cmp sets flags, and iretq restores RFLAGS from the frame, so flags are free here.
        "cmp dword ptr gs:[{active}], 0",
        "jne 2f",
        "mov dword ptr gs:[{active}], 1",
        "push rax",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push rbp",
        "mov rdi, [rsp + {rip_offset}]",
        "mov rsi, [rsp + {cs_offset}]",
        "mov rdx, [rsp + {rsp_offset}]",
        "mov rbp, rsp",
        "and rsp, -16",
        "call {note}",
        "mov rsp, rbp",
        "pop rbp",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rax",
        // Cleared before iretq, never after: delivery is blocked until iretq retires, so no second NMI can find it clear while this one is still on the stack.
        "mov dword ptr gs:[{active}], 0",
        // No EOI: an NMI isn't delivered through the IRR, and acknowledging one would clear an unrelated interrupt.
        "iretq",
        // This frame already overwrote the outer handler's stack: nothing to preserve, so read the two words before aligning.
        "2:",
        "mov rdi, [rsp + {nested_rip}]",
        "mov rsi, [rsp + {nested_rsp}]",
        "and rsp, -16",
        "call {nested}",
        "ud2",
        active = const OFF_NMI_ACTIVE,
        rip_offset = const RIP_OFFSET,
        cs_offset = const CS_OFFSET,
        rsp_offset = const RSP_OFFSET,
        nested_rip = const NESTED_RIP_OFFSET,
        nested_rsp = const NESTED_RSP_OFFSET,
        note = sym note,
        nested = sym nested_nmi,
    );
}

/// Loads all three words in both builds so the observer and shipping handler share one frame layout.
extern "sysv64" fn note(rip: u64, cs: u64, rsp: u64) {
    crate::irq_census::irq_took!(Nmi);
    #[cfg(not(feature = "boot-actuators"))]
    let _ = (cs, rsp);
    #[cfg(feature = "boot-actuators")]
    crate::nmi_gate::observe(rip, cs, rsp);
    crate::sched::dump::note_nmi(rip);
    #[cfg(feature = "boot-actuators")]
    crate::nmi_gate::stage_nested_if_armed();
}

/// A second NMI on a stack the first is still standing on.
/// Logs nothing: the interrupted context may be mid-publish of its own record, and one from here would garble the ring `halt_all_cpus` reads. `src/sourcegate.rs`'s `nmi_does_not_log` is the gate.
/// `panic_raw` takes no lock, so it can't be blocked by whatever either context held.
extern "sysv64" fn nested_nmi(rip: u64, rsp: u64) -> ! {
    let serial = crate::drivers::serial::panic_raw;
    serial(b"\n[nmi] NESTED NMI on cpu ");
    crate::drivers::serial::panic_raw_dec(u64::from(crate::arch::percpu::cpu_id()));
    serial(b": a second NMI entered while IST2 was still in use.\n[nmi]   rip=");
    crate::drivers::serial::panic_raw_hex(rip);
    serial(b" rsp=");
    crate::drivers::serial::panic_raw_hex(rsp);
    serial(b"\n[nmi]   the outer handler's frame is gone; the machine stops here.\n");
    crate::arch::apic::halt_all_cpus()
}
