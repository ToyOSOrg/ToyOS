//! A same-length overwrite of a `/home` file, read back through the name the
//! overwrite rebound.
//!
//! `File::create` unlinks what answered to the path and creates a new file
//! under the same name; a handle still holding the unlinked file keeps it
//! alive, so its teardown runs after the new file has taken the name. The
//! re-read must give back the bytes just written, not the empty entry the
//! create left on the device.
//!
//! Host half: `home_overwrite_reads_back` in `tests/common/storage.rs`.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

/// Mirrored in `tests/common/storage.rs`, whose reader sees these names without
/// the mount point: `/home` is a directory of DATA.
const PINNED: &str = "/home/overwrite-pinned.bin";
const LOOPED: &str = "/home/overwrite-looped.bin";
const LEN: usize = 1_902_104;
const POLL_STEP: Duration = Duration::from_millis(10);
/// The window is the green arm's whole cost, and a step of it is the resolution.
const POLL_STEPS: u32 = 30;

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

/// The recorded shape, with no handle held across anything.
fn recorded_shape(first: &[u8], second: &[u8]) {
    fs::write(LOOPED, first).unwrap_or_else(|e| panic!("write {LOOPED}: {e}"));
    let a = fs::read(LOOPED).unwrap_or_else(|e| panic!("read {LOOPED}: {e}")).len();
    fs::write(LOOPED, second).unwrap_or_else(|e| panic!("overwrite {LOOPED}: {e}"));
    let b = fs::read(LOOPED).unwrap_or_else(|e| panic!("re-read {LOOPED}: {e}")).len();
    println!("  recorded shape: read back {a} then {b}");
    assert_eq!((a, b), (LEN, LEN), "the recorded shape");
}

/// The same overwrite with the displaced file's teardown made to land after the
/// new file holds the name: a reader is held across the `File::create`.
fn pinned_overwrite(first: &[u8], second: &[u8]) {
    fs::write(PINNED, first).unwrap_or_else(|e| panic!("write {PINNED}: {e}"));
    let held = File::open(PINNED).unwrap_or_else(|e| panic!("open {PINNED}: {e}"));
    let mut writer = File::create(PINNED).unwrap_or_else(|e| panic!("create {PINNED}: {e}"));
    writer.write_all(second).unwrap_or_else(|e| panic!("overwrite {PINNED}: {e}"));
    drop(held);

    // Every length over the window, not one after a wait; `fs::metadata` is the `file_cache::size` a read bounds itself by.
    let mut lowest = u64::MAX;
    let mut steps = 0;
    for _ in 0..POLL_STEPS {
        let len = fs::metadata(PINNED).unwrap_or_else(|e| panic!("stat {PINNED}: {e}")).len();
        lowest = lowest.min(len);
        steps += 1;
        if lowest != LEN as u64 {
            break;
        }
        thread::sleep(POLL_STEP);
    }
    println!("  poll: {steps} step(s) of {POLL_STEP:?}, lowest {lowest}");
    let mut got = Vec::new();
    File::open(PINNED)
        .unwrap_or_else(|e| panic!("re-open {PINNED}: {e}"))
        .read_to_end(&mut got)
        .unwrap_or_else(|e| panic!("re-read {PINNED}: {e}"));
    lowest = lowest.min(got.len() as u64);
    // Before any verdict: the host holds this count against the device, and needs it on the failing arm.
    println!("HOME-OVERWRITE {PINNED} read back {lowest} bytes");

    // Dropped first, so `SYS_SHUTDOWN`'s drain carries the overwrite to the device whatever the name answered.
    drop(writer);

    assert_eq!(lowest, LEN as u64, "the same-length overwrite read back short");
    assert!(got == second, "{PINNED} read back bytes that are not the overwrite's");
    println!("  PASS {PINNED} answered {LEN} bytes at every step of the poll");
}
