//! The machine-wide TLB shootdown.
//!
//! `shootdown` returns only once every other CPU has flushed, so callers may
//! free memory as soon as it returns; every `IF`-clear spin able to block one
//! answers it directly instead of waiting on the interrupt vector. A stale
//! memory-type translation surviving an early return is undefined per SDM
//! Vol. 3A §11.12.4.
//!
//! **The target set is every other CPU the machine brought up**, not the CPUs
//! sharing the initiator's address space: nothing a process does puts a CPU
//! into it or takes one out.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::shootdown::{Generation, Shootdown};
use crate::time::{Duration, Tripwire};

use super::{apic, percpu, smp};

static SHOOTDOWN: Shootdown = Shootdown::new();

/// Which path issued a shootdown, so the census names who pays: `Dlopen` (a
/// `Shared` window or rollback unmap), `Pcid` (pool reclaim), `Mmio`, `Unmap`
/// (`Unmapped::drop`), `Pipe`, `Staged` (the ack-delay actuator), `Bench`
/// ([`bench`]'s own, so a measured shootdown is never counted as one a path in
/// this kernel needed).
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Origin {
    Dlopen,
    Pcid,
    Mmio,
    Unmap,
    Pipe,
    #[cfg_attr(not(feature = "test-actuators"), allow(dead_code))]
    Staged,
    #[cfg_attr(not(feature = "boot-actuators"), allow(dead_code))]
    Bench,
}

impl Origin {
    const COUNT: usize = 7;
    /// Order matches the variants; `tests/toyos.rs`'s `irq_census_conservation` reads the line back.
    const NAMES: [&'static str; Self::COUNT] =
        ["dlopen", "pcid", "mmio", "unmap", "pipe", "staged", "bench"];
}

/// Issuer-side census; `irq_census`'s `tlb` column is the receiver side, and a
/// delivery the two disagree on is an uncounted issuing path.
static ISSUED: [AtomicU64; Origin::COUNT] = [const { AtomicU64::new(0) }; Origin::COUNT];
static WAIT_NS: AtomicU64 = AtomicU64::new(0);
static MAX_NS: AtomicU64 = AtomicU64::new(0);
/// Total at the last print; process exit logs once per batch.
static REPORTED: AtomicU64 = AtomicU64::new(0);

/// One machine-wide `tlb:` line when the counts moved, at process exit after
/// `irq_census::log_census`: the conservation check reads deliveries first.
pub fn log_census() {
    let mut counts = [0u64; Origin::COUNT];
    let mut total = 0u64;
    for (slot, count) in ISSUED.iter().zip(counts.iter_mut()) {
        *count = slot.load(Ordering::Relaxed);
        total += *count;
    }
    if total == 0 || REPORTED.swap(total, Ordering::Relaxed) == total {
        return;
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
    crate::log!(
        "tlb: shootdowns={total} wait={}us max={}us{}",
        WAIT_NS.load(Ordering::Relaxed) / 1_000,
        MAX_NS.load(Ordering::Relaxed) / 1_000,
        Fields(&counts)
    );
}

/// Set above xHCI's `CALL_AFTER_BREAK`, the longest a disk call spins with `IF`
/// clear once its transport has broken, so no legitimate wait trips it; that
/// constant's own assertion holds the order.
pub(crate) const ACK_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(5),
    "above the longest IF-clear device spin a target can be inside",
);

/// Spins between deadline checks; `nanos_since_boot`'s 128-bit divide is too
/// costly to call on every iteration.
const SPINS_PER_DEADLINE_CHECK: u32 = 1024;

/// Write the page table, then call this, then free — it returns only once every
/// CPU has flushed. The local-flush early return is uncounted: no IPI, no wait.
pub fn shootdown(origin: Origin) {
    let cpus = smp::cpu_count();
    if !smp::answering() || cpus <= 1 {
        crate::mm::paging::flush_tlb_all();
        return;
    }
    // Counted before the IPI, so a delivery can never precede its issue's count.
    ISSUED[origin as usize].fetch_add(1, Ordering::Relaxed);
    let began = crate::clock::nanos_since_boot();
    let me = percpu::cpu_id() as usize;
    let generation = SHOOTDOWN.issue();
    // This CPU answers itself locally instead of by self-IPI.
    SHOOTDOWN.serve(me, crate::mm::paging::flush_tlb_all);
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
        shootdown(Origin::Bench);
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
    while !SHOOTDOWN.wait_turn(me, cpu as usize, generation, crate::mm::paging::flush_tlb_all) {
        core::hint::spin_loop();
        spins += 1;
        if spins == SPINS_PER_DEADLINE_CHECK {
            spins = 0;
            let now = crate::clock::nanos_since_boot();
            match deadline {
                None => deadline = Some(now.saturating_add(ACK_TIMEOUT.nanos())),
                Some(at) if now >= at => panic!(
                    "tlb: cpu {cpu} has not flushed for generation {generation:?} in {}ns — \
                     it is not taking interrupts",
                    ACK_TIMEOUT.nanos(),
                ),
                Some(_) => {}
            }
        }
    }
}

/// Vector 0xFE's whole body: flush this CPU and say which generation it covers.
pub fn serve_ipi() {
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve(cpu, || {
        crate::mm::paging::flush_tlb_all();
        stage_ack_delay();
    });
}

/// Answers a pending shootdown without taking a lock or allocating, so it is
/// safe from inside `Lock::lock`'s spin.
#[inline]
pub fn poll() {
    if !smp::answering() {
        return;
    }
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve_if_owed(cpu, || {
        crate::mm::paging::flush_tlb_all();
        stage_ack_delay();
    });
}

/// Settle every shootdown issued before this CPU could answer one; called
/// once after the machine is released.
pub fn join() {
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve(cpu, crate::mm::paging::flush_tlb_all);
}

#[cfg(not(feature = "test-actuators"))]
fn stage_ack_delay() {}

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
/// An initiator's own flush is not part of the wait under measurement, so
/// delaying it would spend that wait on the initiator's own hand.
#[cfg(feature = "test-actuators")]
fn stage_ack_delay() {
    let now = crate::clock::nanos_since_boot();
    if now >= delay::UNTIL.load(Ordering::Relaxed) {
        return;
    }
    let target = delay::TARGET.load(Ordering::Relaxed);
    if target != delay::EVERY && target != percpu::cpu_id() {
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
/// about to initiate from, which holds nobody back.
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
            shootdown(Origin::Staged);
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

/// Give the machine its ordinary latency back before the window lapses.
#[cfg(feature = "test-actuators")]
pub fn debug_disarm_ack_delay() -> u64 {
    delay::UNTIL.store(0, Ordering::Relaxed);
    delay::TARGET.store(delay::EVERY, Ordering::Relaxed);
    0
}
