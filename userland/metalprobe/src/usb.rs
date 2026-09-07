//! What the boot stick answers about itself, through the whole stack that
//! reaches it: the FAT32 driver, the page cache, `iod` and the xHCI mass-storage
//! transport.
//!
//! **`/log` is the one writable place on a metal boot.** A flashed image carries
//! an ESP, a read-only ROOT and the `TOYOS-LOG` volume, so the only durable
//! bytes a job can lay down are on the FAT32 partition `logd` also writes — and
//! that partition is the one the driver mounts afterwards, which is what makes
//! the host's own FAT check possible over the same bytes.

use std::fs;
use std::io::{Read, Write};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::{rate, Measured, Refusal};

/// How much each measurement moves. Large enough that the per-call costs are
/// noise against the transfer and small enough that both files plus a boot's
/// `logd` output fit the 34 MiB the log volume is formatted at.
const BYTES: usize = 6 * 1024 * 1024;

/// The chunk a write is handed down in — a whole number of the 4 KiB pages the
/// cache is made of, so the measurement is not about a partial-page read/modify.
const CHUNK: usize = 64 * 1024;

const WRITTEN: &str = "/log/metal-write.bin";
const READ: &str = "/log/metal-read.bin";

/// The payload, derived from its own offset so a block served from the wrong
/// place reads as the wrong bytes rather than as plausible ones.
fn payload() -> Vec<u8> {
    (0..BYTES).map(|i| (i.wrapping_mul(0x9E37) >> 3) as u8).collect()
}

/// Write `BYTES` and make them durable, timing the whole of it: **the fsync is
/// inside the span**, because a rate that stops at the last `write` is a rate
/// for the page cache and not for the stick.
pub fn write() -> Measured {
    let blob = payload();
    let began = Instant::now();
    {
        let mut f = fs::File::create(WRITTEN).map_err(|_| Refusal::NoVolume)?;
        for chunk in blob.chunks(CHUNK) {
            f.write_all(chunk).map_err(|_| Refusal::NoVolume)?;
        }
        f.sync_all().map_err(|_| Refusal::NoVolume)?;
    }
    let span = began.elapsed();
    // The volume is 34 MiB and `logd` shares it; a measurement that left its
    // own file behind would shrink what the next boot's log may write.
    fs::remove_file(WRITTEN).map_err(|_| Refusal::NoVolume)?;
    rate((BYTES / 1024) as u64, span.as_nanos())
}

/// Read `BYTES` back off the device and check them, timing only the read.
///
/// **The staged file is closed and left to drain before the clock starts.**
/// `iod` flushes what the close pinned and drops the file from the cache, so
/// the timed read is a cache miss the stick has to answer — the same reason
/// `writeback_durability` waits here.
pub fn read() -> Measured {
    let blob = payload();
    {
        let mut f = fs::File::create(READ).map_err(|_| Refusal::NoVolume)?;
        for chunk in blob.chunks(CHUNK) {
            f.write_all(chunk).map_err(|_| Refusal::NoVolume)?;
        }
        f.sync_all().map_err(|_| Refusal::NoVolume)?;
    }
    sleep(Duration::from_millis(200));

    let began = Instant::now();
    let mut got = Vec::with_capacity(BYTES);
    fs::File::open(READ)
        .and_then(|mut f| f.read_to_end(&mut got))
        .map_err(|_| Refusal::NoVolume)?;
    let span = began.elapsed();

    fs::remove_file(READ).map_err(|_| Refusal::NoVolume)?;
    if got != blob {
        return Err(Refusal::Disagreed);
    }
    rate((BYTES / 1024) as u64, span.as_nanos())
}
