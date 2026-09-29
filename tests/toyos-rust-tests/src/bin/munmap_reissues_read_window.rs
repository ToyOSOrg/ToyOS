//! A parked reader's copy must never land in a frame a sibling has reissued.
//!
//! The staged sequence is the isolation break `user_ptr.rs`'s window is meant
//! to close: thread A enters a blocking `read` whose buffer is a 2 MiB mapping,
//! parks on an empty pipe, and while it is parked thread B unmaps that mapping
//! and immediately maps its own — which, because the physical allocator hands
//! the lowest free frame back first, is the very frame A is about to be woken
//! to copy into. B fills its mapping with its own byte, then writes the pipe;
//! A wakes and copies the pipe's bytes through the pointer it translated before
//! it parked. On a kernel that does not pin the frame that pointer names, A's
//! copy overwrites B's mapping — one process reading a pipe corrupts another
//! allocation's memory.
//!
//! The assertion is that B's mapping still holds B's byte. B unmaps only once
//! the kernel's roster says A is parked, so every run stages; A's `read`
//! returning the whole buffer is asserted, so nothing reads green for free.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::syscall::{close, mmap, munmap, pipe, read, write, MmapFlags, MmapProt};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// The copy length. One page is enough for the copy to overwrite B's byte, and
/// far under the pipe ring so the whole payload is delivered in one write.
const LEN: usize = 4096;

/// What the parked reader copies out of the pipe, and B never writes.
const PATTERN_A: u8 = 0xA1;
/// What B writes into its own frame, and a safe kernel leaves there.
const PATTERN_B: u8 = 0xB2;

fn map_2m() -> *mut u8 {
    let p = unsafe {
        mmap(
            core::ptr::null_mut(),
            PAGE_2M,
            MmapProt::READ | MmapProt::WRITE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!p.is_null(), "mmap of a 2 MiB region failed");
    p
}

#[path = "../roster.rs"]
mod roster;

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let ends = pipe().expect("a pipe");
    let read_end = ends.read;
    let write_end = ends.write;

    let victim = map_2m();
    let victim_addr = victim as usize;

    let ready = Arc::new(AtomicU32::new(0));
    let result = Arc::new(AtomicI64::new(i64::MIN));

    let a = {
        let ready = Arc::clone(&ready);
        let result = Arc::clone(&result);
        thread::spawn(move || {
            // The buffer is valid when the reference is formed and when the
            // read begins; the kernel owns the pointer once it parks, and B
            // unmaps only then. Nothing in this thread dereferences it.
            let buf = unsafe { core::slice::from_raw_parts_mut(victim_addr as *mut u8, LEN) };
            ready.store(1, Ordering::SeqCst);
            let n = match read(read_end, buf) {
                Ok(n) => n as i64,
                Err(_) => -1,
            };
            result.store(n, Ordering::SeqCst);
        })
    };

    // The victim sets `ready` immediately before the read syscall, and its
    // only park is that read's. The roster first: its syscall is the loop's
    // preemption point.
    roster::await_true(|| {
        roster::my_threads(&cap).iter().any(|&(is_thread, state)| is_thread && state == roster::BLOCKED)
            && ready.load(Ordering::SeqCst) == 1
    });

    unsafe { munmap(victim_addr as *mut u8, PAGE_2M) }.expect("munmap the victim's buffer");

    let sibling = map_2m();
    unsafe { core::ptr::write_bytes(sibling, PATTERN_B, LEN) };

    let payload = [PATTERN_A; LEN];
    write(write_end, &payload).expect("write to wake the reader");

    a.join().expect("the victim thread panicked");
    close(read_end);
    close(write_end);

    let n = result.load(Ordering::SeqCst);
    assert_eq!(n, LEN as i64, "the parked reader's read answered {n}, so the copy the test is about never ran");

    let got = unsafe { core::slice::from_raw_parts(sibling, LEN) };
    let bad = got.iter().position(|&b| b != PATTERN_B);
    assert!(
        bad.is_none(),
        "a parked reader's copy reached a sibling's reissued frame — \
         byte {} of it is {:#x}, not {PATTERN_B:#x}",
        bad.unwrap(),
        got[bad.unwrap()],
    );
    unsafe { munmap(sibling, PAGE_2M) }.expect("munmap the sibling's buffer");
    println!("munmap_reissues_read_window: the frame held under the copy");
}
