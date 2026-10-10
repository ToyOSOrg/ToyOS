//! Vector 2: where `crate::hardlockup` samples the CPU it landed on, which is
//! the one bound a CPU with `IF` clear is under: the frame's `rip`, `rsp` and
//! `rflags` are what a machine whose every CPU stopped taking interrupts has
//! left to say. That path may seal a record and reset from here and never
//! return. Never logs, since the interrupted context may hold the log ring's
//! lock, and never reschedules, so no preempt-count or exit-to-user check
//! either.
//! Runs on IST2 for the `#DF` `arch::syscall`'s CPL-0/user-`rsp` window would
//! otherwise take; `PerCpu::nmi_active` guards IST2's non-reentrancy by
//! routing a second NMI to [`nested_nmi`] instead of corrupting the stack.

use core::arch::naked_asm;

use crate::arch::percpu::OFF_NMI_ACTIVE;

/// Ten pushes of eight bytes place the interrupt frame's `rip` here.
const RIP_OFFSET: usize = 80;
const RSP_OFFSET: usize = RIP_OFFSET + 24;
/// `rflags`' `IF` is what separates a cpu that has stopped taking interrupts from one that is merely slow.
const RFLAGS_OFFSET: usize = RIP_OFFSET + 16;

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
        "mov rsi, [rsp + {rsp_offset}]",
        "mov rdx, [rsp + {rflags_offset}]",
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
        rsp_offset = const RSP_OFFSET,
        rflags_offset = const RFLAGS_OFFSET,
        nested_rip = const NESTED_RIP_OFFSET,
        nested_rsp = const NESTED_RSP_OFFSET,
        note = sym note,
        nested = sym nested_nmi,
    );
}

extern "sysv64" fn note(rip: u64, rsp: u64, rflags: u64) {
    crate::arch::percpu::irq_took!(Nmi);
    // Before the nested-NMI staging: a hard lockup ends the machine from here,
    // and nothing stages a second NMI onto a frame that is sealing a record.
    crate::hardlockup::sample(rip, rsp, rflags);
    #[cfg(feature = "boot-actuators")]
    stage_nested_if_armed();
}

/// Stages one nested NMI entry (an early `iretq` on IST2) if `nmi_nested` is armed; one shot per boot.
#[cfg(feature = "boot-actuators")]
fn stage_nested_if_armed() {
    use core::sync::atomic::{AtomicBool, Ordering};

    use crate::arch::{apic, percpu};

    if !crate::actuator::nmi_nested() {
        return;
    }
    static STAGED: AtomicBool = AtomicBool::new(false);
    if STAGED.swap(true, Ordering::AcqRel) {
        return;
    }
    apic::send_nmi(percpu::cpu_id());
    // No nomem/nostack: the block pushes five words and the NMI it admits may touch any memory.
    // SAFETY: the frame is this CPU's own ss/rsp/rflags/cs with rip = the label below, so `iretq` resumes here with control flow and the stack unchanged.
    unsafe {
        core::arch::asm!(
            "mov {tmp}, rsp",
            "xor {seg:e}, {seg:e}",
            "mov {seg:x}, ss",
            "push {seg}",
            "push {tmp}",
            "pushfq",
            "mov {seg:x}, cs",
            "push {seg}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "iretq",
            "2:",
            tmp = out(reg) _,
            seg = out(reg) _,
        );
    }
}

/// A second NMI on a stack the first is still standing on.
/// Logs nothing: the interrupted context may be mid-publish of its own record, and one from here would garble the ring `halt_all_cpus` reads. `src/sourcegate.rs`'s `nmi_does_not_log` is the gate.
extern "sysv64" fn nested_nmi(rip: u64, rsp: u64) -> ! {
    // Let go of before the halt: its flush, finding this CPU's own fatal path
    // holding the registers, would drain raw and leave virtio-console out.
    {
        let mut uart = crate::drivers::serial::panic_registers();
        uart.write(b"\n[nmi] NESTED NMI on cpu ");
        uart.dec(u64::from(crate::arch::percpu::cpu_id()));
        uart.write(b": a second NMI entered while IST2 was still in use.\n[nmi]   rip=");
        uart.hex(rip);
        uart.write(b" rsp=");
        uart.hex(rsp);
        uart.write(b"\n[nmi]   the outer handler's frame is gone; the machine stops here.\n");
    }
    crate::panic::halt_all_cpus()
}
