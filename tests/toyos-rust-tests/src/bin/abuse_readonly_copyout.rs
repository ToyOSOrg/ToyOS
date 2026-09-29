//! A syscall that writes into user memory writes only where the process could
//! store itself. The kernel copies through its direct map, which the page's
//! user permissions do not govern, so the check is the copy's own: without it,
//! `read(fd, CLOCK_PAGE, n)` rewrites the one clock frame every address space
//! shares, and a `read` into a shared library's `.text` rewrites the code of
//! every process that maps it.
//!
//! **The memory is the verdict, before the return value**: each arm snapshots
//! the target, makes the call, and compares, so a kernel that wrote and then
//! answered an error still fails here.

use std::process::Command;

use toyos_abi::clock::{ClockPage, CLOCK_MAGIC, CLOCK_PAGE};
use toyos_abi::syscall::{
    self, MmapFlags, MmapProt, OpenFlags, SeekFrom, SyscallError, SYS_FSTAT, SYS_READ, SYS_WRITE,
};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_abuse_readonly_copyout";
const CHILD_ARG: &str = "reads-the-clock";
const PAGE_2M: usize = 2 * 1024 * 1024;
const PAGE_4K: u64 = 4096;
/// Longer than `Stat`, and ends inside the file this reads.
const LEN: usize = 64;
/// Bytes `0..=255`, so a probe can offer any page its own byte back, then the
/// bytes the straddling `read` offers, at [`STRADDLE_AT`].
const PROBE_PATH: &[u8] = b"/tmp/abuse_readonly_copyout.bin";
const STRADDLE_AT: u64 = 256;
/// The straddling `read`: half on the last writable page, half on the next.
const STRADDLE: usize = 16;

/// In `.bss`, so its 2 MiB window also holds the pages past the image's end,
/// which the pager maps read-only.
static mut BSS: u8 = 0;

/// The typed wrappers take a `&mut [u8]`, which a read-only page cannot be.
fn raw(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") num,
            in("rsi") a1,
            in("rdx") a2,
            in("r8") a3,
            in("r9") 0u64,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

fn snapshot(addr: u64) -> [u8; LEN] {
    let mut out = [0u8; LEN];
    for (i, b) in out.iter_mut().enumerate() {
        *b = unsafe { (addr as *const u8).add(i).read_volatile() };
    }
    out
}

/// A fresh handle on this binary at offset 0, so every `read` has bytes to give.
fn open_self() -> RawHandle {
    syscall::open(SELF_PATH.as_bytes(), OpenFlags::READ).expect("open self")
}

/// `read` and `fstat` into `addr`: both refused, and not one byte moved.
fn refused(what: &str, addr: u64) {
    let before = snapshot(addr);
    let fd = open_self();
    let ret = raw(SYS_READ, fd.0 as u64, addr, LEN as u64);
    assert_eq!(before, snapshot(addr), "read wrote into {what}");
    assert_eq!(SyscallError::from_u64(ret), Some(SyscallError::BadAddress), "read into {what}: {ret:#x}");
    let ret = raw(SYS_FSTAT, fd.0 as u64, addr, 0);
    assert_eq!(before, snapshot(addr), "fstat wrote into {what}");
    assert_eq!(SyscallError::from_u64(ret), Some(SyscallError::BadAddress), "fstat into {what}: {ret:#x}");
    syscall::close(fd);
}

/// The first page at or above [`BSS`], inside its 2 MiB window, a `read` may
/// not write; each page below it took a 1-byte `read` of its own first byte.
fn first_unwritable_page(probe: RawHandle) -> u64 {
    let pipe = syscall::pipe().expect("pipe");
    let bss = &raw const BSS as u64;
    let window_end = (bss & !(PAGE_2M as u64 - 1)) + PAGE_2M as u64;
    let mut page = bss & !(PAGE_4K - 1);
    while page < window_end {
        let ret = raw(SYS_WRITE, pipe.write.0 as u64, page, 1);
        assert_eq!(ret, 1, "write from {page:#x}, in .bss's window: {ret:#x}");
        syscall::read(pipe.read, &mut [0u8; 1]).expect("drain the pipe");
        let own = unsafe { (page as *const u8).read_volatile() };
        syscall::seek(probe, SeekFrom::Start(own as u64)).expect("seek the probe");
        let ret = raw(SYS_READ, probe.0 as u64, page, 1);
        if SyscallError::from_u64(ret) == Some(SyscallError::BadAddress) {
            syscall::close(pipe.read);
            syscall::close(pipe.write);
            return page;
        }
        assert_eq!(ret, 1, "a 1-byte read into {page:#x}: {ret:#x}");
        page += PAGE_4K;
    }
    panic!("no page above .bss at {bss:#x} refuses a write below {window_end:#x}: the image ends on its window's edge");
}

/// A `read` that starts on a writable page and runs into the read-only page
/// after it, in one 2 MiB window, is refused whole.
fn refused_across_pages() {
    let flags = OpenFlags::READ | OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
    let probe = syscall::open(PROBE_PATH, flags).expect("create the probe file");
    let every: Vec<u8> = (0..=255).collect();
    syscall::write(probe, &every).expect("fill the probe file");
    let page = first_unwritable_page(probe);
    assert!(page > &raw const BSS as u64, "BSS's own page refuses a write");
    let at = page - (STRADDLE / 2) as u64;
    let before = snapshot(at);
    // The writable half is offered its own bytes and the read-only half their
    // complement, so a write to the read-only page shows and one below harms nothing.
    let offered: [u8; STRADDLE] = core::array::from_fn(|i| if i < STRADDLE / 2 { before[i] } else { !before[i] });
    syscall::seek(probe, SeekFrom::Start(STRADDLE_AT)).expect("seek the probe");
    syscall::write(probe, &offered).expect("write the straddle's bytes");
    syscall::seek(probe, SeekFrom::Start(STRADDLE_AT)).expect("seek the probe");
    let ret = raw(SYS_READ, probe.0 as u64, at, STRADDLE as u64);
    assert_eq!(before, snapshot(at), "read wrote across a writable page into the read-only one at {page:#x}");
    assert_eq!(SyscallError::from_u64(ret), Some(SyscallError::BadAddress), "read across into {page:#x}: {ret:#x}");
    syscall::close(probe);
    syscall::delete(PROBE_PATH).expect("delete the probe file");
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD_ARG) {
        let page = unsafe { core::ptr::read_volatile(CLOCK_PAGE as *const ClockPage) };
        assert_eq!(page.magic, CLOCK_MAGIC, "a second process found the clock page rewritten");
        std::process::exit(0);
    }

    // The control: the same calls into memory this process may write succeed,
    // so every refusal below is about the page and nothing else.
    let mut ok = [0u8; LEN];
    let fd = open_self();
    let ret = raw(SYS_READ, fd.0 as u64, ok.as_mut_ptr() as u64, LEN as u64);
    assert_eq!(ret, LEN as u64, "read into a writable buffer: {ret:#x}");
    assert_eq!(&ok[..4], b"\x7fELF", "read put something other than this file in the buffer");
    syscall::close(fd);

    let ro = unsafe {
        syscall::mmap(
            core::ptr::null_mut(),
            PAGE_2M,
            MmapProt::READ,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!ro.is_null(), "mmap a read-only page");
    refused("a read-only mmap", ro as u64);
    unsafe { syscall::munmap(ro, PAGE_2M) }.expect("munmap");

    refused("this program's own text", main as *const () as u64);
    refused_across_pages();

    // Last: a written clock page asserts in every stamp this process and every
    // other one takes, the verdict's own path included, so the arms whose harm
    // is local answer first.
    let clock = snapshot(CLOCK_PAGE);
    refused("the clock page", CLOCK_PAGE);
    assert_eq!(clock, snapshot(CLOCK_PAGE));

    let status = Command::new(SELF_PATH).arg(CHILD_ARG).status().expect("spawn the second reader");
    assert!(status.success(), "the second process could not read the clock: {status:?}");

    println!("a syscall writes only where its caller could store");
}
