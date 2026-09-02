//! Dropping the last handle of a modified file loses no bytes: the write-back
//! queue and the shutdown drain make them durable with no fsync.
//!
//! The write-back queue defers a closed file's flush onto `iod`; this proves the
//! flush still happens. It writes distinctive bytes to a `/log` file, drops the
//! handle **without** fsync, lets `iod` drain — which flushes the pages to the
//! device and drops the file from the cache — and re-reads. The bytes now come
//! from the device, because the cache no longer holds the file; if `iod` had
//! skipped the flush, the device would answer a short/zero file.
//!
//! The host side (`tests/common/volumes.rs::writeback_durability`) then shuts the
//! machine down and reads the `/log` volume off the image with an independent FAT
//! implementation and the fatgen103 checker — neither the kernel's own cache
//! logic — so the bytes on the device and the structure around them are judged
//! by something that is not the code under test.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::thread;
use std::time::Duration;

/// Mirrored in `tests/common/volumes.rs::writeback_durability`.
const PATH: &str = "/log/wb-durable.bin";
const LEN: usize = 5 * 4096 + 91;
/// The second file, and the same mirroring.
const SHRUNK: &str = "/log/wb-shrunk.bin";
const SHRUNK_LEN: usize = 3 * 4096;
pub const CUT: u64 = 100;

fn blob() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(97) ^ 0x5A) as u8).collect()
}

fn seed() -> Vec<u8> {
    (0..SHRUNK_LEN).map(|i| (i.wrapping_mul(31).wrapping_add(7)) as u8 | 1).collect()
}

/// The file shrunk only once cold, and where its read lands; same mirroring.
const REOPENED: &str = "/log/wb-reopened.bin";
const WITNESS: &str = "/log/wb-served.bin";

/// The file `fat-flush-meta-refuse` refuses; mirrored in `kernel/src/fat32_adapter.rs`.
const RETRY: &str = "/log/wb-retry.bin";
const RETRY_AT: u64 = 2 * 4096;

fn kept() -> Vec<u8> {
    (0..4096usize).map(|i| (i.wrapping_mul(53) ^ 0xC3) as u8).collect()
}

fn main() {
    let want = blob();

    // Write and drop the handle. No `sync_all`: durability must come from the
    // write-back queue and the shutdown drain, not from the caller asking.
    {
        let mut f = fs::File::create(PATH).unwrap_or_else(|e| panic!("create {PATH}: {e}"));
        f.write_all(&want).expect("write the blob");
    }

    // Give `iod` a turn to drain the write-back the close pinned. The sleep
    // yields the CPU, so on one core `iod` runs and on more it has long
    // finished — either way the file is flushed to the device and dropped from
    // the cache before the read below, so the read is a cache miss served by the
    // device. The margin is enormous: the drain is microseconds of work.
    thread::sleep(Duration::from_millis(200));

    let mut got = Vec::new();
    {
        let mut f = fs::File::open(PATH).expect("re-open after the write-back drained");
        f.read_to_end(&mut got).expect("read the file back from the device");
    }
    assert_eq!(
        got.len(),
        want.len(),
        "after close + drain, {PATH} is {} bytes on re-read, wrote {} — a deferred flush was lost",
        got.len(),
        want.len()
    );
    if let Some(at) = got.iter().zip(&want).position(|(a, b)| a != b) {
        panic!("{PATH} differs from what was written at byte {at}: the write-back did not reach the device");
    }

    shrink_unflushed_then_regrow();
    shrink_a_file_the_cache_no_longer_holds();
    a_refused_metadata_write_keeps_the_pages_it_wrote();

    println!("wrote {LEN} bytes, closed without fsync, and read them back after the write-back drained");
}

/// A flush that trims a shrink, writes a page above the mark, and is then
/// refused at its metadata write. The retry has no dirty pages left, so a mark
/// that outlived the trim would trim a second time and free the page the first
/// attempt wrote. The volume is where that shows, and the host reads it.
fn a_refused_metadata_write_keeps_the_pages_it_wrote() {
    let mut f = OpenOptions::new()
        .read(true).write(true).create(true).truncate(true)
        .open(RETRY)
        .unwrap_or_else(|e| panic!("create {RETRY}: {e}"));
    f.write_all(&seed()).expect("write the seed");
    f.sync_all().expect("fsync the seed");
    f.set_len(CUT).expect("shrink into the first page");
    f.set_len(SHRUNK_LEN as u64).expect("regrow");
    f.seek(SeekFrom::Start(RETRY_AT)).expect("seek above the mark");
    f.write_all(&kept()).expect("write a page above the mark");

    // Refused once by the actuator and retried by `SYS_FSYNC` itself, so this
    // returning is the retry having succeeded, not the first attempt.
    f.sync_all().expect("the fsync whose second metadata write is refused");
    drop(f);
    println!("shrank {RETRY} to {CUT}, regrew it, wrote a page at {RETRY_AT}, fsynced through one refusal");
}

/// The same shrink and regrow over a page the cache does not hold, with what
/// the kernel *served* carried to the volume for the host to judge — the device
/// is right either way, because `Fat32::set_len` zero-fills on every grow.
fn shrink_a_file_the_cache_no_longer_holds() {
    {
        let mut f = fs::File::create(REOPENED).unwrap_or_else(|e| panic!("create {REOPENED}: {e}"));
        f.write_all(&seed()).expect("write the seed");
        f.sync_all().expect("fsync the seed");
    }
    thread::sleep(Duration::from_millis(200));

    let mut f = OpenOptions::new()
        .read(true).write(true)
        .open(REOPENED)
        .unwrap_or_else(|e| panic!("reopen {REOPENED}: {e}"));
    f.set_len(CUT).expect("shrink into a page the cache does not hold");
    f.set_len(SHRUNK_LEN as u64).expect("regrow");
    let mut served = Vec::new();
    f.read_to_end(&mut served).expect("read the regrown file back");
    drop(f);

    let mut w = fs::File::create(WITNESS).unwrap_or_else(|e| panic!("create {WITNESS}: {e}"));
    w.write_all(&served).expect("write what the read served");
    w.sync_all().expect("fsync the witness");
    drop(w);

    thread::sleep(Duration::from_millis(200));
    println!("shrank a cold {REOPENED} to {CUT}, regrew it, and wrote the {} bytes it served to {WITNESS}", served.len());
}

/// A durable file shrunk and regrown with nothing flushed between the two, and
/// then only closed. The drain's single `update_metadata` carries the final
/// size alone, so the volume the host judges is the only place the dropped
/// clusters can still be named — and the checker cannot see the harm, because
/// a file naming its own old clusters is a structurally valid file.
fn shrink_unflushed_then_regrow() {
    let want = seed();
    let mut f = OpenOptions::new()
        .read(true).write(true).create(true).truncate(true)
        .open(SHRUNK)
        .unwrap_or_else(|e| panic!("create {SHRUNK}: {e}"));
    f.write_all(&want).expect("write the seed");
    f.sync_all().expect("fsync the seed");
    f.set_len(CUT).expect("shrink into the first page");
    f.set_len(SHRUNK_LEN as u64).expect("regrow");
    drop(f);

    thread::sleep(Duration::from_millis(200));
    println!("shrank {SHRUNK} to {CUT} and regrew it to {SHRUNK_LEN}, closed without fsync");
}
