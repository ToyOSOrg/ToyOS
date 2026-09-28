//! A syscall's byte buffer that spans two demand-paged windows is copied through
//! both frames, in whichever order the pager handed them out.
//!
//! `.bss` is demand-paged one 2 MiB window at a time, and `SPAN` holds two whole
//! windows nothing else touches. Faulting the upper one first puts it in the
//! frame the allocator hands out first, so the two frames are never one
//! physically contiguous run — the buffer a kernel that copies through one run
//! refuses with `BadAddress`.

use std::fs::{self, File};
use std::io::{Read, Write};

use toyos_abi::syscall;

const PAGE_2M: usize = 2 * 1024 * 1024;
/// Three windows: whatever the alignment, two whole ones lie inside.
const SPAN_LEN: usize = 3 * PAGE_2M;
/// Bytes of the buffer on each side of the boundary.
const REACH: usize = 32 * 1024;
const CANARY: u8 = 0xA5;
const PATH: &str = "/tmp/user_copy_spans_windows.bin";

static mut SPAN: [u8; SPAN_LEN] = [0; SPAN_LEN];

fn pattern(len: usize, salt: u8) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(131) ^ (i >> 9)) as u8 ^ salt).collect()
}

/// The first byte where `got` and `want` differ, and on which side of the boundary.
fn first_difference(got: &[u8], want: &[u8]) -> Option<String> {
    let i = got.iter().zip(want).position(|(g, w)| g != w)?;
    let side = if i < REACH { "below" } else { "above" };
    Some(format!("byte {i} ({side} the boundary) is {:#04x}, not {:#04x}", got[i], want[i]))
}

fn main() {
    let base = (&raw mut SPAN).cast::<u8>();
    let lower = (base as usize).next_multiple_of(PAGE_2M);
    let boundary = lower + PAGE_2M;
    assert!(boundary + PAGE_2M <= base as usize + SPAN_LEN, "SPAN holds two whole windows");
    let at = |addr: usize| unsafe { base.add(addr - base as usize) };

    // Upper window first, then lower.
    unsafe {
        at(boundary).write_volatile(CANARY);
        at(boundary - 1).write_volatile(CANARY);
        at(boundary - REACH - 1).write_volatile(CANARY);
        at(boundary + REACH).write_volatile(CANARY);
    }
    // SAFETY: `SPAN` is this process's and nothing else names these bytes while the slice lives.
    let buf = unsafe { core::slice::from_raw_parts_mut(at(boundary - REACH), 2 * REACH) };
    let outside_intact = || unsafe {
        at(boundary - REACH - 1).read_volatile() == CANARY && at(boundary + REACH).read_volatile() == CANARY
    };

    // 1. A pipe read: the kernel writes a ring run into the buffer.
    let ends = syscall::pipe().expect("pipe");
    let sent = pattern(buf.len(), 0x5A);
    assert_eq!(syscall::write(ends.write, &sent), Ok(sent.len()), "fill the pipe");
    let got = syscall::read(ends.read, buf);
    assert!(outside_intact(), "a pipe read into the buffer wrote past its ends");
    assert_eq!(got, Ok(buf.len()), "a pipe read into a buffer across two windows");
    if let Some(diff) = first_difference(buf, &sent) {
        panic!("a pipe read across two windows: {diff}");
    }

    // 2. A pipe write: the kernel reads the buffer into a ring run.
    let mine = pattern(buf.len(), 0xC3);
    buf.copy_from_slice(&mine);
    assert_eq!(syscall::write(ends.write, buf), Ok(buf.len()), "a pipe write from a buffer across two windows");
    let mut back = vec![0u8; buf.len()];
    assert_eq!(syscall::read(ends.read, &mut back), Ok(back.len()), "drain the pipe");
    if let Some(diff) = first_difference(&back, &mine) {
        panic!("a pipe write from across two windows: {diff}");
    }
    syscall::close(ends.read);
    syscall::close(ends.write);

    // 3. A file write and read back: the file cache's copies, a page at a time.
    let mine = pattern(buf.len(), 0x3C);
    buf.copy_from_slice(&mine);
    File::create(PATH).and_then(|mut f| f.write_all(buf)).expect("a file write from a buffer across two windows");
    let stored = fs::read(PATH).expect("read the file back");
    if let Some(diff) = first_difference(&stored, &mine) {
        panic!("a file write from across two windows: {diff}");
    }
    buf.fill(0);
    File::open(PATH).and_then(|mut f| f.read_exact(buf)).expect("a file read into a buffer across two windows");
    assert!(outside_intact(), "a file read into the buffer wrote past its ends");
    if let Some(diff) = first_difference(buf, &mine) {
        panic!("a file read across two windows: {diff}");
    }
    fs::remove_file(PATH).expect("cleanup");

    println!("a buffer across two demand windows is copied through both frames");
}
