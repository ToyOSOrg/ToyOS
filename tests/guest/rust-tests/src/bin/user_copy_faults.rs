//! A user copy goes through the process's own mapping (`kernel/src/user_ptr.rs`,
//! `arch::uaccess`), so it meets the pages a program's own loads and stores
//! would: a page never touched faults inside the copy, is served, and the copy
//! resumes where it stopped; a page released, or one the program may not read,
//! ends the copy as `BadAddress`, and the kernel goes on.
//!
//! Each buffer starts in a page the program touched and runs into one it did
//! not, so the first fault comes mid-copy rather than at its first byte.

use toyos_abi::syscall::{self, NamespaceBuild, SyscallError, VmAt, VmProt, PAGE_SIZE};

const PAGE: usize = PAGE_SIZE;
/// Bytes of each buffer on either side of the page boundary it straddles.
const SIDE: usize = 300;

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

/// `pages` fresh pages, none of them touched yet.
fn fresh(pages: usize) -> *mut u8 {
    // SAFETY: a fresh anonymous mapping aliases nothing.
    unsafe { syscall::vm_reserve(pages * PAGE, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("vm_reserve")
}

/// A pipe write whose buffer runs into an untouched page: the kernel's read of
/// it faults there, is served, and resumes. Then a read into a buffer that runs
/// into an untouched page, which faults the same way on the kernel's write.
/// The write's buffer starts `from` bytes into a page with `SIDE` bytes of the
/// program's own, and the read's `into` bytes into one.
fn a_fault_served_mid_copy_resumes(len: usize, seed: u8, from: usize, into: usize) -> usize {
    let (reader, writer) = syscall::pipe().map(|ends| (ends.read, ends.write)).expect("pipe");
    let src = fresh(len.div_ceil(PAGE) + 2);
    let expected = pattern(len, seed);
    // SAFETY: inside the fresh mapping; only `SIDE` bytes, in its first two
    // pages, and none after are touched here.
    let start = unsafe { src.add(from) };
    // SAFETY: `start..start + SIDE` lies in the first two pages, which this writes and so populates.
    unsafe { core::ptr::copy_nonoverlapping(expected.as_ptr(), start, SIDE) };
    // SAFETY: the rest of the buffer, in pages never touched: written here would
    // populate them, so the program's bytes past `SIDE` are written by the kernel's
    // fault service as zeros. The buffer is the mapping's own, `len` long.
    let buf = unsafe { core::slice::from_raw_parts(start, len) };
    let mut want = expected[..SIDE].to_vec();
    want.resize(len, 0);
    let mut wrote = 0;
    while wrote < len {
        wrote += syscall::write(writer, &buf[wrote..]).unwrap_or_else(|e| panic!("write of {len} from a buffer half untouched: {e:?}"));
    }
    let dst = fresh(len.div_ceil(PAGE) + 2);
    // SAFETY: inside the second mapping; its first page is touched once so
    // the read's fault comes past the bytes it holds.
    let into = unsafe {
        dst.add(into).write_volatile(0);
        core::slice::from_raw_parts_mut(dst.add(into), len)
    };
    let mut read = 0;
    while read < len {
        read += syscall::read(reader, &mut into[read..]).unwrap_or_else(|e| panic!("read of {len} into a buffer half untouched: {e:?}"));
    }
    assert!(into == want.as_slice(), "{len} bytes through a pipe, both buffers faulting mid-copy, came back changed");
    // SAFETY: the pipe's ends are this function's alone.
    unsafe {
        syscall::close(reader);
        syscall::close(writer);
    }
    len
}

/// `SYS_NAMESPACE_BUILD` reads its names blob whole before anything else, so a
/// blob that runs off its mapping, or into a page the program may not read,
/// is refused `BadAddress` mid-copy, and a kernel address before any copy.
fn a_byte_nothing_serves_ends_the_copy() {
    let blob = fresh(2);
    // SAFETY: inside the mapping: the first page is touched and the second given back.
    unsafe {
        blob.add(PAGE - SIDE).write_volatile(b'a');
        syscall::vm_release(blob.add(PAGE), PAGE).expect("release the second page");
    }
    let build = |names_ptr: u64, names_len: u64| {
        let args = NamespaceBuild {
            base: toyos_abi::handle::HANDLE_INVALID,
            flags: 0,
            keep_ptr: 0,
            keep_n: 0,
            add_ptr: 0,
            add_n: 0,
            names_ptr,
            names_len,
        };
        // SAFETY: the pointers name memory the kernel must refuse; nothing here reads it.
        unsafe { syscall::namespace_build(&args) }
    };
    let across = build(blob as u64 + (PAGE - SIDE) as u64, 2 * SIDE as u64);
    assert_eq!(across, Err(SyscallError::BadAddress), "a names blob running into a released page");

    let guarded = fresh(2);
    // SAFETY: inside the mapping: the first page is touched, the second made unreadable.
    unsafe {
        guarded.add(PAGE - SIDE).write_volatile(b'a');
        syscall::vm_protect(guarded.add(PAGE), PAGE, VmProt::None).expect("protect the second page");
    }
    let unreadable = build(guarded as u64 + (PAGE - SIDE) as u64, 2 * SIDE as u64);
    assert_eq!(unreadable, Err(SyscallError::BadAddress), "a names blob running into an unreadable page");

    let kernel = build(0xFFFF_8000_0000_0000, 16);
    assert_eq!(kernel, Err(SyscallError::BadAddress), "a names blob at a kernel address");

    let past = build(0x0000_7FFF_FFFF_FF00, 0x200);
    assert_eq!(past, Err(SyscallError::BadAddress), "a names blob running past the user half");
    println!("a byte nothing serves ends the copy as BadAddress");
}

fn main() {
    const AT: usize = PAGE - SIDE;
    println!("a fault served mid-copy resumes: {} bytes", a_fault_served_mid_copy_resumes(2 * SIDE, 7, AT, AT));
    // Past a bounce: the pipe's claimed path copies a chunk at a time.
    println!("a fault served mid-copy resumes: {} bytes", a_fault_served_mid_copy_resumes(3 * PAGE + 2 * SIDE, 11, AT, AT));
    // The pipe's frames begin a page, so each of these buffers is 16 bytes
    // from them modulo a page, on the side an x86-64 kernel copies highest
    // first on a CPU whose forward copy stalls there (`arch::x86_64::copy`).
    let near = a_fault_served_mid_copy_resumes(3 * PAGE + 2 * SIDE, 17, PAGE - 16, 16);
    println!("a fault served mid-copy resumes 16 bytes from the pipe's frames: {near} bytes");
    a_byte_nothing_serves_ends_the_copy();
    // The kernel still answers after every refusal.
    a_fault_served_mid_copy_resumes(2 * SIDE, 13, AT, AT);
    println!("user copies: all checks passed");
}
