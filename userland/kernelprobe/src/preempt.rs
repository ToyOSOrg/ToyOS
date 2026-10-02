//! `preempt`: whether an interrupt takes the CPU from a thread that never
//! enters the kernel itself. A second thread counts in a loop with no syscall,
//! and once it has gone round twice no fault either; this one yields until the
//! count has moved past what it last read, twice. On one CPU it runs between
//! those reads only if the counting thread lost the CPU to an interrupt, so on
//! a kernel whose timer never preempts the line below never comes.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

static COUNT: AtomicU64 = AtomicU64::new(0);

pub fn main(_args: Vec<String>) {
    std::thread::spawn(|| loop {
        COUNT.fetch_add(1, Relaxed);
    });
    let first = moved_past(1);
    let second = moved_past(first);
    println!("preempt: the counting thread was preempted twice, at counts {first} and {second}");
}

/// Yield until the count is past `seen`, and say where it is.
fn moved_past(seen: u64) -> u64 {
    loop {
        std::thread::yield_now();
        let now = COUNT.load(Relaxed);
        if now > seen {
            return now;
        }
    }
}
