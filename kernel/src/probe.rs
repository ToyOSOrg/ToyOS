//! SCRATCH, never lands: cycles and counts per stage of a pipe read or write
//! and of a paged read, off until a program turns them on.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::user_ptr::UserBytesMut;

pub const STAGES: usize = 48;

/// The syscall number `control` answers on.
pub const SYSCALL: u64 = 0xffff_9199;

#[repr(align(64))]
struct Stage {
    cycles: AtomicU64,
    count: AtomicU64,
}

static ON: AtomicBool = AtomicBool::new(false);
static ACC: [Stage; STAGES] = [const { Stage { cycles: AtomicU64::new(0), count: AtomicU64::new(0) } }; STAGES];

/// The counter now, or 0 while off.
#[inline(always)]
pub fn now() -> u64 {
    if ON.load(Relaxed) { crate::arch::cpu::counter() } else { 0 }
}

/// Charge stage `id` the cycles since `from`, and answer the counter now.
#[inline(always)]
pub fn span(id: usize, from: u64) -> u64 {
    if from == 0 {
        return 0;
    }
    let t = crate::arch::cpu::counter();
    ACC[id].cycles.fetch_add(t.wrapping_sub(from), Relaxed);
    ACC[id].count.fetch_add(1, Relaxed);
    t
}

/// Add `n` to stage `id`'s cycles word, counting one.
#[inline(always)]
pub fn add(id: usize, n: u64) {
    if ON.load(Relaxed) {
        ACC[id].cycles.fetch_add(n, Relaxed);
        ACC[id].count.fetch_add(1, Relaxed);
    }
}

/// 0: off. 1: zero and on. 2: copy every stage's two words out.
pub fn control(op: u64, out: Option<&mut UserBytesMut>) -> u64 {
    match (op, out) {
        (0, _) => ON.store(false, Relaxed),
        (1, _) => {
            for stage in &ACC {
                stage.cycles.store(0, Relaxed);
                stage.count.store(0, Relaxed);
            }
            ON.store(true, Relaxed);
        }
        (2, Some(out)) => {
            let mut words = [0u64; 2 * STAGES];
            for (i, stage) in ACC.iter().enumerate() {
                words[2 * i] = stage.cycles.load(Relaxed);
                words[2 * i + 1] = stage.count.load(Relaxed);
            }
            let bytes: &[u8] = unsafe { core::slice::from_raw_parts(words.as_ptr().cast(), 16 * STAGES) };
            if out.len() < bytes.len() || out.write_at(0, bytes).is_err() {
                return u64::MAX;
            }
        }
        _ => return u64::MAX,
    }
    0
}
