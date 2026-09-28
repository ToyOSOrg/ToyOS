//! Where a user window's bytes are in physical memory: one [`Segment`] per
//! physically contiguous run, in address order, and each copy cut at their
//! seams.
//!
//! **A window is asked at every 4 KiB page, whatever leaf maps it.** No leaf
//! is smaller, a split window grants writes page by page, and two neighbouring
//! pages sit in whichever frames the PMM handed out, in either order. A page
//! that is absent or does not grant the access refuses the whole window: the
//! kernel copies nothing through a window it could not wholly place.

use crate::span::{in_user_half, PAGE_4K};

/// `len` bytes of a user window, physically contiguous from `phys`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
    pub phys: u64,
    pub len: u64,
}

/// Walks `[start, start + len)` and hands `emit` each maximal physically
/// contiguous run of it, in order. `leaf` is the page walk: the physical
/// address one user address translates to where it grants the access asked,
/// and `None` where it does not.
///
/// `None` where the range is empty, leaves the user half, or holds a page
/// `leaf` refuses — and runs before the refused page may already have been
/// emitted, so nothing emitted is acted on until this returns `Some`.
pub fn segments(
    start: u64,
    len: u64,
    mut leaf: impl FnMut(u64) -> Option<u64>,
    mut emit: impl FnMut(Segment),
) -> Option<()> {
    if len == 0 || !in_user_half(start, len) {
        return None;
    }
    let end = start + len;
    let mut run = Segment { phys: leaf(start)?, len: 0 };
    let mut at = start;
    while at < end {
        let phys = if at == start { run.phys } else { leaf(at)? };
        let next = ((at & !(PAGE_4K - 1)) + PAGE_4K).min(end);
        if run.phys + run.len != phys {
            emit(run);
            run = Segment { phys, len: 0 };
        }
        run.len += next - at;
        at = next;
    }
    emit(run);
    Some(())
}

/// Hands `copy` each physically contiguous piece of bytes `[off, off + len)`
/// of the window `segs` lays out, as `(phys, at, n)`: the `n` bytes at `phys`
/// are bytes `at..at + n` of that range.
///
/// Panics where the range runs past the window's end: the kernel's own
/// arithmetic, bounded before it gets here, never a user length.
pub fn pieces(segs: &[Segment], off: u64, len: u64, mut copy: impl FnMut(u64, u64, u64)) {
    let mut skip = off;
    let mut done = 0;
    for seg in segs {
        if done == len {
            break;
        }
        if skip >= seg.len {
            skip -= seg.len;
            continue;
        }
        let n = (seg.len - skip).min(len - done);
        copy(seg.phys + skip, done, n);
        skip = 0;
        done += n;
    }
    assert!(done == len, "pieces: {off}+{len} runs {} bytes past the window", len - done);
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;
    use std::vec::Vec;

    use super::*;
    use crate::span::{Access, PAGE_2M, USER_TOP};

    /// Where the fake user range starts: a 2 MiB boundary, so a range at the
    /// top of one fake page and the bottom of the next straddles one too.
    const VBASE: u64 = 5 * PAGE_2M;
    /// Pages in the fake range: two 2 MiB pages' worth would be 1024, and
    /// every property below is about seams, so a few dozen are enough.
    const PAGES: u64 = 48;
    /// Where fake physical memory starts: nonzero, so a phys of 0 is a bug.
    const PBASE: u64 = 0x10_0000_0000;

    /// A deterministic shuffle: xorshift64 driving Fisher-Yates.
    fn shuffled(n: usize, mut seed: u64) -> Vec<u64> {
        let mut order: Vec<u64> = (0..n as u64).collect();
        for i in (1..n).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            order.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        order
    }

    /// A process's pages over fake physical memory: page `i` of the range
    /// lives in frame `frame[i]`, and `ro` pages grant no write.
    struct Space {
        frame: Vec<u64>,
        ro: Vec<u64>,
        absent: Vec<u64>,
        phys: Vec<u8>,
    }

    impl Space {
        /// Every page present and writable, frames in `order`, and each user
        /// byte holding a value its virtual address decides.
        fn new(order: Vec<u64>) -> Self {
            let mut space = Space {
                frame: order,
                ro: Vec::new(),
                absent: Vec::new(),
                phys: vec![0; (PAGES * PAGE_4K) as usize],
            };
            for v in VBASE..VBASE + PAGES * PAGE_4K {
                let p = space.leaf(v, Access::Read).expect("every page is present");
                space.phys[(p - PBASE) as usize] = byte_at(v);
            }
            space
        }

        fn leaf(&self, v: u64, access: Access) -> Option<u64> {
            if !(VBASE..VBASE + PAGES * PAGE_4K).contains(&v) {
                return None;
            }
            let page = (v - VBASE) / PAGE_4K;
            if self.absent.contains(&page) || (access == Access::Write && self.ro.contains(&page)) {
                return None;
            }
            Some(PBASE + self.frame[page as usize] * PAGE_4K + v % PAGE_4K)
        }

        fn window(&self, start: u64, len: u64, access: Access) -> Option<Vec<Segment>> {
            let mut segs = Vec::new();
            segments(start, len, |at| self.leaf(at, access), |s| segs.push(s))?;
            Some(segs)
        }

        /// The kernel's `read_at`, over fake memory.
        fn read(&self, segs: &[Segment], off: u64, len: u64) -> Vec<u8> {
            let mut out = vec![0u8; len as usize];
            pieces(segs, off, len, |phys, at, n| {
                let p = (phys - PBASE) as usize;
                out[at as usize..(at + n) as usize].copy_from_slice(&self.phys[p..p + n as usize]);
            });
            out
        }

        /// The kernel's `write_at`, over fake memory.
        fn write(&mut self, segs: &[Segment], off: u64, src: &[u8]) {
            let phys = &mut self.phys;
            pieces(segs, off, src.len() as u64, |p, at, n| {
                let p = (p - PBASE) as usize;
                phys[p..p + n as usize].copy_from_slice(&src[at as usize..(at + n) as usize]);
            });
        }

        /// What a ring 3 load at `v` sees.
        fn load(&self, v: u64) -> u8 {
            self.phys[(self.leaf(v, Access::Read).unwrap() - PBASE) as usize]
        }
    }

    fn byte_at(v: u64) -> u8 {
        (v.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8
    }

    /// Ranges that start and end on every kind of seam: at a page start, one
    /// byte either side of one, mid-page, and across many pages.
    fn ranges() -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        for first in [0, 1, 7, 13] {
            for head in [0, 1, PAGE_4K / 2, PAGE_4K - 1] {
                let start = VBASE + first * PAGE_4K + head;
                for len in [1, 2, PAGE_4K - head, PAGE_4K, PAGE_4K + 1, 3 * PAGE_4K - 5, 20 * PAGE_4K + 3] {
                    if start + len <= VBASE + PAGES * PAGE_4K {
                        out.push((start, len));
                    }
                }
            }
        }
        out.push((VBASE, PAGES * PAGE_4K));
        out
    }

    #[test]
    fn a_read_across_shuffled_frames_gets_the_bytes_at_their_addresses() {
        for seed in [1, 0x5eed, 0xdead_beef, 42] {
            let space = Space::new(shuffled(PAGES as usize, seed));
            for (start, len) in ranges() {
                let segs = space.window(start, len, Access::Read).expect("every page is readable");
                let want: Vec<u8> = (start..start + len).map(byte_at).collect();
                assert_eq!(space.read(&segs, 0, len), want, "seed {seed:#x}: [{start:#x}, +{len:#x})");
                // A view from any offset inside the window: `UserBytes::sub`.
                for off in [1, PAGE_4K - 1, PAGE_4K, len / 2] {
                    if off < len {
                        let tail = len - off;
                        assert_eq!(space.read(&segs, off, tail), want[off as usize..], "seed {seed:#x}: +{off:#x}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_write_across_shuffled_frames_lands_where_a_ring_3_load_reads_it() {
        for seed in [3, 0xabcdef, 99] {
            for (start, len) in ranges() {
                let mut space = Space::new(shuffled(PAGES as usize, seed));
                let segs = space.window(start, len, Access::Write).expect("every page is writable");
                let src: Vec<u8> = (0..len).map(|i| !byte_at(i ^ seed)).collect();
                space.write(&segs, 0, &src);
                for v in VBASE..VBASE + PAGES * PAGE_4K {
                    let want = if (start..start + len).contains(&v) { src[(v - start) as usize] } else { byte_at(v) };
                    assert_eq!(space.load(v), want, "seed {seed:#x}: [{start:#x}, +{len:#x}) at {v:#x}");
                }
            }
        }
    }

    #[test]
    fn the_segments_are_the_maximal_runs_and_cover_the_window_exactly() {
        for seed in [7, 0x1234_5678] {
            let space = Space::new(shuffled(PAGES as usize, seed));
            for (start, len) in ranges() {
                let segs = space.window(start, len, Access::Read).unwrap();
                assert_eq!(segs.iter().map(|s| s.len).sum::<u64>(), len);
                assert_eq!(segs[0].phys, space.leaf(start, Access::Read).unwrap());
                for pair in segs.windows(2) {
                    assert_ne!(pair[0].phys + pair[0].len, pair[1].phys, "two runs that follow were not joined");
                }
            }
        }
    }

    /// A 2 MiB leaf, or a split window over one frame, is every page in order:
    /// one segment, which is one pin and one copy as before the walk was per page.
    #[test]
    fn frames_in_order_are_one_segment() {
        let space = Space::new((0..PAGES).collect());
        for (start, len) in ranges() {
            let segs = space.window(start, len, Access::Write).unwrap();
            assert_eq!(segs, [Segment { phys: space.leaf(start, Access::Read).unwrap(), len }]);
        }
    }

    /// Frames in reverse: every page seam is a segment seam.
    #[test]
    fn frames_in_reverse_are_one_segment_per_page() {
        let space = Space::new((0..PAGES).rev().collect());
        let segs = space.window(VBASE + PAGE_4K - 3, PAGE_4K + 6, Access::Read).unwrap();
        assert_eq!(
            segs.iter().map(|s| s.len).collect::<Vec<_>>(),
            [3, PAGE_4K, 3],
            "{segs:x?}"
        );
    }

    #[test]
    fn an_absent_page_anywhere_refuses_the_window() {
        for hole in [0, 1, 5, PAGES - 1] {
            let mut space = Space::new(shuffled(PAGES as usize, hole + 1));
            space.absent.push(hole);
            let h = VBASE + hole * PAGE_4K;
            for access in [Access::Read, Access::Write] {
                assert_eq!(space.window(VBASE, PAGES * PAGE_4K, access), None, "hole at page {hole}");
                assert_eq!(space.window(h + PAGE_4K - 1, 1, access), None, "the hole's last byte");
                assert_eq!(space.window(h.saturating_sub(1).max(VBASE), 2, access), None, "across the hole's start");
            }
        }
    }

    #[test]
    fn a_read_only_page_between_writable_ones_refuses_a_write_only() {
        let mut space = Space::new(shuffled(PAGES as usize, 11));
        space.ro.push(2);
        let (start, len) = (VBASE + PAGE_4K, 3 * PAGE_4K);
        assert_eq!(space.window(start, len, Access::Write), None);
        assert!(space.window(start, len, Access::Read).is_some());
        assert!(space.window(start, PAGE_4K, Access::Write).is_some(), "page 1 alone is writable");
    }

    #[test]
    fn an_empty_or_kernel_window_is_refused_before_the_walk() {
        let never = |_: u64| -> Option<u64> { panic!("walked") };
        assert_eq!(segments(PAGE_2M, 0, never, |_| panic!("emitted")), None);
        assert_eq!(segments(USER_TOP - 8, 16, never, |_| panic!("emitted")), None);
        assert_eq!(segments(u64::MAX - 4, 8, never, |_| panic!("emitted")), None);
    }

    #[test]
    #[should_panic(expected = "past the window")]
    fn a_piece_past_the_window_is_a_kernel_bug() {
        let segs = [Segment { phys: PBASE, len: 8 }, Segment { phys: PBASE + 64, len: 8 }];
        pieces(&segs, 4, 13, |_, _, _| {});
    }
}
