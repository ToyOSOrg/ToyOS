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
//! and the heap's growth it covers finds a superframe. Charges count bytes,
//! not the heap's footprint: growth that fragmentation adds past them takes
//! spare superframes, then kept ones
//! (`issues/the-kernel-heaps-fragmentation-is-charged-to-nobody.md`).
//!
//! **Everything a process causes is counted to its account's [`Ledger`],
//! and an account's floor is held back against every other account.** What
//! a floor account does not use of its floor is held as frames, and as the
//! whole superframes it makes, so that it can still be promised, given
//! frames, lent a run and grow its own heap pool however short every other account
//! runs. What it gives back is held for its floor again as it comes back:
//! a frame a copy still pins comes back with its last unpin, and a whole
//! superframe only once one is whole. An ordinary account has no floor and
//! no limit. A refused account's [`ready`](Frames::ready) answers whether
//! what it was refused would be granted now, and
//! [`take_returned`](Frames::take_returned) once it may be.
//!
//! The storage is the caller's, sized by [`supers_for`] and [`pins_for`] for
//! the span of physical memory it manages, because the kernel lays it out of
//! that memory before it has a heap.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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

/// One account's memory: what it holds, and the floor held back for it.
/// Written only under the allocator's lock, by the [`Frames`] methods that
/// take it, and read by anyone.
#[derive(Default)]
pub struct Ledger {
    /// Frames held back for it, however short every other account runs:
    /// zero for an ordinary account.
    floor: AtomicU64,
    /// Pages committed to it, promised or drawn; frames the kernel holds for
    /// it one by one, its page tables' and its pipes' bytes; superframes of
    /// its runs.
    committed: AtomicU64,
    held: AtomicU64,
    runs: AtomicU64,
    /// Heap bytes charged to it, those of them charged to its own pool, and
    /// the superframes that pool is entitled to.
    heap: AtomicU64,
    own: AtomicU64,
    entitled: AtomicU64,
    /// Its footprint once its last refused request would have been granted,
    /// and how many of its requests were refused.
    wanted: AtomicU64,
    refusals: AtomicU64,
    /// That request: its frames, the whole superframes among them, and how
    /// many of those it needed in a row; and the round of waits it is armed
    /// in ([`Frames::ready`]).
    asked: AtomicU64,
    asked_supers: AtomicU64,
    asked_row: AtomicU64,
    armed: AtomicU64,
    peak: AtomicU64,
}

/// A [`Ledger`] read at one moment: frames, superframes and bytes as there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub floor: u64,
    pub committed: u64,
    pub held: u64,
    pub runs: u64,
    pub heap: u64,
    pub entitled: u64,
    pub wanted: u64,
    pub refusals: u64,
    pub peak: u64,
}

/// What one charge holds of each heap pool. Each growth takes from the pool
/// its job charges now, so a charge made before its job had a floor grows
/// from the floor once it has one; what it gives back leaves the job's own
/// pool first.
#[derive(Default, Debug)]
pub struct PoolSplit {
    shared: u64,
    own: u64,
}

impl PoolSplit {
    pub const NONE: Self = Self { shared: 0, own: 0 };

    pub fn bytes(&self) -> u64 {
        self.shared + self.own
    }
}

/// Which heap ledger a charge was taken from, and is given back to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pool {
    /// The one every account without a floor shares.
    Shared,
    /// The account's own: a floor account's, entitled to its superframes.
    Own,
}

impl Ledger {
    pub const fn new() -> Self {
        Self {
            floor: AtomicU64::new(0),
            committed: AtomicU64::new(0),
            held: AtomicU64::new(0),
            runs: AtomicU64::new(0),
            heap: AtomicU64::new(0),
            own: AtomicU64::new(0),
            entitled: AtomicU64::new(0),
            wanted: AtomicU64::new(0),
            refusals: AtomicU64::new(0),
            asked: AtomicU64::new(0),
            asked_supers: AtomicU64::new(0),
            asked_row: AtomicU64::new(0),
            armed: AtomicU64::new(0),
            peak: AtomicU64::new(0),
        }
    }

    pub fn usage(&self) -> Usage {
        let read = |word: &AtomicU64| word.load(Ordering::Relaxed);
        Usage {
            floor: read(&self.floor),
            committed: read(&self.committed),
            held: read(&self.held),
            runs: read(&self.runs),
            heap: read(&self.heap),
            entitled: read(&self.entitled),
            wanted: read(&self.wanted),
            refusals: read(&self.refusals),
            peak: read(&self.peak),
        }
    }

    /// Whether it holds nothing at all.
    pub fn is_empty(&self) -> bool {
        let u = self.usage();
        (u.committed, u.held, u.runs, u.heap) == (0, 0, 0, 0)
    }

    /// The heap pool a charge it takes now is taken from: its own once it
    /// has a floor.
    fn pool(&self) -> Pool {
        if self.floor() > 0 { Pool::Own } else { Pool::Shared }
    }

    fn floor(&self) -> u64 {
        self.floor.load(Ordering::Relaxed)
    }

    /// Frames counted against its floor: its own pool's whole entitlement,
    /// since that is what is held for its heap.
    fn floor_used(&self) -> u64 {
        let u = self.usage();
        u.committed + u.held + PER_SUPER as u64 * (u.runs + u.entitled)
    }

    /// Frames of its floor it does not use.
    fn unused(&self) -> u64 {
        self.floor().saturating_sub(self.floor_used())
    }

    /// The frames it holds, its heap rounded up.
    fn footprint(&self) -> u64 {
        let u = self.usage();
        u.committed + u.held + PER_SUPER as u64 * u.runs + u.heap.div_ceil(FRAME)
    }

    fn add(word: &AtomicU64, n: u64) {
        word.fetch_add(n, Ordering::Relaxed);
    }

    fn sub(word: &AtomicU64, n: u64, what: &str) {
        let was = word.fetch_sub(n, Ordering::Relaxed);
        assert!(was >= n, "frames: {n} {what} given back of {was}");
    }

    /// Its last refused request: frames, whole superframes, those in a row.
    fn asked(&self) -> (u64, u64, u64) {
        let read = |word: &AtomicU64| word.load(Ordering::Relaxed);
        (read(&self.asked), read(&self.asked_supers), read(&self.asked_row))
    }

    fn grew(&self) {
        self.peak.fetch_max(self.footprint(), Ordering::Relaxed);
    }
}

/// What an allocation for a floor account leaves held back for floors.
#[derive(Clone, Copy)]
struct Floors {
    unused: u64,
    held: u64,
    supers: u64,
    whole: u64,
}

/// The least any waiter for memory lacks, as [`Frames::met`] weighs it:
/// frames, whole superframes if it needs any, and a row if it needs one.
#[derive(Clone, Copy)]
struct Want {
    frames: u64,
    supers: Option<Supers>,
    row: u64,
}

/// Whole superframes a waiter lacks, counted two ways against those held
/// for floors: beside every floor's, and beside as many as the floors' held
/// frames make.
#[derive(Clone, Copy)]
struct Supers {
    beside_floors: u64,
    beside_held: u64,
}

impl Want {
    fn least(self, other: Self) -> Self {
        Self {
            frames: self.frames.min(other.frames),
            supers: self.supers.zip(other.supers).map(|(a, b)| Supers {
                beside_floors: a.beside_floors.min(b.beside_floors),
                beside_held: a.beside_held.min(b.beside_held),
            }),
            row: self.row.min(other.row),
        }
    }
}

/// The allocator. Every method but the constructors takes the caller's lock.
///
/// Invariants: `whole ≥ reserved + kept + floors.whole`, `promised +
/// PER_SUPER × (reserved + kept) + floors.held ≤ free` and `PER_SUPER ×
/// floors.whole ≤ floors.held ≤ floors.unused`, so what is promised fits
/// outside what is held back and a draw never has to split a superframe held
/// whole.
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
    /// Bytes charged to the shared pool and the superframes covering them;
    /// the superframes every pool is entitled to; those the heap holds, and
    /// how many of these it grew into from what charges reserved.
    charged: u64,
    shared: u64,
    entitled: u64,
    heap: u64,
    drawn: u64,
    /// What is held back for floors: every floor account's unused frames,
    /// those of them held now (fewer while what came back is still pinned),
    /// and the whole superframes they make, per account, and those held now
    /// (fewer while no whole one has come back for them).
    floors: Floors,
    /// What the waiters of this round of waits for memory lack, the round,
    /// and whether enough came back since one asked.
    want: Option<Want>,
    round: u64,
    returned: bool,
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
            shared: 0,
            entitled: 0,
            heap: 0,
            drawn: 0,
            floors: Floors { unused: 0, held: 0, supers: 0, whole: 0 },
            want: None,
            round: 1,
            returned: false,
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

    /// Bytes charged to the shared pool, and the superframes the heap holds.
    pub fn charges(&self) -> (u64, u64) {
        (self.charged, self.heap)
    }

    /// Frames held back for floors, and the whole superframes among them.
    pub fn floors_held(&self) -> (u64, u64) {
        (self.floors.held, self.floors.whole)
    }

    /// Free frames nothing holds back: neither promised, held for a floor,
    /// nor in a superframe reserved or kept.
    fn room(&self) -> u64 {
        self.room_beside(self.floors.held)
    }

    /// [`room`](Self::room) with `held` frames held for floors.
    fn room_beside(&self, held: u64) -> u64 {
        let held = self.promised + PER_SUPER as u64 * (self.reserved + self.kept) + held;
        self.free.checked_sub(held).unwrap_or_else(|| panic!("frames: {held} held back of {} free", self.free))
    }

    /// Whole superframes nothing holds back.
    fn spare_supers(&self) -> u64 {
        self.spare_beside(self.floors.whole)
    }

    /// [`spare_supers`](Self::spare_supers) with `whole` held for floors.
    fn spare_beside(&self, whole: u64) -> u64 {
        let held = self.reserved + self.kept + whole;
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

    /// Hold back what came back: superframes kept towards the target first,
    /// then the frames of floors, then the whole superframes they make; and
    /// say so, once, if a waiter's want is met.
    fn refill(&mut self) {
        while self.kept < self.keep_target && self.spare(1) {
            self.kept += 1;
        }
        let back = (self.floors.unused - self.floors.held).min(self.room());
        self.floors.held += back;
        let most = self.floors.supers.min(self.floors.held / PER_SUPER as u64);
        while self.floors.whole < most && self.spare_supers() > 0 {
            self.floors.whole += 1;
        }
        if self.want.is_some_and(|want| self.met(want)) {
            self.want = None;
            self.round += 1;
            self.returned = true;
        }
    }

    /// What is held back for floors once `l`, `more` frames past what it now
    /// counts against its floor, has taken them.
    fn floors_after(&self, l: &Ledger, more: u64) -> Floors {
        let (before, after) = (l.unused(), l.floor().saturating_sub(l.floor_used().saturating_add(more)));
        let per = PER_SUPER as u64;
        let unused = self.floors.unused - before + after;
        let supers = self.floors.supers - before / per + after / per;
        let held = self.floors.held.min(unused);
        Floors { unused, held, supers, whole: self.floors.whole.min(supers).min(held / per) }
    }

    /// Whether `l` may take `frames` frames and `supers` whole superframes,
    /// `used` of them counted against its floor: what is held back for every
    /// other floor stays, and what its own floor holds may go. The caller
    /// takes them and holds back the floors this answers.
    fn admit(&self, l: &Ledger, used: u64, frames: u64, supers: u64) -> Option<Floors> {
        let floors = self.floors_after(l, used);
        let fits = self.room_beside(floors.held) >= frames + supers * PER_SUPER as u64 && self.spare_beside(floors.whole) >= supers;
        fits.then_some(floors)
    }

    /// `l` was refused `frames` more, `supers` of them whole superframes and
    /// `row` of those in a row: what a wait for memory to come back waits for,
    /// and what `l` lacks now if it waits.
    fn refused(&mut self, l: &Ledger, frames: u64, supers: u64, row: u64) {
        l.wanted.store(l.footprint().saturating_add(frames), Ordering::Relaxed);
        l.asked.store(frames, Ordering::Relaxed);
        l.asked_supers.store(supers, Ordering::Relaxed);
        l.asked_row.store(row, Ordering::Relaxed);
        l.refusals.fetch_add(1, Ordering::Relaxed);
        self.rearm(l);
    }

    /// `change` gives back some of what `l` holds: its floor holds what it no
    /// longer uses, as frames come back.
    fn give_back_for(&mut self, l: &Ledger, change: impl FnOnce(&Ledger)) {
        let per = PER_SUPER as u64;
        let before = l.unused();
        change(l);
        let after = l.unused();
        self.floors.unused = self.floors.unused - before + after;
        self.floors.supers = self.floors.supers - before / per + after / per;
        assert!(after >= before, "frames: a give-back took {} of a floor", before - after);
        self.rearm(l);
        self.refill();
    }

    /// Hold `frames` back for `l` against every other account: refused,
    /// changing nothing, where what is free beside everything held back
    /// cannot hold the more it adds now. A floor set lower lets go of
    /// what it no longer holds.
    pub fn set_floor(&mut self, l: &Ledger, frames: u64) -> bool {
        let per = PER_SUPER as u64;
        let (before, after) = (l.unused(), frames.saturating_sub(l.floor_used()));
        let (more, supers) = (after.saturating_sub(before), (after / per).saturating_sub(before / per));
        if after > before && (self.room() < more || self.spare_supers() < supers) {
            return false;
        }
        l.floor.store(frames, Ordering::Relaxed);
        self.floors.unused = self.floors.unused - before + after;
        self.floors.supers = self.floors.supers - before / per + after / per;
        self.floors.held = (self.floors.held + more).min(self.floors.unused);
        self.floors.whole = (self.floors.whole + supers).min(self.floors.supers).min(self.floors.held / per);
        // What its own pool kept entitled for the floor it had goes as its
        // charges no longer need it.
        let entitled = l.entitled.load(Ordering::Relaxed);
        let keep = l.own.load(Ordering::Relaxed).div_ceil(SUPER).max(entitled.min(frames.div_ceil(per)));
        if entitled > keep {
            self.unentitle(entitled - keep);
            l.entitled.store(keep, Ordering::Relaxed);
        }
        self.rearm(l);
        self.refill();
        true
    }

    /// Whether what `l` was last refused would be granted now; if not, the
    /// next [`take_returned`](Self::take_returned) answers once it may be.
    pub fn ready(&mut self, l: &Ledger) -> bool {
        let (frames, supers, row) = l.asked();
        if self.admit(l, frames, frames - supers * PER_SUPER as u64, supers).is_some() && self.in_a_row(row) {
            return true;
        }
        self.arm(l);
        false
    }

    /// Take the least of what `l` lacks and what this round's waiters lack.
    /// What it lacks is its request less what its own floor gives it:
    /// [`admit`](Self::admit) lets the request take that much of what is held
    /// for floors, as frames and as the whole superframes they make.
    fn arm(&mut self, l: &Ledger) {
        let per = PER_SUPER as u64;
        let (frames, supers, row) = l.asked();
        let unused = l.unused();
        let rest = unused.saturating_sub(frames);
        let (own, own_supers) = (unused - rest, unused / per - rest / per);
        let lacks = Want {
            frames: frames - own,
            supers: (supers > 0).then(|| Supers {
                beside_floors: supers - own_supers.min(supers),
                beside_held: supers - own.div_ceil(per).min(supers),
            }),
            row: if row > 1 { row } else { 0 },
        };
        self.want = Some(self.want.map_or(lacks, |want| want.least(lacks)));
        l.armed.store(self.round, Ordering::Relaxed);
    }

    /// Arm `l` again if it waits this round: what it gave back of its floor
    /// may be what it lacked.
    fn rearm(&mut self, l: &Ledger) {
        if l.armed.load(Ordering::Relaxed) == self.round {
            self.arm(l);
        }
    }

    /// Whether some waiter may be admitted what it lacked, `want` being the
    /// least they lack. Asked at the end of [`refill`](Self::refill), where
    /// floors are short of frames only once no room is left, and of whole
    /// superframes only once none is spare: then each test is necessary for
    /// some waiter's grant, and for one waiter without a floor sufficient.
    fn met(&self, want: Want) -> bool {
        let short = self.floors.unused - self.floors.held;
        let whole = self.spare_supers() + self.floors.whole;
        let made = self.floors.held / PER_SUPER as u64;
        self.room() >= want.frames.saturating_add(short)
            && want.supers.is_none_or(|lacks| {
                whole >= self.floors.supers + lacks.beside_floors || whole >= made + lacks.beside_held
            })
            && self.in_a_row(want.row)
    }

    /// Whether `count` whole superframes lie in a row, as a run of more than
    /// one takes them.
    fn in_a_row(&self, count: u64) -> bool {
        if count <= 1 {
            return true;
        }
        let mut run = 0;
        self.supers.iter().any(|s| {
            run = if s.state == State::Whole { run + 1 } else { 0 };
            run >= count
        })
    }

    /// Whether memory came back that a waiter wanted, since this last
    /// answered `true`.
    pub fn take_returned(&mut self) -> bool {
        core::mem::take(&mut self.returned)
    }

    /// Promise `frames` to `l`'s commitment if nothing held back for the
    /// kernel or another account is needed for them.
    pub fn promise(&mut self, l: &Ledger, frames: u64) -> bool {
        let Some(floors) = self.admit(l, frames, frames, 0) else {
            self.refused(l, frames, 0, 0);
            return false;
        };
        self.floors = floors;
        self.promised += frames;
        Ledger::add(&l.committed, frames);
        l.grew();
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
        self.give_back(phys, pins);
        self.repromise(1);
        self.refill();
        Discarded::Freed
    }

    /// Promise `frames` again, which a commitment still holds and whose
    /// frames just came back free.
    fn repromise(&mut self, frames: u64) {
        self.promised += frames;
        let _ = self.room();
    }

    /// Take back promises of `frames` none of `l`'s commitments holds any
    /// more, which were never drawn.
    pub fn unpromise(&mut self, l: &Ledger, frames: u64) {
        assert!(frames <= self.promised, "frames: {frames} unpromised of {} promised", self.promised);
        self.promised -= frames;
        self.give_back_for(l, |l| Ledger::sub(&l.committed, frames, "committed pages"));
    }

    /// Give back `frames`, drawn for pages `l` no longer commits; a pinned
    /// one is taken back by its last unpin instead.
    pub fn free_drawn(&mut self, l: &Ledger, frames: impl IntoIterator<Item = u64>, pins: &Pins) {
        let mut count = 0;
        for phys in frames {
            self.give_back(phys, pins);
            count += 1;
        }
        self.give_back_for(l, |l| Ledger::sub(&l.committed, count, "committed pages"));
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
            // The rest of the superframe kept no longer is room a waiter may lack.
            let frame = self.take_frame();
            self.refill();
            return frame;
        }
        self.take_frame()
    }

    /// `count` frames the kernel holds for `l`, such as page tables, which
    /// are not promised, each handed to `each`: refused whole, nothing taken,
    /// rather than take one held back.
    pub fn alloc_held(&mut self, l: &Ledger, count: u64, mut each: impl FnMut(u64)) -> bool {
        let Some(floors) = self.admit(l, count, count, 0) else {
            self.refused(l, count, 0, 0);
            return false;
        };
        self.floors = floors;
        for _ in 0..count {
            each(self.take_frame().expect("frames: an admitted frame is free"));
        }
        Ledger::add(&l.held, count);
        l.grew();
        true
    }

    /// Give back `frames`, which [`alloc_held`](Self::alloc_held) took for
    /// `l`; a pinned one is taken back by its last unpin instead.
    pub fn free_held(&mut self, l: &Ledger, frames: impl IntoIterator<Item = u64>, pins: &Pins) {
        let mut count = 0;
        for phys in frames {
            self.give_back(phys, pins);
            count += 1;
        }
        self.give_back_for(l, |l| Ledger::sub(&l.held, count, "held frames"));
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

    /// `count` superframes in a row for `l`, such as memory a device is lent:
    /// refused rather than take a superframe held back.
    pub fn alloc_run_for(&mut self, l: &Ledger, count: usize) -> Option<u64> {
        let supers = count as u64;
        let frames = supers * PER_SUPER as u64;
        let Some(floors) = self.admit(l, frames, 0, supers) else {
            self.refused(l, frames, supers, supers);
            return None;
        };
        let Some(run) = self.take_run(count) else {
            self.refused(l, frames, supers, supers);
            return None;
        };
        self.floors = floors;
        Ledger::add(&l.runs, supers);
        l.grew();
        Some(run)
    }

    /// Give back a run [`alloc_run_for`](Self::alloc_run_for) took for `l`;
    /// any of its frames still pinned is taken back by its last unpin.
    pub fn free_run_for(&mut self, l: &Ledger, phys: u64, count: usize, pins: &Pins) {
        for s in 0..count as u64 {
            self.give_back_super(phys + s * SUPER, pins);
        }
        self.give_back_for(l, |l| Ledger::sub(&l.runs, count as u64, "run superframes"));
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

    /// Charge `bytes` of the kernel heap to `l`, from `pool`, refused where
    /// covering them would take a superframe held back. What charges cover is
    /// whole superframes: those the heap grew into from what charges
    /// reserved, and reserved ones it grows into first. None the heap took
    /// otherwise covers a charge, since what it holds there is not charged.
    pub fn charge(&mut self, l: &Ledger, pool: Pool, bytes: u64) -> bool {
        let (charged, entitled) = match pool {
            Pool::Shared => (self.charged, self.shared),
            Pool::Own => (l.own.load(Ordering::Relaxed), l.entitled.load(Ordering::Relaxed)),
        };
        let Some(charged) = charged.checked_add(bytes) else {
            self.refused(l, u64::MAX, 0, 0);
            return false;
        };
        let more = charged.div_ceil(SUPER).saturating_sub(entitled);
        if more > 0 {
            let used = match pool {
                Pool::Shared => 0,
                Pool::Own => more * PER_SUPER as u64,
            };
            let Some(floors) = self.admit(l, used, 0, more) else {
                self.refused(l, more * PER_SUPER as u64, more, 0);
                return false;
            };
            self.floors = floors;
            self.entitled += more;
            self.reserved += more;
            match pool {
                Pool::Shared => self.shared += more,
                Pool::Own => Ledger::add(&l.entitled, more),
            }
        }
        match pool {
            Pool::Shared => self.charged = charged,
            Pool::Own => l.own.store(charged, Ordering::Relaxed),
        }
        Ledger::add(&l.heap, bytes);
        l.grew();
        true
    }

    /// Give back `bytes` of a charge to `l` from `pool`. The shared pool
    /// stays entitled to one superframe more than its charges need, so a
    /// charge and its uncharge at a boundary do not take and give back one
    /// each time. A floor account's own pool stays entitled to as much of
    /// its floor as it ever grew into: given back, a superframe the heap
    /// still held would be counted free for the floor and be in use.
    pub fn uncharge(&mut self, l: &Ledger, pool: Pool, bytes: u64) {
        Ledger::sub(&l.heap, bytes, "heap bytes");
        match pool {
            Pool::Shared => {
                self.charged = self.charged.checked_sub(bytes).unwrap_or_else(|| panic!("frames: {bytes} uncharged of {}", self.charged));
                let need = self.charged.div_ceil(SUPER) + 1;
                if self.shared > need {
                    self.unentitle(self.shared - need);
                    self.shared = need;
                    self.refill();
                }
            }
            Pool::Own => {
                Ledger::sub(&l.own, bytes, "heap bytes of its own pool");
                let entitled = l.entitled.load(Ordering::Relaxed);
                let floor = entitled.min(l.floor().div_ceil(PER_SUPER as u64));
                let keep = l.own.load(Ordering::Relaxed).div_ceil(SUPER).max(floor);
                if entitled > keep {
                    self.unentitle(entitled - keep);
                    self.give_back_for(l, |l| Ledger::sub(&l.entitled, entitled - keep, "entitled superframes"));
                }
            }
        }
    }

    /// `bytes` more of `held`, a charge to `l`, from the pool `l` charges now;
    /// refused whole.
    pub fn grow(&mut self, l: &Ledger, held: &mut PoolSplit, bytes: u64) -> bool {
        let pool = l.pool();
        if !self.charge(l, pool, bytes) {
            return false;
        }
        match pool {
            Pool::Shared => held.shared += bytes,
            Pool::Own => held.own += bytes,
        }
        true
    }

    /// `bytes` fewer of `held`, from the job's own pool first.
    pub fn shrink(&mut self, l: &Ledger, held: &mut PoolSplit, bytes: u64) {
        assert!(bytes <= held.bytes(), "frames: {bytes} given back of a charge of {}", held.bytes());
        let own = bytes.min(held.own);
        if own > 0 {
            self.uncharge(l, Pool::Own, own);
            held.own -= own;
        }
        if bytes > own {
            self.uncharge(l, Pool::Shared, bytes - own);
            held.shared -= bytes - own;
        }
    }

    /// `supers` fewer superframes entitled to any pool: reserved ones go
    /// first, and those the heap grew into stay its own.
    fn unentitle(&mut self, supers: u64) {
        self.entitled -= supers;
        self.drawn = self.drawn.min(self.entitled);
        self.reserved = self.entitled - self.drawn;
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
        l: Ledger,
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
        Machine { frames, pins: Pins::new(BASE, words), l: Ledger::new() }
    }

    fn whole(supers: usize) -> Machine {
        machine(supers, &[(BASE, BASE + supers as u64 * SUPER)], &[])
    }

    /// One held frame for `l`, as a page table takes it.
    fn held(frames: &mut Frames, l: &Ledger) -> Option<u64> {
        let mut got = None;
        frames.alloc_held(l, 1, |phys| got = Some(phys)).then(|| got.expect("an admitted frame was handed over"))
    }

    /// A batch is admitted whole or not at all: past what is free it takes
    /// nothing and records what it asked for, and what it took comes back.
    #[test]
    fn held_frames_are_taken_whole_or_not_at_all() {
        let mut m = whole(2);
        m.frames.keep(1);
        let all = PER_SUPER as u64;
        let mut taken = Vec::new();
        assert!(!m.frames.alloc_held(&m.l, all + 1, |phys| taken.push(phys)), "a batch into the kept superframe");
        assert!(taken.is_empty(), "a refused batch handed frames over");
        assert_eq!((m.frames.free_frames(), m.l.usage().held, m.l.usage().refusals), (2 * all, 0, 1));
        assert!(m.frames.alloc_held(&m.l, all, |phys| taken.push(phys)));
        assert_eq!((taken.iter().collect::<BTreeSet<_>>().len() as u64, m.l.usage().held), (all, all));
        m.frames.free_held(&m.l, taken, &m.pins);
        assert_eq!((m.frames.free_frames(), m.l.usage().held), (2 * all, 0));
        assert!(m.l.is_empty());
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
        assert!(!m.frames.promise(&m.l, all + 1), "a promise into the kept superframe");
        assert!(m.frames.promise(&m.l, all - 10));
        assert_eq!(m.frames.promised(), all - 10);
        assert_eq!(m.frames.alloc_run_for(&m.l, 1), None, "a superframe out of promised frames");
        let free: Vec<u64> = core::iter::from_fn(|| held(&mut m.frames, &m.l)).collect();
        assert_eq!(free.len(), 10, "only the unpromised frames go to anyone else");
        assert!(!m.frames.promise(&m.l, 1), "nothing is left to promise");

        let drawn: Vec<u64> = (0..all - 10).map(|_| m.frames.alloc_promised()).collect();
        assert_eq!((m.frames.promised(), m.frames.free_frames()), (0, PER_SUPER as u64));

        // A frame a commitment still holds, given back, is promised to it
        // again; one given back by none is anyone's.
        assert_eq!(m.frames.discard(drawn[0], &m.pins), Discarded::Freed);
        assert_eq!(m.frames.promised(), 1);
        assert_eq!(held(&mut m.frames, &m.l), None, "a frame promised again went to another");
        m.frames.free_drawn(&m.l, [drawn[1]], &m.pins);
        assert!(held(&mut m.frames, &m.l).is_some(), "a frame no promise holds was refused");
        assert_eq!(held(&mut m.frames, &m.l), None, "the promised one went too");
        m.frames.unpromise(&m.l, 1);
        assert!(held(&mut m.frames, &m.l).is_some(), "an unpromised frame stayed held");
    }

    /// A kept superframe is the kernel's alone: refused to whatever a process
    /// causes, a frame or a run, and taken by the kernel only once nothing
    /// spare is left. It is kept again when one comes back whole.
    #[test]
    fn a_kept_superframe_goes_to_the_kernel_alone() {
        let mut m = whole(2);
        m.frames.keep(1);
        let all = PER_SUPER as u64;
        let tables: Vec<u64> = core::iter::from_fn(|| held(&mut m.frames, &m.l)).collect();
        assert_eq!(tables.len() as u64, all, "a table took a frame of the kept superframe");
        assert_eq!(m.frames.alloc_run_for(&m.l, 1), None, "a process's run took the kept superframe");
        assert_eq!(m.frames.held_back(), (0, 1));
        let kernel = m.frames.alloc_frame().expect("the kernel was refused a kept frame");
        assert_eq!(m.frames.held_back(), (0, 0), "a kept superframe split for the kernel stayed kept");
        for &f in &tables {
            m.frames.free_held(&m.l, [f], &m.pins);
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
        assert!(m.frames.charge(&m.l, Pool::Shared, SUPER + 1));
        assert_eq!(m.frames.held_back(), (2, 1));
        assert!(!m.frames.charge(&m.l, Pool::Shared, 2 * SUPER), "a charge into the kept superframe");
        assert_eq!((m.frames.charges(), m.frames.held_back()), ((SUPER + 1, 0), (2, 1)), "a refused charge left something behind");
        assert!(!m.frames.promise(&m.l, PER_SUPER as u64 + 1), "a promise into the reserved superframes");
        assert!(m.frames.promise(&m.l, PER_SUPER as u64));

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
        m.frames.uncharge(&m.l, Pool::Shared, SUPER + 1);
        assert_eq!(m.frames.held_back(), (0, 1), "a superframe past one of slack stayed reserved");
    }

    /// What the heap took for the kernel's own use, its boot-time structures
    /// say, covers no charge: a charge reserves its superframes beside them.
    #[test]
    fn the_heaps_own_superframes_cover_no_charge() {
        let mut m = whole(4);
        let own = [m.frames.alloc_heap().unwrap(), m.frames.alloc_heap().unwrap()];
        assert!(m.frames.charge(&m.l, Pool::Shared, 1));
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
        assert!(m.frames.charge(&m.l, Pool::Shared, 1));
        m.frames.uncharge(&m.l, Pool::Shared, 1);
        assert_eq!(m.frames.held_back(), (1, 0));
        assert!(m.frames.charge(&m.l, Pool::Shared, SUPER));
        assert_eq!(m.frames.held_back(), (1, 0));
        assert!(m.frames.charge(&m.l, Pool::Shared, 1));
        assert_eq!(m.frames.held_back(), (2, 0));
        m.frames.uncharge(&m.l, Pool::Shared, SUPER);
        assert_eq!(m.frames.held_back(), (2, 0));
        m.frames.uncharge(&m.l, Pool::Shared, 1);
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
        assert!(m.frames.promise(&m.l, all));
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

    const PER: u64 = PER_SUPER as u64;

    /// An account with `frames` of floor, set on `m`.
    fn floored(m: &mut Machine, frames: u64) -> Ledger {
        let l = Ledger::new();
        assert!(m.frames.set_floor(&l, frames), "a floor of {frames} frames on an idle machine");
        l
    }

    /// Promise `l` every frame it may still be promised.
    fn exhaust(frames: &mut Frames, l: &Ledger) -> u64 {
        let room = frames.room();
        assert!(frames.promise(l, room));
        assert!(!frames.promise(l, 1), "a frame past the room was promised");
        room
    }

    /// With every frame an ordinary account may take promised to it, the
    /// floor account is promised, tabled and lent a run up to its floor, and
    /// refused past it; the ordinary account is refused each meanwhile.
    #[test]
    fn a_floor_is_held_against_every_other_account() {
        let mut m = whole(8);
        m.frames.keep(1);
        let floor = 2 * PER + 100;
        let f = floored(&mut m, floor);
        exhaust(&mut m.frames, &m.l);
        assert_eq!(held(&mut m.frames, &m.l), None, "an ordinary table took a floor's frame");
        assert_eq!(m.frames.alloc_run_for(&m.l, 1), None, "an ordinary run took a floor's superframe");
        assert!(!m.frames.charge(&m.l, Pool::Shared, SUPER), "an ordinary charge took a floor's superframe");

        let run = m.frames.alloc_run_for(&f, 1).expect("the floor account was refused a run inside its floor");
        assert!(held(&mut m.frames, &f).is_some(), "the floor account was refused a table inside its floor");
        assert!(m.frames.promise(&f, floor - PER - 1), "the floor account was refused the rest of its floor");
        assert!(!m.frames.promise(&f, 1), "the floor account took a frame past its floor");
        assert_eq!(m.frames.floors_held(), (0, 0));
        m.frames.free_run_for(&f, run, 1, &m.pins);
        assert!(!m.frames.promise(&m.l, 1), "a floor's run, given back, went to an ordinary account");
        assert_eq!(m.frames.floors_held(), (PER, 1), "a run given back was not held whole for its floor");
    }

    /// A floor's superframes stay whole: a run another account takes comes
    /// out of whole superframes nothing holds, even where frames scattered
    /// through split ones would cover it.
    #[test]
    fn a_floors_superframes_stay_whole_beside_scattered_room() {
        let mut m = whole(4);
        let f = floored(&mut m, PER);
        // One split superframe: half its frames taken, half free and scattered.
        let tables: Vec<u64> = (0..2 * PER).map(|_| held(&mut m.frames, &m.l).unwrap()).collect();
        for &t in tables.iter().step_by(2) {
            m.frames.free_held(&m.l, [t], &m.pins);
        }
        assert!(m.frames.room() >= PER, "the scattered frames cover a run");
        assert_eq!(m.frames.alloc_run_for(&m.l, 1), Some(BASE + 2 * SUPER), "the one spare whole superframe");
        assert_eq!(m.frames.alloc_run_for(&m.l, 1), None, "a run took the floor's whole superframe");
        assert!(m.frames.alloc_run_for(&f, 1).is_some(), "the floor account found no whole superframe");
    }

    /// A floor account's own heap pool is entitled from its floor, so a
    /// charge needing a new superframe is covered however short the rest of
    /// the machine runs; given back, it stays entitled as far as the floor.
    #[test]
    fn a_floor_accounts_heap_draws_on_its_floor_and_keeps_it() {
        let mut m = whole(8);
        let f = floored(&mut m, 2 * PER);
        exhaust(&mut m.frames, &m.l);
        assert_eq!(f.pool(), Pool::Own);
        assert!(m.frames.charge(&f, Pool::Own, 1));
        assert_eq!(m.frames.held_back().0, 1, "its own pool reserved no superframe");
        assert!(m.frames.charge(&f, Pool::Own, SUPER), "a second superframe inside its floor was refused");
        assert!(!m.frames.charge(&f, Pool::Own, SUPER), "a third superframe past its floor was granted");
        m.frames.uncharge(&f, Pool::Own, SUPER + 1);
        assert_eq!(f.usage().entitled, 2, "its own pool gave back what its floor holds for it");
        assert!(!m.frames.promise(&m.l, 1), "a floor account's uncharge freed a frame for another account");
        assert!(m.frames.charge(&f, Pool::Own, SUPER), "its pool, still entitled, refused a charge");
    }

    /// Past its floor, a floor account competes for the rest as any other
    /// does, and what it gives back is held for its floor first.
    #[test]
    fn past_its_floor_an_account_is_ordinary_and_gives_back_to_its_floor_first() {
        let mut m = whole(4);
        let f = floored(&mut m, PER);
        let room = m.frames.room();
        assert!(m.frames.promise(&f, PER + room), "its floor and the rest");
        assert!(!m.frames.promise(&m.l, 1));
        m.frames.unpromise(&f, 10);
        assert!(m.frames.promise(&m.l, 10), "what it gave back past its floor stayed held");
        m.frames.unpromise(&f, room - 10 + 5);
        assert!(!m.frames.promise(&m.l, room - 5 + 1), "the 5 frames under its floor went to another account");
        assert!(m.frames.promise(&m.l, room - 10));
    }

    /// A floor is set only where it can be held whole now, and set to
    /// nothing lets all of it go.
    #[test]
    fn a_floor_is_set_only_where_it_is_free_and_unset_lets_it_go() {
        let mut m = whole(4);
        let room = exhaust(&mut m.frames, &m.l);
        let f = Ledger::new();
        assert!(!m.frames.set_floor(&f, 1), "a floor set out of frames promised to another account");
        m.frames.unpromise(&m.l, room);
        assert!(m.frames.set_floor(&f, 3 * PER));
        assert_eq!(m.frames.floors_held(), (3 * PER, 3));
        assert!(!m.frames.promise(&m.l, m.frames.free_frames() - 3 * PER + 1));
        assert!(m.frames.set_floor(&f, 0));
        assert_eq!(m.frames.floors_held(), (0, 0));
        assert!(m.frames.promise(&m.l, m.frames.room()));
        assert!(m.frames.room() == 0 && m.frames.free_frames() == 4 * PER);
    }

    /// A charge made before its job had a floor grows from the floor once it
    /// has one, however short the rest of the machine runs, and gives back
    /// the floor's part first.
    #[test]
    fn a_charge_made_before_its_floor_grows_from_it() {
        let mut m = whole(8);
        let f = Ledger::new();
        let mut held = PoolSplit::NONE;
        assert!(m.frames.grow(&f, &mut held, 100));
        assert!(m.frames.set_floor(&f, 2 * PER));
        exhaust(&mut m.frames, &m.l);
        assert!(m.frames.grow(&f, &mut held, SUPER), "a charge made before the floor was refused the floor");
        assert_eq!((m.frames.charges().0, f.usage().entitled), (100, 1), "its growth went to the shared pool");
        m.frames.shrink(&f, &mut held, SUPER + 50);
        assert_eq!((held.bytes(), m.frames.charges().0, f.usage().heap), (50, 50, 50), "the floor's part was not given back first");
        m.frames.shrink(&f, &mut held, 50);
        assert!(f.usage().heap == 0 && held.bytes() == 0);
    }

    /// A floor let go of takes with it what its own heap pool kept entitled
    /// for it, once the pool's charges no longer need it: a job that ends
    /// holds no superframe reserved for nothing.
    #[test]
    fn a_floor_let_go_releases_what_its_pool_kept() {
        let mut m = whole(8);
        let f = floored(&mut m, 2 * PER);
        assert!(m.frames.charge(&f, Pool::Own, SUPER + 1));
        m.frames.uncharge(&f, Pool::Own, SUPER + 1);
        assert_eq!(f.usage().entitled, 2, "its pool kept what its floor holds");
        assert!(m.frames.set_floor(&f, 0));
        assert_eq!((f.usage().entitled, m.frames.held_back().0), (0, 0), "a reserved superframe outlived the floor");
        assert!(f.is_empty());
    }

    /// A refused account's wait for memory is answered once what it lacked
    /// is free, and not before; the answer is given once.
    #[test]
    fn a_wait_for_memory_is_answered_once_what_it_lacked_comes_back() {
        let mut m = whole(4);
        let greedy = Ledger::new();
        exhaust(&mut m.frames, &greedy);
        let starved = Ledger::new();
        assert!(!m.frames.promise(&starved, 300));
        assert_eq!(starved.usage().refusals, 1, "a refusal went uncounted");
        assert!(!m.frames.ready(&starved));
        m.frames.unpromise(&greedy, 299);
        assert!(!m.frames.take_returned(), "answered with 299 of 300 frames back");
        m.frames.unpromise(&greedy, 1);
        assert!(m.frames.take_returned(), "300 frames back went unanswered");
        assert!(!m.frames.take_returned(), "answered twice");
        assert!(m.frames.ready(&starved));
        assert!(m.frames.promise(&starved, 300));
    }

    /// A waiter refused a smaller request while it waits waits for that one.
    #[test]
    fn a_smaller_refusal_while_waiting_is_what_is_waited_for() {
        let mut m = whole(4);
        exhaust(&mut m.frames, &m.l);
        let starved = Ledger::new();
        assert!(!m.frames.promise(&starved, 300));
        assert!(!m.frames.ready(&starved));
        assert!(!m.frames.promise(&starved, 10));
        m.frames.unpromise(&m.l, 10);
        assert!(m.frames.take_returned(), "memory for the smaller request went unanswered");
        assert!(m.frames.ready(&starved));
    }

    /// The kernel taking a frame of a kept superframe frees the rest of it
    /// to the room, which answers a waiter.
    #[test]
    fn a_kept_superframe_split_answers_a_wait() {
        let mut m = whole(2);
        m.frames.keep(1);
        exhaust(&mut m.frames, &m.l);
        let starved = Ledger::new();
        assert!(!m.frames.promise(&starved, 10));
        assert!(!m.frames.ready(&starved));
        assert!(m.frames.alloc_frame().is_some(), "the kernel's frame from its kept superframe");
        assert!(m.frames.take_returned(), "the rest of the superframe went unanswered");
        assert!(m.frames.ready(&starved));
    }

    /// A job refused a whole superframe is not ready while only scattered
    /// frames come back, however many: what it waits for is answered by a
    /// whole superframe, and then once.
    #[test]
    fn a_wait_for_a_superframe_is_answered_by_one_and_not_by_frames() {
        let mut m = whole(4);
        let tables: Vec<u64> = core::iter::from_fn(|| held(&mut m.frames, &m.l)).collect();
        assert_eq!(tables.len(), 4 * PER_SUPER);
        for &t in tables.iter().step_by(2) {
            m.frames.free_held(&m.l, [t], &m.pins);
        }
        assert!(m.frames.room() >= PER && m.frames.spare_supers() == 0, "the frames free are scattered");
        let starved = Ledger::new();
        assert_eq!(m.frames.alloc_run_for(&starved, 1), None);
        assert!(!m.frames.ready(&starved), "ready for a superframe with only scattered frames free");
        // All of the first superframe's frames but one come back: still no whole one.
        let first: Vec<u64> = tables.iter().copied().skip(1).step_by(2).filter(|&t| t < BASE + SUPER).collect();
        for &t in &first[1..] {
            m.frames.free_held(&m.l, [t], &m.pins);
        }
        assert!(!m.frames.take_returned(), "answered with frames back and no whole superframe");
        assert!(!m.frames.ready(&starved));
        m.frames.free_held(&m.l, [first[0]], &m.pins);
        assert!(m.frames.take_returned(), "a whole superframe back went unanswered");
        assert!(!m.frames.take_returned(), "answered twice");
        assert!(m.frames.ready(&starved));
        assert!(m.frames.alloc_run_for(&starved, 1).is_some());
    }

    /// A floor account refused a superframe while its floor's own still comes
    /// back pinned is answered when the pin goes, though nothing else comes
    /// back: its wait counts what its floor holds for it, as its request does.
    #[test]
    fn a_floor_accounts_wait_is_answered_by_its_own_floor() {
        let mut m = whole(4);
        let f = floored(&mut m, PER);
        let table = held(&mut m.frames, &m.l).unwrap();
        assert!(m.pins.pin(table));
        // Every other superframe and frame is taken, not promised: none is spare.
        while m.frames.alloc_run_for(&m.l, 1).is_some() {}
        while held(&mut m.frames, &m.l).is_some() {}
        let run = m.frames.alloc_run_for(&f, 1).expect("a run inside its floor");
        let pinned = run + 9 * FRAME;
        assert!(m.pins.pin(pinned));
        m.frames.free_run_for(&f, run, 1, &m.pins);
        assert_eq!(m.frames.alloc_run_for(&f, 1), None, "a run while its floor's superframe is pinned");
        assert!(!m.frames.ready(&f));
        // Something else is given back, and comes back pinned too: the floor
        // is still short of the frame its superframe needs.
        m.frames.free_held(&m.l, [table], &m.pins);
        assert!(!m.frames.take_returned(), "answered while its floor is short");
        assert_eq!(m.pins.unpin(pinned), Unpinned::Release);
        m.frames.release(pinned);
        assert!(m.frames.take_returned(), "its floor's superframe back went unanswered");
        assert!(!m.frames.take_returned(), "answered twice");
        assert!(m.frames.ready(&f));
        assert!(m.frames.alloc_run_for(&f, 1).is_some());
    }

    /// A wait for a run of superframes is answered by whole ones in a row,
    /// not by as many apart.
    #[test]
    fn a_wait_for_a_run_is_answered_by_a_row() {
        let mut m = whole(6);
        let supers: Vec<u64> = (0..6).map(|_| m.frames.alloc_super().unwrap()).collect();
        m.frames.free_super(supers[0], &m.pins);
        m.frames.free_super(supers[2], &m.pins);
        let starved = Ledger::new();
        assert_eq!(m.frames.alloc_run_for(&starved, 2), None);
        assert!(!m.frames.ready(&starved));
        m.frames.free_super(supers[4], &m.pins);
        assert!(!m.frames.take_returned(), "answered with three whole superframes apart");
        m.frames.free_super(supers[3], &m.pins);
        assert!(m.frames.take_returned(), "a row back went unanswered");
        assert!(m.frames.ready(&starved));
        assert!(m.frames.alloc_run_for(&starved, 2).is_some());
    }

    /// However floors, pins and fragmentation stand, a waiter for memory is
    /// told once what it was refused would be granted: after every step, no
    /// account still waits whose last refused request `admit` would grant.
    #[test]
    fn a_wait_for_memory_is_never_missed() {
        let mut m = whole(12);
        m.frames.keep(1);
        let accounts = [Ledger::new(), floored(&mut m, 2 * PER + 50), floored(&mut m, PER), Ledger::new()];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut drawn: [Vec<u64>; 4] = Default::default();
        let mut tables: [Vec<u64>; 4] = Default::default();
        let mut runs: [Vec<(u64, usize)>; 4] = Default::default();
        let mut pinned: Vec<u64> = Vec::new();
        let mut waiting = [false; 4];
        let (mut posts, mut floor_waits) = (0, 0);
        for step in 0..40_000 {
            // The last account only ever asks, and so only ever waits.
            let a = if next() % 8 == 0 { 3 } else { next() as usize % 3 };
            let l = &accounts[a];
            let refused = match next() % 8 {
                0 if a < 3 => {
                    let n = next() % 700;
                    let ok = m.frames.promise(l, n);
                    if ok {
                        drawn[a].extend((0..n).map(|_| m.frames.alloc_promised()));
                    }
                    !ok
                }
                1 if !drawn[a].is_empty() => {
                    let n = 1 + next() as usize % drawn[a].len().min(400);
                    let gone: Vec<u64> = (0..n).map(|_| drawn[a].swap_remove(next() as usize % drawn[a].len())).collect();
                    for &phys in &gone {
                        if next() % 64 == 0 && m.pins.pin(phys) {
                            pinned.push(phys);
                        }
                    }
                    m.frames.free_drawn(l, gone, &m.pins);
                    false
                }
                2 if !pinned.is_empty() => {
                    let phys = pinned.swap_remove(next() as usize % pinned.len());
                    if m.pins.unpin(phys) == Unpinned::Release {
                        m.frames.release(phys);
                    }
                    false
                }
                3 => {
                    let count = 1 + next() as usize % 2;
                    match m.frames.alloc_run_for(l, count) {
                        Some(run) if a < 3 => {
                            runs[a].push((run, count));
                            false
                        }
                        Some(run) => {
                            m.frames.free_run_for(l, run, count, &m.pins);
                            false
                        }
                        None => true,
                    }
                }
                4 if !runs[a].is_empty() => {
                    let (run, count) = runs[a].swap_remove(next() as usize % runs[a].len());
                    m.frames.free_run_for(l, run, count, &m.pins);
                    false
                }
                5 if a < 3 => match held(&mut m.frames, l) {
                    Some(t) => {
                        tables[a].push(t);
                        false
                    }
                    None => true,
                },
                6 if !tables[a].is_empty() => {
                    let t = tables[a].swap_remove(next() as usize % tables[a].len());
                    m.frames.free_held(l, [t], &m.pins);
                    false
                }
                7 if a == 3 => !m.frames.promise(l, PER + next() % (4 * PER)) || {
                    m.frames.unpromise(l, l.usage().committed);
                    false
                },
                _ => false,
            };
            if refused && !m.frames.ready(l) {
                waiting[a] = true;
                floor_waits += usize::from(l.usage().floor > 0);
            }
            if m.frames.take_returned() {
                posts += 1;
                for (w, l) in waiting.iter_mut().zip(&accounts) {
                    *w = *w && !m.frames.ready(l);
                }
            }
            for (i, l) in accounts.iter().enumerate().filter(|&(i, _)| waiting[i]) {
                let (frames, supers, row) = l.asked();
                let grants = m.frames.admit(l, frames, frames - supers * PER, supers).is_some() && m.frames.in_a_row(row);
                assert!(!grants, "step {step}: account {i} would be granted what it waits for and was not told");
            }
        }
        assert!(posts > 100 && floor_waits > 100, "the model waited too little: {posts} answers, {floor_waits} floor waits");
    }

    /// However ordinary accounts fragment memory around it, an account's
    /// floor is held: at every step, what it does not use is promisable to it
    /// and to no other account, and a promise it is owed always draws.
    #[test]
    fn fragmentation_never_takes_a_floor() {
        let mut m = whole(16);
        m.frames.keep(2);
        let f = floored(&mut m, 3 * PER + 77);
        let mut seed = 0x51f1_5eed_c0ff_ee11u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (mut drawn, mut tables, mut runs, mut floor_drawn) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (mut promised, mut floor_promised) = (0u64, 0u64);
        for step in 0..30_000 {
            match next() % 9 {
                0 => {
                    let n = next() % 600;
                    if m.frames.promise(&m.l, n) {
                        promised += n;
                    }
                }
                1 if promised > 0 => {
                    drawn.push(m.frames.alloc_promised());
                    promised -= 1;
                }
                2 if !drawn.is_empty() => {
                    let i = next() as usize % drawn.len();
                    m.frames.free_drawn(&m.l, [drawn.swap_remove(i)], &m.pins);
                }
                3 => tables.extend(held(&mut m.frames, &m.l)),
                4 if !tables.is_empty() => m.frames.free_held(&m.l, [tables.swap_remove(next() as usize % tables.len())], &m.pins),
                5 => runs.extend(m.frames.alloc_run_for(&m.l, 1)),
                6 if !runs.is_empty() => m.frames.free_run_for(&m.l, runs.swap_remove(next() as usize % runs.len()), 1, &m.pins),
                7 => {
                    let n = next() % 200;
                    if m.frames.promise(&f, n) {
                        floor_promised += n;
                        for _ in 0..n / 2 {
                            floor_drawn.push(m.frames.alloc_promised());
                            floor_promised -= 1;
                        }
                    }
                }
                8 if !floor_drawn.is_empty() => {
                    let n = (next() as usize % floor_drawn.len()).max(1);
                    let gone: Vec<u64> = floor_drawn.drain(..n).collect();
                    m.frames.free_drawn(&f, gone, &m.pins);
                }
                _ => {}
            }
            let unused = f.unused();
            assert_eq!(m.frames.floors_held().0, unused, "step {step}: the floor's frames are not all held");
            if step % 97 == 0 && unused > 0 {
                assert!(!m.frames.promise(&m.l, m.frames.room() + 1), "step {step}: an ordinary promise took a floor's frame");
                assert!(m.frames.promise(&f, unused), "step {step}: the floor account was refused {unused} of its floor");
                m.frames.unpromise(&f, unused);
            }
        }
        while promised + floor_promised > 0 {
            let _ = m.frames.alloc_promised();
            if promised > 0 {
                promised -= 1;
            } else {
                floor_promised -= 1;
            }
        }
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
                    if m.frames.promise(&m.l, n) {
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
                        m.frames.free_drawn(&m.l, [drawn.swap_remove(i)], &m.pins);
                    }
                }
                5 => tables.extend(held(&mut m.frames, &m.l)),
                6 if !tables.is_empty() => m.frames.free_held(&m.l, [tables.swap_remove(next() as usize % tables.len())], &m.pins),
                7 => runs.extend(m.frames.alloc_run_for(&m.l, 1)),
                8 if !runs.is_empty() => m.frames.free_run_for(&m.l, runs.swap_remove(next() as usize % runs.len()), 1, &m.pins),
                9 => {
                    let bytes = next() % (SUPER / 2);
                    if m.frames.charge(&m.l, Pool::Shared, bytes) {
                        charges.push(bytes);
                    }
                }
                10 if !charges.is_empty() => m.frames.uncharge(&m.l, Pool::Shared, charges.swap_remove(next() as usize % charges.len())),
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
