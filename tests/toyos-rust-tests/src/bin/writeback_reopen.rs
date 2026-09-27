//! A re-open racing a pending write-back reads what was written.
//!
//! When the last handle of a modified file drops, the kernel does not tear it
//! down on the closing thread: the file is pinned in the cache and `iod` tears
//! it down later (`kernel::writeback`). This is the invariant the write-back
//! queue rests on: a file's pages outlive the handle that dirtied them, and a
//! re-open before the drain is handed the pinned file.
//!
//! **`writeback-stall` parks `iod` before any teardown**, so the teardown is
//! provably still owed when this re-opens the file. `/tmp` is the one writable
//! directory the kernel serves, so it is where the queue has files: if the
//! last close released the file instead of pinning it, the re-open would find
//! its name and none of its pages, and read an empty file.

use std::fs;
use std::io::{Read, Write};

const PATH: &str = "/tmp/wb_reopen.bin";
/// Three pages and a bit, so the file is several dirty pages rather than one.
const LEN: usize = 3 * 4096 + 137;

fn distinctive() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(31) ^ 0xA5) as u8).collect()
}

fn main() {
    let want = distinctive();

    // Write and drop the handle: the last close pins the file and queues its
    // teardown, which `iod` never runs on this boot.
    {
        let mut f = fs::File::create(PATH).unwrap_or_else(|e| panic!("create {PATH}: {e}"));
        f.write_all(&want).expect("write the distinctive bytes");
    }

    let mut got = Vec::new();
    {
        let mut f = fs::File::open(PATH).expect("re-open before the write-back drains");
        f.read_to_end(&mut got).expect("read back the pinned file");
    }

    assert_eq!(
        got.len(),
        want.len(),
        "re-open read {} bytes, wrote {} — the last close released a file its teardown still owed",
        got.len(),
        want.len()
    );
    if let Some(at) = got.iter().zip(&want).position(|(a, b)| a != b) {
        panic!("re-open differs from what was written at byte {at}: the pinned pages were not read");
    }
    let _ = fs::remove_file(PATH);

    println!("re-open before the write-back drains read all {LEN} pinned bytes");
}
