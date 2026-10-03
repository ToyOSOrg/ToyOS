//! A process's count of mapped regions must not grow without bound.
//!
//! Every `mmap` registers a region in the address space and an `MmapRegion` in
//! the process's `mmap_regions` ledger (`kernel/src/syscall/vm.rs`). A
//! `PROT_NONE` mapping pins no physical page, so a loop of them costs the
//! process nothing and the kernel one 40-byte record each. Nothing but the
//! placement window bounded the count: past 32,768 records the `mmap_regions`
//! `Vec` doubled to 65,536, a 2,621,440-byte allocation past the kernel heap's
//! single-allocation ceiling (`mm::MAX_HEAP_ALLOC`, 2,093,056), where the
//! allocator asserts — a kernel panic from one unprivileged process.
//!
//! The bound belongs on the region ledger, not on `mmap` alone: the same
//! ledger backs `dlopen` and a thread's TLS block. So this drives the cheapest
//! grower, `mmap(PROT_NONE)`, and asserts the kernel refuses by name before the
//! ledger can cross the ceiling, and stays alive and allocating afterwards.
//!
//! Roughly thirty-three thousand syscalls from plain std, no crafted ELF, no
//! large-RAM guest (a `PROT_NONE` reservation owns no memory). Before the fix
//! this panics the kernel; after it, the mapping past the bound is a clean
//! `ResourceExhausted` and the count never crosses it.

use toyos_abi::syscall::{mmap, munmap, MmapFlags, MmapProt};

/// `kernel/src/vma.rs`'s `MAX_REGIONS`, by value. The assertions below are
/// against the bound's *consequences* — a refusal near this count, and a live
/// kernel — not against the number, so moving it there does not make this test
/// vacuous; the band only needs to know roughly where the wall is.
const MAX_REGIONS: usize = 32_768;

const PAGE_2M: usize = 2 * 1024 * 1024;

/// How far below `MAX_REGIONS` the refusal may land and still be the region
/// cap: the process's own regions (its ELF segments, stack, clock page and TLS
/// block) take a few dozen slots the `PROT_NONE` loop never gets. Generous, so
/// a handful more never makes the test flaky; still tight enough that an early
/// spurious refusal — the shape that would also "pass" against a kernel that
/// refused everything — fails it.
const SLACK: usize = 4096;

/// Map one `PROT_NONE` region, or `None` when the kernel refused. A refusal is
/// a null return (`mmap`'s wrapper collapses every error to null, and no valid
/// mapping is at address 0 — the window floor is far above it), never a dead
/// kernel: on the defect the kernel panics instead of returning here, which is
/// what the harness reads as a guest that died.
fn map_none() -> Option<*mut u8> {
    let p = unsafe {
        mmap(
            core::ptr::null_mut(),
            PAGE_2M,
            MmapProt::NONE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    (!p.is_null()).then_some(p)
}

fn main() {
    // A fixed-size store, so the loop's own bookkeeping adds no heap region
    // that would perturb the count it is measuring. These are the first (and so
    // highest) reservations; freeing them reopens the top of the window.
    const FREED: usize = 32;
    let mut top: [*mut u8; FREED] = [core::ptr::null_mut(); FREED];

    let mut count = 0usize;
    let mut refused = false;
    // One attempt past the bound is enough to be refused; the margin only keeps
    // a few dozen process-owned regions from making the loop stop one short.
    for _ in 0..(MAX_REGIONS + 64) {
        match map_none() {
            Some(addr) => {
                if count < FREED {
                    top[count] = addr;
                }
                count += 1;
            }
            None => {
                refused = true;
                break;
            }
        }
    }

    // The whole point: the kernel said no rather than dying. On the defect the
    // loop never reaches a refusal — the kernel panics mid-loop and this line
    // is never printed.
    assert!(
        refused,
        "the region count was never bounded: {count} PROT_NONE mappings all succeeded",
    );
    // Below the documented cap (the process already holds a few non-mmap
    // regions), and near it (so the refusal is the region cap and not an early
    // failure a broken kernel would also give).
    assert!(
        count < MAX_REGIONS && count >= MAX_REGIONS - SLACK,
        "refused after {count} mappings; expected within {SLACK} below MAX_REGIONS {MAX_REGIONS}",
    );

    // The cap is a live limit, not a latched failure, and the two ledgers agree:
    // freeing regions reopens placement. A kernel that refused permanently, or
    // whose address-space ledger disagreed with `mmap_regions`, fails here.
    for &addr in &top {
        unsafe { munmap(addr, PAGE_2M) }.expect("a PROT_NONE region could not be freed");
    }

    // A real RW mapping now fits where a reservation was freed: this allocates
    // physical pages and maps them, so a dead heap or PMM shows here. Written
    // and read back, because a mapping that cannot be touched is not one.
    let live = unsafe {
        mmap(
            core::ptr::null_mut(),
            PAGE_2M,
            MmapProt::READ | MmapProt::WRITE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!live.is_null(), "no RW mapping fit after freeing {FREED} reservations");
    unsafe {
        live.write_volatile(0x5A);
        live.add(PAGE_2M - 1).write_volatile(0xA5);
        assert_eq!(live.read_volatile(), 0x5A, "the live mapping's first byte did not stick");
        assert_eq!(
            live.add(PAGE_2M - 1).read_volatile(),
            0xA5,
            "the live mapping's last byte did not stick",
        );
    }
    unsafe { munmap(live, PAGE_2M) }.expect("the live mapping could not be freed");

    // And a fresh reservation is admitted again, in a slot the frees reopened:
    // the bound tracks the current count, it does not remember having refused.
    let again = map_none().expect("a PROT_NONE mapping was refused after room was freed");
    unsafe { munmap(again, PAGE_2M) }.expect("the reopened reservation could not be freed");

    // The kernel heap is intact: the defect's signature is a panic taken inside
    // the allocator's lock, so "the kernel is still allocating" is the claim
    // that matters, and the guest on the other end is proof it never paniced.
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    for i in 0..64 {
        blocks.push(vec![(i % 251) as u8; 4096]);
    }
    for (i, b) in blocks.iter().enumerate() {
        assert!(b.iter().all(|&x| x == (i % 251) as u8), "kernel heap corrupted block {i}");
    }

    println!("mmap region count bounded: refused at {count} PROT_NONE mappings, kernel alive");
}
