//! A seek past EOF is a position, not an error, and nothing silently moves it.
//!
//! The expected outcomes are POSIX's (`lseek(2)`, `ftruncate(2)`, `write(2)`),
//! not this kernel's, so every assertion below is the standard judging the
//! kernel: the offset asked is the offset answered, the gap reads as zeros, a
//! truncate leaves the pointer, and past the page index's reach is a refusal.

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

const PAGE: u64 = 4096;
/// `file_cache::MAX_FILE_SIZE`: the page index is a `u32`, so `(u32::MAX + 1) * 4096`.
const MAX_FILE_SIZE: u64 = (u32::MAX as u64 + 1) * PAGE;

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 37 + 11) as u8 | 1).collect()
}

fn seek_past_eof_lands_where_asked(dir: &str) {
    let path = format!("{dir}/lseek_hole.bin");
    let head = pattern(100);
    let mut f = OpenOptions::new()
        .read(true).write(true).create(true).truncate(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("create {path}: {e}"));
    f.write_all(&head).expect("write the head");

    let got = f.seek(SeekFrom::Start(200)).expect("seek past EOF");
    assert_eq!(got, 200, "{dir}: lseek(200) past a 100-byte EOF answered {got}");
    let len = f.metadata().expect("stat").len();
    assert_eq!(len, 100, "{dir}: the seek alone changed the size to {len}");

    f.write_all(&[0xAB]).expect("write one byte at the seeked offset");
    let len = f.metadata().expect("stat").len();
    assert_eq!(len, 201, "{dir}: the write landed at EOF, not at the offset asked (size {len})");

    f.seek(SeekFrom::Start(0)).expect("rewind");
    let mut back = vec![0u8; 201];
    f.read_exact(&mut back).expect("read the whole file back");
    assert_eq!(&back[..100], &head[..], "{dir}: the head changed");
    for (i, &b) in back[100..200].iter().enumerate() {
        assert_eq!(b, 0, "{dir}: hole byte {} is {b:#04x}, not zero", i + 100);
    }
    assert_eq!(back[200], 0xAB, "{dir}: the poked byte is not at the offset asked");

    let got = f.seek(SeekFrom::End(99)).expect("seek End(+99) past EOF");
    assert_eq!(got, 300, "{dir}: SEEK_END past EOF answered {got}, not 300");
    let got = f.seek(SeekFrom::Current(50)).expect("seek Current(+50) past EOF");
    assert_eq!(got, 350, "{dir}: SEEK_CUR past EOF answered {got}, not 350");
    let n = f.read(&mut [0u8; 8]).expect("read at a past-EOF position");
    assert_eq!(n, 0, "{dir}: a read past EOF answered {n} bytes, not 0");

    drop(f);
    fs::remove_file(&path).expect("cleanup");
}

fn truncate_leaves_the_pointer(dir: &str) {
    let path = format!("{dir}/lseek_trunc.bin");
    let mut f = OpenOptions::new()
        .read(true).write(true).create(true).truncate(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("create {path}: {e}"));
    f.write_all(&pattern(100)).expect("write the head");

    f.set_len(10).expect("shrink under the pointer");
    let here = f.seek(SeekFrom::Current(0)).expect("tell");
    assert_eq!(here, 100, "{dir}: ftruncate moved the seek pointer to {here}");

    f.write_all(&[0xCD]).expect("write at the old offset");
    let len = f.metadata().expect("stat").len();
    assert_eq!(len, 101, "{dir}: the write after a shrink made a {len}-byte file, not 101");
    f.seek(SeekFrom::Start(0)).expect("rewind");
    let mut back = vec![0u8; 101];
    f.read_exact(&mut back).expect("read back");
    for (i, &b) in back[10..100].iter().enumerate() {
        assert_eq!(b, 0, "{dir}: reopened byte {} is {b:#04x}, not zero", i + 10);
    }
    assert_eq!(back[100], 0xCD, "{dir}: the byte written past the shrink did not land");

    drop(f);
    fs::remove_file(&path).expect("cleanup");
}

/// Past the `u32` page index's reach is a refusal that moves nothing — the
/// alternative was a wrapped index aliasing a low page.
fn the_ceiling_refuses(dir: &str) {
    let path = format!("{dir}/lseek_ceiling.bin");
    let mut f = OpenOptions::new()
        .read(true).write(true).create(true).truncate(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("create {path}: {e}"));
    f.write_all(&[0x11]).expect("write one byte");

    f.seek(SeekFrom::Start(MAX_FILE_SIZE + 1))
        .expect_err("a seek past the ceiling was accepted");
    let here = f.seek(SeekFrom::Current(0)).expect("tell");
    assert_eq!(here, 1, "{dir}: the refused seek moved the pointer to {here}");

    f.set_len(MAX_FILE_SIZE + 1)
        .expect_err("a truncate past the ceiling was accepted");

    let got = f.seek(SeekFrom::Start(MAX_FILE_SIZE)).expect("seek exactly to the ceiling");
    assert_eq!(got, MAX_FILE_SIZE, "{dir}: the ceiling itself is a position");
    f.write_all(&[0x22])
        .expect_err("a write whose end crosses the ceiling was accepted");

    drop(f);
    fs::remove_file(&path).expect("cleanup");
}

fn main() {
    // /tmp is the in-memory control; /home carries the hole through a device-backed mount.
    for dir in ["/tmp", "/home"] {
        seek_past_eof_lands_where_asked(dir);
        truncate_leaves_the_pointer(dir);
    }
    the_ceiling_refuses("/tmp");

    println!("lseek past eof: the offset asked is the offset answered, the gap reads as zeros, and the ceiling refuses");
}
