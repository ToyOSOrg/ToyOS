//! Which leaves map a run of physical pages into a process: a 2 MiB leaf
//! wherever a whole aligned 2 MiB of the run lands on a 2 MiB boundary, and a
//! table of 4 KiB leaves for each 2 MiB slot of the rest.

use crate::span::{PAGE_2M, PAGE_4K};

/// What maps one 2 MiB slot at `va`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leaf {
    /// One 2 MiB leaf over the frame at `pa`.
    Block { va: u64, pa: u64 },
    /// `count` 4 KiB leaves from `pa`, the slot's first `count` pages.
    Pages { va: u64, pa: u64, count: u64 },
}

/// The leaves that map `size` bytes of `pa` at `va`: `va` on a 2 MiB
/// boundary, `pa` and `size` whole 4 KiB pages.
pub fn leaves(va: u64, pa: u64, size: u64) -> impl Iterator<Item = Leaf> {
    assert!(va.is_multiple_of(PAGE_2M), "leaves: {va:#x} is not a 2 MiB boundary");
    assert!(
        pa.is_multiple_of(PAGE_4K) && size.is_multiple_of(PAGE_4K),
        "leaves: {size:#x} bytes at {pa:#x} are not whole 4 KiB pages"
    );
    (0..size.div_ceil(PAGE_2M)).map(move |slot| {
        let offset = slot * PAGE_2M;
        let (va, pa) = (va + offset, pa + offset);
        let bytes = (size - offset).min(PAGE_2M);
        if pa.is_multiple_of(PAGE_2M) && bytes == PAGE_2M {
            Leaf::Block { va, pa }
        } else {
            Leaf::Pages { va, pa, count: bytes / PAGE_4K }
        }
    })
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    const VA: u64 = 0x4_0000_0000;

    fn of(pa: u64, size: u64) -> Vec<Leaf> {
        leaves(VA, pa, size).collect()
    }

    #[test]
    fn a_device_window_is_its_own_pages_and_no_more() {
        // The T14 NVMe's 16 KiB at 0xbce00000, on a 2 MiB boundary: never the
        // 2 MiB leaf there, which would hand over whatever decodes past it.
        assert_eq!(of(0xbce0_0000, 0x4000), [Leaf::Pages { va: VA, pa: 0xbce0_0000, count: 4 }]);
        // The AX210's 16 KiB at 0xbc300000, 1 MiB off one.
        assert_eq!(of(0xbc30_0000, 0x4000), [Leaf::Pages { va: VA, pa: 0xbc30_0000, count: 4 }]);
        // A page-sized BAR off any 2 MiB boundary.
        assert_eq!(of(0xd04c_c000, PAGE_4K), [Leaf::Pages { va: VA, pa: 0xd04c_c000, count: 1 }]);
    }

    #[test]
    fn whole_aligned_frames_are_2_mib_leaves() {
        assert_eq!(
            of(0x20_0000, 2 * PAGE_2M),
            [Leaf::Block { va: VA, pa: 0x20_0000 }, Leaf::Block { va: VA + PAGE_2M, pa: 0x40_0000 }]
        );
        // The tail past the last whole frame is pages.
        assert_eq!(
            of(0x20_0000, PAGE_2M + 0x4000),
            [Leaf::Block { va: VA, pa: 0x20_0000 }, Leaf::Pages { va: VA + PAGE_2M, pa: 0x40_0000, count: 4 }]
        );
    }

    #[test]
    fn a_run_off_a_2_mib_boundary_is_pages_in_every_slot() {
        assert_eq!(
            of(0x6_1000, 2 * PAGE_2M),
            [
                Leaf::Pages { va: VA, pa: 0x6_1000, count: 512 },
                Leaf::Pages { va: VA + PAGE_2M, pa: 0x6_1000 + PAGE_2M, count: 512 },
            ]
        );
    }

    #[test]
    fn nothing_maps_nothing() {
        assert_eq!(of(0x20_0000, 0), []);
    }
}
