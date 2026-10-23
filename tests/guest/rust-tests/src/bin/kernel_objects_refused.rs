//! A process that has taken all the memory a process may take is refused
//! every kernel object it asks for more, as an error at the call, and the
//! machine goes on: the kernel never hands a process what it holds back for
//! itself, so it never panics for want of memory a process took.
//!
//! The process commits every frame it may, which costs nothing while no page
//! is touched; makes ranges, then directories, until each is refused, which
//! leaves less of what charges cover than either takes; is refused one of each
//! other kind; and is served again once it gives everything back. Nothing it
//! does in between allocates in its own heap, which could only abort it.

use std::process::Command;

use toyos_abi::syscall::{self, OpenFlags, SyscallError, VmAt, VmProt, MAX_RANGES};

const PAGE: usize = 4096;

/// The directories made under, and the most the fill makes before it is
/// owed a refusal: what a few superframes of charges hold, many times over.
const DIRS: &[u8] = b"/tmp/kernel_objects_refused";
const MOST_DIRS: usize = 200_000;

fn main() {
    // Made before anything is taken, so what follows needs no heap of its
    // own: the pipe whose ring is not yet allocated, the file not yet
    // written, every list at its bound, and stdout's buffer.
    let unwritten = syscall::pipe().expect("a pipe before anything is taken");
    let file = syscall::open(b"/tmp/kernel_objects_refused.bin", OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE)
        .expect("a /tmp file before anything is taken");
    syscall::mkdir(DIRS).expect("the directory the fill makes its own in");
    let mut committed: Vec<(*mut u8, usize)> = Vec::with_capacity(128);
    let mut ranges: Vec<*mut u8> = Vec::with_capacity(MAX_RANGES);
    let mut dirs = 0usize;
    println!("  kernel_objects_refused: taking every frame a process may");

    commit_every_frame(&mut committed);
    let range_refused = fill_ranges(&mut ranges);
    let dir_refused = fill_dirs(&mut dirs);
    let refused = [
        ("a pipe", syscall::pipe().map(|_| ())),
        ("a port", syscall::port_create().map(|_| ())),
        ("a directory", syscall::mkdir(b"/tmp/kernel_objects_refused_more")),
        ("a pipe's first write, its ring", syscall::write(unwritten.write, b"x").map(|_| ())),
        ("a /tmp file's page", syscall::write(file, &[0x5a; PAGE]).map(|_| ())),
        ("a memory object", syscall::shm_create(PAGE).map(|_| ())),
        // SAFETY: placed anywhere, so it replaces nothing.
        ("a commitment", unsafe { syscall::vm_reserve(PAGE, VmProt::ReadWrite, VmAt::ANYWHERE) }.map(|_| ())),
    ];

    give_back(&committed, &ranges, dirs);
    assert!(range_refused < MAX_RANGES - 64, "ranges were refused at {range_refused}, the per-space bound, not for memory");
    assert!(dir_refused < MOST_DIRS, "{MOST_DIRS} directories were made and none refused: directories are not charged");
    println!("  PASS: {range_refused} ranges, then {dir_refused} directories, and the next of each was refused");
    for (what, answer) in refused {
        assert_eq!(answer, Err(SyscallError::ResourceExhausted), "{what} with every frame taken");
        println!("  PASS: {what} is refused, ResourceExhausted");
    }

    served_again(unwritten.write, file);
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

/// Directories `DIRS/<n>` until one is refused: how many were made.
fn fill_dirs(dirs: &mut usize) -> usize {
    while *dirs < MOST_DIRS {
        match syscall::mkdir(dir_path(&mut [0; 64], *dirs)) {
            Ok(()) => *dirs += 1,
            Err(SyscallError::ResourceExhausted) => return *dirs,
            Err(e) => panic!("directory {} answered {e:?}", *dirs),
        }
    }
    *dirs
}

/// `DIRS/<n>`, written into `buf`.
fn dir_path(buf: &mut [u8; 64], n: usize) -> &[u8] {
    buf[..DIRS.len()].copy_from_slice(DIRS);
    buf[DIRS.len()] = b'/';
    let mut digits = [0u8; 20];
    let (mut n, mut len) = (n, 0);
    loop {
        digits[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let at = DIRS.len() + 1;
    for (i, digit) in digits[..len].iter().rev().enumerate() {
        buf[at + i] = *digit;
    }
    &buf[..at + len]
}

fn give_back(committed: &[(*mut u8, usize)], ranges: &[*mut u8], dirs: usize) {
    for n in 0..dirs {
        syscall::rmdir(dir_path(&mut [0; 64], n)).expect("remove a directory the fill made");
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
fn served_again(unwritten: toyos_abi::handle::RawHandle, file: toyos_abi::handle::RawHandle) {
    let pipe = syscall::pipe().expect("a pipe once memory is given back");
    syscall::close(pipe.read);
    syscall::close(pipe.write);
    assert_eq!(syscall::write(unwritten, b"x"), Ok(1), "a pipe's first write once memory is given back");
    assert_eq!(syscall::write(file, &[0x5a; PAGE]), Ok(PAGE), "a /tmp write once memory is given back");
    syscall::close(file);
    syscall::delete(b"/tmp/kernel_objects_refused.bin").expect("delete the /tmp file");
    syscall::rmdir(DIRS).expect("remove the fill's directory");
    let echo = Command::new("/system/bin/echo").arg("still alive").output().expect("spawn echo once memory is given back");
    assert!(echo.status.success() && String::from_utf8_lossy(&echo.stdout).trim() == "still alive", "echo answered {echo:?}");
    println!("  PASS: every kind is served again once the memory is given back, and a spawn runs");
}
