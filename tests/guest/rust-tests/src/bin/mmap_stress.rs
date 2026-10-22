//! `mmap` and `munmap` as POSIX states them, over 4 KiB pages: no two
//! mappings overlap, `munmap` takes away any run of pages — part of a mapping,
//! across two, or a hole — and `MAP_FIXED` replaces whatever the pages it names
//! held while leaving the rest of each mapping as it was; and the ranges one
//! space holds are bounded.

use std::collections::HashSet;

use toyos_abi::syscall::{mmap, munmap, MmapFlags, MmapProt, SyscallError, MAX_RANGES};

const PAGE: usize = 4096;
const RW: MmapProt = MmapProt(MmapProt::READ.0 | MmapProt::WRITE.0);
const ANON: MmapFlags = MmapFlags(MmapFlags::ANONYMOUS.0 | MmapFlags::PRIVATE.0);

fn map(at: *mut u8, len: usize, flags: MmapFlags) -> *mut u8 {
    unsafe { mmap(at, len, RW, flags) }
}

fn byte(p: *mut u8, page: usize) -> u8 {
    unsafe { p.add(page * PAGE).read_volatile() }
}

fn set(p: *mut u8, page: usize, value: u8) {
    unsafe { p.add(page * PAGE).write_volatile(value) }
}

fn main() {
    // No mapping may overlap another.
    let mut regions: Vec<(*mut u8, usize)> = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    for i in 0..64 {
        let pages = 1 + i % 5;
        let ptr = map(core::ptr::null_mut(), pages * PAGE, ANON);
        assert!(!ptr.is_null(), "mmap #{i} of {pages} pages failed");
        assert!(ptr as usize % PAGE == 0, "mmap #{i} returned {ptr:p}, off a page");
        for page in 0..pages {
            assert!(seen.insert(ptr as usize + page * PAGE), "mmap #{i}: page {page} overlaps an earlier mapping");
        }
        set(ptr, 0, i as u8);
        set(ptr, pages - 1, i as u8);
        regions.push((ptr, pages));
    }
    for (i, &(ptr, pages)) in regions.iter().enumerate() {
        assert_eq!((byte(ptr, 0), byte(ptr, pages - 1)), (i as u8, i as u8), "mapping #{i} lost a byte");
    }
    for (ptr, pages) in regions {
        unsafe { munmap(ptr, pages * PAGE) }.expect("munmap a whole mapping");
    }

    // Part of a mapping goes, and the rest stays as it was.
    let four = map(core::ptr::null_mut(), 4 * PAGE, ANON);
    assert!(!four.is_null(), "mmap four pages");
    (0..4).for_each(|page| set(four, page, 0x10 + page as u8));
    unsafe { munmap(four.add(PAGE), 2 * PAGE) }.expect("munmap the middle two pages");
    assert_eq!((byte(four, 0), byte(four, 3)), (0x10, 0x13), "unmapping the middle took a neighbour");
    // The hole is free: a FIXED mapping there is fresh zeroes.
    let refilled = map(unsafe { four.add(PAGE) }, 2 * PAGE, ANON | MmapFlags::FIXED);
    assert_eq!(refilled, unsafe { four.add(PAGE) }, "a FIXED mapping over the hole");
    assert_eq!((byte(four, 1), byte(four, 2)), (0, 0), "the hole held its old bytes");

    // FIXED over part of a mapping replaces those pages alone.
    set(four, 1, 0x21);
    let over = map(unsafe { four.add(PAGE) }, PAGE, ANON | MmapFlags::FIXED);
    assert_eq!(over, unsafe { four.add(PAGE) }, "FIXED over one page of a mapping");
    assert_eq!((byte(four, 0), byte(four, 1), byte(four, 3)), (0x10, 0, 0x13), "FIXED replaced more than its page");

    // A range across mappings and holes goes whole, and a hole alone is no refusal.
    unsafe { munmap(four, 4 * PAGE) }.expect("munmap across what is left");
    unsafe { munmap(four, 4 * PAGE) }.expect("munmap of a range that holds nothing");

    // What munmap and FIXED refuse: an address off a page, and no bytes.
    let one = map(core::ptr::null_mut(), PAGE, ANON);
    assert!(!one.is_null(), "mmap one page");
    assert_eq!(unsafe { munmap(one.add(1), PAGE) }, Err(SyscallError::InvalidArgument), "munmap off a page");
    assert_eq!(unsafe { munmap(one, 0) }, Err(SyscallError::InvalidArgument), "munmap of no bytes");
    assert!(map(unsafe { one.add(1) }, PAGE, ANON | MmapFlags::FIXED).is_null(), "FIXED off a page");
    set(one, 0, 0x5A);
    assert_eq!(byte(one, 0), 0x5A, "a refused call touched the mapping");

    // A range given back is placed again.
    unsafe { munmap(one, PAGE) }.expect("munmap one page");
    let again = map(core::ptr::null_mut(), PAGE, ANON);
    assert!(!again.is_null(), "mmap after an munmap");

    // A space holds a bounded number of ranges: pages alternately reserved
    // and readable never join, so each is one range more until the next is
    // refused, and every one is still given back at the bound.
    let mut held = Vec::new();
    loop {
        let prot = if held.len() % 2 == 0 { MmapProt::NONE } else { MmapProt::READ };
        let p = unsafe { mmap(core::ptr::null_mut(), PAGE, prot, ANON) };
        if p.is_null() {
            break;
        }
        held.push(p);
        assert!(held.len() <= MAX_RANGES, "{} ranges placed, past the bound of {MAX_RANGES}", held.len());
    }
    assert!(held.len() > MAX_RANGES - 64, "refused at {} ranges, well short of {MAX_RANGES}", held.len());
    for p in held {
        unsafe { munmap(p, PAGE) }.expect("munmap a whole range at the bound");
    }
    assert!(!map(core::ptr::null_mut(), PAGE, ANON).is_null(), "mmap after the ranges went back");

    println!("mmap and munmap take and give back any run of pages, up to a bound of ranges");
}
