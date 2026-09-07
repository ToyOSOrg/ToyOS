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

use crate::{span, Measured, Refusal};

/// The slowest a stick this project will admit may move bytes.
///
/// Not a datasheet figure: it is an order of magnitude under any USB 2.0 flash
/// device, and it exists to bound how long a measurement may take rather than
/// to describe one.
const SLOWEST_KIB_S: u64 = 512;

/// The share of the job list's whole bound one storage measurement may spend,
/// in percent.
const SHARE_PERCENT: u64 = 8;

/// How much each measurement moves — **derived from the bound the job list runs
/// under, not chosen.**
///
/// One bound covers the whole list (`toyos_tco::JOB_BOUND_MS`, measured from
/// boot), and when it expires the runner reboots: a size that could outlast its
/// share puts a machine reset in the middle of a transfer, which is what a
/// mass-storage device does not survive. At [`SLOWEST_KIB_S`] this is
/// [`SHARE_PERCENT`] of the bound, so the pair is under a sixth of it even on a
/// device far slower than any this machine will see.
const BYTES: usize =
    (SLOWEST_KIB_S * toyos_tco::JOB_BOUND_MS * SHARE_PERCENT / 100 / 1_000 * 1024) as usize;

/// Both files, plus a boot's `logd` output, inside the 34 MiB the log volume is
/// formatted at — and each removed as soon as it is measured, so only one is
/// ever on the volume at once.
const _: () = assert!(BYTES * 2 < 16 * 1024 * 1024);

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

/// Write [`BYTES`] and make them durable, timing the whole of it: **the fsync
/// is inside the span**, because a span that stops at the last `write` is a span
/// of the page cache and not of the stick.
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
    let took = began.elapsed();
    // The volume is 34 MiB and `logd` shares it; a measurement that left its
    // own file behind would shrink what the next boot's log may write.
    fs::remove_file(WRITTEN).map_err(|_| Refusal::NoVolume)?;
    span(took.as_nanos())
}

/// Read [`BYTES`] back off the device and check them, timing only the read.
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
    let took = began.elapsed();

    fs::remove_file(READ).map_err(|_| Refusal::NoVolume)?;
    if got != blob {
        return Err(Refusal::Disagreed);
    }
    span(took.as_nanos())
}
