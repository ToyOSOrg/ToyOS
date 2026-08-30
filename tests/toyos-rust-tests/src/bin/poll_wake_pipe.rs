//! The io_uring ring-watcher half of a source's wake, as a lost-wake canary.
//!
//! Every source owes two wakes at its event site — the thread blocked in a
//! plain syscall, and every `io_uring` ring that armed a `POLL_ADD`. The 7a
//! scheduler cutover once deleted the second half for two sources and nothing
//! caught it for two months, because no test blocks on a poll and requires the
//! completion from a real event. This does: a watcher thread arms `POLL_ADD`
//! READABLE on a pipe read end and blocks in `wait`; a writer thread writes
//! after a swept delay; the completion must arrive, [`ROUNDS`] times, inside a
//! wall-clock bound. A dropped ring wake reds as a count short of `ROUNDS`,
//! never as a hang — the writer's own progress is the timeout.
//!
//! `blocking_read_stress` is the same canary for the *other* half, the blocked
//! `sys_read`; between them a wake that lost either half reds one of the two.

use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use toyos::pipe_pair;
use toyos::poller::{Poller, READABLE};

/// Round trips. Enough that a wake lost at some rate shows, small enough to
/// fit the harness's five seconds with room.
const ROUNDS: u32 = 300;

/// What the rounds must fit in — turns a lost wake into a number, not a stall.
const BOUND: Duration = Duration::from_secs(3);

/// The watcher's per-round patience: far above a live wake's latency, so only
/// a genuinely absent completion trips it. A tripped round is a short count.
const WAIT_NANOS: u64 = 200_000_000;

fn main() {
    let (reader, writer) = pipe_pair().expect("a pipe");
    let woken = AtomicU32::new(0);

    let began = Instant::now();
    thread::scope(|s| {
        // The watcher: arm a poll, block on it, count each completion.
        s.spawn(|| {
            let poller = Poller::new(1);
            let mut buf = [0u8; 1];
            for _ in 0..ROUNDS {
                poller.watch(&reader, READABLE, 0);
                let mut got = false;
                poller.wait(1, WAIT_NANOS, |_| got = true);
                if !got {
                    // The completion never arrived — the ring half was lost.
                    return;
                }
                woken.fetch_add(1, Ordering::Relaxed);
                // Drain the byte so the next round arms on an empty pipe.
                let _ = reader.read(&mut buf);
            }
        });

        // The writer: one byte per round, paced behind the watcher's count so
        // the pipe never fills and every write is a distinct readable edge.
        s.spawn(|| {
            for round in 0..ROUNDS {
                while woken.load(Ordering::Relaxed) < round
                    && began.elapsed() < BOUND
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
    assert!(
        elapsed < BOUND,
        "the {ROUNDS} rounds took {elapsed:?}, past the {BOUND:?} bound — a wake was slow \
         enough to be a lost one recovered by a later edge",
    );
    println!("poll_wake_pipe: {ROUNDS} readable edges each woke the armed ring in {elapsed:?}");
}
