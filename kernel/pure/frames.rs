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
//! **A promise is a frame no other allocation may take.** A commitment of
//! memory that is populated later [`promise`](Frames::promise)s its frames
//! when it is made, and each is drawn with
//! [`alloc_promised`](Frames::alloc_promised) when it is first touched; every
//! other allocation is refused rather than take a frame promised. So a
//! commitment is refused when it is made, never at its first touch.
//!
//! **The kernel heap grows by whole superframes, so what it is owed is held
//! back whole.** Kernel memory a process causes is
//! [`charge`](Frames::charge)d in bytes before it is allocated, and the heap
//! grows into a superframe reserved for the charges before any other; a few
//! more are kept for the kernel's own allocations ([`keep`](Frames::keep)).
//! Nothing a process causes is promised or handed one held back, and a whole
//! superframe is split only when no split one has a frame free, so 4 KiB
//! traffic never splits one either. A charge is refused when it is made,
//! never when the heap grows.
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

/// What [`Frames::discard`] did with a frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Discarded {
    /// Freed, and its page promised a frame again.
    Freed,
    /// Pinned by a copy, so kept for its page.
    Kept,
}

/// The allocator. Every method but the constructors takes the caller's lock.
///
/// Invariants: `whole ≥ reserved + kept`, and `promised + PER_SUPER ×
/// (reserved + kept) ≤ free`, so what is promised fits outside the
/// superframes held back and a draw never has to split one of them.
pub struct Frames<'m> {
    base: u64,
    supers: &'m mut [Super],
    whole: List,
    partial: List,
    free: u64,
    usable: u64,
    /// Frames promised and not yet drawn.
    promised: u64,
    /// Whole superframes reserved for the heap to grow into: what charges
    /// cover beyond what it grew into for them, `entitled - drawn`.
    reserved: u64,
    /// Whole superframes kept for the kernel's own allocations, and how many
    /// it is to keep.
    kept: u64,
    keep_target: u64,
    /// Bytes charged, the superframes covering them, those the heap holds,
    /// and how many of these it grew into from what charges reserved.
    charged: u64,
    entitled: u64,
    heap: u64,
    drawn: u64,
}

impl<'m> Frames<'m> {
    /// Nothing usable yet over `supers.len()` superframes from `base`, a
    /// superframe boundary; describe memory with [`add`](Self::add) and
    /// [`withhold`](Self::withhold), then [`seal`](Self::seal).
    pub fn new(base: u64, supers: &'m mut [Super]) -> Self {
        assert!(base.is_multiple_of(SUPER), "frames: base {base:#x} is not on a superframe");
        assert!(supers.len() < NIL as usize, "frames: {} superframes do not fit the list links", supers.len());
        supers.fill(Super::ABSENT);
        Self {
            base,
            supers,
            whole: List::EMPTY,
            partial: List::EMPTY,
            free: 0,
            usable: 0,
            promised: 0,
            reserved: 0,
            kept: 0,
            keep_target: 0,
            charged: 0,
            entitled: 0,
            heap: 0,
            drawn: 0,
        }
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

    /// Frames promised and not yet drawn.
    pub fn promised(&self) -> u64 {
        self.promised
    }

    /// Whole superframes held back for the heap: those charges cover and the
    /// heap does not hold, and those kept for the kernel's own allocations.
    pub fn held_back(&self) -> (u64, u64) {
        (self.reserved, self.kept)
    }

    /// Bytes charged, and the superframes the heap holds.
    pub fn charges(&self) -> (u64, u64) {
        (self.charged, self.heap)
    }

    /// Free frames nothing holds back: neither promised nor in a superframe
    /// reserved or kept.
    fn room(&self) -> u64 {
        let held = self.promised + PER_SUPER as u64 * (self.reserved + self.kept);
        self.free.checked_sub(held).unwrap_or_else(|| panic!("frames: {held} held back of {} free", self.free))
    }

    /// Whole superframes nothing holds back.
    fn spare_supers(&self) -> u64 {
        let held = self.reserved + self.kept;
        u64::from(self.whole.len).checked_sub(held).unwrap_or_else(|| panic!("frames: {held} superframes held back of {} whole", self.whole.len))
    }

    /// Superframes `count` new ones can be taken as without holding back
    /// fewer than it does.
    fn spare(&self, count: u64) -> bool {
        self.spare_supers() >= count && self.room() >= count * PER_SUPER as u64
    }

    /// Keep up to `supers` whole superframes for the kernel's own
    /// allocations, which nothing a process causes takes: as many as are
    /// spare now, and the rest as they come back.
    pub fn keep(&mut self, supers: u64) {
        self.keep_target = supers;
        self.refill();
    }

    /// Keep superframes towards the target while some are spare.
    fn refill(&mut self) {
        while self.kept < self.keep_target && self.spare(1) {
            self.kept += 1;
        }
    }

    /// Promise `frames` if nothing held back is needed for them.
    pub fn promise(&mut self, frames: u64) -> bool {
        if frames > self.room() {
            return false;
        }
        self.promised += frames;
        true
    }

    /// Free the frame at `phys` of a page that stays committed, promising
    /// the page a frame again — or, where a user copy pins the frame, keep it
    /// for the page: the caller zeroes it in place and leaves it in the
    /// page's leaf, not present, for the next touch to map again.
    ///
    /// Sound only where no pin can be taken meanwhile: a copy pins a frame
    /// under the lock of the space that maps it, which the caller holds, so a
    /// frame read unpinned here stays unpinned. Then every promise is backed
    /// by a frame that is free now, never by one deferred to a later unpin.
    pub fn discard(&mut self, phys: u64, pins: &Pins) -> Discarded {
        if pins.pinned(phys) {
            return Discarded::Kept;
        }
        self.free_frame(phys, pins);
        self.repromise(1);
        Discarded::Freed
    }

    /// Promise `frames` again, which a commitment still holds and whose
    /// frames just came back free.
    fn repromise(&mut self, frames: u64) {
        self.promised += frames;
        let _ = self.room();
    }

    /// Take back promises of `frames` no commitment holds any more.
    pub fn unpromise(&mut self, frames: u64) {
        assert!(frames <= self.promised, "frames: {frames} unpromised of {} promised", self.promised);
        self.promised -= frames;
        self.refill();
    }

    /// One frame a promise held, which a promise guarantees is free.
    pub fn alloc_promised(&mut self) -> u64 {
        assert!(self.promised > 0, "frames: a promised frame drawn and none was promised");
        let phys = self.take_frame().unwrap_or_else(|| panic!("frames: {} promised and none free", self.promised));
        self.promised -= 1;
        phys
    }

    /// One frame for the kernel's own use: one nothing holds back, or else
    /// one of a kept superframe, which is kept no longer.
    pub fn alloc_frame(&mut self) -> Option<u64> {
        if self.room() == 0 {
            self.kept = self.kept.checked_sub(1)?;
        }
        self.take_frame()
    }

    /// One frame a process causes without a promise, such as a page table:
    /// refused rather than take one held back.
    pub fn alloc_frame_for_process(&mut self) -> Option<u64> {
        if self.room() == 0 {
            return None;
        }
        self.take_frame()
    }

    // A whole superframe is split only when no split one has a frame free, so
    // a draw that `room` allows never splits one held back: then every free
    // frame is in a whole superframe, and the frames held back are fewer.
    fn take_frame(&mut self) -> Option<u64> {
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
        self.give_back(phys, pins);
        self.refill();
    }

    fn give_back(&mut self, phys: u64, pins: &Pins) {
        let (s, _) = self.index(phys);
        assert!(self.supers[s].state == State::Split, "frames: {phys:#x} is freed as a frame and was not one");
        if pins.defer(phys) {
            self.put_back(phys);
        }
    }

    /// Take back a frame whose last pin went after it was freed (`Pins::unpin`
    /// answered [`Unpinned::Release`]), or one [`free_frame`](Self::free_frame)
    /// found unpinned.
    pub fn release(&mut self, phys: u64) {
        self.put_back(phys);
        self.refill();
    }

    fn put_back(&mut self, phys: u64) {
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

    /// One whole superframe for the kernel's own use.
    pub fn alloc_super(&mut self) -> Option<u64> {
        self.alloc_run(1)
    }

    /// `count` superframes in a row for the kernel's own use, lowest address
    /// first; `None` for none. Kept superframes go to it where no spare ones
    /// are enough.
    pub fn alloc_run(&mut self, count: usize) -> Option<u64> {
        let count64 = count as u64;
        let spare = self.spare_supers().min(self.room() / PER_SUPER as u64);
        let from_kept = count64.saturating_sub(spare);
        if from_kept > self.kept {
            return None;
        }
        let run = self.take_run(count)?;
        self.kept -= from_kept;
        Some(run)
    }

    /// [`alloc_run`](Self::alloc_run) of a run a process causes, such as a
    /// pipe's ring or memory a device is lent: refused rather than take a
    /// superframe held back.
    pub fn alloc_run_for_process(&mut self, count: usize) -> Option<u64> {
        if !self.spare(count as u64) {
            return None;
        }
        self.take_run(count)
    }

    fn take_run(&mut self, count: usize) -> Option<u64> {
        if count == 0 || count as u64 > u64::from(self.whole.len) {
            return None;
        }
        if count == 1 {
            let s = self.pop(Which::Whole)?;
            self.supers[s].state = State::Taken;
            self.free -= PER_SUPER as u64;
            return Some(self.phys(s, 0));
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

    /// Give back the superframe at `phys` that [`alloc_super`](Self::alloc_super)
    /// or [`alloc_run`](Self::alloc_run) handed out; any of its frames still
    /// pinned is taken back by its last unpin.
    pub fn free_super(&mut self, phys: u64, pins: &Pins) {
        self.give_back_super(phys, pins);
        self.refill();
    }

    fn give_back_super(&mut self, phys: u64, pins: &Pins) {
        let (s, i) = self.index(phys);
        assert!(i == 0 && self.supers[s].state == State::Taken, "frames: {phys:#x} is freed as a superframe and was not one");
        let record = &mut self.supers[s];
        // As a split superframe with nothing free, each frame then freed alone:
        // a superframe a copy still reads is whole again only once it is done.
        record.state = State::Split;
        record.free_map = [0; WORDS];
        record.free = 0;
        for frame in 0..PER_SUPER {
            self.give_back(self.phys(s, frame), pins);
        }
    }

    /// Give back a run [`alloc_run`](Self::alloc_run) handed out.
    pub fn free_run(&mut self, phys: u64, count: usize, pins: &Pins) {
        for s in 0..count as u64 {
            self.give_back_super(phys + s * SUPER, pins);
        }
        self.refill();
    }

    /// Charge `bytes` of the kernel heap to a process's cause, refused where
    /// covering them would take a superframe held back. What charges cover is
    /// whole superframes: those the heap grew into from what charges
    /// reserved, and reserved ones it grows into first. None the heap took
    /// otherwise covers a charge, since what it holds there is not charged.
    pub fn charge(&mut self, bytes: u64) -> bool {
        let Some(charged) = self.charged.checked_add(bytes) else { return false };
        let need = charged.div_ceil(SUPER);
        if need > self.entitled {
            let more = need - self.entitled;
            if !self.spare(more) {
                return false;
            }
            self.entitled = need;
            self.reserved += more;
        }
        self.charged = charged;
        true
    }

    /// Give back a charge of `bytes`. Superframes stay reserved for one more
    /// than the charges need, so a charge and its uncharge at a boundary do
    /// not take and give back one each time.
    pub fn uncharge(&mut self, bytes: u64) {
        self.charged = self.charged.checked_sub(bytes).unwrap_or_else(|| panic!("frames: {bytes} uncharged of {}", self.charged));
        let need = self.charged.div_ceil(SUPER) + 1;
        if self.entitled > need {
            self.entitled = need;
            self.drawn = self.drawn.min(need);
            self.reserved = need - self.drawn;
            self.refill();
        }
    }

    /// A superframe for the kernel heap to grow by: a reserved one, or else
    /// one for the kernel's own use.
    pub fn alloc_heap(&mut self) -> Option<u64> {
        let phys = match self.reserved {
            0 => self.alloc_super()?,
            _ => {
                let phys = self.take_run(1).expect("frames: a reserved superframe is whole");
                self.reserved -= 1;
                self.drawn += 1;
                phys
            }
        };
        self.heap += 1;
        Some(phys)
    }

    /// Give back a superframe [`alloc_heap`](Self::alloc_heap) handed out,
    /// which no copy pins: one the heap took otherwise goes first, and one it
    /// grew into for charges is reserved for them again.
    pub fn free_heap(&mut self, phys: u64, pins: &Pins) {
        self.give_back_super(phys, pins);
        let (s, _) = self.index(phys);
        assert!(self.supers[s].state == State::Whole, "frames: the heap's superframe {phys:#x} is pinned");
        self.heap -= 1;
        if self.heap < self.drawn {
            self.drawn -= 1;
            self.reserved += 1;
        }
        self.refill();
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

    /// Whether a copy holds the frame at `phys` pinned.
    pub fn pinned(&self, phys: u64) -> bool {
        self.word(phys).is_some_and(|word| word.load(Ordering::Acquire) & COUNT != 0)
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

    /// What is promised is refused to every other allocation and drawn by
    /// its holder alone; a promise past what is free, less the superframes
    /// kept, is refused whole.
    #[test]
    fn a_promise_holds_its_frames_against_every_other_allocation() {
        let mut m = whole(3);
        m.frames.keep(1);
        let all = 2 * PER_SUPER as u64;
        assert!(!m.frames.promise(all + 1), "a promise into the kept superframe");
        assert!(m.frames.promise(all - 10));
        assert_eq!(m.frames.promised(), all - 10);
        assert_eq!(m.frames.alloc_run_for_process(1), None, "a superframe out of promised frames");
        let free: Vec<u64> = core::iter::from_fn(|| m.frames.alloc_frame_for_process()).collect();
        assert_eq!(free.len(), 10, "only the unpromised frames go to anyone else");
        assert!(!m.frames.promise(1), "nothing is left to promise");

        let drawn: Vec<u64> = (0..all - 10).map(|_| m.frames.alloc_promised()).collect();
        assert_eq!((m.frames.promised(), m.frames.free_frames()), (0, PER_SUPER as u64));

        // A frame a commitment still holds, given back, is promised to it
        // again; one given back by none is anyone's.
        assert_eq!(m.frames.discard(drawn[0], &m.pins), Discarded::Freed);
        assert_eq!(m.frames.promised(), 1);
        assert_eq!(m.frames.alloc_frame_for_process(), None, "a frame promised again went to another");
        m.frames.free_frame(drawn[1], &m.pins);
        assert!(m.frames.alloc_frame_for_process().is_some(), "a frame no promise holds was refused");
        assert_eq!(m.frames.alloc_frame_for_process(), None, "the promised one went too");
        m.frames.unpromise(1);
        assert!(m.frames.alloc_frame_for_process().is_some(), "an unpromised frame stayed held");
    }

    /// A kept superframe is the kernel's alone: refused to whatever a process
    /// causes, a frame or a run, and taken by the kernel only once nothing
    /// spare is left. It is kept again when one comes back whole.
    #[test]
    fn a_kept_superframe_goes_to_the_kernel_alone() {
        let mut m = whole(2);
        m.frames.keep(1);
        let all = PER_SUPER as u64;
        let tables: Vec<u64> = core::iter::from_fn(|| m.frames.alloc_frame_for_process()).collect();
        assert_eq!(tables.len() as u64, all, "a table took a frame of the kept superframe");
        assert_eq!(m.frames.alloc_run_for_process(1), None, "a process's run took the kept superframe");
        assert_eq!(m.frames.held_back(), (0, 1));
        let kernel = m.frames.alloc_frame().expect("the kernel was refused a kept frame");
        assert_eq!(m.frames.held_back(), (0, 0), "a kept superframe split for the kernel stayed kept");
        for &f in &tables {
            m.frames.free_frame(f, &m.pins);
        }
        assert_eq!(m.frames.held_back(), (0, 1), "a superframe back whole was not kept again");
        m.frames.free_frame(kernel, &m.pins);
        assert!(m.frames.alloc_super().is_some());
        assert_eq!(m.frames.held_back(), (0, 1), "the kernel's own superframe came from the kept one while one was spare");
    }

    /// Charges reserve whole superframes before the heap grows, and the heap
    /// grows into them first; a charge they cannot cover is refused whole,
    /// and an uncharge gives back all but one superframe of slack.
    #[test]
    fn a_charge_reserves_what_the_heap_grows_into() {
        let mut m = whole(4);
        m.frames.keep(1);
        assert!(m.frames.charge(SUPER + 1));
        assert_eq!(m.frames.held_back(), (2, 1));
        assert!(!m.frames.charge(2 * SUPER), "a charge into the kept superframe");
        assert_eq!((m.frames.charges(), m.frames.held_back()), ((SUPER + 1, 0), (2, 1)), "a refused charge left something behind");
        assert!(!m.frames.promise(PER_SUPER as u64 + 1), "a promise into the reserved superframes");
        assert!(m.frames.promise(PER_SUPER as u64));

        let first = m.frames.alloc_heap().unwrap();
        let second = m.frames.alloc_heap().unwrap();
        assert_eq!(m.frames.held_back(), (0, 1));
        assert!(m.frames.alloc_heap().is_some());
        assert_eq!(m.frames.held_back(), (0, 0), "the heap past its charges took no kept superframe");
        assert_eq!(m.frames.alloc_heap(), None);
        assert_eq!((0..PER_SUPER).map(|_| m.frames.alloc_promised()).count(), PER_SUPER, "the heap took a promised frame");

        m.frames.free_heap(first, &m.pins);
        assert_eq!(m.frames.held_back(), (0, 1), "the superframe past the charges was not kept again");
        m.frames.free_heap(second, &m.pins);
        assert_eq!(m.frames.held_back(), (1, 1), "a superframe the charges cover was not reserved again");
        m.frames.uncharge(SUPER + 1);
        assert_eq!(m.frames.held_back(), (0, 1), "a superframe past one of slack stayed reserved");
    }

    /// What the heap took for the kernel's own use, its boot-time structures
    /// say, covers no charge: a charge reserves its superframes beside them.
    #[test]
    fn the_heaps_own_superframes_cover_no_charge() {
        let mut m = whole(4);
        let own = [m.frames.alloc_heap().unwrap(), m.frames.alloc_heap().unwrap()];
        assert!(m.frames.charge(1));
        assert_eq!(m.frames.held_back(), (1, 0), "a charge leaned on the heap's own superframes");
        m.frames.free_heap(own[0], &m.pins);
        assert_eq!(m.frames.held_back(), (1, 0));
        let drawn = m.frames.alloc_heap().unwrap();
        assert_eq!(m.frames.held_back(), (0, 0));
        m.frames.free_heap(own[1], &m.pins);
        assert_eq!(m.frames.held_back(), (0, 0), "the heap's own superframe went back as the charge's");
        m.frames.free_heap(drawn, &m.pins);
        assert_eq!(m.frames.held_back(), (1, 0), "the charge's superframe was not reserved again");
    }

    /// One superframe of slack stays reserved past what the charges need, so
    /// charges that rise and fall across a boundary do not take and give one
    /// back each time.
    #[test]
    fn an_uncharge_keeps_one_superframe_of_slack() {
        let mut m = whole(4);
        assert!(m.frames.charge(1));
        m.frames.uncharge(1);
        assert_eq!(m.frames.held_back(), (1, 0));
        assert!(m.frames.charge(SUPER));
        assert_eq!(m.frames.held_back(), (1, 0));
        assert!(m.frames.charge(1));
        assert_eq!(m.frames.held_back(), (2, 0));
        m.frames.uncharge(SUPER);
        assert_eq!(m.frames.held_back(), (2, 0));
        m.frames.uncharge(1);
        assert_eq!((m.frames.charges(), m.frames.held_back()), ((0, 0), (1, 0)));
    }

    /// A frame a copy pins is kept by a discard rather than freed: freed, it
    /// would come back only at its last unpin, and the promise made for it
    /// meanwhile would be backed by no free frame, so a touch of the page, or
    /// any other promised page, could find none.
    #[test]
    fn a_discard_keeps_a_pinned_frame_and_promises_only_what_came_back() {
        let mut m = whole(1);
        let all = PER_SUPER as u64;
        assert!(m.frames.promise(all));
        let drawn: Vec<u64> = (0..all).map(|_| m.frames.alloc_promised()).collect();
        assert!(m.pins.pin(drawn[0]));
        assert_eq!(m.frames.discard(drawn[0], &m.pins), Discarded::Kept, "a pinned frame was freed");
        assert_eq!(m.frames.discard(drawn[1], &m.pins), Discarded::Freed);
        assert_eq!((m.frames.promised(), m.frames.free_frames()), (1, 1));
        // The touch of the page whose frame went finds its frame.
        assert_eq!(m.frames.alloc_promised(), drawn[1]);
        assert_eq!(m.pins.unpin(drawn[0]), Unpinned::Kept, "a kept frame was deferred");
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

    /// However a process's 4 KiB traffic fragments memory, the heap always
    /// finds a whole superframe for what its charges cover, and a promised
    /// frame is always there to draw: a deterministic mix of promises,
    /// draws, discards, a process's tables and runs, the kernel's own frames
    /// and charges, with the heap grown whenever its charges outrun it.
    #[test]
    fn fragmentation_never_takes_what_charges_or_promises_hold() {
        let supers = 16;
        let mut m = whole(supers);
        m.frames.keep(2);
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut drawn, mut tables, mut runs, mut kernel, mut heap, mut charges) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut promised = 0u64;
        for _ in 0..40_000 {
            match next() % 12 {
                0 => {
                    let n = next() % 300;
                    if m.frames.promise(n) {
                        promised += n;
                    }
                }
                1 | 2 if promised > 0 => {
                    drawn.push(m.frames.alloc_promised());
                    promised -= 1;
                }
                3 if !drawn.is_empty() => {
                    // A discard keeps the page committed: its frame is promised again.
                    let f = drawn.swap_remove(next() as usize % drawn.len());
                    assert_eq!(m.frames.discard(f, &m.pins), Discarded::Freed);
                    promised += 1;
                }
                4 if !drawn.is_empty() => {
                    // Every 7th drawn frame goes back unpromised, scattering holes.
                    let i = next() as usize % drawn.len();
                    if i.is_multiple_of(7) {
                        m.frames.free_frame(drawn.swap_remove(i), &m.pins);
                    }
                }
                5 => tables.extend(m.frames.alloc_frame_for_process()),
                6 if !tables.is_empty() => m.frames.free_frame(tables.swap_remove(next() as usize % tables.len()), &m.pins),
                7 => runs.extend(m.frames.alloc_run_for_process(1)),
                8 if !runs.is_empty() => m.frames.free_super(runs.swap_remove(next() as usize % runs.len()), &m.pins),
                9 => {
                    let bytes = next() % (SUPER / 2);
                    if m.frames.charge(bytes) {
                        charges.push(bytes);
                    }
                }
                10 if !charges.is_empty() => m.frames.uncharge(charges.swap_remove(next() as usize % charges.len())),
                11 => match next() % 2 {
                    0 => kernel.extend(m.frames.alloc_frame()),
                    _ if !kernel.is_empty() => m.frames.free_frame(kernel.swap_remove(next() as usize % kernel.len()), &m.pins),
                    _ => {}
                },
                _ => {}
            }
            // The heap holds exactly what its charges need, as one that grows
            // only for them would.
            let need = m.frames.charges().0.div_ceil(SUPER) as usize;
            while heap.len() < need {
                heap.push(m.frames.alloc_heap().expect("the heap found no superframe its charges reserved"));
            }
            while heap.len() > need {
                m.frames.free_heap(heap.pop().unwrap(), &m.pins);
            }
            assert_eq!(m.frames.promised(), promised);
        }
        while promised > 0 {
            drawn.push(m.frames.alloc_promised());
            promised -= 1;
        }
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
