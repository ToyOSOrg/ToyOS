//! A parked reader whose buffer spans two mappings holds both frames: a
//! sibling that unmaps the second one and maps again is never handed the frame
//! the parked copy is about to land in.
//!
//! `munmap_reissues_read_window`'s staging, over a buffer that ends in a second
//! mapping. The two mappings are placed side by side with `FIXED`, the upper
//! one first: the frame allocator hands out the lowest free frame first, so
//! the upper mapping's frame lies below the lower one's and the buffer is two
//! physical runs, of which the upper is the second. Nothing here can see a
//! physical address, so that premise is the allocator's and is not checked.
//!
//! The assertion is that the sibling's new mapping still holds its own byte.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::syscall::{close, mmap, munmap, pipe, read, write, MmapFlags, MmapProt};

const PAGE_2M: usize = 2 * 1024 * 1024;

/// Bytes of the buffer in the lower mapping and in the upper one; the whole
/// buffer is far under the pipe ring, so one write delivers it.
const BELOW: usize = 2048;
const ABOVE: usize = 2048;

/// What the parked reader copies out of the pipe, and the sibling never writes.
const PATTERN_A: u8 = 0xA1;
/// What the sibling writes into its new mapping, and a safe kernel leaves there.
const PATTERN_B: u8 = 0xB2;

fn map(at: *mut u8, len: usize, prot: MmapProt, flags: MmapFlags) -> *mut u8 {
    let p = unsafe { mmap(at, len, prot, MmapFlags::ANONYMOUS | MmapFlags::PRIVATE | flags) };
    assert!(!p.is_null(), "mmap of {len:#x} bytes at {at:?} failed");
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

    // Two 2 MiB slots side by side: reserved, released, then mapped upper first.
    let rw = MmapProt::READ | MmapProt::WRITE;
    let lower = map(core::ptr::null_mut(), 2 * PAGE_2M, MmapProt::NONE, MmapFlags(0));
    unsafe { munmap(lower, 2 * PAGE_2M) }.expect("release the reservation");
    let upper = unsafe { lower.add(PAGE_2M) };
    assert_eq!(map(upper, PAGE_2M, rw, MmapFlags::FIXED), upper);
    assert_eq!(map(lower, PAGE_2M, rw, MmapFlags::FIXED), lower);
    let buf_addr = upper as usize - BELOW;

    let ready = Arc::new(AtomicU32::new(0));
    let result = Arc::new(AtomicI64::new(i64::MIN));

    let a = {
        let ready = Arc::clone(&ready);
        let result = Arc::clone(&result);
        thread::spawn(move || {
            // Valid when formed and when the read begins; the kernel owns the
            // pointer once it parks, and nothing in this thread dereferences it.
            let buf = unsafe { core::slice::from_raw_parts_mut(buf_addr as *mut u8, BELOW + ABOVE) };
            ready.store(1, Ordering::SeqCst);
            let n = match read(read_end, buf) {
                Ok(n) => n as i64,
                Err(_) => -1,
            };
            result.store(n, Ordering::SeqCst);
        })
    };

    roster::await_true(|| {
        roster::my_threads(&cap).iter().any(|&(is_thread, state)| is_thread && state == roster::BLOCKED)
            && ready.load(Ordering::SeqCst) == 1
    });

    unsafe { munmap(upper, PAGE_2M) }.expect("munmap the buffer's second mapping");

    let sibling = map(core::ptr::null_mut(), PAGE_2M, rw, MmapFlags(0));
    unsafe { core::ptr::write_bytes(sibling, PATTERN_B, ABOVE) };

    write(write_end, &[PATTERN_A; BELOW + ABOVE]).expect("write to wake the reader");

    a.join().expect("the reader thread panicked");
    close(read_end);
    close(write_end);

    let n = result.load(Ordering::SeqCst);
    assert_eq!(n, (BELOW + ABOVE) as i64, "the parked reader's read answered {n}, so the copy the test is about never ran");
    let first = unsafe { core::slice::from_raw_parts(buf_addr as *const u8, BELOW) };
    assert!(first.iter().all(|&b| b == PATTERN_A), "the copy's first run never reached the mapping still in place");

    let got = unsafe { core::slice::from_raw_parts(sibling, ABOVE) };
    if let Some(bad) = got.iter().position(|&b| b != PATTERN_B) {
        panic!(
            "a parked reader's second run reached a sibling's reissued frame — byte {bad} of it is {:#x}, not {PATTERN_B:#x}",
            got[bad]
        );
    }
    unsafe { munmap(sibling, PAGE_2M) }.expect("munmap the sibling's mapping");
    unsafe { munmap(lower, PAGE_2M) }.expect("munmap the buffer's first mapping");
    println!("munmap_reissues_second_read_window: both frames held under the copy");
}
