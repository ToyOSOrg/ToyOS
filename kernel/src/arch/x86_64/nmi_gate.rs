//! `syscall-window-nmi`'s NMI storm on the CPU spinning in `syscall`, and
//! `nmi_nested`'s staged re-entrancy hazard.
//!
//! [`storm`] aims its NMIs at the sibling that has taken the most syscalls, one
//! at a time, each waited for on [`observe`]'s count on the CPU it landed on.
//! `observe` runs on IST2: no lock, no allocation, nothing that can fault.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{apic, percpu};
use crate::sched::MAX_CPUS;

/// NMIs each CPU has taken, which is how the storm sees one land.
static SEEN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Syscalls each CPU has taken, which is how the storm finds its victim.
static SYSCALLS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Records one NMI arrival for the current CPU; called from `arch::idt::nmi` before anything else.
pub fn observe() {
    if !crate::actuator::syscall_window_nmi() {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    SEEN[me].fetch_add(1, Ordering::Relaxed);
}

/// Stages one nested NMI entry (an early `iretq` on IST2) if `nmi_nested` is armed; one shot per boot.
pub fn stage_nested_if_armed() {
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

/// Syscalls one CPU must reach before the storm treats it as spinning.
const SPINNING_SYSCALLS: u64 = 1_000_000;

/// NMI ceiling per storm; bounds a boot where the victim stops spinning.
const MAX_NMIS: u64 = 3_000;

/// Wait budget per NMI before the next goes out; a delivery that misses it still counts as sent.
const DELIVERY_BUDGET_NS: u64 = 100_000;

/// Counts one syscall on this CPU while `syscall_window_nmi` is armed; called from every `syscall_dispatch`.
pub fn note_syscall() {
    if !crate::actuator::syscall_window_nmi() {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    SYSCALLS[me].fetch_add(1, Ordering::Relaxed);
}

/// Sibling CPU with the most syscalls, recomputed each round since the scheduler may move the spinner.
fn victim(me: usize, cpus: usize) -> Option<(usize, u64)> {
    SYSCALLS
        .iter()
        .enumerate()
        .take(cpus)
        .filter(|&(cpu, _)| cpu != me)
        .map(|(cpu, n)| (cpu, n.load(Ordering::Relaxed)))
        .max_by_key(|&(_, n)| n)
}

/// Storms whichever sibling CPU is spinning in `syscall`, once per boot.
pub fn storm() {
    static FIRED: AtomicBool = AtomicBool::new(false);

    let cpus = (crate::smp::cpu_count() as usize).min(MAX_CPUS);
    let me = percpu::cpu_id() as usize;
    if cpus < 2 {
        return;
    }
    // Trigger is the victim's syscall count, not a wall clock: a clock could fire before the spinner starts and waste the look.
    match victim(me, cpus) {
        Some((_, taken)) if taken >= SPINNING_SYSCALLS => {}
        _ => return,
    }
    // Checked before this swap so a premature look can't spend the one shot.
    if FIRED.swap(true, Ordering::AcqRel) {
        return;
    }

    for _ in 0..MAX_NMIS {
        let Some((cpu, _)) = victim(me, cpus) else { break };
        let seen = &SEEN[cpu];
        let before = seen.load(Ordering::Relaxed);
        apic::send_nmi(cpu as u32);
        // Waits for delivery before the next send: two NMIs in flight collapse to one delivered plus one latched.
        let deadline = crate::clock::nanos_since_boot().saturating_add(DELIVERY_BUDGET_NS);
        while seen.load(Ordering::Relaxed) == before
            && crate::clock::nanos_since_boot() < deadline
        {
            core::hint::spin_loop();
        }
    }
}
