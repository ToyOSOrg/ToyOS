//! `RingHeader::flags` lives in the page `SYS_PIPE_MAP` maps writable, so a
//! peer can forge `RING_READER_CLOSED`/`RING_WRITER_CLOSED`. The kernel answers
//! "is the other end gone?" from its own reader/writer counts instead — EOF on
//! a read, `Gone` on a write — the facts netd switched to. Here the forged
//! flag and the kernel fact are made to disagree, and the kernel fact is shown
//! true in both directions.

use std::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::ring::{RingHeader, RING_READER_CLOSED, RING_WRITER_CLOSED};
use toyos_abi::syscall::{self, SyscallError};

/// A plain store into the mapped ring header (offset 0), as a peer would forge it.
fn forge(page: *mut u8, bit: u32) {
    let flags = unsafe { &*(page as *const AtomicU32) };
    flags.fetch_or(bit, Ordering::Release);
}

fn reads_reader_closed(page: *mut u8) -> bool {
    let header = unsafe { &*(page as *const RingHeader) };
    header.is_reader_closed()
}

fn main() {
    reader_alive_but_flag_forged();
    reader_gone_is_the_kernels_to_report();
    writer_gone_is_eof_not_a_flag();
    println!("all pipe flag forgery checks passed");
}

/// A live reader and a forged `RING_READER_CLOSED`: the write side answers by
/// the kernel's count, not the bit.
fn reader_alive_but_flag_forged() {
    let ends = syscall::pipe().expect("pipe");
    let (read, write) = (ends.read, ends.write);

    let page = syscall::pipe_map(write).expect("map the write end") as *mut u8;
    forge(page, RING_READER_CLOSED);
    // The premise: the forged flag now lies.
    assert!(
        reads_reader_closed(page),
        "the forged RING_READER_CLOSED bit did not take — this proves nothing"
    );

    // netd's new probe: a zero-byte write is `Ok(0)` while the reader is open.
    let probe = syscall::write_nonblock(write, &[]);
    assert_eq!(
        probe,
        Ok(0),
        "the kernel reported a live reader as gone because a peer forged the flag: {probe:?}"
    );

    let _ = read;
    println!("  PASS: a forged reader-closed flag did not make the kernel report a live reader gone");
}

/// The other direction: a really-gone reader refuses the same write by name.
fn reader_gone_is_the_kernels_to_report() {
    let ends = syscall::pipe().expect("pipe");
    let (read, write) = (ends.read, ends.write);

    syscall::close(read);

    let probe = syscall::write_nonblock(write, &[]);
    assert_eq!(
        probe,
        Err(SyscallError::Gone),
        "a pipe whose reader is really gone did not refuse the write: {probe:?}"
    );
    println!("  PASS: a genuinely closed reader is reported by the kernel as BrokenPipe");
}

/// netd's tx side reads EOF, not a flag: a forged `RING_WRITER_CLOSED` on a
/// live writer must not fake it, and a real drained-and-closed ring must.
fn writer_gone_is_eof_not_a_flag() {
    let live = syscall::pipe().expect("pipe");
    let page = syscall::pipe_map(live.read).expect("map the read end") as *mut u8;
    forge(page, RING_WRITER_CLOSED);
    let mut buf = [0u8; 8];
    let forged = syscall::read_nonblock(live.read, &mut buf);
    assert_eq!(
        forged,
        Err(SyscallError::WouldBlock),
        "a forged writer-closed flag faked EOF on a pipe whose writer is alive: {forged:?}"
    );
    syscall::close(live.read);
    syscall::close(live.write);

    // Real EOF: writer closed, ring empty.
    let ends = syscall::pipe().expect("pipe2");
    syscall::close(ends.write);
    let real = syscall::read_nonblock(ends.read, &mut buf);
    assert_eq!(
        real,
        Ok(0),
        "a pipe whose writer is really gone did not report EOF: {real:?}"
    );
    println!("  PASS: EOF on read is the kernel's, and a forged writer-closed flag cannot fake it");
}
