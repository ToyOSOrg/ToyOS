//! What libc's memory calls refuse before the kernel or the allocator is
//! asked: `mmap` a file, executable memory, no bytes, or a fixed place off a
//! page; `posix_madvise` anything but its five; `madvise` the same, an
//! address off a page, and Linux's discard.

use crate::memreq::{self, MapRefusal, MAP_ANONYMOUS, MAP_FIXED, PROT_EXEC, PROT_READ, PROT_WRITE};

const MAP_SHARED: i32 = 0x01;
const MAP_PRIVATE: i32 = 0x02;

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
        (0, 4096, PROT_READ, MAP_PRIVATE, Some(MapRefusal::File)),
        (0, 4096, rw, MAP_SHARED, Some(MapRefusal::File)),
        (0, 4096, PROT_READ | PROT_EXEC, anonymous, Some(MapRefusal::Exec)),
    ] {
        assert_eq!(memreq::mmap_refusal(addr, len, prot, flags), want, "mmap({addr:#x}, {len}, {prot:#x}, {flags:#x})");
    }
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
