//! Writes the boot stick through usbd for ever, so the boot deadline resets
//! the machine with a write in flight: `stick_survives_a_reset_under_load`'s
//! load. A file in `/home`, which a metal boot's DATA volume holds on the
//! stick, rewritten in place and made durable after every mebibyte, so the
//! device is always programming flash or about to.

use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};

const PATH: &str = "/home/stick-load.bin";
/// Small enough for the stick's DATA volume, rewritten from its start.
const SPAN: u64 = 8 << 20;
const CHUNK: usize = 256 << 10;
const SYNC_EVERY: u64 = 1 << 20;
/// How often it says how far it got.
const SAY_EVERY: u64 = 4 << 20;

fn main() {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(PATH)
        .unwrap_or_else(|e| panic!("stick_load: {PATH}: {e}"));
    let mut chunk = vec![0u8; CHUNK];
    let mut written: u64 = 0;
    loop {
        if written % SPAN == 0 {
            file.seek(SeekFrom::Start(0)).expect("stick_load: seek to the start");
        }
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte = (written as usize + i) as u8;
        }
        file.write_all(&chunk).expect("stick_load: write");
        written += CHUNK as u64;
        if written % SYNC_EVERY == 0 {
            file.sync_all().expect("stick_load: fsync");
        }
        if written % SAY_EVERY == 0 {
            println!("stick_load: {} MiB written and synced", written >> 20);
        }
    }
}
