//! The ring's closed flags are userland's to write, and the kernel does not
//! believe them — netd was the last reader that did.
//!
//! `RingHeader::flags` lives in the page `SYS_PIPE_MAP` maps writable, so a
//! process holding either end can set `RING_READER_CLOSED`/`RING_WRITER_CLOSED`
//! by hand. netd read those bits as facts about its peer and tore connections
//! down on them, until the kernel stopped believing forgeable flags. The
//! kernel answers "is the other end gone?" from its own reader/writer counts,
//! surfaced as EOF on a read and `NotFound` (BrokenPipe) on a write — the two
//! facts netd switched to.
//!
//! This is the differential: for one pipe, at one instant, the forged flag and
//! the kernel fact are made to disagree, and the kernel fact is shown to be the
//! true one — in both directions. No netd, no NIC: the mechanism netd now
//! trusts, exercised where it lives.

use std::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::ring::{RingHeader, RING_READER_CLOSED, RING_WRITER_CLOSED};
use toyos_abi::syscall::{self, SyscallError};

/// Forge a flag bit into a mapped ring page, the way `RingHeader::close_reader`
/// would — a plain store into memory userland owns.
fn forge(page: *mut u8, bit: u32) {
    // The header is at offset 0; `flags` is its only field.
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

/// The forged case that was netd's bug: a reader that is alive, and a
/// `RING_READER_CLOSED` bit that says otherwise. The write side must answer by
/// the kernel's count, not the bit.
fn reader_alive_but_flag_forged() {
    let ends = syscall::pipe().expect("pipe");
    let (read, write) = (ends.read, ends.write);

    // Map the write end and forge "the reader is gone" — with the reader (this
    // process's own `read` handle) very much still open.
    let page = syscall::pipe_map(write).expect("map the write end") as *mut u8;
    forge(page, RING_READER_CLOSED);

    // The premise: the flag is forgeable and now lies.
    assert!(
        reads_reader_closed(page),
        "the forged RING_READER_CLOSED bit did not take — this proves nothing"
    );

    // The kernel fact: a zero-byte write moves nothing and reports the reader
    // as present. `Ok(0)`, never `NotFound`. This is exactly netd's new probe.
    let probe = syscall::write_nonblock(write, &[]);
    assert_eq!(
        probe,
        Ok(0),
        "the kernel reported a live reader as gone because a peer forged the flag: {probe:?}"
    );

    let _ = read;
    println!("  PASS: a forged reader-closed flag did not make the kernel report a live reader gone");
}

/// The other direction, so the probe is not merely blind: once the reader is
/// really gone, the same zero-byte write is refused by name.
fn reader_gone_is_the_kernels_to_report() {
    let ends = syscall::pipe().expect("pipe");
    let (read, write) = (ends.read, ends.write);

    // No forgery this time. Close the real reader.
    syscall::close(read);

    let probe = syscall::write_nonblock(write, &[]);
    assert_eq!(
        probe,
        Err(SyscallError::NotFound),
        "a pipe whose reader is really gone did not refuse the write: {probe:?}"
    );
    println!("  PASS: a genuinely closed reader is reported by the kernel as BrokenPipe");
}

/// netd's tx side reads EOF, not a flag: a drained ring with no writer left
/// answers a non-blocking read with `Ok(0)`, and a forged `RING_WRITER_CLOSED`
/// on a live writer does not fake it.
fn writer_gone_is_eof_not_a_flag() {
    // Forged-writer-closed on a live writer: the read must still block-would.
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
    let ends = syscall::pipe().expect("pipe");
    syscall::close(ends.write);
    let real = syscall::read_nonblock(ends.read, &mut buf);
    assert_eq!(
        real,
        Ok(0),
        "a pipe whose writer is really gone did not report EOF: {real:?}"
    );
    println!("  PASS: EOF on read is the kernel's, and a forged writer-closed flag cannot fake it");
}
