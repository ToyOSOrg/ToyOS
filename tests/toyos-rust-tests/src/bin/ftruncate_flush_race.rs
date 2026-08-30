//! A truncate against a flush's metadata pair, staged inside the window.
//!
//! `flush_file` reads the file's size and writes it to the filesystem as two
//! steps under the VFS lock; `SYS_FTRUNCATE`'s resize once took no VFS lock,
//! so a truncate landing between them recorded the older size on a flush
//! `iod`'s drain had already popped. The `ftruncate-flush-stall` actuator
//! holds every flush of this file inside that window for 400ms and reports
//! which way the race went; this binary makes the race happen — one thread
//! fsyncs into the stall, the main thread truncates against it — and
//! `tests/common/volumes.rs::ftruncate_flush_race` reads the verdict, then
//! judges the shut-down volume with the independent FAT reader and checker.
//!
//! Attempted in a loop rather than once: the stall spins with preemption off,
//! so the truncating thread's sleep can overshoot the whole window and land in
//! a lock-free gap between two stalled flushes. One attempt whose truncate
//! demonstrably waited is the claim; a resize that takes no VFS lock can never
//! produce one, which is what makes the loop a one-sided coin only the fixed
//! kernel can flip.

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
/// Waiting this long, the truncate can only have been serialised against a
/// 400ms-stalled holder; a lockless resize returns in microseconds.
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

    // A settling fsync so the truncated size is durable before the host reads
    // the volume behind the shut-down kernel.
    f.sync_all().expect("the settling fsync");
    println!("attempt {attempt}: the truncate waited {waited:?} for the stalled flush; {SHORT} bytes settled");
}
