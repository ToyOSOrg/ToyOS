//! `unmap_touch`: whether a page's unmapping reaches every TLB that may hold
//! it. Each trial is a child that writes a page, unmaps it and reads it again;
//! that read must end the child, because a translation left cached past the
//! unmap still reaches the frame. In `touch` the unmapping thread reads it; in
//! `seen` a second thread that has been reading it all along, which on a
//! machine of more CPUs than busy threads runs beside the unmap, on a CPU whose
//! TLB only a broadcast invalidation reaches.

use std::os::toyos::process::ExitStatusExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::{Acquire, Release}};

use toyos_abi::end::{End, Fault};
use toyos_abi::syscall::{self, VmAt, VmProt};

const TRIALS: usize = 4;
const PAGE: usize = 2 * 1024 * 1024;
/// How a read of an unmapped page ends a process; a refused unmap or a panic
/// ends the child otherwise.
const FAULTED: End = End::Fault(Fault::Memory);
/// A child's last line before the unmap.
const UNMAPPING: &str = "unmap_touch: written; unmapping";
/// A child's line if its read after the unmap came back.
const READ_BACK: &str = "unmap_touch: read";
/// Reads `seen`'s reader makes before the unmap, each through the translation
/// its CPU caches.
const WARM: u64 = 1_000;

static READS: AtomicU64 = AtomicU64::new(0);
static UNMAPPED: AtomicBool = AtomicBool::new(false);

pub fn main(args: Vec<String>) {
    match args.first().map(String::as_str) {
        None => judge(),
        Some("touch") => touch(),
        Some("seen") => seen(),
        Some(other) => panic!("unmap_touch: unknown mode {other:?}"),
    }
}

fn judge() {
    for mode in ["touch", "seen"] {
        for trial in 0..TRIALS {
            let out = Command::new("/system/bin/unmap_touch")
                .arg(mode)
                .output()
                .unwrap_or_else(|e| panic!("unmap_touch: the child would not spawn: {e}"));
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(said.contains(UNMAPPING), "{mode} trial {trial}: the child never reached the unmap: {said:?}");
            assert!(
                End::from_word(out.status.into_raw()) == Some(FAULTED) && !said.contains(READ_BACK),
                "{mode} trial {trial}: the child's end, {}, was not {FAULTED}, on a read of a page it had just unmapped: {said:?}",
                out.status,
            );
        }
    }
    println!("unmap_touch: {TRIALS} reads of a page just unmapped on the unmapping thread, and {TRIALS} on another, each ended their process");
}

/// A fresh page, written.
fn written() -> *mut u8 {
    // SAFETY: a fresh anonymous mapping this function alone names.
    let page = unsafe { syscall::vm_reserve(PAGE, VmProt::ReadWrite, VmAt::ANYWHERE).unwrap_or(core::ptr::null_mut()) };
    assert!(!page.is_null(), "unmap_touch: mmap refused");
    // SAFETY: inside the mapping just made.
    unsafe { page.write_volatile(0x5A) };
    page
}

fn touch() {
    let page = written();
    println!("{UNMAPPING}");
    // After the last print: a print can switch the CPU, and a switch can drop
    // the translation the first read caches.
    // SAFETY: the mapping just made, whole, which nothing else names; the
    // second read ending this process is what the child exists to make.
    let (first, answer, second) = unsafe { crate::arch::read_unmap_read(page.cast(), PAGE) };
    println!("{READ_BACK} {second:#x} after the unmap answered {answer:#x}, {first:#x} before it");
}

fn seen() {
    let page = written();
    let at = page as usize;
    std::thread::spawn(move || read_until_unmapped(at as *const u8));
    while READS.load(Acquire) < WARM {
        std::hint::spin_loop();
    }
    println!("{UNMAPPING}");
    // SAFETY: the mapping made above, whole; its reader's next read is the probe.
    unsafe { syscall::vm_release(page, PAGE) }.expect("unmap_touch: munmap refused");
    UNMAPPED.store(true, Release);
    // The reader's fault ends this process; this thread waits for it.
    loop {
        std::thread::park();
    }
}

/// Read `page` until the unmap is said to have returned, then once more.
fn read_until_unmapped(page: *const u8) -> ! {
    loop {
        let unmapped = UNMAPPED.load(Acquire);
        // SAFETY: mapped until `unmapped` reads true; the read after that is
        // the probe, and a kernel that keeps its word ends the process there.
        let byte = unsafe { page.read_volatile() };
        if unmapped {
            println!("{READ_BACK} {byte:#x} after the unmap had returned");
            std::process::exit(0);
        }
        READS.fetch_add(1, Release);
    }
}
