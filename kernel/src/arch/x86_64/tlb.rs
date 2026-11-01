//! The machine-wide TLB shootdown.
//!
//! `shootdown` returns only once every other CPU has flushed, so callers may
//! free memory as soon as it returns; every `IF`-clear spin able to block one
//! answers it directly instead of waiting on the interrupt vector. A stale
//! memory-type translation surviving an early return is undefined per SDM
//! Vol. 3A §11.12.4.
//!
//! **A change to one user space's leaves reaches only the CPUs running it**
//! ([`user_change`], over `crate::tlb_tags`): a CPU that ran it before flushes
//! when it next enters it, if it changed since. Every other shootdown targets
//! every other CPU the machine brought up.
//!
//! **A user-half change keeps global translations**: its flush is `INVPCID`
//! type 3 or a `CR3` reload, and only a kernel-half change, whose leaves are
//! global, asks the flush that drops them (SDM Vol. 3A §4.10.4.1, APM Vol. 2
//! §5.5.2). An answer covering both is the wider.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::shootdown::{Generation, Reach, Shootdown};

use super::{apic, percpu};
use crate::smp;

static SHOOTDOWN: Shootdown = Shootdown::new();

/// Which CPUs run each user space, by its PCID, and when each changed.
static USER: crate::tlb_tags::Tags = crate::tlb_tags::Tags::new();

/// An answer for each of the `cpus` CPUs the firmware names.
pub fn size(cpus: usize) {
    SHOOTDOWN.size(cpus);
    USER.size(usize::from(kernel::pcid::MAX_USER_PCID) + 1, cpus);
}

/// This CPU loads the root of the space tagged `to` with `load`: what
/// `paging::Cr3::activate` owes the targeting. It runs `to` from before the
/// load, flushing every user translation first where `to` changed since its
/// last, and leaves the space it ran only after it.
pub fn switching(to: u16, load: impl FnOnce()) {
    const _: () = assert!(kernel::pcid::KERNEL_PCID == 0, "the targeting's tag 0 is no space");
    if !USER.sized() {
        return load();
    }
    let cpu = percpu::cpu_id() as usize;
    let switched = USER.switch(cpu, usize::from(to));
    if switched.flush {
        flush(Reach::Narrow);
    }
    load();
    USER.left(cpu, switched.from);
}

/// Pages a change flushes one at a time on the initiator; past this, its
/// whole space.
const BY_PAGE: u64 = 64;

/// The user space tagged `tag` changed the leaves of `[start, end)`, already
/// written: every CPU running it has flushed them before this returns, and
/// every other flushes before it next enters the space. So the change's
/// frames may go as soon as it returns.
/// Answers whether it reached another CPU.
pub fn user_change(origin: Origin, tag: u16, start: u64, end: u64) -> bool {
    let me = percpu::cpu_id() as usize;
    let mut targets = [0u64; 4];
    let mut beyond = false;
    USER.changed(usize::from(tag), |cpu| match targets.get_mut(cpu / 64) {
        Some(word) => *word |= 1 << (cpu % 64),
        None => beyond = true,
    });
    // A CPU past what the targets hold reaches every other, as before.
    let Some(mine) = targets.get(me / 64).copied() else {
        shootdown(origin, Half::User);
        return true;
    };
    // This CPU first: it may be running the space.
    if mine & (1 << (me % 64)) != 0 {
        if (end - start) / crate::mm::PAGE_SIZE <= BY_PAGE {
            (start..end).step_by(crate::mm::PAGE_SIZE as usize).for_each(|va| crate::mm::paging::flush_user_page(tag, va));
        } else {
            flush(Reach::Narrow);
        }
        targets[me / 64] &= !(1 << (me % 64));
    }
    if beyond {
        // More CPUs than the targets hold: every one of them, as before.
        shootdown(origin, Half::User);
        return true;
    }
    if targets.iter().all(|&word| word == 0) || !smp::answering() {
        return false;
    }
    ISSUED[origin as usize].fetch_add(1, Ordering::Relaxed);
    let began = crate::clock::nanos_since_boot();
    let generation = SHOOTDOWN.issue(Reach::Narrow);
    let each = |f: &mut dyn FnMut(u32)| {
        for (w, &word) in targets.iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                f((w * 64 + bits.trailing_zeros() as usize) as u32);
                bits &= bits - 1;
            }
        }
    };
    each(&mut |cpu| apic::tlb_ipi_to(cpu));
    each(&mut |cpu| wait_for(me, cpu, generation));
    let took = crate::clock::nanos_since_boot().saturating_sub(began);
    WAIT_NS.fetch_add(took, Ordering::Relaxed);
    MAX_NS.fetch_max(took, Ordering::Relaxed);
    true
}

use crate::invalidation::{Half, Origin};

/// Issuer-side census; `irq_census`'s `tlb` column is the receiver side, and a
/// delivery the two disagree on is an uncounted issuing path.
static ISSUED: [AtomicU64; Origin::COUNT] = [const { AtomicU64::new(0) }; Origin::COUNT];
static WAIT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_NS: AtomicU64 = AtomicU64::new(0);

/// The machine-wide `tlb:` line of `crate::census`, after the `irq:` lines:
/// the conservation check reads deliveries first. Said at zero too.
pub fn census(say: &mut impl FnMut(core::fmt::Arguments<'_>)) {
    let mut counts = [0u64; Origin::COUNT];
    let mut total = 0u64;
    for (slot, count) in ISSUED.iter().zip(counts.iter_mut()) {
        *count = slot.load(Ordering::Relaxed);
        total += *count;
    }
    struct Fields<'a>(&'a [u64; Origin::COUNT]);
    impl core::fmt::Display for Fields<'_> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            for (name, count) in Origin::NAMES.iter().zip(self.0) {
                write!(f, " {name}={count}")?;
            }
            Ok(())
        }
    }
    say(format_args!(
        "tlb: shootdowns={total} wait={}us max={}us{}",
        WAIT_NS.load(Ordering::Relaxed) / 1_000,
        MAX_NS.load(Ordering::Relaxed) / 1_000,
        Fields(&counts)
    ));
}

/// Spins between deadline checks; `nanos_since_boot`'s 128-bit divide is too
/// costly to call on every iteration.
const SPINS_PER_DEADLINE_CHECK: u32 = 1024;

/// Write the page table, then call this, then free — it returns only once every
/// CPU has flushed what a change to `half` owes. The local-flush early return
/// is uncounted: no IPI, no wait.
pub fn shootdown(origin: Origin, half: Half) {
    let reach = match half {
        Half::User => Reach::Narrow,
        Half::Kernel => Reach::Wide,
    };
    let cpus = smp::cpu_count();
    if !smp::answering() || cpus <= 1 {
        flush(reach);
        return;
    }
    // Counted before the IPI, so a delivery can never precede its issue's count.
    ISSUED[origin as usize].fetch_add(1, Ordering::Relaxed);
    let began = crate::clock::nanos_since_boot();
    let me = percpu::cpu_id() as usize;
    let generation = SHOOTDOWN.issue(reach);
    // This CPU answers itself locally instead of by self-IPI.
    SHOOTDOWN.serve(me, answer(Answerer::Initiator));
    apic::tlb_ipi();
    for cpu in 0..cpus {
        if cpu as usize != me {
            wait_for(me, cpu, generation);
        }
    }
    let took = crate::clock::nanos_since_boot().saturating_sub(began);
    WAIT_NS.fetch_add(took, Ordering::Relaxed);
    MAX_NS.fetch_max(took, Ordering::Relaxed);
}

/// What one machine-wide shootdown costs its initiator, as a distribution.
///
/// **The census above cannot answer this.** `wait`/`max` are a sum and a
/// maximum over whatever the boot happened to unmap, so the average moves with
/// the workload and the tail is one sample. This issues a fixed count against
/// the CPUs the machine actually brought up, with nothing else running, and
/// sorts what it measured — so two boots of one machine are comparable and a
/// boot that widened the tail is visible as one.
///
/// Runs on the BSP after `smp::set_ready` and before the idle loop: the targets
/// are halted in `ap_idle` with interrupts on, which is the state a shootdown
/// finds them in. `boot_phase!("complete")` is already out, so what this spends
/// is not in any boot-time number.
#[cfg(feature = "boot-actuators")]
pub fn bench() {
    const ROUNDS: usize = 256;
    let cpus = smp::cpu_count();
    let mut took = [0u64; ROUNDS];
    for sample in took.iter_mut() {
        let began = crate::clock::nanos_since_boot();
        shootdown(Origin::Bench, Half::User);
        *sample = crate::clock::nanos_since_boot().saturating_sub(began);
    }
    took.sort_unstable();
    crate::log!(
        "tlb: bench {ROUNDS} shootdowns across {cpus} cpus min={}ns p50={}ns p90={}ns p99={}ns \
         max={}ns",
        took[0],
        took[ROUNDS / 2],
        took[ROUNDS * 9 / 10],
        took[ROUNDS * 99 / 100],
        took[ROUNDS - 1],
    );
}

/// Never logs: `drivers::serial`'s lock under its `IrqGuard` would deadlock a
/// target that cannot answer while blocked on it.
fn wait_for(me: usize, cpu: u32, generation: Generation) {
    let mut spins = 0u32;
    let mut deadline = None;
    while !SHOOTDOWN.wait_turn(me, cpu as usize, generation, answer(Answerer::Initiator)) {
        core::hint::spin_loop();
        spins += 1;
        if spins == SPINS_PER_DEADLINE_CHECK {
            spins = 0;
            let now = crate::clock::nanos_since_boot();
            match deadline {
                None => deadline = Some(now.saturating_add(crate::time::DEAF_CPU.nanos())),
                Some(at) if now >= at => panic!(
                    "tlb: cpu {cpu} has not flushed for generation {generation:?} in {}ns — \
                     it is not taking interrupts",
                    crate::time::DEAF_CPU.nanos(),
                ),
                Some(_) => {}
            }
        }
    }
}

/// What `reach` asks of this CPU: every translation, or every one but the
/// global ones a user-half change leaves.
fn flush(reach: Reach) {
    let stamp = USER.flushing();
    match reach {
        Reach::Wide => crate::mm::paging::flush_tlb_all(),
        Reach::Narrow => crate::mm::paging::flush_tlb_user(),
    }
    if USER.sized() {
        USER.flushed(percpu::cpu_id() as usize, stamp);
    }
}

/// Which side of a shootdown this CPU answers from: asked, by the interrupt or
/// a spin's poll, or inside a shootdown of its own, whose serves answer every
/// generation this CPU owes as well as its own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answerer {
    Asked,
    Initiator,
}

/// The flush, then the staged delay: every serve's closure but `join`'s.
fn answer(by: Answerer) -> impl FnOnce(Reach) {
    move |reach| {
        flush(reach);
        stage_ack_delay(by);
    }
}

/// Vector 0xFE's whole body: flush this CPU and say which generation it covers.
pub fn serve_ipi() {
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve(cpu, answer(Answerer::Asked));
}

/// Answers a pending shootdown without taking a lock or allocating, so it is
/// safe from inside `Lock::lock`'s spin; and, on the boot processor, a write
/// to `SMI_CMD` asked of it, and a demoted clock's handover, which a spinner
/// with `IF` clear owes the same way.
#[inline]
pub fn poll() {
    super::smi_cmd::serve_here();
    if !smp::answering() {
        return;
    }
    super::clocksource::serve_here();
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve_if_owed(cpu, answer(Answerer::Asked));
}

/// Settle every shootdown issued before this CPU could answer one; called
/// once after the machine is released. Wide whatever is owed: a kernel-half
/// change made before the machine answered issued no generation to say so.
pub fn join() {
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve(cpu, |_| crate::mm::paging::flush_tlb_all());
}

#[cfg(not(feature = "test-actuators"))]
fn stage_ack_delay(_by: Answerer) {}

#[cfg(feature = "test-actuators")]
mod delay {
    use core::sync::atomic::{AtomicU32, AtomicU64};

    pub static NANOS: AtomicU64 = AtomicU64::new(0);
    /// Absolute nanoseconds past which the arming lapses.
    pub static UNTIL: AtomicU64 = AtomicU64::new(0);
    /// Whose acknowledgement is held back; [`EVERY`] is all of them.
    pub static TARGET: AtomicU32 = AtomicU32::new(EVERY);
    /// Outside the CPU-id range, so it can never name one.
    pub const EVERY: u32 = u32::MAX;
}

/// Expires rather than latches, so a panicked test can't leave it armed
/// forever.
#[cfg(feature = "test-actuators")]
const ARM_WINDOW_NANOS: u64 = 2_000_000_000;

#[cfg(feature = "test-actuators")]
fn open_arm_window() {
    delay::UNTIL.store(
        crate::clock::nanos_since_boot().saturating_add(ARM_WINDOW_NANOS),
        Ordering::Relaxed,
    );
}

/// Delays after the flush and before publication, so it can only slow a
/// correct answer, never hide an incorrect one.
///
/// **One held CPU is held whichever side it answers from**: inside a shootdown
/// of its own it answers the staged generation from that shootdown's serves,
/// and an unheld answer there brings a correct wait for it back fast.
/// **Under [`delay::EVERY`] no initiator is held**: the shootdown being timed
/// is an initiator's, and its own answer is not the wait under measurement.
#[cfg(feature = "test-actuators")]
fn stage_ack_delay(by: Answerer) {
    let now = crate::clock::nanos_since_boot();
    if now >= delay::UNTIL.load(Ordering::Relaxed) {
        return;
    }
    let held = match delay::TARGET.load(Ordering::Relaxed) {
        delay::EVERY => by == Answerer::Asked,
        target => target == percpu::cpu_id(),
    };
    if !held {
        return;
    }
    let until = now.saturating_add(delay::NANOS.load(Ordering::Relaxed));
    while crate::clock::nanos_since_boot() < until {
        core::hint::spin_loop();
    }
}

/// Hold each other CPU's acknowledgement back for `nanos` in turn, take one
/// shootdown against each, and report the **smallest** wait any of them cost
/// the initiator, which measures the width of the target set and not only its
/// depth. The arming is then left standing against every other CPU until a
/// disarm or the end of a fresh [`ARM_WINDOW_NANOS`], whichever comes first, so
/// a caller can time a syscall's own shootdown inside that window.
///
/// Preemption is off across the sweep because each held-back CPU is chosen
/// against this one: a thread that migrated mid-sweep would arm the CPU it is
/// about to initiate from, which spends the delay on its own hand and holds
/// back no wait.
#[cfg(feature = "test-actuators")]
pub fn debug_arm_ack_delay(nanos: u64) -> u64 {
    delay::NANOS.store(nanos, Ordering::Relaxed);
    let mut least = u64::MAX;
    crate::sched::driver::preempt_off(|_| {
        let me = percpu::cpu_id();
        for cpu in (0..smp::cpu_count()).filter(|cpu| *cpu != me) {
            delay::TARGET.store(cpu, Ordering::Relaxed);
            open_arm_window();
            let began = crate::clock::nanos_since_boot();
            shootdown(Origin::Staged, Half::User);
            least = least.min(crate::clock::nanos_since_boot().saturating_sub(began));
        }
    });
    delay::TARGET.store(delay::EVERY, Ordering::Relaxed);
    open_arm_window();
    // A machine with one CPU has no other to hold back and `shootdown` takes
    // its local-flush return there, so there is no wait to report.
    if least == u64::MAX {
        0
    } else {
        least
    }
}

/// [`shootdown`], answering whether this CPU's own answers inside it reached
/// wide: a kernel-half generation another CPU issued meanwhile widens them.
#[cfg(feature = "test-actuators")]
pub fn debug_shootdown_answered_wide(origin: Origin, half: Half) -> bool {
    let me = percpu::cpu_id() as usize;
    let before = SHOOTDOWN.widened(me);
    shootdown(origin, half);
    SHOOTDOWN.widened(me) != before
}

/// Give the machine its ordinary latency back before the window lapses.
#[cfg(feature = "test-actuators")]
pub fn debug_disarm_ack_delay() -> u64 {
    delay::UNTIL.store(0, Ordering::Relaxed);
    delay::TARGET.store(delay::EVERY, Ordering::Relaxed);
    0
}
