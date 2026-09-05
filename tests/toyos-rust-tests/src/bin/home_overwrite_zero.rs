//! A same-length overwrite of a `/home` file, read back through the name the
//! overwrite rebound.
//!
//! `File::create` on a path that exists unlinks what answered to it and creates
//! a new file under the same name. A handle still holding the unlinked file
//! keeps it alive, so its teardown runs *after* the new file has taken the
//! name. The re-read below is what that teardown may not answer for: it must
//! give back the bytes just written, not the empty entry the create left on the
//! device.
//!
//! Host half: `home_overwrite_reads_back` in `tests/common/storage.rs`, which
//! reads the file off the NVMe image once the guest is gone and compares its
//! length against the one printed here.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

/// Mirrored in `tests/common/storage.rs`; `/home` is a directory of DATA, so
/// the host's reader sees these names without the mount point.
const PINNED: &str = "/home/overwrite-pinned.bin";
const LOOPED: &str = "/home/overwrite-looped.bin";
/// The recorded length, to the byte.
const LEN: usize = 1_902_104;
const ROUNDS: usize = 4;
/// Long enough for `iod` to run the queued teardown of the unlinked file.
const DRAIN: Duration = Duration::from_millis(300);

fn payload(seed: u8) -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(131) ^ seed as usize) as u8).collect()
}

fn main() {
    let first = payload(0x11);
    let second = payload(0x22);
    recorded_shape(&first, &second);
    pinned_overwrite(&first, &second);
    println!("all home overwrite tests passed");
}

/// The shape the defect was recorded at, with no handle held across anything:
/// write, read, overwrite at the same length, read.
fn recorded_shape(first: &[u8], second: &[u8]) {
    for round in 0..ROUNDS {
        fs::write(LOOPED, first).unwrap_or_else(|e| panic!("write {LOOPED}: {e}"));
        let a = fs::read(LOOPED).unwrap_or_else(|e| panic!("read {LOOPED}: {e}")).len();
        fs::write(LOOPED, second).unwrap_or_else(|e| panic!("overwrite {LOOPED}: {e}"));
        let b = fs::read(LOOPED).unwrap_or_else(|e| panic!("re-read {LOOPED}: {e}")).len();
        println!("  round {round}: read back {a} then {b}");
        assert_eq!((a, b), (LEN, LEN), "round {round} of the recorded shape");
    }
    println!("  PASS {ROUNDS} rounds of write/read/write/read at {LEN} bytes");
}

/// The same overwrite with the displaced file's teardown made to land after the
/// new file holds the name: a reader holds the old file open across the
/// `File::create`, and is dropped while the new file is still open.
fn pinned_overwrite(first: &[u8], second: &[u8]) {
    fs::write(PINNED, first).unwrap_or_else(|e| panic!("write {PINNED}: {e}"));
    let held = File::open(PINNED).unwrap_or_else(|e| panic!("open {PINNED}: {e}"));
    let mut writer = File::create(PINNED).unwrap_or_else(|e| panic!("create {PINNED}: {e}"));
    writer.write_all(second).unwrap_or_else(|e| panic!("overwrite {PINNED}: {e}"));
    drop(held);
    thread::sleep(DRAIN);

    let mut got = Vec::new();
    File::open(PINNED)
        .unwrap_or_else(|e| panic!("re-open {PINNED}: {e}"))
        .read_to_end(&mut got)
        .unwrap_or_else(|e| panic!("re-read {PINNED}: {e}"));
    // Printed before any verdict: the host compares this count against what it
    // reads off the device, and needs the line on the failing arm too.
    println!("HOME-OVERWRITE {PINNED} read back {} bytes", got.len());

    // Dropped and drained before the assertions, so the device carries the
    // overwrite whatever the re-read answered.
    drop(writer);
    thread::sleep(DRAIN);

    assert_eq!(got.len(), LEN, "the same-length overwrite read back short");
    assert!(got == second, "{PINNED} read back bytes that are not the overwrite's");
    println!("  PASS {PINNED} read back all {LEN} overwritten bytes");
}
