//! Memory beyond `system`'s: `memcpy` across sizes, what a first touch costs
//! per page size, changing and giving back part of a mapping, unmapping a
//! page another CPU touched, the allocator under many threads, and a large
//! `realloc`.
//!
//! Both systems fault each anonymous page in on its first touch: ToyOS in
//! 4 KiB pages always, Linux in 4 KiB pages where huge pages are refused and
//! in 2 MiB pages where they are asked for and given. `first_touch` faults in
//! the same bytes either way, and `map_touch_unmap` is the whole cost. The
//! allocator is each system's own: ToyOS std's dlmalloc under one lock,
//! Linux's static musl malloc.

use std::alloc::{alloc, dealloc, realloc, Layout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use crate::measure::each;
use crate::place;
use crate::report::{refuse, result};
use crate::sys::{self, Mapping};
use crate::{ns_since, wake};

const MIB: usize = 1 << 20;
const PAGE: usize = 4096;
const HUGE: usize = 2 * MIB;

pub fn suite() {
    memcpy_sweep();
    first_touch();
    protect();
    unmap_part();
    unmap_touched_remote();
    for size in [16usize, 4096, MIB] {
        churn(size, 1);
        churn(size, crate::cpus());
    }
    realloc_grow();
}

/// A fresh 64 MiB mapping written once per 4 KiB, so every byte of it is
/// faulted in whatever page size backs it: ns per MiB touched, each round's,
/// and the whole map-touch-unmap per MiB; with huge pages refused (`4k`) and
/// asked for (`2m`). Ten rounds after one.
fn first_touch() {
    const LEN: usize = 64 * MIB;
    const MIBS: u64 = (LEN / MIB) as u64;
    for (name, map) in [("4k", sys::map_anon_4k as fn(usize) -> *mut u8), ("2m", sys::map_anon_huge)] {
        let round = || {
            let t = Instant::now();
            let ptr = map(LEN);
            let mapped = ns_since(t);
            let before = sys::counts();
            let t = Instant::now();
            for page in 0..LEN / PAGE {
                // SAFETY: inside the mapping just made.
                unsafe { ptr.add(page * PAGE).write_volatile(1) };
            }
            let touched = ns_since(t);
            let faults = sys::counts().faults.zip(before.faults).map(|(b, a)| b - a);
            let huge = sys::huge_backing();
            let t = Instant::now();
            // SAFETY: the whole mapping, unused from here.
            unsafe { sys::unmap(ptr, LEN) };
            (touched / MIBS, (mapped + touched + ns_since(t)) / MIBS, faults, huge)
        };
        round();
        crate::clock::settle();
        let rounds: Vec<(u64, u64, Option<u64>, String)> = (0..10).map(|_| round()).collect();
        let faults = rounds.iter().filter_map(|r| r.2).max().map_or("unread".to_string(), |f| f.to_string());
        let huge = rounds.last().map(|r| r.3.clone()).unwrap_or_default();
        let extra = [("mapping_bytes", LEN.to_string()), ("faults_per_round_max", faults), ("huge_pages_last_round", huge)];
        result(&format!("mem.first_touch.{name}"), "ns/MiB", &mut rounds.iter().map(|r| r.0).collect::<Vec<_>>(), &extra);
        result(&format!("mem.map_touch_unmap.{name}"), "ns/MiB", &mut rounds.iter().map(|r| r.1).collect::<Vec<_>>(), &extra);
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
    let placed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let round = || -> u64 {
        let start = Arc::new(Barrier::new(threads + 1));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicU64::new(0));
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                let (start, stop, done, placed) = (start.clone(), stop.clone(), done.clone(), placed.clone());
                thread::spawn(move || {
                    // SAFETY: `layout` has a non-zero size; every block is
                    // freed with it once, below.
                    let mut live: Vec<*mut u8> = (0..ring).map(|_| unsafe { alloc(layout) }).collect();
                    start.wait();
                    let began_on = crate::sys::current_cpu();
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
                    placed.lock().expect("a churn worker panicked holding the placements").push((began_on, crate::sys::current_cpu()));
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
    placed.lock().expect("no worker runs now").clear();
    let around = crate::around::begin();
    let mut rates: Vec<u64> = (0..5).map(|_| round()).collect();
    around.end(&bench, &placed.lock().expect("no worker runs now"));
    result(&bench, "kops/s", &mut rates, &[("block_bytes", size.to_string()), ("threads", threads.to_string()), ("ring", ring.to_string())]);
}

/// A block grown by `realloc` from 1 MiB to 256 MiB, doubling, its first
/// mebibyte written: ns for the whole growth and for its last doubling, five
/// rounds after one. An allocator that can move the pages answers without
/// copying; one that cannot copies each time. The block escapes through
/// `black_box`, or the compiler drops every call.
fn realloc_grow() {
    let round = || -> (u64, u64) {
        let mut size = MIB;
        let layout = Layout::from_size_align(size, 16).expect("a layout");
        // SAFETY: a non-zero layout.
        let mut ptr = std::hint::black_box(unsafe { alloc(layout) });
        assert!(!ptr.is_null());
        // SAFETY: inside the block.
        unsafe { std::ptr::write_bytes(ptr, 0x33, MIB) };
        let (mut total, mut last) = (0, 0);
        while size < 256 * MIB {
            let t = Instant::now();
            // SAFETY: `ptr` holds `size` bytes allocated with this alignment.
            ptr = std::hint::black_box(unsafe { realloc(ptr, Layout::from_size_align(size, 16).expect("a layout"), size * 2) });
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

/// `copy_from_slice`, each system's `memcpy`, at every size from 64 B to
/// 64 MiB by fours, between two buffers asked for huge pages: MB/s per
/// round, a round being as many copies as make at least 64 MiB, 15 rounds
/// after one per size. Small sizes run from cache, large ones from memory.
fn memcpy_sweep() {
    const LEN: usize = 64 * MIB;
    let mut src = Mapping::huge(LEN);
    let mut dst = Mapping::huge(LEN);
    for (i, w) in src.words().iter_mut().enumerate() {
        *w = i as u64;
    }
    dst.words().fill(1);
    let huge = sys::huge_backing();
    let (s, d) = (src.ptr(), dst.ptr());
    let mut size = 64;
    while size <= LEN {
        let copies = (LEN / size).max(1);
        let round = || {
            let t = Instant::now();
            for _ in 0..copies {
                // SAFETY: both mappings are `LEN` long and distinct; `size`
                // is at most `LEN`.
                let (from, to) = unsafe {
                    (std::slice::from_raw_parts(std::hint::black_box(s), size), std::slice::from_raw_parts_mut(std::hint::black_box(d), size))
                };
                to.copy_from_slice(from);
            }
            (copies * size) as u64 * 1000 / ns_since(t).max(1)
        };
        round();
        crate::clock::settle();
        let mut mbps: Vec<u64> = (0..15).map(|_| round()).collect();
        let extra = [("copy_bytes", size.to_string()), ("copies_per_round", copies.to_string()), ("huge_pages", huge.clone())];
        result(&format!("mem.memcpy.{size}"), "MB/s", &mut mbps, &extra);
        size *= 4;
    }
}

/// The munmap pair's shared words: the page, the round the initiator posted
/// it in, the round the sibling touched it in, and the windows the initiator
/// has finished. `u64::MAX` in `posted` tells the sibling to stop.
#[derive(Default)]
struct Pair {
    page: AtomicU64,
    posted: AtomicU64,
    touched: AtomicU64,
    ended: AtomicU64,
}

const PAIR_WARMUP: u64 = 50;
const PAIR_WINDOWS: u64 = 10;
const PAIR_PER_WINDOW: u64 = 200;

/// A 4 KiB page mapped and written by the initiator, read by a sibling thread
/// spinning on a CPU of another core, then unmapped by the initiator: ns per
/// munmap, each of which has to reach the sibling's TLB. Ten windows of 200
/// after 50. Each side reads its APIC ID before and after each window, and a
/// window counts only where `osbench_rules::pair::admit` admits the four
/// readings; `windows_refused` counts the rest. Linux pins both sides; ToyOS
/// stages them. Neither is put on the interrupts' CPU.
fn unmap_touched_remote() {
    const BENCH: &str = "mem.munmap_touched_remote.4k";
    if let Err(why) = crate::arch::cpu_id() {
        return crate::report::unsupported(BENCH, why);
    }
    if crate::cpus() < 3 {
        return refuse(BENCH, "the initiator, the sibling's other core and the interrupts' CPU need three CPUs");
    }
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let initiator = {
        let outcome = outcome.clone();
        place::spawn_on(|on| on != wake::IRQ_CPU, move || *outcome.lock().unwrap() = Some(initiate()))
    };
    match initiator {
        Ok(placed) => placed.thread.join().expect("the initiator panicked"),
        Err(why) => return refuse(BENCH, format!("the initiator was not placed: {why}")),
    }
    let measured = outcome.lock().unwrap().take().expect("the initiator said nothing");
    match measured {
        Ok((ns, extra)) if ns.is_empty() => {
            let first = extra.iter().find(|(key, _)| *key == "first_refusal").map_or("", |(_, why)| why.as_str());
            refuse(BENCH, format!("every window was refused, the first as: {first}"))
        }
        Ok((mut ns, extra)) => {
            let total: u64 = ns.iter().sum();
            let rate = ("munmaps_per_s", (ns.len() as u64 * 1_000_000_000 / total.max(1)).to_string());
            let fields: Vec<(&str, String)> = extra.into_iter().chain([rate]).collect();
            result(BENCH, "ns", &mut ns, &fields);
        }
        Err(why) => refuse(BENCH, why),
    }
}

type PairMeasured = Result<(Vec<u64>, Vec<(&'static str, String)>), String>;

/// The initiator's side, on the CPU it was placed on: the sibling started,
/// every round run, the windows judged.
fn initiate() -> PairMeasured {
    use osbench_rules::pair::{admit, Readings};
    let me = sys::current_cpu();
    let pair = Arc::new(Pair::default());
    let (said, heard) = std::sync::mpsc::channel();
    let sibling = {
        let pair = pair.clone();
        let wanted = move |on: u32| on != wake::IRQ_CPU && sys::core_of(on) != sys::core_of(me);
        let body = move || {
            let _ = said.send(touch(&pair));
        };
        place::spawn_on(wanted, body).map_err(|why| format!("the sibling was not placed: {why}"))?
    };
    let sibling_cpu = sibling.cpu;
    let stop = |pair: &Pair| pair.posted.store(u64::MAX, Ordering::Release);
    let mut round = 0u64;
    let mut once = |pair: &Pair| -> Result<u64, String> {
        round += 1;
        let page = sys::map_anon(PAGE);
        // SAFETY: inside the mapping just made.
        unsafe { page.write_volatile(round as u8) };
        pair.page.store(page as u64, Ordering::Relaxed);
        pair.posted.store(round, Ordering::Release);
        wake::spin_until("the sibling's touch", || pair.touched.load(Ordering::Acquire) == round)?;
        let t = Instant::now();
        // SAFETY: the whole mapping, which the sibling read before saying so
        // and reads no more.
        unsafe { sys::unmap(page, PAGE) };
        Ok(ns_since(t))
    };
    let mut run = || -> Result<(Vec<Vec<u64>>, Vec<Readings>), String> {
        for _ in 0..PAIR_WARMUP {
            once(&pair)?;
        }
        crate::clock::settle();
        let (mut windows, mut ids) = (Vec::new(), Vec::new());
        for w in 1..=PAIR_WINDOWS {
            let before = crate::arch::cpu_id()?;
            let ns = (0..PAIR_PER_WINDOW).map(|_| once(&pair)).collect::<Result<Vec<u64>, String>>()?;
            ids.push(Readings { before, after: crate::arch::cpu_id()? });
            pair.ended.store(w, Ordering::Release);
            windows.push(ns);
        }
        Ok((windows, ids))
    };
    let ran = run();
    stop(&pair);
    sibling.thread.join().map_err(|_| "the sibling panicked".to_string())?;
    let (windows, ids) = ran?;
    let sibling_ids = heard.recv().map_err(|_| "the sibling said nothing".to_string())??;
    let (mut kept, mut refused, mut first_refusal) = (Vec::new(), 0, String::from("none"));
    for ((ns, initiator), sibling) in windows.into_iter().zip(ids).zip(sibling_ids) {
        match admit(initiator, sibling) {
            Ok(()) => kept.extend(ns),
            Err(why) => {
                if refused == 0 {
                    first_refusal = why.to_string();
                }
                refused += 1;
            }
        }
    }
    Ok((
        kept,
        vec![
            ("first_refusal", first_refusal),
            ("windows_refused", format!("{refused}/{PAIR_WINDOWS}")),
            ("initiator_cpu", me.to_string()),
            ("sibling_cpu", sibling_cpu.to_string()),
            ("id", crate::arch::cpu_id_is().to_string()),
        ],
    ))
}

/// The sibling's side: each posted page read, and its APIC ID read before
/// the first round of each window and after the window has ended.
fn touch(pair: &Pair) -> Result<Vec<osbench_rules::pair::Readings>, String> {
    let mut ids = Vec::new();
    let mut seen = 0u64;
    let mut before = None;
    loop {
        wake::spin_until("the initiator's next page", || pair.posted.load(Ordering::Acquire) != seen)?;
        let posted = pair.posted.load(Ordering::Acquire);
        if posted == u64::MAX {
            return Ok(ids);
        }
        let into_windows = posted.checked_sub(PAIR_WARMUP + 1);
        if into_windows.is_some_and(|n| n % PAIR_PER_WINDOW == 0) {
            before = Some(crate::arch::cpu_id()?);
        }
        let page = pair.page.load(Ordering::Relaxed) as *const u8;
        // SAFETY: the initiator unmaps the page only after `touched` names
        // this round.
        std::hint::black_box(unsafe { page.read_volatile() });
        pair.touched.store(posted, Ordering::Release);
        seen = posted;
        if into_windows.is_some_and(|n| n % PAIR_PER_WINDOW == PAIR_PER_WINDOW - 1) {
            let window = (posted - PAIR_WARMUP) / PAIR_PER_WINDOW;
            wake::spin_until("the window's end", || pair.ended.load(Ordering::Acquire) >= window)?;
            let before = before.take().expect("a window's first round came before its last");
            ids.push(osbench_rules::pair::Readings { before, after: crate::arch::cpu_id()? });
        }
    }
}
