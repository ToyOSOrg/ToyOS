//! A truncate raced against a flush's stalled `update_metadata` window.
//!
//! `SYS_FTRUNCATE`'s resize once took no VFS lock, so a truncate could land
//! between a flush's two steps and record the older size. `ftruncate-flush-stall`
//! holds every flush of this file open for 400ms; this binary races a truncate
//! against it, [`ATTEMPTS`] times, and the host reads the size the volume kept.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::thread;
use std::time::Duration;

/// Mirrored in `tests/common/volumes.rs::ftruncate_flush_race`, and in the
/// actuator's own path filter (`kernel/src/vfs.rs::stalled_metadata_window`).
const PATH: &str = "/log/truncate-race.bin";
const FULL: usize = 3 * 4096;
const SHORT: u64 = 5000;

/// Into the stalled window before the truncate: a pace that aims the race, and
/// never a verdict.
const INTO_WINDOW: Duration = Duration::from_millis(50);
const ATTEMPTS: u32 = 10;

fn main() {
    let mut f = OpenOptions::new().create(true).write(true).open(PATH).expect("create");

    for _ in 0..ATTEMPTS {
        f.seek(SeekFrom::Start(0)).expect("rewind");
        f.write_all(&vec![0xB6u8; FULL]).expect("fill");

        let flusher = {
            let f = f.try_clone().expect("clone handle");
            thread::spawn(move || f.sync_all().expect("the stalled fsync"))
        };
        thread::sleep(INTO_WINDOW);
        f.set_len(SHORT).expect("truncate");
        flusher.join().expect("flusher panicked");
    }

    // The truncated size durable before the host reads the shut-down volume.
    f.sync_all().expect("the settling fsync");
    println!("{ATTEMPTS} truncates raced the stalled flush; {SHORT} bytes settled");
}
