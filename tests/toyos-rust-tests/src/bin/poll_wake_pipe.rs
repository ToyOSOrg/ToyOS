//! The io_uring ring-watcher half of a source's wake, as a lost-wake canary.
//!
//! A source owes two wakes — the blocked syscall and every armed `POLL_ADD` —
//! and the 7a cutover once deleted the second for two sources undetected. A
//! watcher arms `POLL_ADD` READABLE on a pipe read end and blocks in `wait`, a
//! writer writes each round, and every readable edge must wake the ring: a
//! dropped ring wake reds as a short count, which is the whole verdict.
//! `blocking_read_stress` is the same canary for the other half.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use toyos::pipe_pair;
use toyos::poller::{Poller, READABLE};

const ROUNDS: u32 = 300;

/// The pacing spin's escape, so a lost wake reds as a short count, not a hang.
const PACE_ESCAPE: Duration = Duration::from_secs(3);

/// Per-round patience, far above a live wake's latency, so only an absent completion trips it.
const WAIT_NANOS: u64 = 200_000_000;

fn main() {
    let (reader, writer) = pipe_pair().expect("a pipe");
    let woken = AtomicU32::new(0);

    let began = Instant::now();
    thread::scope(|s| {
        s.spawn(|| {
            let poller = Poller::new(1);
            let mut buf = [0u8; 1];
            for _ in 0..ROUNDS {
                poller.watch(&reader, READABLE, 0);
                let mut got = false;
                poller.wait(1, WAIT_NANOS, |_| got = true);
                if !got {
                    return; // the completion never arrived — the ring half was lost
                }
                woken.fetch_add(1, Ordering::Relaxed);
                let _ = reader.read(&mut buf); // drain so the next round arms empty
            }
        });

        // One byte per round, paced behind the watcher so every write is a distinct edge.
        s.spawn(|| {
            for round in 0..ROUNDS {
                while woken.load(Ordering::Relaxed) < round
                    && began.elapsed() < PACE_ESCAPE
                {
                    std::hint::spin_loop();
                }
                if writer.write(&[0x5A]).is_err() {
                    return;
                }
            }
        });
    });

    let woken = woken.load(Ordering::Relaxed);
    let elapsed = began.elapsed();
    assert_eq!(
        woken, ROUNDS,
        "the poll completed {woken} of {ROUNDS} readable edges in {elapsed:?} — a ring \
         watcher's wake was lost",
    );
    println!("poll_wake_pipe: {ROUNDS} readable edges each woke the armed ring in {elapsed:?}");
}
