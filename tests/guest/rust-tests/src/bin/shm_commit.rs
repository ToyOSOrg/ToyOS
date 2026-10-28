//! A region made with only its first page committed is memory there and
//! nowhere else until `shm_commit` makes more of it: a touch of a page past
//! it ends the process, a commit makes the page zeroed memory every mapping
//! sees, a second commit of it changes nothing, and `fstat` says how much is
//! committed.
//!
//! It needs the guest: a fault that never allocates, and a commit that a
//! mapping made before it sees, are the kernel's page tables and frames.

use std::process::{Command, Stdio};

use toyos_abi::syscall::{self, SyscallError, PAGE_SIZE};

const ME: &str = "/system/bin/test_rs_shm_commit";
const PAGES: usize = 4;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("touch") {
        return touch();
    }
    let shm = syscall::shm_reserve(PAGES * PAGE_SIZE, PAGE_SIZE).expect("a region of four pages, one committed");
    let committed = |what: &str| {
        let stat = syscall::fstat(shm).expect("fstat of the region");
        assert_eq!(stat.size, (PAGES * PAGE_SIZE) as u64, "{what}: the size is the whole region");
        stat.committed
    };
    assert_eq!(committed("made"), PAGE_SIZE as u64);
    // SAFETY: a fresh region, mapped whole, which nothing else names.
    let at = unsafe { syscall::shm_map(shm) }.expect("map the region");
    // SAFETY: the first page is committed.
    unsafe {
        assert_eq!(at.read_volatile(), 0, "a committed page is zeroed");
        at.write_volatile(0x5A);
    }
    println!("  PASS: a region made with one page committed is memory there");

    dies();

    syscall::shm_commit(shm, PAGE_SIZE + 100, 2 * PAGE_SIZE).expect("commit what pages 1 to 3 are touched by");
    assert_eq!(committed("committed"), 4 * PAGE_SIZE as u64, "every page the range touched");
    // SAFETY: pages 1 to 3 are committed now, and the mapping was made before.
    unsafe {
        for page in 1..PAGES {
            let byte = at.add(page * PAGE_SIZE + 7);
            assert_eq!(byte.read_volatile(), 0, "page {page} is zeroed memory");
            byte.write_volatile(page as u8);
        }
    }
    syscall::shm_commit(shm, 0, PAGES * PAGE_SIZE).expect("a commit of what is committed");
    // SAFETY: as above.
    unsafe {
        assert_eq!(at.read_volatile(), 0x5A, "a second commit kept page 0");
        assert_eq!(at.add(3 * PAGE_SIZE + 7).read_volatile(), 3, "a second commit kept page 3");
    }
    println!("  PASS: a commit makes zeroed memory a mapping made before sees, and a second keeps it");

    assert_eq!(syscall::shm_commit(shm, 0, 0), Err(SyscallError::InvalidArgument), "no bytes");
    assert_eq!(syscall::shm_commit(shm, PAGES * PAGE_SIZE, 1), Err(SyscallError::InvalidArgument), "past the region");
    assert_eq!(syscall::shm_reserve(PAGE_SIZE, 2 * PAGE_SIZE), Err(SyscallError::InvalidArgument), "more committed than made");
    println!("  PASS: a commit of no bytes or past the region, and a reservation committing more than it makes, are refused");
    println!("shm_commit: committed pages are memory, and only they");
}

/// A child touches page 1 of a region committed only on page 0, and dies of it.
fn dies() {
    let child = Command::new(ME).arg("touch").stdout(Stdio::piped()).spawn().expect("spawn the toucher");
    let out = child.wait_with_output().expect("wait for the toucher");
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("armed"), "the child never reached the touch, so it proved nothing:\n{said}");
    assert!(!said.contains("SURVIVED"), "a touch past what was committed was served:\n{said}");
    assert!(!out.status.success(), "a touch past what was committed did not end the process ({:?})", out.status.code());
    println!("  PASS: a touch of a page not committed ends the process");
}

fn touch() {
    let shm = syscall::shm_reserve(2 * PAGE_SIZE, PAGE_SIZE).expect("a region of two pages, one committed");
    // SAFETY: a fresh region, mapped whole; page 1 is past what is committed, which is the test.
    let at = unsafe { syscall::shm_map(shm) }.expect("map the region");
    println!("armed");
    // SAFETY: as above.
    let byte = unsafe { at.add(PAGE_SIZE).read_volatile() };
    println!("SURVIVED with {byte}");
}
