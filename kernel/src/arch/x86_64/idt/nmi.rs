//! Vector 2: where `crate::hardlockup` samples the CPU it landed on, which is
//! the one bound a CPU with `IF` clear is under: the frame's `rflags` says
//! whether it was. That path may reset from here and never return. Never
//! logs, since the interrupted context may hold the log ring's lock, and never
//! reschedules, so no preempt-count or exit-to-user check either.
//! Runs on IST2 for the `#DF` `arch::syscall`'s CPL-0/user-`rsp` window would
//! otherwise take; `PerCpu::nmi_active` guards IST2's non-reentrancy by
//! routing a second NMI to [`nested_nmi`] instead of corrupting the stack.

use core::arch::naked_asm;

use crate::arch::percpu::OFF_NMI_ACTIVE;

/// Ten pushes of eight bytes place the interrupt frame's `rip` at 80, and its
/// `rflags` two words above. `IF` is what separates a cpu that has stopped
/// taking interrupts from one that is merely slow.
const RFLAGS_OFFSET: usize = 80 + 16;

/// Before any push, the CPU's own five words start at `rsp`.
const NESTED_RIP_OFFSET: usize = 0;
const NESTED_RSP_OFFSET: usize = 24;

#[unsafe(naked)]
pub(super) extern "sysv64" fn nmi_entry() {
    naked_asm!(
        // Not routed through `arch::entry::ring3_naked_asm`: cld must run here, since `note`'s sysv64 call requires DF clear.
        "cld",
        // And from Ring 3 it arrives with that thread's `AC`, as every gate does.
        "clac",
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
        "mov rdi, [rsp + {rflags_offset}]",
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
        rflags_offset = const RFLAGS_OFFSET,
        nested_rip = const NESTED_RIP_OFFSET,
        nested_rsp = const NESTED_RSP_OFFSET,
        note = sym note,
        nested = sym nested_nmi,
    );
}

extern "sysv64" fn note(rflags: u64) {
    crate::arch::percpu::irq_took!(Nmi);
    // Before the nested-NMI staging: a hard lockup ends the machine from here.
    crate::hardlockup::sample(rflags);
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::stack_overflow_nmi() {
        crate::actuator::overflow_stack(0);
    }
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

/// `nmi-user-gs`: vector 2 entered at CPL 0 with a user's GS base live, the
/// state between an entry and its `swapgs` and after an exit's. The base
/// staged is a zeroed kernel-heap block, a kernel-half address as `WRGSBASE`
/// may give one: the handler has to count itself in this CPU's block, leave
/// the decoy untouched, and return to the base it found.
#[cfg(feature = "boot-actuators")]
pub fn user_gs_selftest() {
    use crate::arch::{cpu, percpu};
    use crate::irq_census::{self, Source};

    const MSR_KERNEL_GS_BASE: u32 = 0xC000_0102;

    let cpu = percpu::cpu_id();
    let Some(before) = irq_census::deliveries(cpu, Source::Nmi) else {
        crate::log!("nmi-user-gs: FAIL — cpu{cpu} publishes no census block");
        return;
    };
    let decoy = alloc::vec![0u64; core::mem::size_of::<percpu::PerCpu>().div_ceil(8)];
    let staged = decoy.as_ptr() as u64;
    let returned_to;
    {
        // Closed across the swapped span: a maskable gate there would read a
        // Ring 0 frame and keep the decoy.
        let _masked = crate::arch::IrqGuard::close();
        let kept = cpu::rdmsr(MSR_KERNEL_GS_BASE);
        // SAFETY: the swapped span is `int 2` alone, which touches no
        // GS-relative memory of this code's; the second `swapgs` leaves in
        // `IA32_KERNEL_GS_BASE` the base the handler returned to, and this
        // thread's user GS base goes back there before interrupts open.
        unsafe {
            cpu::wrmsr(MSR_KERNEL_GS_BASE, staged);
            core::arch::asm!("swapgs", "int 2", "swapgs");
            returned_to = cpu::rdmsr(MSR_KERNEL_GS_BASE);
            cpu::wrmsr(MSR_KERNEL_GS_BASE, kept);
        }
    }
    let after = irq_census::deliveries(cpu, Source::Nmi).unwrap_or(before);
    let touched = decoy.iter().filter(|word| **word != 0).count();
    let verdict = if after > before && touched == 0 && returned_to == staged { "PASS" } else { "FAIL" };
    crate::log!(
        "nmi-user-gs: {verdict} — vector 2 at CPL 0 under GS base {staged:#x}: cpu{cpu}'s own count \
         {before} -> {after}, {touched} decoy words written, returned to GS base {returned_to:#x}"
    );
}

/// A second NMI on a stack the first is still standing on.
/// Logs nothing: the interrupted context may be mid-publish of its own record, and one from here would garble the ring `halt_all_cpus` reads. `build/sourcegate.rs`'s `nmi_does_not_log` is the gate.
extern "sysv64" fn nested_nmi(rip: u64, rsp: u64) -> ! {
    // Let go of before the halt: its flush, finding this CPU's own fatal path
    // holding the registers, would drain raw.
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
