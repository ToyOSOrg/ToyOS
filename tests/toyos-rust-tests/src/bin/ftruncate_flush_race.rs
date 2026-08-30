//! A truncate against a flush's size-read/`update_metadata` pair, staged inside it.
//!
//! `SYS_FTRUNCATE`'s resize once took no VFS lock, so a truncate landing
//! between a flush's two steps recorded the older size. `ftruncate-flush-stall`
//! holds every flush of this file open for 400ms; this binary races a truncate
//! against it, and `tests/common/volumes.rs::ftruncate_flush_race` reads the
//! kernel's verdict and re-judges the shut-down volume with the FAT reader and
//! checker. Looped, not once: the stall spins preemption-off, so one sleep can
//! overshoot into a lock-free gap — a lockless resize can never make a
//! contended attempt, which keeps the loop one-sided.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::thread;
use std::time::{Duration, Instant};

/// Mirrored in `tests/common/volumes.rs::ftruncate_flush_race`, and in the
/// actuator's own path filter (`kernel/src/vfs.rs::stalled_metadata_window`).
const PATH: &str = "/log/truncate-race.bin";
const FULL: usize = 3 * 4096;
const SHORT: u64 = 5000;

const INTO_WINDOW: Duration = Duration::from_millis(50);
/// Only a serialised truncate waits this long against the 400ms stall; a lockless one returns in microseconds.
const CONTENDED: Duration = Duration::from_millis(150);
const ATTEMPTS: u32 = 10;

fn main() {
    let mut f = OpenOptions::new().create(true).write(true).open(PATH).expect("create");

    let mut contended = None;
    for attempt in 0..ATTEMPTS {
        f.seek(SeekFrom::Start(0)).expect("rewind");
        f.write_all(&vec![0xB6u8; FULL]).expect("fill");

        let flusher = {
            let f = f.try_clone().expect("clone handle");
            thread::spawn(move || f.sync_all().expect("the stalled fsync"))
        };
        thread::sleep(INTO_WINDOW);
        let began = Instant::now();
        f.set_len(SHORT).expect("truncate");
        let waited = began.elapsed();
        flusher.join().expect("flusher panicked");

        if waited >= CONTENDED {
            contended = Some((attempt, waited));
            break;
        }
    }
    let Some((attempt, waited)) = contended else {
        panic!(
            "in {ATTEMPTS} attempts the truncate never once waited for the stalled flush — \
             the resize does not serialise with the metadata window",
        );
    };

    // The truncated size durable before the host reads the shut-down volume.
    f.sync_all().expect("the settling fsync");
    println!("attempt {attempt}: the truncate waited {waited:?} for the stalled flush; {SHORT} bytes settled");
}
