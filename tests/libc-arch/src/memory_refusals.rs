//! What libc's memory calls refuse before the kernel or the allocator is
//! asked: `mmap` a file, executable memory, no bytes, a fixed place off a
//! page, or no one disposition; `posix_madvise` anything but its five; `madvise` the same, an
//! address off a page, and Linux's discard.

use crate::memreq::{self, MapRefusal, MAP_ANONYMOUS, MAP_FIXED, MAP_PRIVATE, MAP_SHARED, PROT_EXEC, PROT_READ, PROT_WRITE};

#[test]
fn mmap_refuses_what_the_kernel_cannot_map_and_nothing_else() {
    let rw = PROT_READ | PROT_WRITE;
    let anonymous = MAP_PRIVATE | MAP_ANONYMOUS;
    for (addr, len, prot, flags, want) in [
        (0, 4096, rw, anonymous, None),
        (0, 1, PROT_READ, MAP_SHARED | MAP_ANONYMOUS, None),
        (0, 4096, 0, anonymous, None),
        (0x4000_0000, 4096, rw, anonymous | MAP_FIXED, None),
        // Off a page, which only a fixed place has to be on.
        (0x4000_0001, 4096, rw, anonymous, None),
        (0x4000_0001, 4096, rw, anonymous | MAP_FIXED, Some(MapRefusal::Invalid)),
        (0, 0, rw, anonymous, Some(MapRefusal::Invalid)),
        // Neither disposition, and both.
        (0, 4096, rw, MAP_ANONYMOUS, Some(MapRefusal::Invalid)),
        (0, 4096, rw, MAP_SHARED | MAP_PRIVATE | MAP_ANONYMOUS, Some(MapRefusal::Invalid)),
        (0, 4096, PROT_READ, 0, Some(MapRefusal::Invalid)),
        (0, 4096, PROT_READ, MAP_PRIVATE, Some(MapRefusal::File)),
        (0, 4096, rw, MAP_SHARED, Some(MapRefusal::File)),
        (0, 4096, PROT_READ | PROT_EXEC, anonymous, Some(MapRefusal::Exec)),
    ] {
        assert_eq!(memreq::mmap_refusal(addr, len, prot, flags), want, "mmap({addr:#x}, {len}, {prot:#x}, {flags:#x})");
    }
}

/// The kernel unmaps a mapping only by the length it was asked to map, so
/// `munmap` names the mapping exactly when both lengths are the same pages.
#[test]
fn a_length_names_the_pages_it_reaches_into() {
    for (len, want) in [
        (0, None),
        (1, Some(4096)),
        (4095, Some(4096)),
        (4096, Some(4096)),
        (4097, Some(8192)),
        (8192, Some(8192)),
        (usize::MAX - 4095, Some(usize::MAX - 4095)),
        (usize::MAX - 4094, None),
        (usize::MAX, None),
    ] {
        assert_eq!(memreq::whole_pages(len), want, "{len:#x}");
    }
    // Two pages mapped: their first page alone is another length, and so is a
    // byte past them; a length ending anywhere in the second page is theirs.
    let mapped = memreq::whole_pages(8192);
    for (len, whole) in [(4096, false), (4097, true), (8000, true), (8192, true), (8193, false)] {
        assert_eq!(memreq::whole_pages(len) == mapped, whole, "munmap of {len} bytes of an 8192-byte mapping");
    }
    // And a mapping asked for by a length off a page is named by its pages.
    assert_eq!(memreq::whole_pages(5000), memreq::whole_pages(8192));
}

#[test]
fn posix_madvise_takes_its_five_and_only_them() {
    let taken: Vec<i32> = (-2..8).filter(|&a| memreq::is_advice(a)).collect();
    assert_eq!(taken, [0, 1, 2, 3, 4]);
}

#[test]
fn madvise_takes_linuxs_hints_and_refuses_its_discard() {
    use memreq::AdviceRefusal::{Discard, Invalid};
    let page = 0x4000_0000;
    for (addr, advice, want) in [
        (page, 0, None),
        (page, 1, None),
        (page, 2, None),
        (page, 3, None),
        (page, 4, Some(Discard)),
        (page, 5, Some(Invalid)),
        (page, -1, Some(Invalid)),
        (page + 1, 3, Some(Invalid)),
    ] {
        assert_eq!(memreq::madvise_refusal(addr, advice), want, "madvise({addr:#x}, {advice})");
    }
}
