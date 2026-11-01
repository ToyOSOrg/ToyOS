//! A region made with only its first page committed is memory there and
//! nowhere else until `shm_commit` makes more of it: a touch of a page past
//! it ends the process, a syscall's output into one is `BadAddress`, a spawn
//! from a region nothing committed is refused and the machine goes on, a
//! commit makes the page zeroed memory every mapping sees, a second commit of
//! it changes nothing, and the job's account rises by each page a commit
//! makes and falls back when the region goes.
//!
//! It needs the guest: a fault that never allocates, and a commit that a
//! mapping made before it sees, are the kernel's page tables and frames.

use std::process::{Command, Stdio};

use toyos_abi::syscall::{self, SpawnArgs, SyscallError, VmProt, PAGE_SIZE};

const ME: &str = "/system/bin/test_rs_shm_commit";
const PAGES: usize = 4;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("touch") {
        return touch();
    }
    let own = toyos::job::own().expect("a handle to this job");
    let account = || own.account().expect("this job's account").committed;
    let before = account();
    let shm = syscall::shm_reserve(PAGES * PAGE_SIZE, PAGE_SIZE).expect("a region of four pages, one committed");
    assert_eq!(account() - before, PAGE_SIZE as u64, "a region made with one page committed counts one");
    // SAFETY: a fresh region, mapped whole, which nothing else names.
    let at = unsafe { syscall::shm_map(shm, VmProt::ReadWrite) }.expect("map the region");
    // SAFETY: the first page is committed.
    unsafe {
        assert_eq!(at.read_volatile(), 0, "a committed page is zeroed");
        at.write_volatile(0x5A);
    }
    println!("  PASS: a region made with one page committed is memory there, and counted");

    dies();
    output_is_refused(at);
    a_spawn_from_nothing_committed_is_refused();

    let held = account();
    syscall::shm_commit(shm, PAGE_SIZE + 100, 2 * PAGE_SIZE).expect("commit what pages 1 to 3 are touched by");
    assert_eq!(account() - held, (3 * PAGE_SIZE) as u64, "every page the range touched is counted");
    // SAFETY: pages 1 to 3 are committed now, and the mapping was made before.
    unsafe {
        for page in 1..PAGES {
            let byte = at.add(page * PAGE_SIZE + 7);
            assert_eq!(byte.read_volatile(), 0, "page {page} is zeroed memory");
            byte.write_volatile(page as u8);
        }
    }
    let held = account();
    syscall::shm_commit(shm, 0, PAGES * PAGE_SIZE).expect("a commit of what is committed");
    assert_eq!(account(), held, "a second commit counts nothing more");
    // SAFETY: as above.
    unsafe {
        assert_eq!(at.read_volatile(), 0x5A, "a second commit kept page 0");
        assert_eq!(at.add(3 * PAGE_SIZE + 7).read_volatile(), 3, "a second commit kept page 3");
    }
    println!("  PASS: a commit makes zeroed memory a mapping made before sees, counted once, and a second keeps it");

    assert_eq!(syscall::shm_commit(shm, 0, 0), Err(SyscallError::InvalidArgument), "no bytes");
    assert_eq!(syscall::shm_commit(shm, PAGES * PAGE_SIZE, 1), Err(SyscallError::InvalidArgument), "past the region");
    assert_eq!(syscall::shm_reserve(PAGE_SIZE, 2 * PAGE_SIZE), Err(SyscallError::InvalidArgument), "more committed than made");
    println!("  PASS: a commit of no bytes or past the region, and a reservation committing more than it makes, are refused");

    let held = account();
    // SAFETY: the mapping is this test's, and nothing reads it after.
    unsafe { syscall::vm_release(at, PAGES * PAGE_SIZE) }.expect("unmap the region");
    // SAFETY: this test's own handle, used no more.
    unsafe { syscall::close(shm) };
    assert_eq!(held - account(), (PAGES * PAGE_SIZE) as u64, "the region's pages went back with it");
    println!("  PASS: the region's pages leave the account with it");
    println!("shm_commit: committed pages are memory, and only they");
}

/// A syscall's output into a page not committed is refused, not dropped:
/// a pipe's bytes read into page 1, and nothing taken off the pipe.
fn output_is_refused(at: *mut u8) {
    let ends = syscall::pipe().expect("a pipe");
    assert_eq!(syscall::write(ends.write, b"held"), Ok(4));
    // SAFETY: page 1 of the mapping, never touched: the kernel judges it.
    let page = unsafe { core::slice::from_raw_parts_mut(at.add(PAGE_SIZE), 4) };
    assert_eq!(syscall::read(ends.read, page), Err(SyscallError::BadAddress), "a read into a page not committed");
    let mut out = [0u8; 4];
    assert_eq!(syscall::read(ends.read, &mut out), Ok(4), "the refused read took nothing");
    assert_eq!(&out, b"held");
    // SAFETY: this test's own pipe, used no more.
    unsafe {
        syscall::close(ends.read);
        syscall::close(ends.write);
    }
    println!("  PASS: a syscall's output into a page not committed is BadAddress, and nothing is lost");
}

/// A spawn whose image is a region nothing committed: the loader reads its
/// pages as zeros, refuses the image, and the machine goes on.
fn a_spawn_from_nothing_committed_is_refused() {
    let image = syscall::shm_reserve(2 * PAGE_SIZE, 0).expect("a region of two pages, none committed");
    let argv = format!("{ME}\0");
    let cwd = "/";
    // SAFETY: every pointer names a live local for the whole call.
    let spawned = unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: ME.as_ptr() as u64,
            path_len: ME.len() as u64,
            argv_ptr: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: 0,
            endow_count: 0,
            labels_ptr: 0,
            labels_len: 0,
            cwd_ptr: cwd.as_ptr() as u64,
            cwd_len: cwd.len() as u64,
            image: image.0 as u64,
            image_len: (2 * PAGE_SIZE) as u64,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    };
    assert!(spawned.is_err(), "an image of nothing committed was spawned");
    // SAFETY: this test's own handle, used no more.
    unsafe { syscall::close(image) };
    println!("  PASS: a spawn from a region nothing committed is refused: {spawned:?}");
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
    let at = unsafe { syscall::shm_map(shm, VmProt::ReadWrite) }.expect("map the region");
    println!("armed");
    // SAFETY: as above.
    let byte = unsafe { at.add(PAGE_SIZE).read_volatile() };
    println!("SURVIVED with {byte}");
}
