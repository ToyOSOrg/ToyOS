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

use std::fs;
use std::io::{Read, Write};
use std::thread;
use std::time::Duration;

/// Mirrored in `tests/common/volumes.rs::writeback_durability`.
const PATH: &str = "/log/wb-durable.bin";
const LEN: usize = 5 * 4096 + 91;

fn blob() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(97) ^ 0x5A) as u8).collect()
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

    println!("wrote {LEN} bytes, closed without fsync, and read them back after the write-back drained");
}
