//! Physical memory as 4 KiB frames carved from 2 MiB superframes.
//!
//! A superframe is absent (no memory the kernel manages), whole (every frame
//! free), taken (handed out as one 2 MiB unit or as part of a run of them), or
//! split (handed out frame by frame). A split superframe whose frames all come
//! back is whole again, so 2 MiB and run allocations stay possible after 4 KiB
//! traffic. Single frames come from the superframe that most recently regained
//! a free frame, or was split, which keeps 4 KiB traffic in few superframes.
//!
//! **A pinned frame is never reissued.** [`Pins`] counts, per frame and
//! without the allocator's lock, the user copies reading or writing it through
//! the direct map. A frame freed while pinned is deferred: the allocator does
//! not take it back until its last pin goes, and [`Pins::unpin`] says when
//! that is.
//!
//! The storage is the caller's, sized by [`supers_for`] and [`pins_for`] for
//! the span of physical memory it manages, because the kernel lays it out of
//! that memory before it has a heap.

use core::sync::atomic::{AtomicU32, Ordering};

/// One frame.
pub const FRAME: u64 = 4096;
/// One superframe.
pub const SUPER: u64 = 2 * 1024 * 1024;
/// Frames in one superframe.
pub const PER_SUPER: usize = (SUPER / FRAME) as usize;

const WORDS: usize = PER_SUPER / 64;
const NIL: u32 = u32::MAX;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Absent,
    Whole,
    Taken,
    Split,
}

/// One superframe's record.
#[derive(Clone, Copy)]
pub struct Super {
    /// Bit `i` set: frame `i` is free. Read only while split.
    free_map: [u64; WORDS],
    /// Frames free in it.
    free: u16,
    /// Frames that are ever free: what is neither outside every usable range
    /// nor withheld. A superframe short of [`PER_SUPER`] is never whole.
    usable: u16,
    state: State,
    prev: u32,
    next: u32,
}

impl Super {
    /// The record of memory not yet described.
    pub const ABSENT: Self = Self { free_map: [0; WORDS], free: 0, usable: 0, state: State::Absent, prev: NIL, next: NIL };
}

/// How many superframe records `[base, top)` takes, `base` and `top` on
/// superframe boundaries.
pub const fn supers_for(base: u64, top: u64) -> usize {
    ((top - base) / SUPER) as usize
}

/// How many pin counters `supers` superframes take: one per frame.
pub const fn pins_for(supers: usize) -> usize {
    supers * PER_SUPER
}

/// A doubly linked list of superframes, threaded through their records.
#[derive(Clone, Copy)]
struct List {
    head: u32,
    len: u32,
}

impl List {
    const EMPTY: Self = Self { head: NIL, len: 0 };
}

/// Which list a superframe is on, if any: whole ones on one, split ones with
/// a free frame on the other.
#[derive(Clone, Copy)]
enum Which {
    Whole,
    Partial,
}

/// The allocator. Every method but the constructors takes the caller's lock.
pub struct Frames<'m> {
    base: u64,
    supers: &'m mut [Super],
    whole: List,
    partial: List,
    free: u64,
    usable: u64,
}

impl<'m> Frames<'m> {
    /// Nothing usable yet over `supers.len()` superframes from `base`, a
    /// superframe boundary; describe memory with [`add`](Self::add) and
    /// [`withhold`](Self::withhold), then [`seal`](Self::seal).
    pub fn new(base: u64, supers: &'m mut [Super]) -> Self {
        assert!(base.is_multiple_of(SUPER), "frames: base {base:#x} is not on a superframe");
        assert!(supers.len() < NIL as usize, "frames: {} superframes do not fit the list links", supers.len());
        supers.fill(Super::ABSENT);
        Self { base, supers, whole: List::EMPTY, partial: List::EMPTY, free: 0, usable: 0 }
    }

    /// One past the last byte the records cover.
    pub fn top(&self) -> u64 {
        self.base + self.supers.len() as u64 * SUPER
    }

    pub fn base(&self) -> u64 {
        self.base
    }

    /// Frames free now.
    pub fn free_frames(&self) -> u64 {
        self.free
    }

    /// Frames free or handed out: every frame the allocator manages.
    pub fn usable_frames(&self) -> u64 {
        self.usable
    }

    /// Whole superframes free now.
    pub fn whole_supers(&self) -> u64 {
        u64::from(self.whole.len)
    }

    /// Every whole frame inside `[start, end)` is usable and free; parts of
    /// it outside the records, or described already, are ignored. Before
    /// [`seal`](Self::seal) only.
    pub fn add(&mut self, start: u64, end: u64) {
        let (lo, hi) = (start.next_multiple_of(FRAME).max(self.base), (end & !(FRAME - 1)).min(self.top()));
        let mut at = lo;
        while at < hi {
            let (s, i) = self.index(at);
            let record = &mut self.supers[s];
            if record.free_map[i / 64] & 1 << (i % 64) == 0 {
                record.free_map[i / 64] |= 1 << (i % 64);
                record.free += 1;
                record.usable += 1;
                record.state = State::Split;
                self.free += 1;
                self.usable += 1;
            }
            at += FRAME;
        }
    }

    /// Every frame touching `[start, end)` is never handed out. Before
    /// [`seal`](Self::seal) only.
    pub fn withhold(&mut self, start: u64, end: u64) {
        let (lo, hi) = ((start & !(FRAME - 1)).max(self.base), end.next_multiple_of(FRAME).min(self.top()));
        let mut at = lo;
        while at < hi {
            let (s, i) = self.index(at);
            let record = &mut self.supers[s];
            if record.free_map[i / 64] & 1 << (i % 64) != 0 {
                record.free_map[i / 64] &= !(1 << (i % 64));
                record.free -= 1;
                record.usable -= 1;
                self.free -= 1;
                self.usable -= 1;
            }
            at += FRAME;
        }
    }

    /// Put every described superframe on its list, lowest address first, so
    /// the first allocations come from the bottom of memory.
    pub fn seal(&mut self) {
        for s in (0..self.supers.len()).rev() {
            let record = &mut self.supers[s];
            match (record.usable, record.free) {
                (0, _) => record.state = State::Absent,
                (u, f) if u as usize == PER_SUPER && f as usize == PER_SUPER => {
                    record.state = State::Whole;
                    self.push(Which::Whole, s);
                }
                _ => {
                    record.state = State::Split;
                    self.push(Which::Partial, s);
                }
            }
        }
    }

    /// One frame, from a split superframe if one has a frame free and from a
    /// whole one otherwise.
    pub fn alloc_frame(&mut self) -> Option<u64> {
        let s = match self.partial.head {
            NIL => {
                let s = self.pop(Which::Whole)?;
                let record = &mut self.supers[s];
                record.state = State::Split;
                record.free_map = [u64::MAX; WORDS];
                self.push(Which::Partial, s);
                s
            }
            head => head as usize,
        };
        let record = &mut self.supers[s];
        let word = record.free_map.iter().position(|&w| w != 0).expect("frames: a partial superframe has a free frame");
        let bit = record.free_map[word].trailing_zeros() as usize;
        record.free_map[word] &= !(1 << bit);
        record.free -= 1;
        self.free -= 1;
        if record.free == 0 {
            self.unlink(Which::Partial, s);
        }
        Some(self.phys(s, word * 64 + bit))
    }

    /// Give back the frame at `phys`, which [`alloc_frame`](Self::alloc_frame)
    /// handed out; a pinned one is taken back by its last unpin instead.
    pub fn free_frame(&mut self, phys: u64, pins: &Pins) {
        let (s, _) = self.index(phys);
        assert!(self.supers[s].state == State::Split, "frames: {phys:#x} is freed as a frame and was not one");
        if pins.defer(phys) {
            self.release(phys);
        }
    }

    /// Take back a frame whose last pin went after it was freed (`Pins::unpin`
    /// answered [`Unpinned::Release`]), or one [`free_frame`](Self::free_frame)
    /// found unpinned.
    pub fn release(&mut self, phys: u64) {
        let (s, i) = self.index(phys);
        let record = &mut self.supers[s];
        assert!(
            record.state == State::Split && record.free_map[i / 64] & 1 << (i % 64) == 0,
            "frames: {phys:#x} is freed and was not handed out"
        );
        record.free_map[i / 64] |= 1 << (i % 64);
        record.free += 1;
        self.free += 1;
        let (free, usable) = (record.free, record.usable);
        if free == 1 {
            self.push(Which::Partial, s);
        }
        if free as usize == PER_SUPER && usable as usize == PER_SUPER {
            self.unlink(Which::Partial, s);
            self.supers[s].state = State::Whole;
            self.push(Which::Whole, s);
        }
    }

    /// One whole superframe.
    pub fn alloc_super(&mut self) -> Option<u64> {
        let s = self.pop(Which::Whole)?;
        self.supers[s].state = State::Taken;
        self.free -= PER_SUPER as u64;
        Some(self.phys(s, 0))
    }

    /// Give back the superframe at `phys` that [`alloc_super`](Self::alloc_super)
    /// or [`alloc_run`](Self::alloc_run) handed out; any of its frames still
    /// pinned is taken back by its last unpin.
    pub fn free_super(&mut self, phys: u64, pins: &Pins) {
        let (s, i) = self.index(phys);
        assert!(i == 0 && self.supers[s].state == State::Taken, "frames: {phys:#x} is freed as a superframe and was not one");
        let record = &mut self.supers[s];
        // As a split superframe with nothing free, each frame then freed alone:
        // a superframe a copy still reads is whole again only once it is done.
        record.state = State::Split;
        record.free_map = [0; WORDS];
        record.free = 0;
        for frame in 0..PER_SUPER {
            self.free_frame(self.phys(s, frame), pins);
        }
    }

    /// `count` superframes in a row, lowest address first; `None` for none.
    pub fn alloc_run(&mut self, count: usize) -> Option<u64> {
        if count == 0 || count as u64 > u64::from(self.whole.len) {
            return None;
        }
        let mut run = 0;
        for s in 0..self.supers.len() {
            run = if self.supers[s].state == State::Whole { run + 1 } else { 0 };
            if run == count {
                let first = s + 1 - count;
                for t in first..=s {
                    self.unlink(Which::Whole, t);
                    self.supers[t].state = State::Taken;
                }
                self.free -= (count * PER_SUPER) as u64;
                return Some(self.phys(first, 0));
            }
        }
        None
    }

    /// Give back a run [`alloc_run`](Self::alloc_run) handed out.
    pub fn free_run(&mut self, phys: u64, count: usize, pins: &Pins) {
        for s in 0..count as u64 {
            self.free_super(phys + s * SUPER, pins);
        }
    }

    fn index(&self, phys: u64) -> (usize, usize) {
        assert!((self.base..self.top()).contains(&phys), "frames: {phys:#x} is outside {:#x}..{:#x}", self.base, self.top());
        let offset = phys - self.base;
        ((offset / SUPER) as usize, ((offset % SUPER) / FRAME) as usize)
    }

    fn phys(&self, s: usize, frame: usize) -> u64 {
        self.base + s as u64 * SUPER + frame as u64 * FRAME
    }

    fn list(&mut self, which: Which) -> &mut List {
        match which {
            Which::Whole => &mut self.whole,
            Which::Partial => &mut self.partial,
        }
    }

    fn push(&mut self, which: Which, s: usize) {
        let head = self.list(which).head;
        self.supers[s].prev = NIL;
        self.supers[s].next = head;
        if head != NIL {
            self.supers[head as usize].prev = s as u32;
        }
        let list = self.list(which);
        list.head = s as u32;
        list.len += 1;
    }

    fn unlink(&mut self, which: Which, s: usize) {
        let (prev, next) = (self.supers[s].prev, self.supers[s].next);
        if prev == NIL {
            self.list(which).head = next;
        } else {
            self.supers[prev as usize].next = next;
        }
        if next != NIL {
            self.supers[next as usize].prev = prev;
        }
        self.supers[s].prev = NIL;
        self.supers[s].next = NIL;
        self.list(which).len -= 1;
    }

    fn pop(&mut self, which: Which) -> Option<usize> {
        match self.list(which).head {
            NIL => None,
            head => {
                self.unlink(which, head as usize);
                Some(head as usize)
            }
        }
    }
}

/// A pin word: the pin count, and whether the frame was freed under a pin.
const DEFERRED: u32 = 1 << 31;
const COUNT: u32 = DEFERRED - 1;

/// Every frame's pins, read and written without the allocator's lock.
pub struct Pins<'m> {
    base: u64,
    words: &'m [AtomicU32],
}

/// What an unpin leaves the caller to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unpinned {
    /// The frame is still pinned, or in use.
    Kept,
    /// It was freed under the pin that went: [`Frames::release`] it.
    Release,
}

impl<'m> Pins<'m> {
    /// One counter per frame from `base`, as [`pins_for`] sizes them.
    pub fn new(base: u64, words: &'m [AtomicU32]) -> Self {
        Self { base, words }
    }

    fn word(&self, phys: u64) -> Option<&AtomicU32> {
        let frame = phys.checked_sub(self.base)? / FRAME;
        self.words.get(usize::try_from(frame).ok()?)
    }

    /// Pin the frame holding `phys`, which is handed out; `false`, with
    /// nothing pinned, at the count's limit. Memory outside the records — a
    /// device's — has nothing to reissue and pins as `true`.
    pub fn pin(&self, phys: u64) -> bool {
        let Some(word) = self.word(phys) else { return true };
        let was = word.fetch_add(1, Ordering::Acquire);
        if was & COUNT == COUNT - 1 {
            word.fetch_sub(1, Ordering::Release);
            return false;
        }
        true
    }

    /// Drop one pin [`pin`](Self::pin) took.
    pub fn unpin(&self, phys: u64) -> Unpinned {
        let Some(word) = self.word(phys) else { return Unpinned::Kept };
        let was = word.fetch_sub(1, Ordering::Release);
        assert!(was & COUNT != 0, "frames: {phys:#x} unpinned and was not pinned");
        if was == DEFERRED | 1 {
            word.store(0, Ordering::Relaxed);
            Unpinned::Release
        } else {
            Unpinned::Kept
        }
    }

    /// Whether a frame being freed may go back now; one still pinned is marked
    /// for its last unpin to release.
    fn defer(&self, phys: u64) -> bool {
        let Some(word) = self.word(phys) else { return true };
        let mut now = word.load(Ordering::Acquire);
        loop {
            assert!(now & DEFERRED == 0, "frames: {phys:#x} freed twice");
            if now == 0 {
                return true;
            }
            match word.compare_exchange(now, now | DEFERRED, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return false,
                Err(seen) => now = seen,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::boxed::Box;
    use std::collections::BTreeSet;
    use std::vec::Vec;

    const BASE: u64 = 64 * SUPER;

    struct Machine {
        frames: Frames<'static>,
        pins: Pins<'static>,
    }

    /// `supers` superframes from `BASE`, `usable` of memory described, then sealed.
    fn machine(supers: usize, usable: &[(u64, u64)], withheld: &[(u64, u64)]) -> Machine {
        let records = Box::leak(std::vec![Super::ABSENT; supers].into_boxed_slice());
        let words = Box::leak((0..pins_for(supers)).map(|_| AtomicU32::new(0)).collect::<Vec<_>>().into_boxed_slice());
        let mut frames = Frames::new(BASE, records);
        for &(start, end) in usable {
            frames.add(start, end);
        }
        for &(start, end) in withheld {
            frames.withhold(start, end);
        }
        frames.seal();
        Machine { frames, pins: Pins::new(BASE, words) }
    }

    fn whole(supers: usize) -> Machine {
        machine(supers, &[(BASE, BASE + supers as u64 * SUPER)], &[])
    }

    #[test]
    fn a_frame_carves_one_superframe_and_its_return_makes_it_whole() {
        let mut m = whole(2);
        let frame = m.frames.alloc_frame().unwrap();
        assert_eq!(frame, BASE, "the lowest superframe is carved first");
        assert_eq!((m.frames.whole_supers(), m.frames.free_frames()), (1, 2 * PER_SUPER as u64 - 1));
        m.frames.free_frame(frame, &m.pins);
        assert_eq!((m.frames.whole_supers(), m.frames.free_frames()), (2, 2 * PER_SUPER as u64));
        assert!(m.frames.alloc_run(2).is_some(), "both superframes are whole again");
    }

    #[test]
    fn frames_fill_a_split_superframe_before_carving_another() {
        let mut m = whole(3);
        let first: Vec<u64> = (0..PER_SUPER).map(|_| m.frames.alloc_frame().unwrap()).collect();
        assert!(first.iter().all(|f| (BASE..BASE + SUPER).contains(f)));
        assert_eq!(first.iter().collect::<BTreeSet<_>>().len(), PER_SUPER, "no frame twice");
        assert_eq!(m.frames.whole_supers(), 2);
        let next = m.frames.alloc_frame().unwrap();
        assert!((BASE + SUPER..BASE + 2 * SUPER).contains(&next));
        // A frame freed into the first goes out before the second is used up.
        m.frames.free_frame(first[7], &m.pins);
        assert_eq!(m.frames.alloc_frame(), Some(first[7]));
    }

    #[test]
    fn exhaustion_answers_none_and_a_return_answers_again() {
        let mut m = whole(1);
        let all: Vec<u64> = core::iter::from_fn(|| m.frames.alloc_frame()).collect();
        assert_eq!(all.len(), PER_SUPER);
        assert_eq!((m.frames.alloc_frame(), m.frames.alloc_super(), m.frames.free_frames()), (None, None, 0));
        m.frames.free_frame(all[100], &m.pins);
        assert_eq!(m.frames.alloc_frame(), Some(all[100]));
    }

    #[test]
    fn a_superframe_and_a_run_come_only_from_whole_superframes() {
        let mut m = whole(4);
        let carved = m.frames.alloc_frame().unwrap();
        assert_eq!(m.frames.alloc_run(4), None, "one superframe is split");
        let run = m.frames.alloc_run(3).unwrap();
        assert_eq!(run, BASE + SUPER);
        assert_eq!(m.frames.alloc_super(), None);
        m.frames.free_run(run, 3, &m.pins);
        assert!(m.frames.alloc_super().is_some_and(|s| (run..run + 3 * SUPER).contains(&s)));
        m.frames.free_frame(carved, &m.pins);
        assert_eq!(m.frames.whole_supers(), 3);
    }

    #[test]
    fn a_run_is_contiguous_and_skips_a_gap() {
        let mut m = whole(5);
        let middle = m.frames.alloc_super().unwrap();
        let _ = m.frames.alloc_super().unwrap();
        let _ = m.frames.alloc_super().unwrap();
        m.frames.free_super(middle + SUPER, &m.pins);
        // Taken: 0 and 2; whole: 1, 3, 4.
        assert_eq!(m.frames.alloc_run(2), Some(BASE + 3 * SUPER));
        assert_eq!(m.frames.alloc_run(2), None);
        assert_eq!(m.frames.alloc_run(1), Some(BASE + SUPER));
    }

    #[test]
    fn memory_off_superframe_boundaries_is_used_and_never_whole() {
        let start = BASE + 3 * FRAME + 1;
        let end = BASE + SUPER + 10 * FRAME;
        let mut m = machine(2, &[(start, end)], &[]);
        // Frame 3 starts below `start`, so frames 4..512 and 10 of the next.
        let usable = (PER_SUPER - 4 + 10) as u64;
        assert_eq!((m.frames.usable_frames(), m.frames.free_frames(), m.frames.whole_supers()), (usable, usable, 0));
        let got: Vec<u64> = core::iter::from_fn(|| m.frames.alloc_frame()).collect();
        assert_eq!(got.len() as u64, usable);
        assert!(got.iter().all(|&f| f >= start && f + FRAME <= end));
        for &f in &got {
            m.frames.free_frame(f, &m.pins);
        }
        assert_eq!(m.frames.whole_supers(), 0, "neither superframe is wholly usable");
    }

    #[test]
    fn a_withheld_range_is_never_handed_out() {
        let withheld = (BASE + SUPER + 5 * FRAME + 7, BASE + SUPER + 6 * FRAME + 1);
        let mut m = machine(2, &[(BASE, BASE + 2 * SUPER)], &[withheld]);
        assert_eq!(m.frames.usable_frames(), 2 * PER_SUPER as u64 - 2);
        let got: BTreeSet<u64> = core::iter::from_fn(|| m.frames.alloc_frame()).collect();
        assert!(!got.contains(&(BASE + SUPER + 5 * FRAME)) && !got.contains(&(BASE + SUPER + 6 * FRAME)));
        assert_eq!(got.len(), 2 * PER_SUPER - 2);
    }

    #[test]
    fn a_hole_is_absent() {
        let mut m = machine(3, &[(BASE, BASE + SUPER), (BASE + 2 * SUPER, BASE + 3 * SUPER)], &[]);
        assert_eq!(m.frames.whole_supers(), 2);
        assert_eq!(m.frames.alloc_run(2), None, "the hole between them is no memory");
        assert_eq!(m.frames.alloc_super(), Some(BASE));
        assert_eq!(m.frames.alloc_super(), Some(BASE + 2 * SUPER));
    }

    #[test]
    fn a_frame_freed_while_pinned_goes_back_with_its_last_pin() {
        let mut m = whole(1);
        let a = m.frames.alloc_frame().unwrap();
        assert!(m.pins.pin(a) && m.pins.pin(a));
        m.frames.free_frame(a, &m.pins);
        assert_eq!(m.frames.free_frames(), PER_SUPER as u64 - 1, "deferred, not free");
        let rest: Vec<u64> = core::iter::from_fn(|| m.frames.alloc_frame()).collect();
        assert!(!rest.contains(&a), "a pinned frame is never reissued");
        assert_eq!(m.pins.unpin(a), Unpinned::Kept);
        assert_eq!(m.pins.unpin(a), Unpinned::Release);
        m.frames.release(a);
        assert_eq!(m.frames.alloc_frame(), Some(a));
    }

    #[test]
    fn an_unpin_before_the_free_leaves_nothing_deferred() {
        let mut m = whole(1);
        let a = m.frames.alloc_frame().unwrap();
        assert!(m.pins.pin(a));
        assert_eq!(m.pins.unpin(a), Unpinned::Kept);
        m.frames.free_frame(a, &m.pins);
        assert_eq!(m.frames.free_frames(), PER_SUPER as u64);
    }

    #[test]
    fn a_superframe_freed_with_a_pinned_frame_is_whole_only_after_the_unpin() {
        let mut m = whole(1);
        let s = m.frames.alloc_super().unwrap();
        let pinned = s + 9 * FRAME;
        assert!(m.pins.pin(pinned));
        m.frames.free_super(s, &m.pins);
        assert_eq!((m.frames.whole_supers(), m.frames.free_frames()), (0, PER_SUPER as u64 - 1));
        assert_eq!(m.frames.alloc_super(), None);
        assert_eq!(m.pins.unpin(pinned), Unpinned::Release);
        m.frames.release(pinned);
        assert_eq!(m.frames.alloc_super(), Some(s));
    }

    #[test]
    fn memory_outside_the_records_pins_as_nothing() {
        let m = whole(1);
        assert!(m.pins.pin(BASE - FRAME));
        assert!(m.pins.pin(BASE + SUPER));
        assert_eq!(m.pins.unpin(BASE + SUPER), Unpinned::Kept);
    }

    #[test]
    fn a_pin_count_at_its_limit_refuses_and_pins_nothing() {
        let m = whole(1);
        let word = m.pins.word(BASE).unwrap();
        word.store(COUNT - 1, Ordering::Relaxed);
        assert!(!m.pins.pin(BASE));
        assert_eq!(word.load(Ordering::Relaxed), COUNT - 1);
    }

    #[test]
    #[should_panic(expected = "was not handed out")]
    fn a_double_free_is_a_kernel_bug() {
        let mut m = whole(1);
        let a = m.frames.alloc_frame().unwrap();
        let _kept = m.frames.alloc_frame().unwrap();
        m.frames.free_frame(a, &m.pins);
        m.frames.free_frame(a, &m.pins);
    }

    #[test]
    #[should_panic(expected = "unpinned and was not pinned")]
    fn an_unpin_with_no_pin_is_a_kernel_bug() {
        let m = whole(1);
        let _ = m.pins.unpin(BASE);
    }

    /// Every frame handed out is distinct, inside usable memory, and the counts
    /// add up, through a deterministic mix of every operation.
    #[test]
    fn a_mixed_load_keeps_every_frame_once() {
        let supers = 8;
        let mut m = whole(supers);
        let mut frames: Vec<u64> = Vec::new();
        let mut taken: Vec<u64> = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            match next() % 5 {
                0 | 1 => frames.extend(m.frames.alloc_frame()),
                2 if !frames.is_empty() => {
                    let f = frames.swap_remove(next() as usize % frames.len());
                    m.frames.free_frame(f, &m.pins);
                }
                3 => taken.extend(m.frames.alloc_super()),
                4 if !taken.is_empty() => {
                    let s = taken.swap_remove(next() as usize % taken.len());
                    m.frames.free_super(s, &m.pins);
                }
                _ => {}
            }
            let held = frames.len() as u64 + taken.len() as u64 * PER_SUPER as u64;
            assert_eq!(m.frames.free_frames() + held, (supers * PER_SUPER) as u64);
        }
        let mut seen = BTreeSet::new();
        for &f in &frames {
            assert!(seen.insert(f & !(FRAME - 1)), "{f:#x} twice");
        }
        for &s in &taken {
            for i in 0..PER_SUPER as u64 {
                assert!(seen.insert(s + i * FRAME), "{:#x} twice", s + i * FRAME);
            }
        }
    }
}
