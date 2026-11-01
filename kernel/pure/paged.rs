//! A pager's resident pages: every present page of every paged object it
//! serves, in one table of `capacity` slots made when the pager is, so no
//! supply, read, write or write-back allocates.
//!
//! **A page is present, failed or absent.** A present page holds a frame and
//! its dirty state; a failed one holds no frame and answers `EIO` until it is
//! supplied again; an absent one has no slot. Nothing here waits: a caller
//! that finds a page absent records its request in its own wait record.
//!
//! **A dirty page holds a slot until a write-back cleans it.** Slots are
//! bounded by `dirty_ceiling`. A write-back begin clears the dirty bit and
//! keeps the slot; its end releases the slot of each page that was not
//! dirtied again, not write-pinned since the begin, and written. A dirty page,
//! one under write-back or one write-pinned is never withdrawn.
//!
//! **The size is the object's.** A supply or write that ends past it extends
//! it in the same step, so the size never covers bytes not yet in the object;
//! a shrink removes every page past it, dirty or not.
//!
//! **A holder's read is atomic against every write** (POSIX XSH 2.9.7): it
//! begins only while no write is under way ([`Object::read_begin`]), and its
//! bytes count only where no write has begun since ([`Object::unchanged`]).
//! A write is one call, or, where a call does not take all it was given, every
//! call until [`Object::end_write`]: the pager's whole write.
//!
//! What a removed frame owes (the last pin's free, a zeroed tail) is the
//! caller's: every removal answers its frames.

#![forbid(unsafe_code)]

use crate::chunked::Chunked;

pub use crate::vm::PAGE;

/// The furthest a page of an object ends: every offset, size and range end is
/// at or below it, so no page's end overflows.
pub const LAST_END: u64 = u64::MAX / PAGE * PAGE;

/// Whether `len` bytes from `offset` end at or below [`LAST_END`]: what every
/// offset and length a pager names is checked against before it is used.
pub fn within_reach(offset: u64, len: u64) -> bool {
    offset.checked_add(len).is_some_and(|end| end <= LAST_END)
}

/// The most pages one write-back range covers: a 64-byte bitmap.
pub const RANGE_PAGES: u64 = 512;

/// The write-back ranges a pager has open at once, across its objects.
pub const OPEN_RANGES: usize = 64;

const NONE: u32 = u32::MAX;

/// A pager's core commitment: `core` frames committed at its making and kept
/// for its life, each promised, in the index, out on a supply or held aside
/// for the next one. A frame leaving the index while the core would hold
/// less without it is held aside rather than given back, so no later supply
/// within the core waits on a promise the allocator may refuse.
pub struct Core {
    core: u64,
    pool: u64,
    drawn: u64,
    aside: u64,
    out: u64,
}

/// Where a supply's frame comes from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Draw {
    /// One held aside: the caller's list hands it out.
    Aside,
    /// A promise the core holds.
    Pool,
    /// None the core holds: the caller asks the allocator for a promise.
    Beyond,
}

/// Where a frame that leaves the index, or never entered it, goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leave {
    /// Held aside for the core.
    Aside,
    /// Given back: the core holds enough without it.
    Free,
}

impl Core {
    pub fn new(core: u64) -> Self {
        Self { core, pool: core, drawn: 0, aside: 0, out: 0 }
    }

    /// Frames the core holds now.
    pub fn held(&self) -> u64 {
        self.pool + self.drawn + self.aside + self.out
    }

    /// Promised and not drawn.
    pub fn pool(&self) -> u64 {
        self.pool
    }

    /// A frame for a supply, from aside where `aside_ready` (one held aside is
    /// unpinned), then the pool, else beyond the core.
    pub fn draw(&mut self, aside_ready: bool) -> Draw {
        if aside_ready && self.aside > 0 {
            self.aside -= 1;
            self.out += 1;
            Draw::Aside
        } else if self.pool > 0 {
            self.pool -= 1;
            self.out += 1;
            Draw::Pool
        } else {
            Draw::Beyond
        }
    }

    /// A promise the allocator made beyond the core, now out on a supply.
    pub fn promised(&mut self) {
        self.out += 1;
    }

    /// A frame out on a supply entered the index.
    pub fn installed(&mut self) {
        self.out -= 1;
        self.drawn += 1;
    }

    /// A frame leaves: out of the index where `drawn`, else one out on a
    /// supply that did not enter it.
    pub fn leave(&mut self, drawn: bool) -> Leave {
        match drawn {
            true => self.drawn -= 1,
            false => self.out -= 1,
        }
        if self.held() < self.core {
            self.aside += 1;
            Leave::Aside
        } else {
            Leave::Free
        }
    }

    /// The pager ended: every frame goes back, and the promises with them.
    /// The promises left to take back.
    pub fn end(&mut self) -> u64 {
        let pool = self.pool;
        *self = Self { core: 0, pool: 0, drawn: 0, aside: 0, out: 0 };
        pool
    }
}

/// Why a call on an object was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// The object's pager ended: nothing on it is served again.
    Gone,
    /// The page lies at or past the object's size.
    PastEnd,
    /// Every slot of the pager's table holds a page.
    NoRoom,
    /// The dirty ceiling is reached; the pager writes back first.
    DirtyFull,
    /// A range that is empty, too long, overlaps an open write-back, or names
    /// none that was begun.
    InvalidArgument,
}

/// What a page is, for a read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lookup<F> {
    Present(F),
    Absent,
    Failed,
    PastEnd,
    Gone,
}

/// What a supply did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Supplied {
    Installed,
    /// The page was present; the frame offered is the caller's again.
    Present,
}

#[derive(Clone, Copy)]
struct State {
    dirty: bool,
    writeback: bool,
    write_pins: u16,
    pinned_since_begin: bool,
}

impl State {
    const CLEAN: Self = Self { dirty: false, writeback: false, write_pins: 0, pinned_since_begin: false };

    fn counts_dirty(&self) -> bool {
        self.dirty || self.writeback
    }
}

struct Slot<F> {
    object: u32,
    page: u64,
    /// `None` for a failed page.
    frame: Option<F>,
    state: State,
    prev: u32,
    next: u32,
}

/// One object's part of the table: its size, whether its pager ended, the
/// list of its slots, and its writes under way.
pub struct Object {
    id: u32,
    size: u64,
    gone: bool,
    head: u32,
    pages: usize,
    /// Moved at every write's begin and end, and at every resize.
    version: u64,
    /// Writes under way: each call's, and the held one.
    writing: u32,
    /// A call that did not take all it was given left its write under way.
    held: bool,
    /// What its pager last set: nanoseconds, as the file's volume keeps it.
    mtime: u64,
}

/// What a holder's read began at ([`Object::read_begin`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Version(u64);

impl Object {
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn gone(&self) -> bool {
        self.gone
    }

    /// The pages it holds, present or failed.
    pub fn pages(&self) -> usize {
        self.pages
    }

    fn page_end(&self) -> u64 {
        self.size.div_ceil(PAGE)
    }

    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    pub fn set_mtime(&mut self, mtime: u64) {
        self.mtime = mtime;
    }

    /// The version a holder's stat publishes: `None` while a write is under
    /// way, as for a read.
    pub fn published(&self) -> Option<u64> {
        (self.writing == 0).then_some(self.version)
    }

    /// Where a holder's read begins: `None` while a write is under way.
    pub fn read_begin(&self) -> Option<Version> {
        (self.writing == 0).then_some(Version(self.version))
    }

    /// Whether no write or resize has begun since the read began at `v`.
    pub fn unchanged(&self, v: Version) -> bool {
        self.version == v.0
    }

    /// A write call begins, before its first byte lands.
    pub fn write_begin(&mut self) {
        self.writing = self.writing.checked_add(1).expect("an object's writes overflowed");
        self.version += 1;
    }

    /// A write call ends, after its last byte landed. One that did not take
    /// all it was given leaves the write under way until
    /// [`end_write`](Self::end_write), unless an earlier call's already is.
    pub fn write_end(&mut self, whole: bool) {
        if !whole && !self.held {
            self.held = true;
            return;
        }
        self.writing = self.writing.checked_sub(1).expect("a write ended that never began");
        self.version += 1;
    }

    /// The pager ends the write a short call left under way.
    pub fn end_write(&mut self) -> Result<(), Refused> {
        if !core::mem::take(&mut self.held) {
            return Err(Refused::InvalidArgument);
        }
        self.write_end(true);
        Ok(())
    }
}

/// A pager's table: `capacity` slots and a hash of them, both made here.
pub struct Pages<F> {
    slots: Chunked<Option<Slot<F>>>,
    /// The free slots: a stack, `free_len` deep.
    free: Chunked<u32>,
    free_len: usize,
    buckets: Chunked<u32>,
    dirty_ceiling: usize,
    dirty_held: usize,
    /// Each write-back range open: its object, first page and count.
    open: [Option<(u32, u64, u64)>; OPEN_RANGES],
    /// Probes [`find`](Self::find) made, which a test bounds a walk by.
    #[cfg(test)]
    probes: core::cell::Cell<usize>,
}

impl<F: Copy> Pages<F> {
    /// The most pages a table holds.
    pub const MAX_CAPACITY: usize = NONE as usize / 4;

    /// A table of `capacity` pages, of which at most `dirty_ceiling` are dirty
    /// at once; `None` past [`Self::MAX_CAPACITY`], for a ceiling past the
    /// capacity, or where the heap refuses it. Every allocation it will ever
    /// make is made here.
    pub fn new(capacity: usize, dirty_ceiling: usize) -> Option<Self> {
        if capacity == 0 || capacity > Self::MAX_CAPACITY || dirty_ceiling > capacity {
            return None;
        }
        let width = (capacity * 2).next_power_of_two();
        let slots = Chunked::new(capacity, || None)?;
        let mut next = capacity as u32;
        let free = Chunked::new(capacity, || {
            next -= 1;
            next
        })?;
        let buckets = Chunked::new(width, || NONE)?;
        Some(Self {
            slots,
            free,
            free_len: capacity,
            buckets,
            dirty_ceiling,
            dirty_held: 0,
            open: [None; OPEN_RANGES],
            #[cfg(test)]
            probes: core::cell::Cell::new(0),
        })
    }

    /// The heap a table of `capacity` pages takes, all of it made by [`new`](Self::new).
    pub fn heap_bytes(capacity: usize) -> usize {
        Chunked::<Option<Slot<F>>>::heap_bytes(capacity)
            + Chunked::<u32>::heap_bytes(capacity)
            + Chunked::<u32>::heap_bytes((capacity * 2).next_power_of_two())
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Pages present or failed, across every object.
    pub fn used(&self) -> usize {
        self.slots.len() - self.free_len
    }

    /// Pages dirty or under write-back, which the dirty ceiling bounds.
    pub fn dirty_held(&self) -> usize {
        self.dirty_held
    }

    /// An object of `size` bytes, at most [`LAST_END`], named `id`: an id no
    /// live object of this table has, which a released one's may be, since
    /// [`release`](Self::release) takes every page it held.
    pub fn object(&mut self, id: u32, size: u64, mtime: u64) -> Object {
        assert!(size <= LAST_END, "an object of {size} bytes past the last page");
        Object { id, size, gone: false, head: NONE, pages: 0, version: 0, writing: 0, held: false, mtime }
    }

    fn hash(&self, object: u32, page: u64) -> usize {
        let x = (u64::from(object) << 40 ^ page).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (x >> 32) as usize & (self.buckets.len() - 1)
    }

    fn find(&self, object: u32, page: u64) -> Option<u32> {
        #[cfg(test)]
        self.probes.set(self.probes.get() + 1);
        let mut at = self.hash(object, page);
        loop {
            let slot = self.buckets[at];
            if slot == NONE {
                return None;
            }
            let s = self.slots[slot as usize].as_ref().expect("a bucket names a held slot");
            if s.object == object && s.page == page {
                return Some(slot);
            }
            at = (at + 1) & (self.buckets.len() - 1);
        }
    }

    fn slot(&self, i: u32) -> &Slot<F> {
        self.slots[i as usize].as_ref().expect("a held slot")
    }

    fn slot_mut(&mut self, i: u32) -> &mut Slot<F> {
        self.slots[i as usize].as_mut().expect("a held slot")
    }

    fn insert(&mut self, o: &mut Object, page: u64, frame: Option<F>, state: State) -> Result<(), Refused> {
        if self.free_len == 0 {
            return Err(Refused::NoRoom);
        }
        self.free_len -= 1;
        let i = self.free[self.free_len];
        let next = o.head;
        if next != NONE {
            self.slot_mut(next).prev = i;
        }
        self.slots[i as usize] = Some(Slot { object: o.id, page, frame, state, prev: NONE, next });
        o.head = i;
        o.pages += 1;
        let mut at = self.hash(o.id, page);
        while self.buckets[at] != NONE {
            at = (at + 1) & (self.buckets.len() - 1);
        }
        self.buckets[at] = i;
        if state.counts_dirty() {
            self.dirty_held += 1;
        }
        Ok(())
    }

    /// Take slot `i` out of its object and the hash: its frame, if any.
    fn remove(&mut self, o: &mut Object, i: u32) -> Option<F> {
        let (object, page) = {
            let s = self.slot(i);
            (s.object, s.page)
        };
        // Backward-shift deletion keeps every probe chain whole.
        let mask = self.buckets.len() - 1;
        let mut hole = self.hash(object, page);
        while self.buckets[hole] != i {
            hole = (hole + 1) & mask;
        }
        let mut at = hole;
        loop {
            at = (at + 1) & mask;
            let next = self.buckets[at];
            if next == NONE {
                break;
            }
            let home = {
                let s = self.slot(next);
                self.hash(s.object, s.page)
            };
            // `next` may move to the hole when its home is not in (hole, at].
            let between = if hole <= at { hole < home && home <= at } else { hole < home || home <= at };
            if !between {
                self.buckets[hole] = next;
                hole = at;
            }
        }
        self.buckets[hole] = NONE;
        let s = self.slots[i as usize].take().expect("a held slot");
        if s.prev != NONE {
            self.slot_mut(s.prev).next = s.next;
        } else {
            o.head = s.next;
        }
        if s.next != NONE {
            self.slot_mut(s.next).prev = s.prev;
        }
        o.pages -= 1;
        if s.state.counts_dirty() {
            self.dirty_held -= 1;
        }
        self.free[self.free_len] = i;
        self.free_len += 1;
        s.frame
    }

    pub fn lookup(&self, o: &Object, page: u64) -> Lookup<F> {
        if o.gone {
            return Lookup::Gone;
        }
        if page >= o.page_end() {
            return Lookup::PastEnd;
        }
        match self.find(o.id, page) {
            None => Lookup::Absent,
            Some(i) => self.slot(i).frame.map_or(Lookup::Failed, Lookup::Present),
        }
    }

    /// Install `frame` at `page`, clean, or dirty with a slot. A dirty page
    /// extends the size to `end`, where its bytes end past it; a clean one is
    /// what the file holds and moves nothing. A failed page is replaced.
    pub fn supply(&mut self, o: &mut Object, page: u64, frame: F, dirty: bool, end: u64) -> Result<Supplied, Refused> {
        if o.gone {
            return Err(Refused::Gone);
        }
        if page >= end.div_ceil(PAGE).max(o.page_end()) || end > (page + 1) * PAGE {
            return Err(Refused::PastEnd);
        }
        if !dirty && page >= o.page_end() {
            // A clean page is what the file holds, which never lies past its size.
            return Err(Refused::PastEnd);
        }
        match self.find(o.id, page) {
            Some(i) if self.slot(i).frame.is_some() => return Ok(Supplied::Present),
            Some(i) => {
                self.remove(o, i);
            }
            None => {}
        }
        if dirty && self.dirty_held >= self.dirty_ceiling {
            return Err(Refused::DirtyFull);
        }
        let state = State { dirty, ..State::CLEAN };
        self.insert(o, page, Some(frame), state)?;
        if dirty {
            o.size = o.size.max(end);
        }
        Ok(Supplied::Installed)
    }

    /// A write pin on present `page`, for a copy into it that runs outside the
    /// object's lock: the page is dirty from here, and a write-back that
    /// begins or ends before [`write_unpin`](Self::write_unpin) keeps it so.
    /// The size is not moved until the unpin, so no reader sees bytes past it
    /// that the copy has not written yet.
    pub fn write_pin(&mut self, o: &mut Object, page: u64) -> Result<F, Refused> {
        if o.gone {
            return Err(Refused::Gone);
        }
        let i = self.find(o.id, page).ok_or(Refused::InvalidArgument)?;
        let s = self.slot(i);
        let frame = s.frame.ok_or(Refused::InvalidArgument)?;
        let needs_slot = !s.state.counts_dirty();
        if needs_slot && self.dirty_held >= self.dirty_ceiling {
            return Err(Refused::DirtyFull);
        }
        let s = self.slot_mut(i);
        s.state.dirty = true;
        s.state.write_pins = s.state.write_pins.checked_add(1).expect("a page's write pins overflowed");
        if s.state.writeback {
            s.state.pinned_since_begin = true;
        }
        if needs_slot {
            self.dirty_held += 1;
        }
        Ok(frame)
    }

    /// The end of a [`write_pin`](Self::write_pin) of `frame`, once its
    /// bytes are in: the size grows to `end`. A page a shrink or a failure
    /// removed meanwhile, even one supplied again since, holds another frame
    /// or none; there is nothing left to unpin, and the bytes are not the
    /// file's. The caller keeps `frame` pinned until this, so it is not
    /// another page's meanwhile.
    pub fn write_unpin(&mut self, o: &mut Object, page: u64, frame: F, end: u64)
    where
        F: PartialEq,
    {
        assert!(end <= (page + 1) * PAGE && end > page * PAGE, "a write pin's end {end} outside page {page}");
        let Some(i) = self.find(o.id, page) else { return };
        let s = self.slot_mut(i);
        if s.frame != Some(frame) {
            return;
        }
        s.state.write_pins = s.state.write_pins.checked_sub(1).expect("a write pin released twice");
        o.size = o.size.max(end);
    }

    fn range_ok(o: &Object, first: u64, count: u64) -> bool {
        (1..=RANGE_PAGES).contains(&count) && first.checked_add(count).is_some() && !o.gone
    }

    fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
        a.0 < b.0 + b.1 && b.0 < a.0 + a.1
    }

    /// Begin writing back `count` pages from `first`: each dirty page is
    /// cleaned and keeps its slot until the end. The pages begun, as a bitmap.
    pub fn writeback_begin(&mut self, o: &mut Object, first: u64, count: u64) -> Result<[u64; 8], Refused> {
        let overlapping = self.open.iter().flatten().any(|&(id, a, n)| id == o.id && Self::overlaps((a, n), (first, count)));
        if !Self::range_ok(o, first, count) || overlapping {
            return Err(Refused::InvalidArgument);
        }
        let at = self.open.iter().position(Option::is_none).ok_or(Refused::InvalidArgument)?;
        self.open[at] = Some((o.id, first, count));
        let mut begun = [0u64; 8];
        for k in 0..count {
            let Some(i) = self.find(o.id, first + k) else { continue };
            let s = self.slot_mut(i);
            if s.frame.is_some() && s.state.dirty {
                s.state.dirty = false;
                s.state.writeback = true;
                s.state.pinned_since_begin = s.state.write_pins > 0;
                begun[k as usize / 64] |= 1 << (k % 64);
            }
        }
        Ok(begun)
    }

    /// End the write-back of exactly a range begun: a bit per page still
    /// dirty, holding its slot. A page removed meanwhile is clear.
    pub fn writeback_end(&mut self, o: &mut Object, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        let at = self.open.iter().position(|r| *r == Some((o.id, first, count))).ok_or(Refused::InvalidArgument)?;
        self.open[at] = None;
        let mut kept = [0u64; 8];
        for k in 0..count {
            let Some(i) = self.find(o.id, first + k) else { continue };
            let s = self.slot_mut(i);
            if !s.state.writeback {
                continue;
            }
            s.state.writeback = false;
            let keep = s.state.dirty || s.state.pinned_since_begin || !written;
            s.state.pinned_since_begin = false;
            if keep {
                s.state.dirty = true;
                kept[k as usize / 64] |= 1 << (k % 64);
            } else {
                self.dirty_held -= 1;
            }
        }
        Ok(kept)
    }

    /// Remove the clean pages of the range that no write-back or write pin
    /// holds: their frames, for the caller to free at their last pin.
    /// A range longer than the object holds pages walks its pages, not the range.
    pub fn withdraw(&mut self, o: &mut Object, first: u64, count: u64, out: &mut impl FnMut(F)) -> usize {
        let end = first.saturating_add(count);
        let mut removed = 0;
        let mut take = |pages: &mut Self, o: &mut Object, i: u32| {
            let s = pages.slot(i);
            if s.state.counts_dirty() || s.state.write_pins > 0 {
                return;
            }
            if let Some(frame) = pages.remove(o, i) {
                out(frame);
            }
            removed += 1;
        };
        if count > o.pages as u64 {
            let mut at = o.head;
            while at != NONE {
                let s = self.slot(at);
                let (next, page) = (s.next, s.page);
                if (first..end).contains(&page) {
                    take(self, o, at);
                }
                at = next;
            }
        } else {
            for page in first..end {
                if let Some(i) = self.find(o.id, page) {
                    take(self, o, i);
                }
            }
        }
        removed
    }

    /// Mark the range failed: present pages give up their frames, and a read
    /// of any page of it answers `Failed` until it is supplied again.
    pub fn fail(&mut self, o: &mut Object, first: u64, count: u64, out: &mut impl FnMut(F)) -> Result<(), Refused> {
        let end = first.saturating_add(count).min(o.page_end());
        for page in first..end {
            if let Some(i) = self.find(o.id, page) {
                if let Some(frame) = self.remove(o, i) {
                    out(frame);
                }
            }
            self.insert(o, page, None, State::CLEAN)?;
        }
        Ok(())
    }

    /// Set the size. A shrink removes every page past it, dirty or not, and
    /// answers the frame of a partial last page, whose tail past the size the
    /// caller zeroes.
    pub fn resize(&mut self, o: &mut Object, size: u64, out: &mut impl FnMut(F)) -> Option<F> {
        o.version += 1;
        let keep = size.div_ceil(PAGE);
        if size < o.size {
            let mut at = o.head;
            while at != NONE {
                let (next, page) = {
                    let s = self.slot(at);
                    (s.next, s.page)
                };
                if page >= keep {
                    if let Some(frame) = self.remove(o, at) {
                        out(frame);
                    }
                }
                at = next;
            }
        }
        o.size = size;
        if size.is_multiple_of(PAGE) {
            return None;
        }
        match self.lookup(o, size / PAGE) {
            Lookup::Present(frame) => Some(frame),
            _ => None,
        }
    }

    /// The pager ended: every page goes, and every later call answers `Gone`.
    pub fn invalidate(&mut self, o: &mut Object, out: &mut impl FnMut(F)) {
        while o.head != NONE {
            if let Some(frame) = self.remove(o, o.head) {
                out(frame);
            }
        }
        o.gone = true;
        for range in &mut self.open {
            if range.is_some_and(|(id, _, _)| id == o.id) {
                *range = None;
            }
        }
    }

    /// The object is let go: every page goes, as at [`invalidate`](Self::invalidate).
    pub fn release(&mut self, mut o: Object, out: &mut impl FnMut(F)) {
        self.invalidate(&mut o, out);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::vec::Vec;

    use super::*;

    fn none(_: u64) {}

    fn sink(freed: &mut Vec<u64>) -> impl FnMut(u64) + '_ {
        |f| freed.push(f)
    }

    /// XSH 2.9.7: a read that began before a write, or while one was under
    /// way, never counts; one that began after it does.
    #[test]
    fn a_read_counts_only_where_no_write_began_or_was_under_way() {
        let mut p = Pages::<u64>::new(8, 4).unwrap();
        let mut o = p.object(1, PAGE, 0);
        let before = o.read_begin().expect("nothing writes yet");
        o.write_begin();
        assert_eq!(o.read_begin(), None, "a read meets the write in flight");
        assert!(!o.unchanged(before), "a read that began before it meets it");
        o.write_end(true);
        assert!(!o.unchanged(before), "and still does once it ended");
        let after = o.read_begin().expect("the write ended");
        assert!(o.unchanged(after));
        p.resize(&mut o, 0, &mut none);
        assert!(!o.unchanged(after), "a shrink is a change a read meets");
    }

    /// A pager's write that takes several calls is one write: the first short
    /// call leaves it under way, later ones nest in it, and only its end lets
    /// a read begin.
    #[test]
    fn a_short_call_holds_the_write_until_its_end() {
        let mut p = Pages::<u64>::new(8, 4).unwrap();
        let mut o = p.object(2, PAGE, 0);
        assert_eq!(o.end_write(), Err(Refused::InvalidArgument), "nothing is held");
        o.write_begin();
        o.write_end(false);
        assert_eq!(o.read_begin(), None, "a short call's write is under way");
        o.write_begin();
        o.write_end(false);
        o.write_begin();
        o.write_end(true);
        assert_eq!(o.read_begin(), None, "a later call, short or whole, ends no held write");
        assert_eq!(o.published(), None, "nor does a stat publish one half written");
        let held = o.version;
        assert_eq!(o.end_write(), Ok(()));
        let v = o.read_begin().expect("the end lets a read begin");
        assert!(o.unchanged(v) && v != Version(held), "the end moved the version");
        assert_eq!(o.published(), Some(v.0), "a stat publishes the version a read begins at");
        assert_eq!(o.end_write(), Err(Refused::InvalidArgument), "a write ends once");
    }

    #[test]
    fn a_supplied_page_is_found_and_a_second_supply_changes_nothing() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(1, 3 * PAGE, 0);
        assert_eq!(p.lookup(&o, 1), Lookup::Absent);
        assert_eq!(p.supply(&mut o, 1, 100, false, 2 * PAGE), Ok(Supplied::Installed));
        assert_eq!(p.supply(&mut o, 1, 200, false, 2 * PAGE), Ok(Supplied::Present));
        assert_eq!(p.lookup(&o, 1), Lookup::Present(100));
        assert_eq!(p.lookup(&o, 3), Lookup::PastEnd);
    }

    #[test]
    fn a_clean_supply_past_the_size_is_refused_and_a_dirty_one_extends_it() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(2, PAGE, 0);
        assert_eq!(p.supply(&mut o, 1, 1, false, 2 * PAGE), Err(Refused::PastEnd));
        assert_eq!(o.size(), PAGE);
        let mut short = p.object(3, 100, 0);
        assert_eq!(p.supply(&mut short, 0, 2, false, PAGE), Ok(Supplied::Installed));
        assert_eq!(short.size(), 100, "a clean page ending past the size leaves it");
        assert_eq!(p.supply(&mut o, 1, 1, true, PAGE + 10), Ok(Supplied::Installed));
        assert_eq!(o.size(), PAGE + 10, "the size covers exactly the bytes put in");
    }

    /// R3: a reader between the pin and the unpin sees the old size, so the
    /// bytes the copy has yet to write are never read as the file's zeros.
    /// Growing the size first (`resize`, then the write) is what this refuses.
    #[test]
    fn a_reader_racing_an_extending_write_never_sees_its_unwritten_bytes() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(4, 100, 0);
        p.supply(&mut o, 0, 7, false, 100).unwrap();
        assert_eq!(p.write_pin(&mut o, 0), Ok(7));
        assert_eq!(o.size(), 100, "the copy is in flight: a reader still stops at the old end");
        p.write_unpin(&mut o, 0, 7, 300);
        assert_eq!(o.size(), 300, "the bytes are in, and the size covers them");
        assert_eq!(p.write_pin(&mut o, 1), Err(Refused::InvalidArgument), "an absent page is supplied, not pinned");
    }

    #[test]
    fn an_unpin_after_a_shrink_removed_the_page_grows_nothing() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(5, PAGE, 0);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.write_pin(&mut o, 0).unwrap();
        p.resize(&mut o, 0, &mut none);
        p.supply(&mut o, 0, 8, true, 10).unwrap();
        p.write_unpin(&mut o, 0, 7, PAGE);
        assert_eq!(o.size(), 10, "the shrink's size, and the new page's, stand over a copy they overtook");
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut none), 0, "the new page is dirty");
        p.writeback_begin(&mut o, 0, 1).unwrap();
        assert_eq!(p.writeback_end(&mut o, 0, 1, true).unwrap()[0], 0, "and holds no pin of the old one");
    }

    #[test]
    fn the_dirty_ceiling_refuses_and_a_write_back_frees_it() {
        let mut p = Pages::new(8, 2).unwrap();
        let mut o = p.object(6, 4 * PAGE, 0);
        for page in 0..2 {
            p.supply(&mut o, page, page, true, (page + 1) * PAGE).unwrap();
        }
        assert_eq!(p.supply(&mut o, 2, 2, true, 3 * PAGE), Err(Refused::DirtyFull));
        p.writeback_begin(&mut o, 0, 2).unwrap();
        assert_eq!(p.dirty_held(), 2, "a page under write-back keeps its slot");
        assert_eq!(p.writeback_end(&mut o, 0, 2, true), Ok([0; 8]));
        assert_eq!(p.dirty_held(), 0);
        assert_eq!(p.supply(&mut o, 2, 2, true, 3 * PAGE), Ok(Supplied::Installed));
    }

    #[test]
    fn kept_is_exactly_the_pages_dirtied_or_pinned_since_the_begin_or_unwritten() {
        let mut p = Pages::new(16, 16).unwrap();
        let mut o = p.object(7, 8 * PAGE, 0);
        for page in 0..6 {
            p.supply(&mut o, page, page, true, (page + 1) * PAGE).unwrap();
        }
        p.write_pin(&mut o, 0).unwrap(); // pinned across the begin, released inside
        assert_eq!(p.writeback_begin(&mut o, 0, 6).unwrap()[0], 0b11_1111);
        p.write_unpin(&mut o, 0, 0, PAGE);
        p.write_pin(&mut o, 1).unwrap(); // dirtied inside
        p.write_unpin(&mut o, 1, 1, 2 * PAGE);
        let mut freed = Vec::new();
        p.resize(&mut o, 5 * PAGE, &mut sink(&mut freed)); // page 5 removed inside
        p.fail(&mut o, 4, 1, &mut sink(&mut freed)).unwrap(); // page 4 failed inside
        let kept = p.writeback_end(&mut o, 0, 6, true).unwrap()[0];
        assert_eq!(kept, 0b11, "pinned since the begin and dirtied since it; the removed are clear");
        assert_eq!(p.dirty_held(), 2);
        p.writeback_begin(&mut o, 0, 2).unwrap();
        assert_eq!(p.writeback_end(&mut o, 0, 2, false).unwrap()[0], 0b11, "a failed write-back keeps every page");
    }

    #[test]
    fn a_range_must_be_ended_exactly_as_begun_and_once() {
        let mut p: Pages<u64> = Pages::new(8, 8).unwrap();
        let mut o = p.object(8, PAGE * 1024, 0);
        p.writeback_begin(&mut o, 0, 4).unwrap();
        assert_eq!(p.writeback_begin(&mut o, 3, 2), Err(Refused::InvalidArgument), "overlapping");
        assert_eq!(p.writeback_end(&mut o, 0, 3, true), Err(Refused::InvalidArgument), "not the range begun");
        assert!(p.writeback_end(&mut o, 0, 4, true).is_ok());
        assert_eq!(p.writeback_end(&mut o, 0, 4, true), Err(Refused::InvalidArgument), "twice");
        assert_eq!(p.writeback_begin(&mut o, 0, RANGE_PAGES + 1), Err(Refused::InvalidArgument));
        assert_eq!(p.writeback_begin(&mut o, 0, 0), Err(Refused::InvalidArgument));
    }

    #[test]
    fn only_clean_unheld_pages_are_withdrawn() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(9, 4 * PAGE, 0);
        p.supply(&mut o, 0, 10, false, PAGE).unwrap();
        p.supply(&mut o, 1, 11, true, 2 * PAGE).unwrap();
        p.supply(&mut o, 2, 12, true, 3 * PAGE).unwrap();
        p.supply(&mut o, 3, 13, false, 4 * PAGE).unwrap();
        p.writeback_begin(&mut o, 2, 1).unwrap(); // clean but under write-back
        p.write_pin(&mut o, 3).unwrap(); // write-pinned
        let mut freed = Vec::new();
        assert_eq!(p.withdraw(&mut o, 0, 4, &mut sink(&mut freed)), 1);
        assert_eq!(freed, [10]);
        assert_eq!(p.lookup(&o, 0), Lookup::Absent);
    }

    /// R6: however often the allocator refuses, a pager whose pages come and
    /// go never holds less than its core, and supplies its core's worth of
    /// pages without a promise. Giving each leaving frame back and promising
    /// again, which the allocator may refuse, is what this refuses.
    #[test]
    fn a_core_is_kept_whatever_the_allocator_refuses() {
        let mut core = Core::new(4);
        for round in 0..3 {
            let mut supplied = 0;
            for _ in 0..4 {
                match core.draw(true) {
                    Draw::Aside | Draw::Pool => {
                        core.installed();
                        supplied += 1;
                    }
                    // The allocator refuses every promise beyond the core.
                    Draw::Beyond => {}
                }
            }
            assert_eq!(supplied, 4, "round {round}: the core supplied {supplied} of 4");
            for _ in 0..4 {
                assert_eq!(core.leave(true), Leave::Aside, "round {round}: a core frame was given back");
            }
            assert_eq!(core.held(), 4);
        }
        // One beyond the core goes back when it leaves.
        assert_eq!(core.draw(true), Draw::Aside);
        core.installed();
        for _ in 0..3 {
            core.draw(true);
            core.installed();
        }
        assert_eq!(core.draw(true), Draw::Beyond);
        core.promised();
        core.installed();
        assert_eq!(core.leave(true), Leave::Free, "a frame beyond the core");
        assert_eq!(core.leave(true), Leave::Aside);
        assert_eq!(core.end(), 0);
    }

    /// A withdrawal of a range far longer than the object costs the pages it
    /// holds, never a probe per page of the range.
    #[test]
    fn a_withdrawal_of_a_long_range_walks_the_objects_pages() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, LAST_END, 0);
        p.supply(&mut o, 7, 1, false, 8 * PAGE).unwrap();
        p.supply(&mut o, 1 << 40, 2, false, ((1 << 40) + 1) * PAGE).unwrap();
        let mut freed = Vec::new();
        p.probes.set(0);
        assert_eq!(p.withdraw(&mut o, 8, 1 << 24, &mut sink(&mut freed)), 0, "neither page lies in 8..8+2^24");
        assert!(p.probes.get() < 8, "{} probes for a range of 2^24 pages", p.probes.get());
        assert_eq!(p.withdraw(&mut o, 8, u64::MAX, &mut sink(&mut freed)), 1);
        assert_eq!(freed, [2], "only the page inside the range");
        assert_eq!(p.withdraw(&mut o, 0, u64::MAX, &mut sink(&mut freed)), 1);
        assert_eq!(o.pages(), 0);
    }

    /// Every offset a pager names is bounded where it enters, so no page's
    /// end overflows past it.
    #[test]
    fn an_offset_or_range_past_the_last_page_is_out_of_reach() {
        assert!(within_reach(LAST_END - PAGE, PAGE));
        assert!(!within_reach(LAST_END - PAGE + 1, PAGE));
        assert!(!within_reach(u64::MAX - PAGE + 1, PAGE), "the supply that wrapped `ends`");
        assert!(!within_reach(1, u64::MAX));
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, LAST_END, 0);
        let last = LAST_END / PAGE - 1;
        assert_eq!(p.supply(&mut o, last, 1, true, LAST_END), Ok(Supplied::Installed), "the last page");
        assert!(Pages::<u64>::new(Pages::<u64>::MAX_CAPACITY + 1, 0).is_none());
    }

    #[test]
    fn a_shrink_removes_dirty_pages_releases_their_slots_and_names_the_tail() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(11, 3 * PAGE, 0);
        for page in 0..3 {
            p.supply(&mut o, page, page + 100, true, (page + 1) * PAGE).unwrap();
        }
        let mut freed = Vec::new();
        assert_eq!(p.resize(&mut o, PAGE + 5, &mut sink(&mut freed)), Some(101));
        assert_eq!(freed, [102]);
        assert_eq!(p.dirty_held(), 2);
        assert_eq!(p.lookup(&o, 2), Lookup::PastEnd);
        assert_eq!(p.resize(&mut o, 4 * PAGE, &mut none), None, "growth removes nothing");
        assert_eq!(p.lookup(&o, 2), Lookup::Absent, "and leaves the new range absent");
    }

    #[test]
    fn a_failed_page_answers_failed_until_supplied_again() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(12, 2 * PAGE, 0);
        p.supply(&mut o, 0, 5, false, PAGE).unwrap();
        let mut freed = Vec::new();
        p.fail(&mut o, 0, 2, &mut sink(&mut freed)).unwrap();
        assert_eq!(freed, [5]);
        assert_eq!((p.lookup(&o, 0), p.lookup(&o, 1)), (Lookup::Failed, Lookup::Failed));
        assert_eq!(p.supply(&mut o, 1, 6, false, 2 * PAGE), Ok(Supplied::Installed));
        assert_eq!(p.lookup(&o, 1), Lookup::Present(6));
    }

    #[test]
    fn an_invalidated_object_answers_gone_and_gives_every_frame_back() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(13, 4 * PAGE, 0);
        p.supply(&mut o, 0, 1, true, PAGE).unwrap();
        p.supply(&mut o, 2, 3, false, 3 * PAGE).unwrap();
        let mut freed = Vec::new();
        p.invalidate(&mut o, &mut sink(&mut freed));
        freed.sort();
        assert_eq!(freed, [1, 3]);
        assert_eq!((p.used(), p.dirty_held()), (0, 0));
        assert_eq!(p.lookup(&o, 0), Lookup::Gone);
        assert_eq!(p.supply(&mut o, 0, 9, false, PAGE), Err(Refused::Gone));
    }

    #[test]
    fn a_full_table_refuses_and_every_slot_is_reached_after_churn() {
        let mut p = Pages::new(64, 64).unwrap();
        let mut objects: Vec<Object> = (0..4).map(|i| p.object(i, 1 << 30, 0)).collect();
        let mut model: BTreeSet<(usize, u64)> = BTreeSet::new();
        let mut x = 1u64;
        for round in 0..20_000u64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let which = (x >> 33) as usize % 4;
            let page = (x >> 40) % 97;
            let o = &mut objects[which];
            if (x >> 20).is_multiple_of(3) {
                let mut freed = Vec::new();
                p.withdraw(o, page, 1, &mut sink(&mut freed));
                if !freed.is_empty() {
                    assert!(model.remove(&(which, page)));
                }
            } else {
                match p.supply(o, page, round, false, (page + 1) * PAGE) {
                    Ok(Supplied::Installed) => assert!(model.insert((which, page))),
                    Ok(Supplied::Present) => assert!(model.contains(&(which, page))),
                    Err(Refused::NoRoom) => assert_eq!(model.len(), 64),
                    other => panic!("{other:?}"),
                }
            }
            if round % 997 == 0 {
                for (w, object) in objects.iter().enumerate() {
                    for pg in 0..97 {
                        let present = matches!(p.lookup(object, pg), Lookup::Present(_));
                        assert_eq!(present, model.contains(&(w, pg)), "object {w} page {pg} after round {round}");
                    }
                }
            }
        }
        assert_eq!(p.used(), model.len());
    }
}
