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
use toyos_abi::syscall::{self, MmapFlags, MmapProt, OpenFlags, SyscallError, SYS_FSTAT, SYS_READ};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_abuse_readonly_copyout";
const CHILD_ARG: &str = "reads-the-clock";
const PAGE_2M: usize = 2 * 1024 * 1024;
/// Longer than `Stat`, and ends inside the file this reads.
const LEN: usize = 64;

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
