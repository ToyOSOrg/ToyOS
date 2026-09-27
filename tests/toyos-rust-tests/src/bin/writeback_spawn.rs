//! A program written, closed and spawned runs, with its write-back still owed.
//!
//! Write, close, exec is what a compiler does with the binary it just produced,
//! and it is the project's self-hosting north star. The last close does not
//! tear the file down: it is pinned in the file cache and `iod` tears it down
//! later (`kernel::writeback`). `loader::spawn` does not read the file through
//! a handle — it takes a view of the file (`Vfs::open_backing`), which drains
//! the queue inline first, so the spawn runs the teardown `iod` owes.
//!
//! **`writeback-stall` parks `iod` before any teardown**, so the teardown is
//! provably the spawn's own. `/tmp` is the one writable directory the kernel
//! serves: a teardown that released a live file's pages there would leave the
//! spawn's view reading zeros where the binary was, and the load refused.
//!
//! The payload is this binary itself, re-run with an argument, so the thing
//! spawned is a megabyte-scale PIE whose text, relocations and symbols all
//! demand-page through the view — a short file would prove the header and
//! nothing after it.

use std::fs;
use std::io::Write;
use std::process::Command;

const DIR: &str = "/tmp/writeback_spawn";
const IN_ROOT: &str = "/system/bin/test_rs_writeback_spawn";
const WRITTEN: &str = "/tmp/writeback_spawn/child";
const STILL_OPEN: &str = "/tmp/writeback_spawn/held";
/// What tells this binary it is the copy being run rather than the test.
const CHILD: &str = "spawned-from-tmp";

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        // The marker is the proof the child reached its own code, rather than
        // an exit status a refused spawn could also produce.
        println!("  child: running from {WRITTEN}");
        return;
    }

    let _ = fs::create_dir(DIR);

    let image = fs::read(IN_ROOT).unwrap_or_else(|e| panic!("read {IN_ROOT}: {e}"));
    // `fs::write` opens, writes and drops the handle. With `iod` parked that
    // drop pins the file and queues its teardown.
    fs::write(WRITTEN, &image).unwrap_or_else(|e| panic!("write {WRITTEN}: {e}"));
    println!("  copied {} bytes to {WRITTEN} with its teardown still owed", image.len());

    let status = Command::new(WRITTEN)
        .arg(CHILD)
        .status()
        .unwrap_or_else(|e| panic!("spawn {WRITTEN}: {e}"));
    assert!(
        status.success(),
        "the child spawned off a file whose teardown was still owed exited {:?}",
        status.code()
    );

    // The same bytes read back a second way, through a handle, after the
    // spawn ran the teardown: compared against ROOT's copy, a different mount,
    // so a length that matched by construction could not hide a wrong byte.
    let back = fs::read(WRITTEN).unwrap_or_else(|e| panic!("read back {WRITTEN}: {e}"));
    assert_eq!(back.len(), image.len(), "read back {} bytes, wrote {}", back.len(), image.len());
    if let Some(at) = back.iter().zip(&image).position(|(a, b)| a != b) {
        panic!("what the file holds after its teardown differs from what was written at byte {at}");
    }

    println!("  PASS: spawned {WRITTEN} before its teardown, {} bytes verified after it", back.len());

    still_open_and_dirty(&image);

    let _ = fs::remove_dir_all(DIR);
    println!("writeback spawn test passed");
}

/// The same with the handle still held, which no queue knows about: the view
/// a spawn takes flushes a file still open and dirty before it reads it.
fn still_open_and_dirty(image: &[u8]) {
    let mut held = fs::File::create(STILL_OPEN).unwrap_or_else(|e| panic!("create {STILL_OPEN}: {e}"));
    held.write_all(image).unwrap_or_else(|e| panic!("write {STILL_OPEN}: {e}"));

    let status = Command::new(STILL_OPEN)
        .arg(CHILD)
        .status()
        .unwrap_or_else(|e| panic!("spawn {STILL_OPEN}: {e}"));
    assert!(
        status.success(),
        "the child spawned off a still-open dirty file exited {:?}",
        status.code()
    );

    drop(held);
    let back = fs::read(STILL_OPEN).unwrap_or_else(|e| panic!("read back {STILL_OPEN}: {e}"));
    assert_eq!(back.len(), image.len(), "read back {} bytes, wrote {}", back.len(), image.len());
    if let Some(at) = back.iter().zip(image).position(|(a, b)| a != b) {
        panic!("what the file holds differs from what was written at byte {at}");
    }
    println!("  PASS: spawned {STILL_OPEN} while its writer still held it, {} bytes verified", back.len());
}
