//! A process's own memory over 4 KiB pages, through the `vm_*` calls: no two
//! ranges overlap; `vm_release` takes away any run of pages — part of a
//! range, across two, or a hole — and a fixed reserve replaces whatever the
//! pages it names held; `vm_protect` commits a reservation and changes
//! protection in place, contents kept; `vm_discard` keeps the pages and reads
//! back zeros; `vm_decommit` makes them a reservation again. The ranges one
//! space holds are bounded.
//!
//! A page left unreachable must fault, which ends the process, so each such
//! case runs in a child that makes the page itself and then loads from it.

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

/// The unreachable pages a child makes and loads from.
const UNREACHABLE: [&str; 3] = ["reserved", "protected-none", "decommitted"];

fn main() {
    if let Some(case) = std::env::args().nth(1) {
        return load_from(&case);
    }
    placement_and_release();
    protection();
    discard_and_decommit();
    ranges_are_bounded();
    for case in UNREACHABLE {
        faults(case);
    }
    println!("vm_reserve, vm_protect, vm_discard, vm_decommit and vm_release do what each says");
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

    // A reservation costs no memory, so a tebibyte of one is placed.
    let tib = reserve(1 << 40, VmProt::None);
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

/// A child that makes the page `case` names and loads from it must die of it.
fn faults(case: &str) {
    let out = Command::new("/system/bin/test_rs_mmap_stress")
        .arg(case)
        .output()
        .unwrap_or_else(|e| panic!("spawn the {case} child: {e}"));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("armed"), "the {case} child never reached the load:\n{said}");
    assert!(!said.contains("SURVIVED"), "a {case} page was readable:\n{said}");
    assert!(!out.status.success(), "a load from a {case} page did not end the child");
    println!("  PASS: a load from a {case} page ends the process");
}

/// Make a page, written once where the case has one, unreachable as `case`
/// says, then load from it.
fn load_from(case: &str) {
    let page = match case {
        "reserved" => reserve(PAGE, VmProt::None),
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
