//! The machine-wide TLB shootdown.
//!
//! `shootdown` returns only once every other CPU has flushed, so callers may
//! free memory as soon as it returns; every `IF`-clear spin able to block one
//! answers it directly instead of waiting on the interrupt vector. A stale
//! memory-type translation surviving an early return is undefined per SDM
//! Vol. 3A §11.12.4.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::shootdown::{Generation, Shootdown};
use crate::time::{Duration, Tripwire};

use super::{apic, percpu, smp};

static SHOOTDOWN: Shootdown = Shootdown::new();

/// False until [`siblings_answer`] runs: an AP is still spinning on
/// `SMP_READY` with `IF` clear and cannot answer an IPI yet.
static SIBLINGS_ANSWER: AtomicBool = AtomicBool::new(false);

/// Set above `USB_TIMEOUT_NS`, xHCI's longest `IF`-clear device spin, so no
/// legitimate wait trips it.
const ACK_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(5),
    "above the longest IF-clear device spin a target can be inside",
);

/// Spins between deadline checks; `nanos_since_boot`'s 128-bit divide is too
/// costly to call on every iteration.
const SPINS_PER_DEADLINE_CHECK: u32 = 1024;

/// Write the page table, then call this, then free — it returns only once
/// every CPU has flushed.
pub fn shootdown() {
    let cpus = smp::cpu_count();
    if !SIBLINGS_ANSWER.load(Ordering::Acquire) || cpus <= 1 {
        crate::mm::paging::flush_tlb_all();
        return;
    }
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
}

/// Never logs: `drivers::serial`'s lock under `save_and_cli` would deadlock a
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
    if !SIBLINGS_ANSWER.load(Ordering::Relaxed) {
        return;
    }
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve_if_owed(cpu, crate::mm::paging::flush_tlb_all);
}

/// Settle every shootdown issued before this CPU could answer one; called
/// once after `SMP_READY`.
pub fn join() {
    let cpu = percpu::cpu_id() as usize;
    SHOOTDOWN.serve(cpu, crate::mm::paging::flush_tlb_all);
}

/// From here on, `shootdown` waits for siblings; called once by `smp::set_ready`.
pub fn siblings_answer() {
    SIBLINGS_ANSWER.store(true, Ordering::Release);
}

#[cfg(not(feature = "test-actuators"))]
fn stage_ack_delay() {}

#[cfg(feature = "test-actuators")]
mod delay {
    use core::sync::atomic::{AtomicU32, AtomicU64};

    pub static NANOS: AtomicU64 = AtomicU64::new(0);
    pub static CPU: AtomicU32 = AtomicU32::new(u32::MAX);
    /// Absolute nanoseconds past which the arming lapses.
    pub static UNTIL: AtomicU64 = AtomicU64::new(0);
}

/// Expires rather than latches, so a panicked test can't leave it armed
/// forever.
#[cfg(feature = "test-actuators")]
const ARM_WINDOW_NANOS: u64 = 2_000_000_000;

/// Delays after the flush and before publication, so it can only slow a
/// correct answer, never hide an incorrect one.
#[cfg(feature = "test-actuators")]
fn stage_ack_delay() {
    use core::sync::atomic::Ordering;
    if delay::CPU.load(Ordering::Relaxed) != percpu::cpu_id() {
        return;
    }
    let now = crate::clock::nanos_since_boot();
    if now >= delay::UNTIL.load(Ordering::Relaxed) {
        return;
    }
    let until = now.saturating_add(delay::NANOS.load(Ordering::Relaxed));
    while crate::clock::nanos_since_boot() < until {
        core::hint::spin_loop();
    }
}

/// The last CPU `shootdown` waits for, so the delay is measured regardless of
/// iteration order.
#[cfg(feature = "test-actuators")]
fn last_target() -> Option<u32> {
    let top = smp::cpu_count().checked_sub(1)?;
    match percpu::cpu_id() {
        me if me == top => top.checked_sub(1),
        _ => Some(top),
    }
}

/// Arms the last-waited CPU to answer `nanos` late, takes one shootdown, and
/// reports what it cost; the arming outlives the call, so a caller can then
/// time what follows too.
#[cfg(feature = "test-actuators")]
pub fn debug_arm_ack_delay(nanos: u64) -> u64 {
    use core::sync::atomic::Ordering;
    let Some(target) = last_target() else { return 0 };
    delay::CPU.store(target, Ordering::Relaxed);
    delay::NANOS.store(nanos, Ordering::Relaxed);
    delay::UNTIL.store(
        crate::clock::nanos_since_boot().saturating_add(ARM_WINDOW_NANOS),
        Ordering::Relaxed,
    );
    let start = crate::clock::nanos_since_boot();
    shootdown();
    crate::clock::nanos_since_boot() - start
}

/// Give the machine its ordinary latency back before the window lapses.
#[cfg(feature = "test-actuators")]
pub fn debug_disarm_ack_delay() -> u64 {
    use core::sync::atomic::Ordering;
    delay::UNTIL.store(0, Ordering::Relaxed);
    delay::CPU.store(u32::MAX, Ordering::Relaxed);
    0
}
