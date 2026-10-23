//! Memory beyond `system`'s: what a first touch costs per page size, changing
//! and giving back part of a mapping, the allocator under many threads, and a
//! large `realloc`.
//!
//! Both systems fault each anonymous page in on its first touch: ToyOS in
//! 4 KiB pages always, Linux in 4 KiB pages where huge pages are refused and
//! in 2 MiB pages where they are asked for and given. `map_touch_unmap` is the
//! whole cost. The allocator is each system's own: ToyOS std's dlmalloc
//! under one lock, Linux's static musl malloc.

use std::alloc::{alloc, dealloc, realloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use crate::measure::each;
use crate::report::{refuse, result};
use crate::sys;
use crate::ns_since;

const MIB: usize = 1 << 20;
const PAGE: usize = 4096;
const HUGE: usize = 2 * MIB;

pub fn suite() {
    first_touch();
    protect();
    unmap_part();
    for size in [16usize, 4096, MIB] {
        churn(size, 1);
        churn(size, crate::cpus());
    }
    realloc_grow();
}

/// A fresh 64 MiB mapping written once per page: ns per page touched, each
/// round's mean, and the whole map-touch-unmap per page; 4 KiB pages with
/// huge pages refused, and 2 MiB pages with them asked for. Ten rounds after one.
fn first_touch() {
    const LEN: usize = 64 * MIB;
    for (name, step, map) in [("4k", PAGE, sys::map_anon_4k as fn(usize) -> *mut u8), ("2m", HUGE, sys::map_anon_huge)] {
        let pages = (LEN / step) as u64;
        let round = || {
            let t = Instant::now();
            let ptr = map(LEN);
            let mapped = ns_since(t);
            let before = sys::counts();
            let t = Instant::now();
            for page in 0..LEN / step {
                // SAFETY: inside the mapping just made.
                unsafe { ptr.add(page * step).write_volatile(1) };
            }
            let touched = ns_since(t);
            let faults = sys::counts().faults.zip(before.faults).map(|(b, a)| b - a);
            let t = Instant::now();
            // SAFETY: the whole mapping, unused from here.
            unsafe { sys::unmap(ptr, LEN) };
            (touched / pages, (mapped + touched + ns_since(t)) / pages, faults)
        };
        round();
        crate::clock::settle();
        let rounds: Vec<(u64, u64, Option<u64>)> = (0..10).map(|_| round()).collect();
        let faults = rounds.iter().filter_map(|r| r.2).max().map_or("unread".to_string(), |f| f.to_string());
        let extra = [("page_bytes", step.to_string()), ("faults_per_round_max", faults), ("huge_pages", sys::huge_backing())];
        result(&format!("mem.first_touch.{name}"), "ns/page", &mut rounds.iter().map(|r| r.0).collect::<Vec<_>>(), &extra);
        result(&format!("mem.map_touch_unmap.{name}"), "ns/page", &mut rounds.iter().map(|r| r.1).collect::<Vec<_>>(), &extra);
    }
}

/// One touched 4 KiB page of a 2 MiB mapping made read-only and writable
/// again, each change timed: 2,000 after 100.
fn protect() {
    const BENCH: &str = "mem.mprotect.4k";
    let ptr = sys::map_anon(HUGE);
    // SAFETY: inside the mapping.
    unsafe { ptr.write_volatile(1) };
    match sys::protect(ptr, PAGE, false).and_then(|()| sys::protect(ptr, PAGE, true)) {
        Ok(()) => each(BENCH, 100, 2000, &[], |i| sys::protect(ptr, PAGE, i % 2 == 1).map_err(sys::Refused::why)),
        Err(refused) => refused.report(BENCH),
    }
    // SAFETY: the whole mapping, unused from here.
    unsafe { sys::unmap(ptr, HUGE) };
}

/// The first 4 KiB of a touched 2 MiB mapping given back, timed, then the
/// rest: 500 after 20.
fn unmap_part() {
    const BENCH: &str = "mem.munmap_part.4k";
    let once = || -> Result<u64, sys::Refused> {
        let ptr = sys::map_anon(HUGE);
        // SAFETY: inside the mapping.
        unsafe { ptr.add(PAGE).write_volatile(1) };
        let t = Instant::now();
        // SAFETY: the first page, never used again.
        let part = unsafe { sys::unmap_part(ptr, PAGE) };
        let ns = ns_since(t);
        match part {
            Ok(()) => {
                // SAFETY: the rest of the mapping, never used again.
                unsafe { sys::unmap_part(ptr.add(PAGE), HUGE - PAGE) }?;
                Ok(ns)
            }
            Err(why) => {
                // SAFETY: the whole mapping, untouched by the refused call.
                unsafe { sys::unmap(ptr, HUGE) };
                Err(why)
            }
        }
    };
    match once() {
        Ok(_) => {
            for _ in 0..20 {
                let _ = once();
            }
            crate::clock::settle();
            match (0..500).map(|_| once()).collect::<Result<Vec<u64>, sys::Refused>>() {
                Ok(mut ns) => result(BENCH, "ns", &mut ns, &[]),
                Err(refused) => refuse(BENCH, refused.why()),
            }
        }
        Err(refused) => refused.report(BENCH),
    }
}

/// `threads` threads each freeing and allocating `size`-byte blocks for
/// 200 ms, each keeping a ring of live ones (64, or 4 of a mebibyte) and
/// writing a byte into each new block: thousands of alloc-free pairs a second
/// over all threads, five rounds after one.
fn churn(size: usize, threads: usize) {
    let bench = format!("mem.malloc_churn.{size}.t{threads}");
    let ring = if size >= MIB { 4 } else { 64 };
    let layout = Layout::from_size_align(size, 16).expect("a small layout");
    let round = || -> u64 {
        let start = Arc::new(Barrier::new(threads + 1));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicU64::new(0));
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let (start, stop, done) = (start.clone(), stop.clone(), done.clone());
                thread::spawn(move || {
                    // SAFETY: `layout` has a non-zero size; every block is
                    // freed with it once, below.
                    let mut live: Vec<*mut u8> = (0..ring).map(|_| unsafe { alloc(layout) }).collect();
                    start.wait();
                    let mut ops = 0u64;
                    while !stop.load(Ordering::Relaxed) {
                        for slot in live.iter_mut() {
                            // SAFETY: `*slot` came from `alloc(layout)`.
                            unsafe { dealloc(*slot, layout) };
                            // SAFETY: as above.
                            let block = unsafe { alloc(layout) };
                            assert!(!block.is_null(), "the allocator refused {size} bytes");
                            // SAFETY: a fresh block of at least one byte.
                            unsafe { block.write_volatile(ops as u8) };
                            *slot = block;
                        }
                        ops += ring as u64;
                    }
                    for block in live {
                        // SAFETY: as above.
                        unsafe { dealloc(block, layout) };
                    }
                    done.fetch_add(ops, Ordering::Relaxed);
                })
            })
            .collect();
        start.wait();
        let t = Instant::now();
        thread::sleep(Duration::from_millis(200));
        stop.store(true, Ordering::Relaxed);
        let ns = ns_since(t);
        for w in workers {
            w.join().expect("a churn worker panicked");
        }
        done.load(Ordering::Relaxed) * 1_000_000 / ns.max(1)
    };
    round();
    crate::clock::settle();
    let mut rates: Vec<u64> = (0..5).map(|_| round()).collect();
    result(&bench, "kops/s", &mut rates, &[("block_bytes", size.to_string()), ("threads", threads.to_string()), ("ring", ring.to_string())]);
}

/// A block grown by `realloc` from 1 MiB to 256 MiB, doubling, its first
/// mebibyte written: ns for the whole growth and for its last doubling, five
/// rounds after one. An allocator that can move the pages answers without
/// copying; one that cannot copies each time.
fn realloc_grow() {
    let round = || -> (u64, u64) {
        let mut size = MIB;
        let layout = Layout::from_size_align(size, 16).expect("a layout");
        // SAFETY: a non-zero layout.
        let mut ptr = unsafe { alloc(layout) };
        assert!(!ptr.is_null());
        // SAFETY: inside the block.
        unsafe { std::ptr::write_bytes(ptr, 0x33, MIB) };
        let (mut total, mut last) = (0, 0);
        while size < 256 * MIB {
            let t = Instant::now();
            // SAFETY: `ptr` holds `size` bytes allocated with this alignment.
            ptr = unsafe { realloc(ptr, Layout::from_size_align(size, 16).expect("a layout"), size * 2) };
            last = ns_since(t);
            total += last;
            assert!(!ptr.is_null(), "realloc to {} bytes was refused", size * 2);
            size *= 2;
        }
        // SAFETY: the block as last grown.
        unsafe { dealloc(ptr, Layout::from_size_align(size, 16).expect("a layout")) };
        (total, last)
    };
    round();
    crate::clock::settle();
    let rounds: Vec<(u64, u64)> = (0..5).map(|_| round()).collect();
    result("mem.realloc_grow.1m_256m", "ns", &mut rounds.iter().map(|r| r.0).collect::<Vec<_>>(), &[("doublings", "8".into())]);
    result("mem.realloc_grow.last_128m_256m", "ns", &mut rounds.iter().map(|r| r.1).collect::<Vec<_>>(), &[]);
}
