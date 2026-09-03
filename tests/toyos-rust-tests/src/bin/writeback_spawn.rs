//! A program written, closed and spawned runs, with the write-back still owed.
//!
//! Write, close, exec is what a compiler does with the binary it just produced,
//! and it is the project's self-hosting north star. Since the write-back queue
//! landed the last close no longer flushes: the dirty pages are pinned in the
//! file cache and `iod` writes them out later (`kernel::writeback`). But
//! `loader::spawn` does not read the file through the cache at all — it takes a
//! *device* view (`Vfs::open_backing`), which on `/home` is the extent list and
//! the length bcachefs has recorded. Before the drain those still say what
//! `create` wrote: no extents, length 0. The kernel answered
//!
//! ```text
//! spawn: /home/disk_backtrace/child: ELF: fewer bytes than a file header
//! ```
//!
//! **`writeback-stall` parks `iod` before any teardown**, so the flush is
//! provably still owed when the spawn below happens — the whole race, held open
//! for as long as the test needs it, instead of the few milliseconds a loaded
//! host happened to give CI. `disk_backtrace` is the same sequence unstaged, and
//! reproduced this once in eleven local runs and four times out of four on a
//! starved hosted shard.
//!
//! `/home` and not `/tmp`: tmpfs pages *are* the file, so nothing there can be
//! behind. The claim is about a device.
//!
//! The payload is this binary itself, re-run with an argument, so the thing
//! spawned off the disk is a megabyte-scale PIE whose text, relocations and
//! symbols all demand-page through the backing — a short file would prove the
//! header and nothing after it.

use std::fs;
use std::io::Write;
use std::process::Command;

const DIR: &str = "/home/writeback_spawn";
const IN_ROOT: &str = "/bin/test_rs_writeback_spawn";
const ON_DISK: &str = "/home/writeback_spawn/child";
const STILL_OPEN: &str = "/home/writeback_spawn/held";
/// What tells this binary it is the copy being run rather than the test.
const CHILD: &str = "spawned-from-disk";

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        // The marker is the proof the child reached its own code, rather than
        // an exit status a refused spawn could also produce.
        println!("  child: running from {ON_DISK}");
        return;
    }

    let _ = fs::create_dir(DIR);

    let image = fs::read(IN_ROOT).unwrap_or_else(|e| panic!("read {IN_ROOT}: {e}"));
    // `fs::write` opens, writes and drops the handle. With `iod` parked that
    // drop pins the pages and enqueues the file; nothing has reached the device.
    fs::write(ON_DISK, &image).unwrap_or_else(|e| panic!("write {ON_DISK}: {e}"));
    println!("  copied {} bytes to {ON_DISK} with the write-back still owed", image.len());

    let status = Command::new(ON_DISK)
        .arg(CHILD)
        .status()
        .unwrap_or_else(|e| panic!("spawn {ON_DISK}: {e}"));
    assert!(
        status.success(),
        "the child spawned off a file whose write-back is still pending exited {:?}",
        status.code()
    );

    // The differential: the same bytes read back a second way, off the device.
    // The spawn drained the queue, so the file left the cache and this re-open
    // resolves fresh extents and reads NVMe blocks — nothing of what was written
    // is still buffered. Compared against ROOT's copy, which is a different
    // mount and a different filesystem, so a length that matched by construction
    // could not hide a wrong byte.
    let back = fs::read(ON_DISK).unwrap_or_else(|e| panic!("read back {ON_DISK}: {e}"));
    assert_eq!(
        back.len(),
        image.len(),
        "read back {} bytes off the device, wrote {}",
        back.len(),
        image.len()
    );
    if let Some(at) = back.iter().zip(&image).position(|(a, b)| a != b) {
        panic!("what the device holds differs from what was written at byte {at}");
    }

    println!("  PASS: spawned {ON_DISK} before its write-back drained, {} bytes verified off the device", back.len());

    still_open_and_dirty(&image);

    println!("writeback spawn test passed");
}

/// The same race with the handle still held, which no queue knows about.
///
/// A closed file is on the write-back queue and `Vfs::open_backing` drains it;
/// a file still open is on no queue, so its bytes are cache pages and the
/// mount's record is what `create` wrote. Handle reads answered from the cache
/// and a spawn's device view did not, so which of two readers of one file a
/// caller got was decided by which syscall it used.
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
        panic!("what the device holds differs from what was written at byte {at}");
    }
    println!("  PASS: spawned {STILL_OPEN} while its writer still held it, {} bytes verified", back.len());
}
