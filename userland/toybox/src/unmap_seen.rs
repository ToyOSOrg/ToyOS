//! `unmap_seen`: whether an unmap reaches a translation another thread holds.
//! A child maps a page and a second thread reads it in a loop; once that
//! thread has read it, the first unmaps it and says so through memory, and the
//! reader's next read must end the child. On a machine of more CPUs than busy
//! threads the reader runs beside the unmap, on a CPU whose TLB only a
//! broadcast invalidation reaches.

use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::{Acquire, Release}};

use toyos_abi::syscall::{mmap, munmap, MmapFlags, MmapProt};

const TRIALS: usize = 4;
const PAGE: usize = 2 * 1024 * 1024;
/// Reads the reader makes before the unmap, each through the translation its
/// CPU caches.
const WARM: u64 = 1_000;
/// The status `kill_process(-1)` gives a process whose fault nothing serves;
/// a refused unmap or a panic ends the child with another.
const FAULTED: i32 = -1;
/// The child's last line before the unmap.
const UNMAPPING: &str = "unmap_seen: the reader holds the page; unmapping";
/// The reader's line if its read after the unmap came back.
const READ_BACK: &str = "unmap_seen: the reader read";

static READS: AtomicU64 = AtomicU64::new(0);
static UNMAPPED: AtomicBool = AtomicBool::new(false);

pub fn main(args: Vec<String>) {
    match args.first().map(String::as_str) {
        None => judge(),
        Some("child") => child(),
        Some(other) => panic!("unmap_seen: unknown mode {other:?}"),
    }
}

fn judge() {
    for trial in 0..TRIALS {
        let out = Command::new("/system/bin/unmap_seen")
            .arg("child")
            .output()
            .unwrap_or_else(|e| panic!("unmap_seen: the child would not spawn: {e}"));
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(said.contains(UNMAPPING), "trial {trial}: the child never reached the unmap: {said:?}");
        assert!(
            out.status.code() == Some(FAULTED) && !said.contains(READ_BACK),
            "trial {trial}: the child exited {:?}, not faulted ({FAULTED}), once its reader read a page \
             unmapped beside it: {said:?}",
            out.status.code(),
        );
    }
    println!("unmap_seen: {TRIALS} reads on another thread of a page just unmapped each ended their process");
}

fn child() {
    // SAFETY: a fresh anonymous mapping this function alone names.
    let page = unsafe { mmap(core::ptr::null_mut(), PAGE, MmapProt::READ | MmapProt::WRITE, MmapFlags::ANONYMOUS | MmapFlags::PRIVATE) };
    assert!(!page.is_null(), "unmap_seen: mmap refused");
    // SAFETY: inside the mapping just made.
    unsafe { page.write_volatile(0x5A) };
    let at = page as usize;
    std::thread::spawn(move || read_until_unmapped(at as *const u8));
    while READS.load(Acquire) < WARM {
        std::hint::spin_loop();
    }
    println!("{UNMAPPING}");
    // SAFETY: the mapping made above, whole; its reader's next read is the probe.
    unsafe { munmap(page, PAGE) }.expect("unmap_seen: munmap refused");
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
