//! Where a user window's bytes are in physical memory: one [`Segment`] per
//! physically contiguous run, in address order, each copy cut at their seams,
//! and every run pinned for the window's life.
//!
//! **A window is asked once per leaf that maps it.** A page that is absent or
//! does not grant the access refuses the whole window, and so does a run that
//! cannot be pinned: the kernel copies nothing through a window it could not
//! wholly place and hold.

use crate::span::in_user_half;

/// `len` bytes of a user window, physically contiguous from `phys`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Segment {
    pub phys: u64,
    pub len: u64,
}

/// Walks `[start, start + len)` and hands `emit` each maximal physically
/// contiguous run of it, in order. `leaf` is the page walk: for one user
/// address, where it grants the access asked, the physical address it
/// translates to and the bytes from it to the end of the leaf that maps it,
/// at least one; `None` where it does not.
///
/// `None` where the range is empty, leaves the user half, or holds a page
/// `leaf` refuses — and runs before the refused page may already have been
/// emitted, so nothing emitted is acted on until this returns `Some`.
pub fn segments(
    start: u64,
    len: u64,
    mut leaf: impl FnMut(u64) -> Option<(u64, u64)>,
    mut emit: impl FnMut(Segment),
) -> Option<()> {
    if len == 0 || !in_user_half(start, len) {
        return None;
    }
    let end = start + len;
    let mut at = start;
    let (mut phys, mut extent) = leaf(at)?;
    let mut run = Segment { phys, len: 0 };
    loop {
        if run.phys + run.len != phys {
            emit(run);
            run = Segment { phys, len: 0 };
        }
        let n = extent.min(end - at);
        run.len += n;
        at += n;
        if at == end {
            break;
        }
        (phys, extent) = leaf(at)?;
    }
    emit(run);
    Some(())
}

/// Hands `copy` each physically contiguous piece of bytes `[off, off + len)`
/// of the window `segs` lays out, as `(phys, at, n)`: the `n` bytes at `phys`
/// are bytes `at..at + n` of that range. The caller bounds the range inside
/// the window.
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
}

/// What holds a frame against reissue: the kernel's frame allocator, or a
/// test's counter.
pub trait Pins {
    /// Pins every frame `run` touches; `false`, with nothing pinned, when it
    /// cannot.
    fn pin(&mut self, run: Segment) -> bool;
    /// Releases a pin [`pin`](Self::pin) took over the same run.
    fn unpin(&mut self, run: Segment);
}

/// A window's runs, every one of them pinned until this drops.
pub struct Pinned<R: AsRef<[Segment]>, P: Pins> {
    runs: R,
    pins: P,
}

impl<R: AsRef<[Segment]>, P: Pins> Pinned<R, P> {
    /// Pins every run, or none: a run that cannot be pinned unpins the runs
    /// before it, and the window is refused.
    pub fn pin(runs: R, mut pins: P) -> Option<Self> {
        if let Some(refused) = runs.as_ref().iter().position(|&run| !pins.pin(run)) {
            for &run in &runs.as_ref()[..refused] {
                pins.unpin(run);
            }
            return None;
        }
        Some(Pinned { runs, pins })
    }

    pub fn runs(&self) -> &[Segment] {
        self.runs.as_ref()
    }
}

impl<R: AsRef<[Segment]>, P: Pins> Drop for Pinned<R, P> {
    fn drop(&mut self) {
        for &run in self.runs.as_ref() {
            self.pins.unpin(run);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::cell::{Cell, RefCell};
    use std::vec;
    use std::vec::Vec;

    use super::*;
    use crate::span::{Access, PAGE_2M, PAGE_4K, USER_TOP};

    /// Where the fake user range starts: a 2 MiB boundary.
    const VBASE: u64 = 5 * PAGE_2M;
    /// Pages in the fake range.
    const PAGES: u64 = 48;
    /// Where fake physical memory starts: nonzero, so a phys of 0 is a bug.
    const PBASE: u64 = 0x10_0000_0000;

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

    /// A process's pages over fake physical memory, mapped by leaves of
    /// `grain` pages: leaf `i` of the range lives in frame `frame[i]`, and `ro`
    /// and `absent` pages (with a `grain` of one) grant no write and nothing.
    struct Space {
        grain: u64,
        frame: Vec<u64>,
        ro: Vec<u64>,
        absent: Vec<u64>,
        phys: Vec<u8>,
        lookups: Cell<u64>,
    }

    impl Space {
        /// Every page present and writable, leaves in frames `order`, and each
        /// user byte holding a value its virtual address decides.
        fn new(order: Vec<u64>, grain: u64) -> Self {
            assert_eq!(order.len() as u64 * grain, PAGES);
            let mut space = Space {
                grain,
                frame: order,
                ro: Vec::new(),
                absent: Vec::new(),
                phys: vec![0; (PAGES * PAGE_4K) as usize],
                lookups: Cell::new(0),
            };
            for v in VBASE..VBASE + PAGES * PAGE_4K {
                let p = space.phys_of(v).expect("every page is present");
                space.phys[(p - PBASE) as usize] = byte_at(v);
            }
            space
        }

        fn leaf_bytes(&self) -> u64 {
            self.grain * PAGE_4K
        }

        fn phys_of(&self, v: u64) -> Option<u64> {
            if !(VBASE..VBASE + PAGES * PAGE_4K).contains(&v) {
                return None;
            }
            let off = v - VBASE;
            Some(PBASE + self.frame[(off / self.leaf_bytes()) as usize] * self.leaf_bytes() + off % self.leaf_bytes())
        }

        /// The kernel's `AddressSpace::leaf`, over fake memory.
        fn leaf(&self, v: u64, access: Access) -> Option<(u64, u64)> {
            self.lookups.set(self.lookups.get() + 1);
            let page = v.checked_sub(VBASE)? / PAGE_4K;
            if self.absent.contains(&page) || (access == Access::Write && self.ro.contains(&page)) {
                return None;
            }
            Some((self.phys_of(v)?, self.leaf_bytes() - (v - VBASE) % self.leaf_bytes()))
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
            self.phys[(self.phys_of(v).unwrap() - PBASE) as usize]
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

    /// Each leaf size the fake maps with, in pages.
    const GRAINS: [u64; 3] = [1, 4, 16];

    #[test]
    fn a_read_across_shuffled_frames_gets_the_bytes_at_their_addresses() {
        for grain in GRAINS {
            for seed in [1, 0x5eed, 0xdead_beef, 42] {
                let space = Space::new(shuffled((PAGES / grain) as usize, seed), grain);
                for (start, len) in ranges() {
                    let segs = space.window(start, len, Access::Read).expect("every page is readable");
                    let want: Vec<u8> = (start..start + len).map(byte_at).collect();
                    assert_eq!(space.read(&segs, 0, len), want, "grain {grain}, seed {seed:#x}: [{start:#x}, +{len:#x})");
                    // A view from any offset inside the window: `UserBytes::sub`.
                    for off in [1, PAGE_4K - 1, PAGE_4K, len / 2] {
                        if off < len {
                            let tail = len - off;
                            assert_eq!(space.read(&segs, off, tail), want[off as usize..], "grain {grain}, seed {seed:#x}: +{off:#x}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_write_across_shuffled_frames_lands_where_a_ring_3_load_reads_it() {
        for grain in GRAINS {
            for seed in [3, 0xabcdef, 99] {
                for (start, len) in ranges() {
                    let mut space = Space::new(shuffled((PAGES / grain) as usize, seed), grain);
                    let segs = space.window(start, len, Access::Write).expect("every page is writable");
                    let src: Vec<u8> = (0..len).map(|i| !byte_at(i ^ seed)).collect();
                    space.write(&segs, 0, &src);
                    for v in VBASE..VBASE + PAGES * PAGE_4K {
                        let want = if (start..start + len).contains(&v) { src[(v - start) as usize] } else { byte_at(v) };
                        assert_eq!(space.load(v), want, "grain {grain}, seed {seed:#x}: [{start:#x}, +{len:#x}) at {v:#x}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_segments_are_the_maximal_runs_and_cover_the_window_exactly() {
        for grain in GRAINS {
            for seed in [7, 0x1234_5678] {
                let space = Space::new(shuffled((PAGES / grain) as usize, seed), grain);
                for (start, len) in ranges() {
                    let segs = space.window(start, len, Access::Read).unwrap();
                    assert_eq!(segs.iter().map(|s| s.len).sum::<u64>(), len);
                    assert_eq!(segs[0].phys, space.phys_of(start).unwrap());
                    for pair in segs.windows(2) {
                        assert_ne!(pair[0].phys + pair[0].len, pair[1].phys, "two runs that follow were not joined");
                    }
                }
            }
        }
    }

    /// The cost the walk is held to: one lookup per leaf the range touches,
    /// whatever the leaf's size.
    #[test]
    fn a_range_is_looked_up_once_per_leaf() {
        for grain in GRAINS {
            let space = Space::new(shuffled((PAGES / grain) as usize, 5), grain);
            for (start, len) in ranges() {
                space.lookups.set(0);
                space.window(start, len, Access::Read).unwrap();
                let leaves = (start + len - 1 - VBASE) / space.leaf_bytes() - (start - VBASE) / space.leaf_bytes() + 1;
                assert_eq!(space.lookups.get(), leaves, "grain {grain}: [{start:#x}, +{len:#x})");
            }
        }
        // 64 MiB from a byte past a 2 MiB boundary: 33 leaves of 2 MiB, frames
        // in order, so 33 lookups and one run.
        let (start, len) = (VBASE + 1, 32 * PAGE_2M);
        let mut lookups = 0;
        let mut segs = Vec::new();
        segments(
            start,
            len,
            |at| {
                lookups += 1;
                Some((PBASE + at, PAGE_2M - at % PAGE_2M))
            },
            |s| segs.push(s),
        )
        .unwrap();
        assert_eq!(lookups, 33);
        assert_eq!(segs, [Segment { phys: PBASE + start, len }]);
    }

    /// A 2 MiB leaf, or a split window over one frame, is every page in order.
    #[test]
    fn frames_in_order_are_one_segment() {
        for grain in GRAINS {
            let space = Space::new((0..PAGES / grain).collect(), grain);
            for (start, len) in ranges() {
                let segs = space.window(start, len, Access::Write).unwrap();
                assert_eq!(segs, [Segment { phys: space.phys_of(start).unwrap(), len }]);
            }
        }
    }

    /// Frames in reverse: every leaf seam is a segment seam.
    #[test]
    fn frames_in_reverse_are_one_segment_per_leaf() {
        let space = Space::new((0..PAGES).rev().collect(), 1);
        let segs = space.window(VBASE + PAGE_4K - 3, PAGE_4K + 6, Access::Read).unwrap();
        assert_eq!(segs.iter().map(|s| s.len).collect::<Vec<_>>(), [3, PAGE_4K, 3], "{segs:x?}");
    }

    #[test]
    fn an_absent_page_anywhere_refuses_the_window() {
        for hole in [0, 1, 5, PAGES - 1] {
            let mut space = Space::new(shuffled(PAGES as usize, hole + 1), 1);
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
        let mut space = Space::new(shuffled(PAGES as usize, 11), 1);
        space.ro.push(2);
        let (start, len) = (VBASE + PAGE_4K, 3 * PAGE_4K);
        assert_eq!(space.window(start, len, Access::Write), None);
        assert!(space.window(start, len, Access::Read).is_some());
        assert!(space.window(start, PAGE_4K, Access::Write).is_some(), "page 1 alone is writable");
    }

    #[test]
    fn an_empty_or_kernel_window_is_refused_before_the_walk() {
        let never = |_: u64| -> Option<(u64, u64)> { panic!("walked") };
        assert_eq!(segments(PAGE_2M, 0, never, |_| panic!("emitted")), None);
        assert_eq!(segments(USER_TOP - 8, 16, never, |_| panic!("emitted")), None);
        assert_eq!(segments(u64::MAX - 4, 8, never, |_| panic!("emitted")), None);
    }

    /// Pin counts per fake 4 KiB frame, which refuse the pin call numbered
    /// `refuse` and panic on an unpin of a frame nothing pinned, as the PMM does.
    struct Frames {
        count: RefCell<Vec<u32>>,
        calls: Cell<usize>,
        refuse: Option<usize>,
    }

    impl Frames {
        fn new(refuse: Option<usize>) -> Self {
            Frames { count: RefCell::new(vec![0; PAGES as usize]), calls: Cell::new(0), refuse }
        }

        fn of(run: Segment) -> core::ops::RangeInclusive<usize> {
            ((run.phys - PBASE) / PAGE_4K) as usize..=((run.phys + run.len - 1 - PBASE) / PAGE_4K) as usize
        }

        fn pinned(&self) -> Vec<u32> {
            self.count.borrow().clone()
        }
    }

    impl Pins for &Frames {
        fn pin(&mut self, run: Segment) -> bool {
            let call = self.calls.get();
            self.calls.set(call + 1);
            if self.refuse == Some(call) {
                return false;
            }
            for f in Frames::of(run) {
                self.count.borrow_mut()[f] += 1;
            }
            true
        }

        fn unpin(&mut self, run: Segment) {
            for f in Frames::of(run) {
                let mut count = self.count.borrow_mut();
                count[f] = count[f].checked_sub(1).expect("unpin of a frame that was not pinned");
            }
        }
    }

    /// Frames in a shuffled order with one frame mapped twice, as a shared
    /// mapping is: every run holds its own pin on it.
    fn twice_mapped() -> Space {
        let mut order = shuffled(PAGES as usize, 0x77);
        order[9] = order[3];
        Space::new(order, 1)
    }

    #[test]
    fn every_run_is_pinned_until_the_window_is_dropped() {
        let space = twice_mapped();
        for (start, len) in ranges() {
            let segs = space.window(start, len, Access::Read).unwrap();
            let frames = Frames::new(None);
            let mut want = vec![0; PAGES as usize];
            for &run in &segs {
                for f in Frames::of(run) {
                    want[f] += 1;
                }
            }
            {
                let pinned = Pinned::pin(&segs[..], &frames).expect("nothing refuses");
                assert_eq!(frames.pinned(), want, "[{start:#x}, +{len:#x}) while pinned");
                let got = space.read(pinned.runs(), 0, len);
                assert_eq!(got, (start..start + len).map(|v| space.load(v)).collect::<Vec<_>>());
            }
            assert_eq!(frames.pinned(), vec![0; PAGES as usize], "[{start:#x}, +{len:#x}) after the copy");
        }
    }

    #[test]
    fn a_run_that_cannot_be_pinned_refuses_the_window_and_leaves_nothing_pinned() {
        let space = twice_mapped();
        let segs = space.window(VBASE + PAGE_4K / 2, 20 * PAGE_4K, Access::Read).unwrap();
        assert!(segs.len() > 3, "{segs:x?}");
        for refused in 0..segs.len() {
            let frames = Frames::new(Some(refused));
            assert!(Pinned::pin(&segs[..], &frames).is_none(), "run {refused} refused");
            assert_eq!(frames.calls.get(), refused + 1, "no run after the refused one is tried");
            assert_eq!(frames.pinned(), vec![0; PAGES as usize], "run {refused} refused");
        }
    }
}
