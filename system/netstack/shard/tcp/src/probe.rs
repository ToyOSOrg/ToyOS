//! MEASUREMENT ONLY, never lands: what the receive side of every connection did, summed over the
//! process, for netstack's `probe` line. Gauges hold the last connection's value; `MAX_*` hold the
//! most since the line last read them.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub static SEGS: AtomicU64 = AtomicU64::new(0);
pub static BYTES: AtomicU64 = AtomicU64::new(0);
pub static IN_ORDER: AtomicU64 = AtomicU64::new(0);
pub static FILLED: AtomicU64 = AtomicU64::new(0);
pub static OOO: AtomicU64 = AtomicU64::new(0);
pub static NOTHING: AtomicU64 = AtomicU64::new(0);
pub static RANGE_LIMIT: AtomicU64 = AtomicU64::new(0);
pub static OLD: AtomicU64 = AtomicU64::new(0);
pub static ACKS: AtomicU64 = AtomicU64::new(0);
pub static SACKS: AtomicU64 = AtomicU64::new(0);
pub static GROWS: AtomicU64 = AtomicU64::new(0);
pub static CAPACITY: AtomicU64 = AtomicU64::new(0);
pub static WINDOW: AtomicU64 = AtomicU64::new(0);
pub static MIN_WINDOW: AtomicU64 = AtomicU64::new(u64::MAX);
pub static RTT_US: AtomicU64 = AtomicU64::new(0);
pub static MAX_UNREAD: AtomicU64 = AtomicU64::new(0);
pub static READS: AtomicU64 = AtomicU64::new(0);
pub static READ_BYTES: AtomicU64 = AtomicU64::new(0);

pub fn add(counter: &AtomicU64, n: u64) {
    counter.fetch_add(n, Relaxed);
}

pub fn set(gauge: &AtomicU64, v: usize) {
    gauge.store(u64::try_from(v).unwrap_or(u64::MAX), Relaxed);
}

pub fn max(gauge: &AtomicU64, v: usize) {
    gauge.fetch_max(u64::try_from(v).unwrap_or(u64::MAX), Relaxed);
}

pub fn min(gauge: &AtomicU64, v: usize) {
    gauge.fetch_min(u64::try_from(v).unwrap_or(u64::MAX), Relaxed);
}

/// Every counter and gauge as `name=value` words; the `MAX_*`/`MIN_*` ones start again.
pub fn words() -> alloc::string::String {
    let read = |c: &AtomicU64| c.load(Relaxed);
    alloc::format!(
        "segs={} bytes={} in_order={} filled={} ooo={} nothing={} range_limit={} old={} acks={} sacks={} \
         reads={} read_bytes={} grows={} cap={} wnd={} min_wnd={} max_unread={} rtt_us={}",
        read(&SEGS),
        read(&BYTES),
        read(&IN_ORDER),
        read(&FILLED),
        read(&OOO),
        read(&NOTHING),
        read(&RANGE_LIMIT),
        read(&OLD),
        read(&ACKS),
        read(&SACKS),
        read(&READS),
        read(&READ_BYTES),
        read(&GROWS),
        read(&CAPACITY),
        read(&WINDOW),
        MIN_WINDOW.swap(u64::MAX, Relaxed),
        MAX_UNREAD.swap(0, Relaxed),
        read(&RTT_US),
    )
}
