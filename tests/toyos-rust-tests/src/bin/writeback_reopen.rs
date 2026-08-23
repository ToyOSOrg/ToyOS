//! A re-open racing a pending write-back reads the buffered pages, not the
//! device.
//!
//! When the last handle of a modified file drops, its dirty pages no longer
//! flush on the closing thread — they are pinned in the cache and `iod` flushes
//! them later (`kernel::writeback`). This is the invariant the whole write-back
//! queue rests on: a file's dirty pages outlive the handle that dirtied them,
//! and a re-open before the drain sees them rather than the device.
//!
//! **`writeback-stall` parks `iod` before any teardown**, so the write-back is
//! provably still pending when this re-opens the file: the re-open precedes the
//! flush and must still read what was written. This is on `/home`, which is
//! NVMe-backed, so if the pin were broken — the last close discarding the pages,
//! as it did before this change — the re-open would read the device, which has
//! not been written, and get zeros or a short file.

use std::fs;
use std::io::{Read, Write};

const PATH: &str = "/home/wb_reopen.bin";
/// Three pages and a bit, so the file is several dirty pages rather than one.
const LEN: usize = 3 * 4096 + 137;

fn distinctive() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(31) ^ 0xA5) as u8).collect()
}

fn main() {
    let want = distinctive();

    // Write and drop the handle. No `sync_all`: the point is the un-flushed,
    // buffered write. The bytes now live only in the file cache, pinned by the
    // write-back queue because the last handle just went.
    {
        let mut f = fs::File::create(PATH).unwrap_or_else(|e| panic!("create {PATH}: {e}"));
        f.write_all(&want).expect("write the distinctive bytes");
    }

    // `iod` is parked before it can flush (writeback-stall), so this re-open
    // provably precedes the flush. It must read the pinned pages, not the
    // device.
    let mut got = Vec::new();
    {
        let mut f = fs::File::open(PATH).expect("re-open before the write-back drains");
        f.read_to_end(&mut got).expect("read back the buffered file");
    }

    assert_eq!(
        got.len(),
        want.len(),
        "re-open read {} bytes, wrote {} — a pending write-back was not read from the cache",
        got.len(),
        want.len()
    );
    if let Some(at) = got.iter().zip(&want).position(|(a, b)| a != b) {
        panic!("re-open differs from what was written at byte {at}: the buffered pages were not read");
    }

    println!("re-open before the write-back drains read all {LEN} buffered bytes");
}
