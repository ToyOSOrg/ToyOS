//! `unmap_touch`: whether a page's unmapping reaches the TLB. A child writes
//! a page, unmaps it and reads it at once; the read must end the child,
//! because a translation left cached past the unmap still reaches the frame.

use std::process::Command;

use toyos_abi::syscall::{mmap, MmapFlags, MmapProt};

const TRIALS: usize = 4;
const PAGE: usize = 2 * 1024 * 1024;
/// The status `kill_process(-1)` gives a process whose fault nothing serves;
/// a refused unmap or a panic ends the child with another.
const FAULTED: i32 = -1;
/// The child's last line before the unmap and the read.
const UNMAPPING: &str = "unmap_touch: written; unmapping and reading";
/// The child's line if the read came back.
const READ_BACK: &str = "unmap_touch: read";

pub fn main(args: Vec<String>) {
    match args.first().map(String::as_str) {
        None => judge(),
        Some("touch") => touch(),
        Some(other) => panic!("unmap_touch: unknown mode {other:?}"),
    }
}

fn judge() {
    for trial in 0..TRIALS {
        let out = Command::new("/system/bin/unmap_touch")
            .arg("touch")
            .output()
            .unwrap_or_else(|e| panic!("unmap_touch: the child would not spawn: {e}"));
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains(UNMAPPING), "trial {trial}: the child never reached the unmap: {said:?}");
        assert!(
            out.status.code() == Some(FAULTED) && !said.contains(READ_BACK),
            "trial {trial}: the child exited {:?}, not faulted ({FAULTED}), on the read of a page it had just unmapped: {said:?}",
            out.status.code(),
        );
    }
    println!("unmap_touch: {TRIALS} reads of a page just unmapped each ended their process");
}

fn touch() {
    // SAFETY: a fresh anonymous mapping this function alone names.
    let page = unsafe { mmap(core::ptr::null_mut(), PAGE, MmapProt::READ | MmapProt::WRITE, MmapFlags::ANONYMOUS | MmapFlags::PRIVATE) };
    assert!(!page.is_null(), "unmap_touch: mmap refused");
    // SAFETY: inside the mapping just made.
    unsafe { page.write_volatile(0x5A) };
    println!("{UNMAPPING}");
    // After the last print: a print can switch the CPU, and a switch can drop
    // the translation the first read caches.
    // SAFETY: the mapping just made, whole, which nothing else names; the
    // second read ending this process is what the child exists to make.
    let (first, answer, second) = unsafe { crate::arch::read_unmap_read(page.cast(), PAGE) };
    println!("{READ_BACK} {second:#x} after the unmap answered {answer:#x}, {first:#x} before it");
}
