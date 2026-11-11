//! MEASUREMENT ONLY, never lands: where a hot path's time goes. Each CPU
//! keeps the counter reading of its last probe; a probe adds the ticks since
//! then to its own id's sum and count, so the segments on one CPU tile its
//! time. An id's time is the time to reach it from whichever probe ran last
//! on that CPU, an interrupt handler's included.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// Fixed probes below 256, `256 + num` at a syscall's entry, `512 + num` at
/// the end of its body.
pub const IDS: usize = 768;
const CPUS: usize = 32;

#[repr(C, align(64))]
struct Table {
    prev: u64,
    sum: [u64; IDS],
    count: [u32; IDS],
}

struct Slot(UnsafeCell<Table>);
// SAFETY: a table is written by its own CPU's probes; the control call reads
// and clears with the probes switched off, and a probe it races only spoils
// a reading.
unsafe impl Sync for Slot {}

static TABLES: [Slot; CPUS] = [const { Slot(UnsafeCell::new(Table { prev: 0, sum: [0; IDS], count: [0; IDS] })) }; CPUS];
static ON: AtomicBool = AtomicBool::new(false);

#[inline(always)]
pub fn at(id: u16) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let now = crate::arch::cpu::counter();
    let cpu = crate::arch::percpu::cpu_id() as usize;
    if cpu >= CPUS {
        return;
    }
    let table = TABLES[cpu].0.get();
    // SAFETY: this CPU's own table; see `Slot`.
    unsafe {
        let prev = (*table).prev;
        (*table).prev = now;
        // An interrupt's probe between the read and here leaves a later `prev`.
        if prev != 0 && now >= prev {
            (*table).sum[id as usize] = (*table).sum[id as usize].wrapping_add(now.wrapping_sub(prev));
            (*table).count[id as usize] = (*table).count[id as usize].wrapping_add(1);
        }
    }
}

/// One reading on the wire: 16 bytes.
#[repr(C)]
#[derive(Clone, Copy)]
struct Wire {
    cpu: u16,
    id: u16,
    count: u32,
    sum: u64,
}

/// Action 0 stops and clears, 3 starts, 4 stops, 1 stops and copies every
/// non-empty reading out, answering how many; 2 is two probes back to back,
/// the price of one.
pub fn control(action: u64, out: Option<&mut crate::user_ptr::UserBytesMut<'_>>) -> u64 {
    match action {
        0 => {
            ON.store(false, Ordering::SeqCst);
            for slot in &TABLES {
                // SAFETY: probes are off; see `Slot`.
                unsafe { slot.0.get().write_bytes(0, 1) };
            }
            0
        }
        3 => {
            ON.store(true, Ordering::SeqCst);
            0
        }
        4 => {
            ON.store(false, Ordering::SeqCst);
            0
        }
        1 => {
            ON.store(false, Ordering::SeqCst);
            let Some(out) = out else { return u64::MAX };
            let mut n = 0usize;
            for (cpu, slot) in TABLES.iter().enumerate() {
                for id in 0..IDS {
                    // SAFETY: probes are off; see `Slot`.
                    let (count, sum) = unsafe { ((*slot.0.get()).count[id], (*slot.0.get()).sum[id]) };
                    if count == 0 {
                        continue;
                    }
                    let wire = Wire { cpu: cpu as u16, id: id as u16, count, sum };
                    // SAFETY: `Wire` is plain integers with no padding.
                    let bytes = unsafe { core::slice::from_raw_parts((&raw const wire).cast::<u8>(), 16) };
                    if (n + 1) * 16 > out.len() || out.write_at(n * 16, bytes).is_err() {
                        return n as u64;
                    }
                    n += 1;
                }
            }
            n as u64
        }
        _ => {
            at(250);
            at(251);
            0
        }
    }
}
