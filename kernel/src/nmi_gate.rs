//! Per-CPU counters for `syscall_window_nmi`'s aimed-NMI storm and
//! `nmi_nested`'s staged re-entrancy hazard.
//!
//! [`observe`] classifies each NMI by its interrupted frame: `window` when
//! CPL is 0 and `rsp` is a user address — the gap in `arch::syscall`'s
//! entry/exit — `ring3` when the frame was Ring 3, `ring0` otherwise. Runs on
//! IST2: no lock, no allocation, nothing that can fault.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{apic, percpu};
use crate::sched::MAX_CPUS;

static SEEN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Syscalls each CPU has taken, which is how the storm finds its victim.
static SYSCALLS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static WINDOW: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static RING3: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Where the first window arrival was, for the report to symbolize.
static FIRST_WINDOW_RIP: AtomicU64 = AtomicU64::new(0);

/// Records one NMI arrival for the current CPU; called from `arch::idt::nmi` before anything else.
pub fn observe(rip: u64, cs: u64, rsp: u64) {
    if !crate::actuator::syscall_window_nmi() {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    // Release: storm()'s load of this word is Acquire, so it is a handshake and not only a counter.
    SEEN[me].fetch_add(1, Ordering::Release);
    if toyos_userbound::Ring::of_cs(cs).is_user() {
        RING3[me].fetch_add(1, Ordering::Relaxed);
        return;
    }
    if !crate::mm::is_kernel_addr(rsp) {
        WINDOW[me].fetch_add(1, Ordering::Relaxed);
        let _ = FIRST_WINDOW_RIP.compare_exchange(
            0,
            rip,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
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

/// Window arrivals that end the storm early: more than one, since one arrival is a fact, not a rate.
const ENOUGH: u64 = 64;

/// Wait budget per NMI before the next goes out; a delivery that misses it still counts as sent.
const DELIVERY_BUDGET_NS: u64 = 100_000;

/// Counts one syscall on this CPU while `syscall_window_nmi` is armed; called from every `syscall_dispatch`.
pub fn note_syscall() {
    if !crate::actuator::syscall_window_nmi() {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me < MAX_CPUS {
        SYSCALLS[me].fetch_add(1, Ordering::Relaxed);
    }
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

/// Storms whichever sibling CPU is spinning in `syscall`, once per boot, and logs where the NMIs landed.
pub fn storm() {
    static FIRED: AtomicBool = AtomicBool::new(false);

    let cpus = (crate::arch::smp::cpu_count() as usize).min(MAX_CPUS);
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

    // Syscall count either side of the storm proves the victim kept running throughout.
    let spun_before: u64 = SYSCALLS.iter().map(|n| n.load(Ordering::Relaxed)).sum();

    let mut sent = 0u64;
    while sent < MAX_NMIS {
        // Aimed at the victim CPU only: broadcasting would sample idle siblings instead of the window.
        let Some((cpu, _)) = victim(me, cpus) else { break };
        let seen = &SEEN[cpu];
        let before = seen.load(Ordering::Acquire);
        apic::send_nmi(cpu as u32);
        sent += 1;
        // Waits for delivery before the next send: two NMIs in flight collapse to one delivered plus one latched.
        let deadline = crate::clock::nanos_since_boot().saturating_add(DELIVERY_BUDGET_NS);
        while seen.load(Ordering::Acquire) == before
            && crate::clock::nanos_since_boot() < deadline
        {
            core::hint::spin_loop();
        }
        if total(&WINDOW) >= ENOUGH {
            break;
        }
    }

    let spun: u64 = SYSCALLS
        .iter()
        .map(|n| n.load(Ordering::Relaxed))
        .sum::<u64>()
        .saturating_sub(spun_before);
    report(sent, spun, cpus);
}

fn total(counter: &[AtomicU64; MAX_CPUS]) -> u64 {
    counter.iter().map(|c| c.load(Ordering::Relaxed)).sum()
}

/// Logs one line per CPU that saw anything, then the summary line the gate reads last; each field is key=value, read by name not position.
fn report(sent: u64, spun: u64, cpus: usize) {
    for cpu in 0..cpus {
        let seen = SEEN[cpu].load(Ordering::Relaxed);
        if seen == 0 {
            continue;
        }
        let window = WINDOW[cpu].load(Ordering::Relaxed);
        let ring3 = RING3[cpu].load(Ordering::Relaxed);
        log!(
            "syscall-window-nmi: cpu={cpu} seen={seen} window={window} ring3={ring3} ring0={}",
            seen - window - ring3,
        );
    }
    let rip = FIRST_WINDOW_RIP.load(Ordering::Relaxed);
    if rip != 0 {
        log!("syscall-window-nmi: the first window arrival was here:");
        crate::symbols::resolve_kernel(rip);
    }
    let seen = total(&SEEN);
    let window = total(&WINDOW);
    let ring3 = total(&RING3);
    log!(
        "syscall-window-nmi: sent={sent} seen={seen} window={window} ring3={ring3} ring0={} \
         spun={spun}",
        seen - window - ring3,
    );
}
