//! A syscall that writes into user memory writes only where the process could
//! store itself. The kernel copies through its direct map, which the page's
//! user permissions do not govern, so the check is the copy's own: without it,
//! `read(fd, CLOCK_PAGE, n)` rewrites the one clock frame every address space
//! shares, and a `read` into a shared library's `.text` rewrites the code of
//! every process that maps it.
//!
//! **The memory is the verdict, before the return value**: each arm snapshots
//! the target, makes the call, and compares, so a kernel that wrote and then
//! answered an error still fails here. Each arm asks through both copies a
//! syscall writes with: `read`'s bulk window and `process_stats`'s typed value.
//!
//! **Every arm runs, and the exit status carries one bit per arm that let a
//! write through**, which the kernel's own exit record prints: a rewritten
//! clock page asserts in every stamp any process takes, `logkeeper`'s among them, so
//! the clock arm's verdict may reach no other line.

#[path = "../image.rs"]
mod image;

use std::os::toyos::io::AsRawHandle;
use std::process::Command;

use toyos_abi::clock::{ClockPage, CLOCK_PAGE};
use toyos_abi::syscall::{self, OpenFlags, VmAt, VmProt, ProcessStats, SeekFrom, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_abuse_readonly_copyout";
const CHILD_ARG: &str = "reads-the-clock";
const PAGE_4K: u64 = 4096;
/// Covers a `ProcessStats`, and ends inside the file this reads.
const LEN: usize = core::mem::size_of::<ProcessStats>();
/// The bytes the straddling `read` offers, at [`STRADDLE_AT`].
const PROBE_PATH: &[u8] = b"/tmp/abuse_readonly_copyout.bin";
const STRADDLE_AT: u64 = 256;
/// The straddling `read`: half on the last writable page, half on the next.
const STRADDLE: usize = 16;

/// Each arm's bit in the exit status.
const MMAP_BIT: i32 = 1;
const TEXT_BIT: i32 = 2;
const STRADDLE_BIT: i32 = 4;
const CLOCK_BIT: i32 = 8;

fn snapshot(addr: u64) -> [u8; LEN] {
    let mut out = [0u8; LEN];
    for (i, b) in out.iter_mut().enumerate() {
        *b = unsafe { (addr as *const u8).add(i).read_volatile() };
    }
    out
}

/// A fresh handle on this binary at offset 0, so every `read` has bytes to give:
/// its copy in the kernel's `/tmp`, made once.
fn open_self() -> RawHandle {
    static OWN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let own = OWN.get_or_init(|| image::in_tmp(SELF_PATH));
    syscall::open(own.as_bytes(), OpenFlags::READ).expect("open self")
}

/// `len` bytes at `addr`, as the buffer a `read` is asked to fill.
///
/// # Safety
/// Nothing reads or writes through the slice while it lives: it only names the
/// address the kernel is asked to write, which may be a page nothing may write.
unsafe fn target(addr: u64, len: usize) -> &'static mut [u8] {
    unsafe { core::slice::from_raw_parts_mut(addr as *mut u8, len) }
}

/// `read` and `process_stats` (of `stats`) into `addr`, which is 8-aligned:
/// both refused, and not one byte moved.
fn refused(what: &str, addr: u64, stats: RawHandle) -> Result<(), String> {
    let before = snapshot(addr);
    let fd = open_self();
    let ret = syscall::read(fd, unsafe { target(addr, LEN) });
    // SAFETY: no type holds this number.
    unsafe { syscall::close(fd) };
    if before != snapshot(addr) {
        return Err(format!("read wrote into {what}"));
    }
    if ret != Err(SyscallError::BadAddress) {
        return Err(format!("read into {what}: {ret:?}"));
    }
    // SAFETY: as `target`'s; `ProcessStats` is 8-aligned and every bit pattern is one.
    let ret = syscall::process_stats(stats, unsafe { &mut *(addr as *mut ProcessStats) });
    if before != snapshot(addr) {
        return Err(format!("process_stats wrote into {what}"));
    }
    if ret != Err(SyscallError::BadAddress) {
        return Err(format!("process_stats into {what}: {ret:?}"));
    }
    Ok(())
}

fn refused_mmap(stats: RawHandle) -> Result<(), String> {
    // SAFETY: placed anywhere, so it replaces nothing.
    let ro = unsafe { syscall::vm_reserve(PAGE_4K as usize, VmProt::Read, VmAt::ANYWHERE) }.expect("reserve a read-only page");
    let verdict = refused("a read-only mmap", ro as u64, stats);
    unsafe { syscall::vm_release(ro, PAGE_4K as usize) }.expect("release");
    verdict
}

/// A `read` that starts on a writable page and runs into the read-only page
/// after it is refused whole.
fn refused_across_pages() -> Result<(), String> {
    let flags = OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
    let probe = syscall::open(PROBE_PATH, flags).expect("create the probe file");
    // SAFETY: placed anywhere, so it replaces nothing.
    let pages = unsafe { syscall::vm_reserve(2 * PAGE_4K as usize, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("reserve two pages");
    let page = pages as u64 + PAGE_4K;
    // SAFETY: nothing holds a reference into the page.
    unsafe { syscall::vm_protect(page as *mut u8, PAGE_4K as usize, VmProt::Read) }.expect("make the second page read-only");
    let at = page - (STRADDLE / 2) as u64;
    let before = snapshot(at);
    // The writable half is offered its own bytes and the read-only half their
    // complement, so a write to the read-only page shows and one below harms nothing.
    let offered: [u8; STRADDLE] = core::array::from_fn(|i| if i < STRADDLE / 2 { before[i] } else { !before[i] });
    syscall::seek(probe, SeekFrom::Start(STRADDLE_AT)).expect("seek the probe");
    syscall::write(probe, &offered).expect("write the straddle's bytes");
    syscall::seek(probe, SeekFrom::Start(STRADDLE_AT)).expect("seek the probe");
    let ret = syscall::read(probe, unsafe { target(at, STRADDLE) });
    let verdict = if before != snapshot(at) {
        Err(format!("read wrote across a writable page into the read-only one at {page:#x}"))
    } else if ret != Err(SyscallError::BadAddress) {
        Err(format!("read across into {page:#x}: {ret:?}"))
    } else {
        Ok(())
    };
    unsafe { syscall::vm_release(pages, 2 * PAGE_4K as usize) }.expect("release");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(probe) };
    syscall::delete(PROBE_PATH).expect("delete the probe file");
    verdict
}

/// A second process still reads the clock.
fn second_reader() -> bool {
    Command::new(SELF_PATH).arg(CHILD_ARG).status().expect("spawn the second reader").success()
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD_ARG) {
        // SAFETY: the kernel maps the page read-only here in every address
        // space, and every field of it is an atomic.
        let page = unsafe { &*(CLOCK_PAGE as *const ClockPage) };
        assert!(page.read().is_some(), "a second process found the clock page rewritten");
        std::process::exit(0);
    }

    // The control: the same calls into memory this process may write succeed,
    // so every refusal below is about the page and nothing else. The first
    // reader is the process `process_stats` answers for.
    let mut first = Command::new(SELF_PATH).arg(CHILD_ARG).spawn().expect("spawn the first reader");
    assert!(first.wait().expect("wait for the first reader").success(), "the first reader failed");
    let stats = RawHandle(first.as_raw_handle());
    let mut ok = [0u8; LEN];
    let fd = open_self();
    assert_eq!(syscall::read(fd, &mut ok), Ok(LEN), "read into a writable buffer");
    assert_eq!(&ok[..4], b"\x7fELF", "read put something other than this file in the buffer");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(fd) };
    syscall::process_stats(stats, &mut ProcessStats::default()).expect("process_stats into a writable value");

    let mut wrote = 0;
    for (bit, verdict) in [
        (MMAP_BIT, refused_mmap(stats)),
        (TEXT_BIT, refused("this program's own text", main as *const () as u64 & !7, stats)),
        (STRADDLE_BIT, refused_across_pages()),
    ] {
        if let Err(why) = verdict {
            eprintln!("{why}");
            wrote |= bit;
        }
    }

    // Last, and said by its bit alone: its harm reaches every stamping reader.
    // Half a page in, past the clock's own words, which the kernel rewrites
    // when it retunes: the same frame, and bytes nothing moves.
    const CLOCK_SPARE: u64 = CLOCK_PAGE + PAGE_4K / 2;
    const _: () = assert!(core::mem::size_of::<ClockPage>() <= (PAGE_4K / 2) as usize && LEN <= (PAGE_4K / 2) as usize);
    if refused("the clock page", CLOCK_SPARE, stats).is_err() || !second_reader() {
        wrote |= CLOCK_BIT;
    }
    if wrote != 0 {
        std::process::exit(wrote);
    }
    println!("a syscall writes only where its caller could store");
}
