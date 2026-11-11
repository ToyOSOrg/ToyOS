//! MEASUREMENT ONLY: where the kernel waits, for the spawn diagnosis. Off
//! until a job asks (`SYS_DEBUG` 900), then: each contended `sync::Lock`
//! wait by the site that took it, and per CPU the ticks spent spinning on a
//! lock, waiting as a shootdown's initiator and serving others' shootdowns.

use alloc::vec::Vec;
use core::panic::Location;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};

static ON: AtomicBool = AtomicBool::new(false);

const SITES: usize = 1024;
const CPUS: usize = 64;

struct Site {
    at: AtomicUsize,
    waits: AtomicU64,
    ticks: AtomicU64,
}

static SITE: [Site; SITES] = [const { Site { at: AtomicUsize::new(0), waits: AtomicU64::new(0), ticks: AtomicU64::new(0) } }; SITES];
static LOST: AtomicU64 = AtomicU64::new(0);

/// Per CPU, counter ticks: spinning on a contended lock, an initiator's
/// shootdown wait, and serving a shootdown another CPU issued.
pub static SPIN: [AtomicU64; CPUS] = [const { AtomicU64::new(0) }; CPUS];
pub static SHOOT: [AtomicU64; CPUS] = [const { AtomicU64::new(0) }; CPUS];
pub static SERVED: [AtomicU64; CPUS] = [const { AtomicU64::new(0) }; CPUS];

#[inline]
pub fn on() -> bool {
    ON.load(Relaxed)
}

fn cpu() -> usize {
    (crate::arch::percpu::cpu_id() as usize).min(CPUS - 1)
}

#[inline]
pub fn now() -> u64 {
    crate::arch::cpu::counter()
}

/// The running CPU's (spin, shoot) ticks so far.
pub fn here() -> (u64, u64) {
    let c = cpu();
    (SPIN[c].load(Relaxed), SHOOT[c].load(Relaxed))
}

pub fn lock_waited(at: &'static Location<'static>, ticks: u64) {
    SPIN[cpu()].fetch_add(ticks, Relaxed);
    let key = at as *const Location<'static> as usize;
    let mut i = ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 54) as usize % SITES;
    for _ in 0..SITES {
        let site = &SITE[i];
        let held = match site.at.compare_exchange(0, key, Relaxed, Relaxed) {
            Ok(_) => key,
            Err(other) => other,
        };
        if held == key {
            site.waits.fetch_add(1, Relaxed);
            site.ticks.fetch_add(ticks, Relaxed);
            return;
        }
        i = (i + 1) % SITES;
    }
    LOST.fetch_add(1, Relaxed);
}

pub fn shot(ticks: u64) {
    if on() {
        SHOOT[cpu()].fetch_add(ticks, Relaxed);
    }
}

pub fn served(ticks: u64) {
    if on() {
        SERVED[cpu()].fetch_add(ticks, Relaxed);
    }
}

/// Log what was gathered since the last ask under `label`, then start again.
pub fn dump(label: u64) {
    let us = |t: u64| crate::clock::nanos_of_ticks(t) / 1000;
    if on() {
        let mut sites: Vec<(u64, u64, usize)> = SITE
            .iter()
            .filter(|s| s.at.load(Relaxed) != 0)
            .map(|s| (s.ticks.load(Relaxed), s.waits.load(Relaxed), s.at.load(Relaxed)))
            .collect();
        sites.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        let total: u64 = sites.iter().map(|s| s.0).sum();
        crate::log!("contention {label}: sites={} lost={} lock_wait_us={}", sites.len(), LOST.load(Relaxed), us(total));
        for &(ticks, waits, at) in sites.iter().take(40) {
            // SAFETY: a key is the address of a `&'static Location`.
            let at = unsafe { &*(at as *const Location<'static>) };
            crate::log!("contention {label}: site {}:{} waits={waits} us={}", at.file(), at.line(), us(ticks));
        }
        let column = |a: &[AtomicU64; CPUS]| {
            let mut s = alloc::string::String::new();
            let cpus = (crate::smp::cpu_count() as usize).min(CPUS);
            for c in a.iter().take(cpus) {
                let _ = core::fmt::Write::write_fmt(&mut s, format_args!("{},", us(c.load(Relaxed))));
            }
            s
        };
        crate::log!("contention {label}: per cpu spin_us=[{}] shoot_us=[{}] served_us=[{}]", column(&SPIN), column(&SHOOT), column(&SERVED));
    }
    for s in SITE.iter() {
        s.at.store(0, Relaxed);
        s.waits.store(0, Relaxed);
        s.ticks.store(0, Relaxed);
    }
    LOST.store(0, Relaxed);
    for a in [&SPIN, &SHOOT, &SERVED] {
        for c in a.iter() {
            c.store(0, Relaxed);
        }
    }
    ON.store(true, Relaxed);
}
