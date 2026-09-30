//! What DATA's server evicted from its cache it reads back off its disk, byte
//! for byte.
//!
//! The file is longer than the clean blocks the server keeps: once it is
//! durable every block of it is clean, the oldest are let go, and reading it
//! back from the start is a round trip through the disk for every block the
//! cache no longer holds, each checked against what was written.

use std::fs::{self, File};
use std::io::{Read, Write};

/// Mirrored from `userland/fsd/src/cache.rs`, in 4 KiB blocks.
const CLEAN_LIMIT: usize = 16 * 1024;

const PATH: &str = "/home/fs_cache_eviction.bin";
const CHUNK: usize = 256 * 1024;
/// 8 MiB past what the cache keeps.
const LEN: usize = (CLEAN_LIMIT + 2048) * 4096;

/// The byte at `at`: each block opens with its own index, so a block read back
/// from anywhere else is seen, and the rest varies with the offset in it.
fn byte(at: usize) -> u8 {
    let (block, within) = (at / 4096, at % 4096);
    match within {
        0..8 => (block as u64).to_le_bytes()[within],
        _ => (block ^ within.wrapping_mul(7)) as u8,
    }
}

fn main() {
    let mut chunk = vec![0u8; CHUNK];
    {
        let mut f = File::create(PATH).unwrap_or_else(|e| panic!("create {PATH}: {e}"));
        for start in (0..LEN).step_by(CHUNK) {
            for (i, b) in chunk.iter_mut().enumerate() {
                *b = byte(start + i);
            }
            f.write_all(&chunk).unwrap_or_else(|e| panic!("write at {start}: {e}"));
        }
        f.sync_all().expect("the file is durable");
    }
    println!("fs_cache_eviction: wrote {} MiB, past the {} MiB the cache keeps", LEN >> 20, (CLEAN_LIMIT * 4096) >> 20);

    let mut f = File::open(PATH).expect("open it again");
    let mut done = 0;
    while done < LEN {
        f.read_exact(&mut chunk).unwrap_or_else(|e| panic!("read at {done}: {e}"));
        if let Some(i) = (0..CHUNK).find(|&i| chunk[i] != byte(done + i)) {
            panic!("byte {} read back {:#04x}, written {:#04x}", done + i, chunk[i], byte(done + i));
        }
        done += CHUNK;
    }
    drop(f);
    fs::remove_file(PATH).expect("remove the file");
    println!("fs_cache_eviction: PASS");
}
