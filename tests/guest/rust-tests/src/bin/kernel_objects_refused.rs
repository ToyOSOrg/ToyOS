//! A process that has taken all the memory a process may take is refused
//! every kernel object it asks for more, as an error at the call, and the
//! machine goes on: the kernel never hands a process what it holds back for
//! itself, so it never panics for want of memory a process took.
//!
//! The process commits every frame it may, then fills ranges and smaller
//! null objects until each is refused. It checks the remaining generic
//! allocations and gives everything back before checking recovery. No
//! filesystem service is asked to allocate while the machine has no room.
//!
//! While it holds every frame, every other process's allocation is refused
//! too, so it has a boot of its own: `kernel_objects_refused_alone`.

use std::process::Command;

use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt, MAX_RANGES};

const PAGE: usize = 4096;

fn main() {
    // All bookkeeping storage is allocated before exhausting memory.
    let unwritten = syscall::pipe().expect("a pipe before anything is taken");
    let mut small = Vec::with_capacity(toyos_abi::RawHandle::MAX_SLOTS);
    let mut committed: Vec<(*mut u8, usize)> = Vec::with_capacity(128);
    let mut ranges: Vec<*mut u8> = Vec::with_capacity(MAX_RANGES);
    println!("  kernel_objects_refused: taking every frame a process may");

    commit_every_frame(&mut committed);
    let range_refused = fill_ranges(&mut ranges);
    let objects_refused = fill_small(&mut small);
    let refused = [
        ("a pipe", syscall::pipe().map(|_| ())),
        ("a port", syscall::port_create().map(|_| ())),
        ("an event", syscall::event_create().map(|_| ())),
        ("a null object", syscall::null(toyos_abi::Rights::READ).map(|_| ())),
        ("a pipe's first write, its frame", syscall::write(unwritten.write, b"x").map(|_| ())),
        ("a memory object", syscall::shm_create(PAGE).map(|_| ())),
        // SAFETY: placed anywhere, so it replaces nothing.
        ("a commitment", unsafe { syscall::vm_reserve(PAGE, VmProt::ReadWrite, VmAt::ANYWHERE) }.map(|_| ())),
    ];

    give_back(&committed, &ranges, &small);
    assert!(range_refused < MAX_RANGES - 64, "ranges were refused at {range_refused}, the per-space bound, not for memory");
    assert!(objects_refused < toyos_abi::RawHandle::MAX_SLOTS - 64, "the handle limit was reached before memory ran out");
    println!("  PASS: {range_refused} ranges, then {objects_refused} null objects, and the next of each was refused");
    for (what, answer) in refused {
        assert_eq!(answer, Err(SyscallError::ResourceExhausted), "{what} with every frame taken");
        println!("  PASS: {what} is refused, ResourceExhausted");
    }

    served_again(unwritten.write);
    println!("kernel_objects_refused: every kind is refused at its call while nothing is left, and served once it is given back");
}

/// Commit, untouched, every page a promise can still be made for: halving
/// what is asked each time it is refused, down to one page.
fn commit_every_frame(committed: &mut Vec<(*mut u8, usize)>) {
    let mut len = 1usize << 40;
    while len >= PAGE {
        // SAFETY: placed anywhere, so it replaces nothing.
        match unsafe { syscall::vm_reserve(len, VmProt::ReadWrite, VmAt::ANYWHERE) } {
            Ok(at) if committed.len() < committed.capacity() => committed.push((at, len)),
            Ok(at) => {
                // SAFETY: the range just made, which nothing reaches.
                unsafe { syscall::vm_release(at, len) }.expect("release");
                panic!("{} commitments made and more promised still", committed.len());
            }
            Err(SyscallError::ResourceExhausted | SyscallError::InvalidArgument) => len /= 2,
            Err(e) => panic!("a commitment of {len} bytes answered {e:?}"),
        }
    }
}

/// Reserved pages, each on a multiple of two pages so that no two join and
/// none is committed, until one is refused: how many were made.
fn fill_ranges(ranges: &mut Vec<*mut u8>) -> usize {
    loop {
        // SAFETY: placed anywhere, so it replaces nothing.
        match unsafe { syscall::vm_reserve(PAGE, VmProt::None, VmAt::Anywhere { align: 2 * PAGE }) } {
            Ok(at) => ranges.push(at),
            Err(SyscallError::ResourceExhausted) => return ranges.len(),
            Err(e) => panic!("a range at {} answered {e:?}", ranges.len()),
        }
        assert!(ranges.len() < MAX_RANGES, "{MAX_RANGES} ranges made with every frame taken");
    }
}

/// Consume the smaller object charges left after a range no longer fits.
fn fill_small(objects: &mut Vec<toyos_abi::RawHandle>) -> usize {
    loop {
        match syscall::null(toyos_abi::Rights::READ) {
            Ok(object) => objects.push(object),
            Err(SyscallError::ResourceExhausted) => return objects.len(),
            Err(e) => panic!("a null object answered {e:?}"),
        }
        assert!(objects.len() < objects.capacity(), "every handle filled before memory ran out");
    }
}

fn give_back(committed: &[(*mut u8, usize)], ranges: &[*mut u8], small: &[toyos_abi::RawHandle]) {
    for &object in small {
        // SAFETY: raw handles this function owns, each closed once.
        unsafe { syscall::close(object) };
    }
    for &at in ranges {
        // SAFETY: a range this test made, which nothing reaches.
        unsafe { syscall::vm_release(at, PAGE) }.expect("release a range");
    }
    for &(at, len) in committed {
        // SAFETY: a commitment this test made, which nothing touched.
        unsafe { syscall::vm_release(at, len) }.expect("release a commitment");
    }
}

/// Everything refused above is served, and a spawn runs: the machine went on.
fn served_again(unwritten: toyos_abi::handle::RawHandle) {
    let pipe = syscall::pipe().expect("a pipe once memory is given back");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(pipe.read) };
    // SAFETY: no type holds this number.
    unsafe { syscall::close(pipe.write) };
    assert_eq!(syscall::write(unwritten, b"x"), Ok(1), "a pipe's first write once memory is given back");
    let null = syscall::null(toyos_abi::Rights::READ).expect("a null object after release");
    let memory = syscall::shm_create(PAGE).expect("memory object after release");
    let port = syscall::port_create().expect("port after release");
    let event = syscall::event_create().expect("event after release");
    // SAFETY: each raw handle was created here and closed once.
    unsafe {
        syscall::close(null); syscall::close(memory);
        syscall::close(port.acceptor); syscall::close(port.connector);
        syscall::close(event.signal); syscall::close(event.wait);
    }
    let echo = Command::new("/system/bin/echo").arg("still alive").output().expect("spawn echo once memory is given back");
    assert!(echo.status.success() && String::from_utf8_lossy(&echo.stdout).trim() == "still alive", "echo answered {echo:?}");
    println!("  PASS: every kind is served again once the memory is given back, and a spawn runs");
}
