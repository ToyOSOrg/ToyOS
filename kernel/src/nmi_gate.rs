//! Per-CPU counters for `syscall_window_nmi`'s aimed-NMI storm and
//! `nmi_nested`'s staged re-entrancy hazard.
//!
//! [`observe`] classifies each NMI by its interrupted frame: `window` when
//! CPL is 0 and `rsp` is a user address — the gap in `arch::syscall`'s
//! entry/exit — `ring3` when the frame was Ring 3, `ring0` otherwise. Runs on
//! IST2: no lock, no allocation, nothing that can fault. Every `window`
//! arrival's `rip` is held against the entry's own extent, and one outside it
//! is counted apart: the classifier's check, on every arrival.
//!
//! **The first arrival is arranged; the rest are sprayed.** Where a sprayed NMI
//! lands is the accelerator's answer — under KVM it is injected wherever the
//! host's kick exited, which can be outside the window for a whole storm — so
//! [`storm`] first holds the victim inside the entry through [`hold`]'s word,
//! which the entry acknowledges and spins on at CPL 0 on the user's stack, and
//! aims one NMI at it there. That arrival is the premise every verdict on the
//! window rests on: with IST2 it is the window arrival every host witnesses,
//! and without it the `#DF` the control stages.
//!
//! **The held arrival is the one taken under the hold, and the CPU that took it
//! is who says so.** [`observe`] counts it only when its own CPU's word has
//! both bits at the NMI, and keeps its frame apart from the sprayed ones';
//! [`release`]'s line is the hold's end in the record, for the control whose
//! held arrival never reaches `observe`.
//!
//! **A hold ends without its asker.** The ask carries a budget of turns and
//! the entry spends one a turn, so a CPU whose asker died leaves the window by
//! itself with [`hold::EXPIRED`] in its word, and [`note_syscall`] is where it
//! says so.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{apic, percpu};
use crate::sched::MAX_CPUS;

static SEEN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Syscalls each CPU has taken, which is how the storm finds its victim.
static SYSCALLS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static WINDOW: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static RING3: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
/// Window arrivals whose `rip` was outside `arch::syscall`'s entry, and where the first one was.
static OUTSIDE: AtomicU64 = AtomicU64::new(0);
static FIRST_OUTSIDE_RIP: AtomicU64 = AtomicU64::new(0);
/// Where the first sprayed window arrival was, for the report to symbolize: the sample of the classifier, which the held arrival — inside the entry by arrangement — is not.
static FIRST_SPRAYED_RIP: AtomicU64 = AtomicU64::new(0);
/// Window arrivals taken under an acknowledged hold, and the last one's frame.
static HELD_TAKEN: AtomicU64 = AtomicU64::new(0);
static HELD_RIP: AtomicU64 = AtomicU64::new(0);
static HELD_FRAME_RSP: AtomicU64 = AtomicU64::new(0);

/// The bits of `PerCpu::nmi_hold`, the word `arch::syscall`'s entry spins on inside its window.
pub mod hold {
    /// Set by the storm to ask the next entry on that CPU to hold, cleared by the storm to release it; the entry spins while it is set and only while it is set.
    pub const ASKED: u64 = 1;
    /// Set by the entry once it holds; a claim only while `ASKED` is still set, since the entry leaves the moment that clears.
    pub const HELD: u64 = 2;
    /// The whole word, stored by an entry whose budget ran out under a standing ask; cleared by that CPU's `note_syscall` once it has said so.
    pub const EXPIRED: u64 = 4;
    /// One turn of the entry's spin, in the budget the word carries above the flags.
    pub const SPIN: u64 = 1 << 8;
    /// Turns an ask grants. A count and not a time, because the entry can read no clock without a register; a bound, not a measurement.
    pub const TURNS: u64 = 1 << 25;
    /// What the storm stores to ask.
    pub const ASK: u64 = ASKED | TURNS * SPIN;
}

/// Each CPU's hold word and the slot its entry parks the user `rsp` in, as addresses; published by `percpu::alloc_percpu` before that CPU runs, `irq_census`'s way.
static HOLD_WORDS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static USER_RSPS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Publishes one CPU's two words; called from `percpu::alloc_percpu` before that CPU runs an instruction.
pub fn publish(cpu_id: u32, hold: *const AtomicU64, user_rsp: *const u64) {
    let cpu = cpu_id as usize;
    let (Some(hold_slot), Some(rsp_slot)) = (HOLD_WORDS.get(cpu), USER_RSPS.get(cpu)) else {
        return;
    };
    // The hold word last: it is the one a reader checks, and finding it is what says the other is there.
    rsp_slot.store(user_rsp as u64, Ordering::Release);
    hold_slot.store(hold as u64, Ordering::Release);
}

/// One CPU's hold word, or `None` for a CPU never built.
fn hold_word(cpu: usize) -> Option<&'static AtomicU64> {
    let addr = HOLD_WORDS.get(cpu)?.load(Ordering::Acquire);
    if addr == 0 {
        return None;
    }
    // SAFETY: `PerCpu::nmi_hold`'s address, published by `percpu::alloc_percpu` for a block that lives as long as the machine; an atomic is shared across CPUs by design.
    Some(unsafe { &*(addr as *const AtomicU64) })
}

/// The user `rsp` a held CPU's entry parked, read while the hold is acknowledged and not yet released — after the entry's store of it and before its next.
fn held_user_rsp(cpu: usize) -> u64 {
    let addr = USER_RSPS[cpu].load(Ordering::Acquire);
    // SAFETY: `PerCpu::user_rsp`'s address, published before the hold word the caller found, for a block that lives as long as the machine; the caller's acknowledged hold is what keeps the owning CPU from writing it, and the read is volatile because the writer is that CPU's entry stub.
    unsafe { core::ptr::read_volatile(addr as *const u64) }
}

/// Records one NMI arrival for the current CPU; called from `arch::idt::nmi` before anything else.
pub fn observe(rip: u64, cs: u64, rsp: u64) {
    if !crate::actuator::syscall_window_nmi() {
        return;
    }
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    if toyos_userbound::Ring::of_cs(cs).is_user() {
        RING3[me].fetch_add(1, Ordering::Relaxed);
    } else if !crate::mm::is_kernel_addr(rsp) {
        WINDOW[me].fetch_add(1, Ordering::Relaxed);
        if !crate::arch::syscall::entry_extent().contains(&rip) {
            OUTSIDE.fetch_add(1, Ordering::Relaxed);
            let _ = FIRST_OUTSIDE_RIP.compare_exchange(0, rip, Ordering::Relaxed, Ordering::Relaxed);
        }
        // This CPU's own word, read by the CPU the NMI interrupted: both bits say the entry was spinning on it when the NMI came.
        const BOTH: u64 = hold::ASKED | hold::HELD;
        if hold_word(me).is_some_and(|word| word.load(Ordering::Acquire) & BOTH == BOTH) {
            HELD_RIP.store(rip, Ordering::Relaxed);
            HELD_FRAME_RSP.store(rsp, Ordering::Relaxed);
            HELD_TAKEN.fetch_add(1, Ordering::Relaxed);
        } else {
            let _ = FIRST_SPRAYED_RIP.compare_exchange(
                0,
                rip,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }
    // Last, and Release: `storm`'s Acquire load of this word is a handshake, and what it hands over is the classification above, not only a count.
    SEEN[me].fetch_add(1, Ordering::Release);
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

/// Asks before the spray goes out without a held arrival, and how long each waits for the entry's acknowledgement, and a released victim for its next syscall. Bounds, not measurements.
const HOLD_ATTEMPTS: u32 = 10;
const HOLD_ACK_NS: u64 = 100_000_000;

/// How long the held NMI is waited for: the one delivery whose landing is the premise gets a host's scheduling latency rather than [`DELIVERY_BUDGET_NS`], and the held CPU spins with `IF` clear throughout, which keeps this well under `hardlockup`'s bound. A bound, not a measurement.
const HELD_DELIVERY_NS: u64 = 100_000_000;

// The budget outlasts the storm's longest lawful hold wherever a turn — a `pause`, a locked subtract and a test — takes 3 ns or more, which is an estimate of hardware and not a measurement.
const _: () = assert!(hold::TURNS * 3 >= HELD_DELIVERY_NS);

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
    // The entry cannot log where it gives up; the syscall it then lets through says it here, on a kernel stack.
    // A load in front of the write, so a syscall that has nothing to say writes nothing to a word the storm reads.
    let expired = hold_word(me).is_some_and(|word| {
        word.load(Ordering::Acquire) & hold::EXPIRED != 0
            && word.fetch_and(!hold::EXPIRED, Ordering::AcqRel) & hold::EXPIRED != 0
    });
    if expired {
        log!(
            "syscall-window-nmi: hold expired cpu={me} — the entry spent all {} turns under a standing ask and left the window by itself",
            hold::TURNS,
        );
        // An asker that never released may be a CPU that stopped passing, with `klogd` queued on it: the record goes out from here.
        crate::log::console::drain_inline();
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

/// Ends one acknowledged hold, and says so only once it has: the line is the hold's end in the record, so a `#DF` below it is a sprayed arrival's and never the held one's. The turns are how much of the budget this machine's hold took, read off the word the release found.
fn release(word: &AtomicU64, cpu: usize) {
    let left = word.fetch_and(!hold::ASKED, Ordering::AcqRel) / hold::SPIN;
    log!(
        "syscall-window-nmi: released cpu={cpu} turns={} budget={}",
        hold::TURNS.saturating_sub(left),
        hold::TURNS,
    );
}

/// Holds the victim inside `syscall_entry`'s window and aims one NMI at it there, so the arrival the fixed arm counts and the `#DF` the control stages are arranged rather than hoped for; whether one NMI was aimed.
fn hold_one(me: usize, cpus: usize) -> bool {
    for _ in 0..HOLD_ATTEMPTS {
        let Some((cpu, _)) = victim(me, cpus) else { break };
        let Some(word) = hold_word(cpu) else { break };
        // A whole store: a fresh ask takes a stale acknowledgement with it, so what the wait below sees is this round's, and the budget is whole.
        word.store(hold::ASK, Ordering::Release);
        let deadline = crate::clock::nanos_since_boot().saturating_add(HOLD_ACK_NS);
        while word.load(Ordering::Acquire) & hold::HELD == 0
            && crate::clock::nanos_since_boot() < deadline
        {
            core::hint::spin_loop();
        }
        if word.load(Ordering::Acquire) & hold::HELD == 0 {
            // Withdrawn before the next ask: an entry that read this one late acknowledges into a clear word and leaves at once.
            word.fetch_and(!hold::ASKED, Ordering::AcqRel);
            continue;
        }
        let rsp = held_user_rsp(cpu);
        let spin = crate::arch::syscall::hold_spin();
        let seen = &SEEN[cpu];
        let before = seen.load(Ordering::Acquire);
        // Before the send: the control's machine ends at it.
        log!(
            "syscall-window-nmi: held cpu={cpu} rsp={rsp:#018x} spin={:#018x} end={:#018x}, one NMI aimed at it",
            spin.start,
            spin.end,
        );
        apic::send_nmi(cpu as u32);
        let deadline = crate::clock::nanos_since_boot().saturating_add(HELD_DELIVERY_NS);
        while seen.load(Ordering::Acquire) == before
            && crate::clock::nanos_since_boot() < deadline
        {
            core::hint::spin_loop();
        }
        // Read under the hold, where it cannot move: the held syscall counts itself once it is past the entry.
        let taken = SYSCALLS[cpu].load(Ordering::Relaxed);
        release(word, cpu);
        // A victim still leaving the hold is inside the entry whatever the classifier says, and its frame would vouch for any classifier.
        let deadline = crate::clock::nanos_since_boot().saturating_add(HOLD_ACK_NS);
        while SYSCALLS[cpu].load(Ordering::Relaxed) == taken {
            if crate::clock::nanos_since_boot() >= deadline {
                log!("syscall-window-nmi: cpu={cpu} made no syscall within {HOLD_ACK_NS} ns of its release, so the spray's first samples may be of a victim still leaving the hold");
                break;
            }
            core::hint::spin_loop();
        }
        return true;
    }
    log!("syscall-window-nmi: held nobody — no syscall entered on a CPU asked to hold, so the spray goes out without an arranged arrival");
    false
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

    // No hold under `nmi_nested`: the first NMI ends that machine from `nested_nmi`'s lock-free raw writer, and `release`'s line would be logged into the middle of its report.
    let mut sent = u64::from(!crate::actuator::nmi_nested() && hold_one(me, cpus));
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
    let held = HELD_TAKEN.load(Ordering::Relaxed);
    if held != 0 {
        let rip = HELD_RIP.load(Ordering::Relaxed);
        log!(
            "syscall-window-nmi: the held arrival had rip={rip:#018x} rsp={:#018x} and was here:",
            HELD_FRAME_RSP.load(Ordering::Relaxed),
        );
        crate::symbols::resolve_kernel(rip);
    }
    let rip = FIRST_SPRAYED_RIP.load(Ordering::Relaxed);
    if rip != 0 {
        log!("syscall-window-nmi: the first sprayed window arrival was here:");
        crate::symbols::resolve_kernel(rip);
    }
    let outside = OUTSIDE.load(Ordering::Relaxed);
    if outside != 0 {
        log!("syscall-window-nmi: the first window arrival outside the entry was here:");
        crate::symbols::resolve_kernel(FIRST_OUTSIDE_RIP.load(Ordering::Relaxed));
    }
    let seen = total(&SEEN);
    let window = total(&WINDOW);
    let ring3 = total(&RING3);
    log!(
        "syscall-window-nmi: sent={sent} seen={seen} window={window} ring3={ring3} ring0={} \
         spun={spun} held={held} outside={outside}",
        seen - window - ring3,
    );
}
