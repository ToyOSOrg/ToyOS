//! 32 MiB written to the boot stick through usbd and made durable after every
//! mebibyte, then an end: the stick-write load `mask_windows` reads the
//! windows under.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

const PATH: &str = "/home/stick-write.bin";
const SPAN: u64 = 8 << 20;
const TOTAL: u64 = 32 << 20;
const CHUNK: usize = 256 << 10;
const SYNC_EVERY: u64 = 1 << 20;

fn main() {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(PATH)
        .unwrap_or_else(|e| panic!("stick_write: {PATH}: {e}"));
    let chunk = vec![0xa5u8; CHUNK];
    let mut written: u64 = 0;
    while written < TOTAL {
        if written % SPAN == 0 {
            file.seek(SeekFrom::Start(0)).expect("stick_write: seek");
        }
        file.write_all(&chunk).expect("stick_write: write");
        written += CHUNK as u64;
        if written % SYNC_EVERY == 0 {
            file.sync_all().expect("stick_write: fsync");
        }
    }
    println!("stick_write: {} MiB written and synced", written >> 20);
}
