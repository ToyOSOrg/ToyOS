//! A process's own memory over 4 KiB pages, through the `vm_*` calls: no two
//! ranges overlap; `vm_release` takes away any run of pages — part of a
//! range, across two, or a hole — and a fixed reserve replaces whatever the
//! pages it names held; `vm_protect` commits a reservation and changes
//! protection in place, contents kept; `vm_discard` keeps the pages and reads
//! back zeros; `vm_decommit` makes them a reservation again; `vm_remap`
//! grows, moves and shrinks committed memory, contents kept, and loses no
//! frame or promise when it moves. A page
//! populated with its neighbours is populated as its own range says. The
//! ranges one space holds are bounded, and a commitment past what memory
//! holds is refused when it is asked for.
//!
//! A page left unreachable, or read-only and written, must fault, which ends
//! the process, so each such case runs in a child that makes the page itself
//! and then touches it.

use std::collections::HashSet;
use std::process::Command;

use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt, MAX_RANGES};

const PAGE: usize = 4096;

fn reserve(len: usize, prot: VmProt) -> *mut u8 {
    // SAFETY: placed anywhere, so it replaces nothing.
    unsafe { syscall::vm_reserve(len, prot, VmAt::ANYWHERE) }.unwrap_or_else(|e| panic!("reserve {len} bytes {prot:?}: {e:?}"))
}

fn fixed(at: *mut u8, len: usize) -> Result<*mut u8, SyscallError> {
    // SAFETY: every fixed range here lies over pages this test owns and
    // reaches through raw pointers only.
    unsafe { syscall::vm_reserve(len, VmProt::ReadWrite, VmAt::Fixed(at)) }
}

fn byte(p: *mut u8, page: usize) -> u8 {
    unsafe { p.add(page * PAGE).read_volatile() }
}

fn set(p: *mut u8, page: usize, value: u8) {
    unsafe { p.add(page * PAGE).write_volatile(value) }
}

/// The pages a child makes and touches, none of which allows the touch.
const FAULTING: [&str; 5] = ["reserved", "protected-none", "decommitted", "read-only-beside-a-run", "moved-from"];

fn main() {
    if let Some(case) = std::env::args().nth(1) {
        return touch(&case);
    }
    placement_and_release();
    protection();
    discard_and_decommit();
    ranges_are_bounded();
    resize();
    remap_keeps_memory();
    for case in FAULTING {
        faults(case);
    }
    println!("vm_reserve, vm_protect, vm_discard, vm_decommit, vm_remap and vm_release do what each says");
}

fn placement_and_release() {
    // No range may overlap another.
    let mut ranges: Vec<(*mut u8, usize)> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    for i in 0..64 {
        let pages = 1 + i % 5;
        let ptr = reserve(pages * PAGE, VmProt::ReadWrite);
        assert!(ptr as usize % PAGE == 0, "reserve #{i} returned {ptr:p}, off a page");
        for page in 0..pages {
            assert!(seen.insert(ptr as usize + page * PAGE), "reserve #{i}: page {page} overlaps an earlier range");
        }
        set(ptr, 0, i as u8);
        set(ptr, pages - 1, i as u8);
        ranges.push((ptr, pages));
    }
    for (i, &(ptr, pages)) in ranges.iter().enumerate() {
        assert_eq!((byte(ptr, 0), byte(ptr, pages - 1)), (i as u8, i as u8), "range #{i} lost a byte");
    }
    for (ptr, pages) in ranges {
        unsafe { syscall::vm_release(ptr, pages * PAGE) }.expect("release a whole range");
    }

    // A reservation costs no memory, so a tebibyte of one is placed; a
    // commitment is promised its frames, so a tebibyte of one is refused
    // when it is asked for, whether it is reserved committed or committed
    // later, and the reservation is as it was.
    let tib = reserve(1 << 40, VmProt::None);
    assert_eq!(
        unsafe { syscall::vm_reserve(1 << 40, VmProt::ReadWrite, VmAt::ANYWHERE) },
        Err(SyscallError::ResourceExhausted),
        "a committed tebibyte",
    );
    assert_eq!(
        unsafe { syscall::vm_protect(tib, 1 << 40, VmProt::ReadWrite) },
        Err(SyscallError::ResourceExhausted),
        "a tebibyte reservation committed",
    );
    unsafe { syscall::vm_protect(tib, PAGE, VmProt::ReadWrite) }.expect("commit one page of the reservation");
    set(tib, 0, 0x7E);
    unsafe { syscall::vm_release(tib, 1 << 40) }.expect("release the tebibyte");

    // Part of a range goes, and the rest stays as it was.
    let four = reserve(4 * PAGE, VmProt::ReadWrite);
    (0..4).for_each(|page| set(four, page, 0x10 + page as u8));
    unsafe { syscall::vm_release(four.add(PAGE), 2 * PAGE) }.expect("release the middle two pages");
    assert_eq!((byte(four, 0), byte(four, 3)), (0x10, 0x13), "releasing the middle took a neighbour");
    // The hole is free: a fixed range there is fresh zeroes.
    assert_eq!(fixed(unsafe { four.add(PAGE) }, 2 * PAGE), Ok(unsafe { four.add(PAGE) }), "a fixed range over the hole");
    assert_eq!((byte(four, 1), byte(four, 2)), (0, 0), "the hole held its old bytes");

    // Fixed over part of a range replaces those pages alone.
    set(four, 1, 0x21);
    assert_eq!(fixed(unsafe { four.add(PAGE) }, PAGE), Ok(unsafe { four.add(PAGE) }), "fixed over one page of a range");
    assert_eq!((byte(four, 0), byte(four, 1), byte(four, 3)), (0x10, 0, 0x13), "fixed replaced more than its page");

    // A run across ranges and holes goes whole, and a hole alone is no refusal.
    unsafe { syscall::vm_release(four, 4 * PAGE) }.expect("release across what is left");
    unsafe { syscall::vm_release(four, 4 * PAGE) }.expect("release of a run that holds nothing");

    // What release and fixed refuse: an address off a page, and no bytes.
    let one = reserve(PAGE, VmProt::ReadWrite);
    assert_eq!(unsafe { syscall::vm_release(one.add(1), PAGE) }, Err(SyscallError::InvalidArgument), "release off a page");
    assert_eq!(unsafe { syscall::vm_release(one, 0) }, Err(SyscallError::InvalidArgument), "release of no bytes");
    assert_eq!(fixed(unsafe { one.add(1) }, PAGE), Err(SyscallError::InvalidArgument), "fixed off a page");
    set(one, 0, 0x5A);
    assert_eq!(byte(one, 0), 0x5A, "a refused call touched the range");
    unsafe { syscall::vm_release(one, PAGE) }.expect("release one page");
    println!("  PASS: ranges are placed apart, and any run of pages is released or replaced");
}

fn protection() {
    // A reservation is committed by protecting it, a page of it if asked.
    let three = reserve(3 * PAGE, VmProt::None);
    unsafe { syscall::vm_protect(three.add(PAGE), PAGE, VmProt::ReadWrite) }.expect("commit the middle page");
    set(three, 1, 0x31);
    assert_eq!(byte(three, 1), 0x31, "a committed page lost its byte");

    // Read-only keeps the bytes, and so does None and back.
    unsafe { syscall::vm_protect(three.add(PAGE), PAGE, VmProt::Read) }.expect("make the page read-only");
    assert_eq!(byte(three, 1), 0x31, "read-only lost the page's byte");
    unsafe { syscall::vm_protect(three.add(PAGE), PAGE, VmProt::None) }.expect("make the page unreachable");
    unsafe { syscall::vm_protect(three.add(PAGE), PAGE, VmProt::ReadWrite) }.expect("make the page writable again");
    assert_eq!(byte(three, 1), 0x31, "None and back lost the page's byte");

    // What protect refuses, all or nothing: a page no range holds, and
    // executable memory, which no call maps yet.
    unsafe { syscall::vm_release(three.add(2 * PAGE), PAGE) }.expect("release the last page");
    assert_eq!(unsafe { syscall::vm_protect(three, 3 * PAGE, VmProt::Read) }, Err(SyscallError::BadAddress), "protect over a hole");
    assert_eq!(unsafe { syscall::vm_protect(three, PAGE, VmProt::ReadExec) }, Err(SyscallError::NotSupported), "protect executable");
    set(three, 1, 0x32);
    assert_eq!(byte(three, 1), 0x32, "a refused protect changed the committed page");
    unsafe { syscall::vm_release(three, 2 * PAGE) }.expect("release the rest");

    // A shared object's pages take no protection past what they were mapped
    // with: the clock page, one object mapped read-only into every process,
    // is not made writable for all of them by one.
    let clock = toyos_abi::clock::CLOCK_PAGE as *mut u8;
    assert_eq!(
        unsafe { syscall::vm_protect(clock, PAGE, VmProt::ReadWrite) },
        Err(SyscallError::PermissionDenied),
        "the clock page made writable",
    );
    // And the page still reads as the clock: `page` asserts its magic.
    let _ = toyos_abi::clock::page();
    println!("  PASS: protect commits reservations and changes protection with contents kept, all or nothing");
}

fn discard_and_decommit() {
    let two = reserve(2 * PAGE, VmProt::ReadWrite);
    set(two, 0, 0x41);
    set(two, 1, 0x42);
    unsafe { syscall::vm_discard(two, PAGE) }.expect("discard the first page");
    assert_eq!((byte(two, 0), byte(two, 1)), (0, 0x42), "discard took the wrong page or kept its bytes");
    set(two, 0, 0x43);
    assert_eq!(byte(two, 0), 0x43, "a discarded page is not writable again");

    unsafe { syscall::vm_decommit(two.add(PAGE), PAGE) }.expect("decommit the second page");
    unsafe { syscall::vm_protect(two.add(PAGE), PAGE, VmProt::ReadWrite) }.expect("commit it again");
    assert_eq!(byte(two, 1), 0, "a decommitted page came back with its old bytes");
    assert_eq!(byte(two, 0), 0x43, "decommitting a neighbour took a byte");

    assert_eq!(unsafe { syscall::vm_discard(two, 3 * PAGE) }, Err(SyscallError::BadAddress), "discard past the range");
    assert_eq!(byte(two, 0), 0x43, "a refused discard took a byte");
    unsafe { syscall::vm_release(two, 2 * PAGE) }.expect("release");
    println!("  PASS: discard reads back zeros in place, and decommit makes pages a reservation again");
}

/// Pages alternately reserved and readable never join, so each is one range
/// more until the next is refused; every one is still given back at the
/// bound, where releasing a whole range cuts none.
fn ranges_are_bounded() {
    let mut held = Vec::new();
    loop {
        let prot = if held.len() % 2 == 0 { VmProt::None } else { VmProt::Read };
        // SAFETY: placed anywhere, so it replaces nothing.
        match unsafe { syscall::vm_reserve(PAGE, prot, VmAt::ANYWHERE) } {
            Ok(p) => held.push(p),
            Err(e) => {
                assert_eq!(e, SyscallError::ResourceExhausted, "a reserve at {} ranges", held.len());
                break;
            }
        }
        assert!(held.len() <= MAX_RANGES, "{} ranges placed, past the bound of {MAX_RANGES}", held.len());
    }
    assert!(held.len() > MAX_RANGES - 64, "refused at {} ranges, well short of {MAX_RANGES}", held.len());
    for p in held {
        unsafe { syscall::vm_release(p, PAGE) }.expect("release a whole range at the bound");
    }
    let again = reserve(PAGE, VmProt::ReadWrite);
    unsafe { syscall::vm_release(again, PAGE) }.expect("release");
    println!("  PASS: a space holds at most {MAX_RANGES} ranges, and gives each back at the bound");
}

fn remap(p: *mut u8, old: usize, new: usize, may_move: bool) -> Result<*mut u8, SyscallError> {
    // SAFETY: every range remapped here is this test's own, reached through
    // raw pointers only, and through the answer alone once it moves.
    unsafe { syscall::vm_remap(p, old * PAGE, new * PAGE, may_move) }
}

/// `pages` committed pages followed by a read-only one of the same
/// reservation, its own range, so the first `pages` cannot grow in place.
fn blocked(pages: usize) -> *mut u8 {
    let p = reserve((pages + 1) * PAGE, VmProt::ReadWrite);
    unsafe { syscall::vm_protect(p.add(pages * PAGE), PAGE, VmProt::Read) }.expect("protect the blocking page");
    p
}

fn resize() {
    // Grown into the free pages after it: the same address, the bytes kept,
    // and a tail of zeros that takes writes.
    let p = reserve(8 * PAGE, VmProt::ReadWrite);
    unsafe { syscall::vm_release(p.add(4 * PAGE), 4 * PAGE) }.expect("free the last four pages");
    (0..4).for_each(|page| set(p, page, 0x50 + page as u8));
    assert_eq!(remap(p, 4, 8, false), Ok(p), "growth into free pages");
    assert_eq!((0..8).map(|page| byte(p, page)).collect::<Vec<_>>(), [0x50, 0x51, 0x52, 0x53, 0, 0, 0, 0], "grown in place");
    set(p, 7, 0x57);
    assert_eq!(byte(p, 7), 0x57, "the grown tail is not writable");

    // Shrunk: the tail goes and the rest stays.
    assert_eq!(remap(p, 8, 2, false), Ok(p), "a shrink");
    assert_eq!((byte(p, 0), byte(p, 1)), (0x50, 0x51), "a shrink took a kept page's byte");
    assert_eq!(unsafe { syscall::vm_protect(p, 3 * PAGE, VmProt::ReadWrite) }, Err(SyscallError::BadAddress), "a shrink kept its tail");
    unsafe { syscall::vm_release(p, 2 * PAGE) }.expect("release");

    // A neighbour in the way refuses a growth that may not move, and moves
    // one that may: every page's byte at the new place, nothing at the old,
    // the neighbour untouched. 768 pages cross tables of leaves and batches.
    const PAGES: usize = 768;
    let p = blocked(PAGES);
    (0..PAGES).step_by(3).for_each(|page| set(p, page, page as u8 | 1));
    assert_eq!(remap(p, PAGES, 2 * PAGES, false), Err(SyscallError::ResourceExhausted), "growth into a held page");
    assert_eq!(byte(p, 3), 3 | 1, "a refused growth touched the range");
    let q = remap(p, PAGES, 2 * PAGES, true).expect("a growth that may move");
    assert_ne!(q, p, "growth past a held page stayed in place");
    for page in 0..2 * PAGES {
        let want = if page < PAGES && page % 3 == 0 { page as u8 | 1 } else { 0 };
        assert_eq!(byte(q, page), want, "page {page} of the moved range");
    }
    set(q, 1, 0x61);
    assert_eq!(byte(q, 1), 0x61, "a moved page is not writable");
    assert_eq!(unsafe { syscall::vm_protect(p, PAGE, VmProt::ReadWrite) }, Err(SyscallError::BadAddress), "the old place is still held");
    assert_eq!(unsafe { syscall::vm_protect(p.add(PAGES * PAGE), PAGE, VmProt::Read) }, Ok(()), "the neighbour went");
    unsafe { syscall::vm_release(p.add(PAGES * PAGE), PAGE) }.expect("release the neighbour");
    unsafe { syscall::vm_release(q, 2 * PAGES * PAGE) }.expect("release the moved range");

    // What remap refuses: any memory but committed private pages of one
    // protection, a page no range holds, and a length of nothing.
    let none = reserve(2 * PAGE, VmProt::None);
    assert_eq!(remap(none, 2, 4, true), Err(SyscallError::InvalidArgument), "a reservation");
    unsafe { syscall::vm_release(none, 2 * PAGE) }.expect("release");
    let mixed = blocked(1);
    assert_eq!(remap(mixed, 2, 4, true), Err(SyscallError::InvalidArgument), "two protections");
    assert_eq!(remap(mixed, 1, 0, true), Err(SyscallError::InvalidArgument), "no bytes");
    assert_eq!(remap(unsafe { mixed.add(1) }, 1, 2, true), Err(SyscallError::InvalidArgument), "off a page");
    unsafe { syscall::vm_release(mixed, 2 * PAGE) }.expect("release");
    assert_eq!(remap(mixed, 1, 2, true), Err(SyscallError::BadAddress), "a page no range holds");
    let clock = toyos_abi::clock::CLOCK_PAGE as *mut u8;
    assert_eq!(remap(clock, 1, 2, true), Err(SyscallError::InvalidArgument), "a shared object's page");
    // `page` asserts the clock page's magic.
    let _ = toyos_abi::clock::page();
    println!("  PASS: remap grows in place or moves the pages, shrinks in place, and refuses all but committed private memory");
}

/// Bytes the machine has in use or promised.
fn used() -> u64 {
    let mut header = [0u8; syscall::SYSINFO_HEADER_SIZE];
    assert_eq!(toyos::system::sysinfo(&mut header), header.len(), "the sysinfo header");
    syscall::SysinfoHeader::decode(&header).memory_used
}

/// A written 16 MiB moved 32 times, each time grown by the page that becomes
/// the next move's neighbour, gives every frame and promise back: the
/// machine's memory in use after is what it was before, give or take what the
/// rest of it did meanwhile, where one move that kept its range's promise
/// would cost 16 MiB.
fn remap_keeps_memory() {
    const PAGES: usize = 4096;
    let before = used();
    let mut p = blocked(PAGES);
    (0..PAGES).for_each(|page| set(p, page, page as u8));
    for round in 0..32 {
        let q = remap(p, PAGES, PAGES + 1, true).unwrap_or_else(|e| panic!("move {round}: {e:?}"));
        assert_ne!(q, p, "move {round} stayed in place");
        unsafe { syscall::vm_release(p.add(PAGES * PAGE), PAGE) }.expect("release the old neighbour");
        unsafe { syscall::vm_protect(q.add(PAGES * PAGE), PAGE, VmProt::Read) }.expect("make the grown page the neighbour");
        p = q;
    }
    assert!((0..PAGES).all(|page| byte(p, page) == page as u8), "a page lost its byte over the moves");
    unsafe { syscall::vm_release(p, (PAGES + 1) * PAGE) }.expect("release the moved range");
    let after = used();
    assert!(after < before + 8 * (1 << 20), "moves kept memory: {before} bytes in use before, {after} after");
    println!("  PASS: a range moved 32 times gives back every frame and promise");
}

/// A child that makes the page `case` names and touches it must die of it.
fn faults(case: &str) {
    let out = Command::new("/system/bin/test_rs_mmap_stress")
        .arg(case)
        .output()
        .unwrap_or_else(|e| panic!("spawn the {case} child: {e}"));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("armed"), "the {case} child never reached the touch:\n{said}");
    assert!(!said.contains("SURVIVED"), "a {case} page allowed the touch:\n{said}");
    assert!(!out.status.success(), "a touch of a {case} page did not end the child");
    println!("  PASS: a touch of a {case} page ends the process");
}

/// Make the page `case` names, then touch it as its range does not allow: a
/// load from an unreachable page, written once where the case has one, or a
/// write to a read-only one.
fn touch(case: &str) {
    let page = match case {
        "reserved" => reserve(PAGE, VmProt::None),
        "read-only-beside-a-run" => {
            // A run of 16 pages whose last half is read-only, its first half
            // written in order: populating the run populates no page of the
            // read-only range as writable.
            // SAFETY: placed anywhere, so it replaces nothing.
            let run = unsafe { syscall::vm_reserve(16 * PAGE, VmProt::ReadWrite, VmAt::Anywhere { align: 16 * PAGE }) }.expect("reserve");
            unsafe { syscall::vm_protect(run.add(8 * PAGE), 8 * PAGE, VmProt::Read) }.expect("protect the last half");
            (0..8).for_each(|page| set(run, page, 1));
            assert_eq!(byte(run, 8), 0, "a read-only page read other than zero");
            println!("armed at {run:p}");
            set(run, 8, 1);
            println!("SURVIVED, wrote a read-only page");
            return;
        }
        "moved-from" => {
            // Written, then moved past its neighbour: the old place is gone.
            let page = blocked(1);
            set(page, 0, 0x77);
            assert_ne!(remap(page, 1, 2, true).expect("move"), page, "a growth past a held page stayed");
            page
        }
        "protected-none" | "decommitted" => {
            let page = reserve(PAGE, VmProt::ReadWrite);
            set(page, 0, 0x77);
            let done = if case == "decommitted" {
                unsafe { syscall::vm_decommit(page, PAGE) }
            } else {
                unsafe { syscall::vm_protect(page, PAGE, VmProt::None) }
            };
            done.unwrap_or_else(|e| panic!("{case}: {e:?}"));
            page
        }
        other => panic!("no case {other}"),
    };
    println!("armed at {page:p}");
    let v = byte(page, 0);
    println!("SURVIVED, read {v:#x}");
}
