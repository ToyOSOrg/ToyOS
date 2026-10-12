//! Measurement scout, never landed: how long an ordinary wake takes from its
//! post to the woken task's dispatch, split by what the target CPU was doing
//! when the wake was posted — the poster's own CPU, a CPU that had published
//! SLEEPING, or a busy one.
//!
//! One word per task slot carries the post's clock, its class and a tag of the
//! task's key; the switch onto the task consumes it, and the switch off a task
//! drops it, so a wake that raced its own park is never read later as a slow
//! one. Only the armed process's tasks are recorded.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed};

use kernel::sched::hw::CpuId;
use kernel::sched::task::TaskKey;

/// `SYS_DEBUG` actions. Arm: reset and record the process `a2`. Dump: log
/// every class's quantiles under the phase `a2`, then reset.
pub const ARM: u64 = 0x5C00_0001;
pub const DUMP: u64 = 0x5C00_0002;

const SLOTS: usize = 4096;
const NS_BITS: u32 = 40;
const NS_MASK: u64 = (1 << NS_BITS) - 1;
const TAG_SHIFT: u32 = NS_BITS + 2;

const CLASSES: [&str; 3] = ["same", "sleeping", "busy"];
const BUCKETS: usize = 16 + 8 * 44;

static SLOT: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static ARMED: AtomicU32 = AtomicU32::new(u32::MAX);
static HIST: [[AtomicU64; BUCKETS]; 3] = [const { [const { AtomicU64::new(0) }; BUCKETS] }; 3];
static MAX: [AtomicU64; 3] = [const { AtomicU64::new(0) }; 3];

fn slot(task: TaskKey) -> &'static AtomicU64 {
    &SLOT[task.0 as usize % SLOTS]
}

fn tag(task: TaskKey) -> u64 {
    task.0 & ((1 << (64 - TAG_SHIFT)) - 1)
}

/// Eight buckets per octave above 16 ns: a quantile is read to within 12.5%.
fn bucket(ns: u64) -> usize {
    if ns < 16 {
        return ns as usize;
    }
    let octave = 63 - ns.leading_zeros() as usize;
    let sub = ((ns >> (octave - 3)) & 7) as usize;
    (16 + (octave - 4) * 8 + sub).min(BUCKETS - 1)
}

fn upper(b: usize) -> u64 {
    if b < 16 {
        return b as u64;
    }
    let octave = (b - 16) / 8 + 4;
    let sub = ((b - 16) % 8) as u64;
    ((9 + sub) << (octave - 3)) - 1
}

pub fn posted(target: CpuId, task: TaskKey, sleeping: bool) {
    if ARMED.load(Relaxed) == u32::MAX {
        return;
    }
    let class = if target.0 == crate::arch::percpu::cpu_id() {
        0
    } else if sleeping {
        1
    } else {
        2
    };
    let now = crate::clock::nanos_since_boot() & NS_MASK;
    slot(task).store(tag(task) << TAG_SHIFT | class << NS_BITS | now, Relaxed);
}

/// At the switch, before the stack moves.
pub fn switched(incoming: Option<TaskKey>, incoming_pid: Option<u32>, outgoing: Option<TaskKey>) {
    let armed = ARMED.load(Relaxed);
    if armed == u32::MAX {
        return;
    }
    if let Some(out) = outgoing {
        let word = slot(out).load(Relaxed);
        if word >> TAG_SHIFT == tag(out) {
            let _ = slot(out).compare_exchange(word, 0, Relaxed, Relaxed);
        }
    }
    let Some(task) = incoming else { return };
    let word = slot(task).swap(0, Relaxed);
    if word == 0 || word >> TAG_SHIFT != tag(task) || incoming_pid != Some(armed) {
        return;
    }
    let now = crate::clock::nanos_since_boot() & NS_MASK;
    let ns = now.wrapping_sub(word & NS_MASK) & NS_MASK;
    let class = ((word >> NS_BITS) & 3) as usize;
    HIST[class][bucket(ns)].fetch_add(1, Relaxed);
    MAX[class].fetch_max(ns, Relaxed);
}

fn reset() {
    for class in &HIST {
        for b in class {
            b.store(0, Relaxed);
        }
    }
    for m in &MAX {
        m.store(0, Relaxed);
    }
}

pub fn debug(action: u64, arg: u64) -> u64 {
    match action {
        ARM => {
            reset();
            ARMED.store(arg as u32, Relaxed);
            0
        }
        DUMP => {
            for (class, name) in CLASSES.iter().enumerate() {
                let counts: [u64; BUCKETS] = core::array::from_fn(|b| HIST[class][b].load(Relaxed));
                let n: u64 = counts.iter().sum();
                let q = |p: u64| -> u64 {
                    if n == 0 {
                        return 0;
                    }
                    let want = (n * p).div_ceil(1000).max(1);
                    let mut seen = 0;
                    for (b, c) in counts.iter().enumerate() {
                        seen += c;
                        if seen >= want {
                            return upper(b);
                        }
                    }
                    upper(BUCKETS - 1)
                };
                crate::log!(
                    "scout-wake: phase={arg} target={name} n={n} p50<={}ns p90<={}ns p99<={}ns p999<={}ns max={}ns",
                    q(500),
                    q(900),
                    q(990),
                    q(999),
                    MAX[class].load(Relaxed),
                );
            }
            reset();
            0
        }
        _ => toyos_abi::syscall::SyscallError::InvalidArgument.to_u64(),
    }
}
