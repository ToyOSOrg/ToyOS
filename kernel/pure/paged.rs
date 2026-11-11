//! A pager's resident pages: every present page of every paged object it
//! serves, in one table of `capacity` slots made when the pager is, so no
//! supply, read, write or write-back allocates.
//!
//! **A page is present, failed, revoking or absent.** A present page holds a
//! frame and its dirty state; a failed one holds no frame and answers `EIO`
//! until it is supplied again; an absent one has no slot. Unwritten anonymous
//! memory reads zeros without a slot, and its present pages cannot be
//! withdrawn. Nothing here waits: a caller that finds a page absent records
//! its request in its own wait record.
//!
//! **A removed page keeps its frame until no mapping reaches it.** A
//! withdraw, failure, shrink or the object's end marks the page revoking: it
//! reads as absent, takes no supply or write, and keeps its slot and frame,
//! so no page is ever two frames at once. One walk per object visits each of
//! its mappers ([`Mappers`]) after the marking, and a removal that finds a
//! walk running is taken by that walk's next pass; the last pass's end gives
//! every revoking page's frame out ([`Pages::walk`]).
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
//! caller's: the walk's end answers every frame.

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

/// A waiting reader's want of an object's pages, as its pager hears of it: a
/// range is named in one notice and not again while the reader waits for it,
/// unless a withdraw takes back a page of it, which may have been supplied
/// for that notice. Every other way a page becomes absent again moves the
/// object's version, which ends the wait ([`Pages::wait_over`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Want {
    first: u64,
    count: u64,
    reported: bool,
}

impl Want {
    /// No pages wanted yet.
    pub const NONE: Self = Self { first: 0, count: 0, reported: true };

    /// The reader wants `count` pages from `first`: whether that is news its
    /// pager must hear.
    pub fn ask(&mut self, first: u64, count: u64) -> bool {
        if self.first == first && self.count == count {
            return false;
        }
        *self = Self { first, count, reported: false };
        true
    }

    /// `count` pages from `first` were withdrawn: whether that makes the
    /// range news again.
    pub fn withdrawn(&mut self, first: u64, count: u64) -> bool {
        let meets = first < self.first.saturating_add(self.count) && self.first < first.saturating_add(count);
        let news = meets && self.reported;
        self.reported &= !news;
        news
    }

    /// The range to name in a notice, once per ask or withdraw.
    pub fn report(&mut self) -> Option<(u64, u64)> {
        (!self.reported).then(|| {
            self.reported = true;
            (self.first, self.count)
        })
    }

    /// Whether a notice is owed.
    pub fn unreported(&self) -> bool {
        !self.reported
    }

    /// The range last asked for.
    pub fn range(&self) -> (u64, u64) {
        (self.first, self.count)
    }
}

/// The wants of the readers waiting on one object, which a withdraw marks.
pub trait Waiters {
    /// `f` on each want, written back after.
    fn each(&mut self, f: &mut dyn FnMut(&mut Want));
}

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
    /// Unwritten anonymous memory, without a resident frame.
    Zero,
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

/// What a revoking page becomes once no mapping reaches its frame, by the
/// object as the walk's end finds it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Revoked {
    /// Absent inside the object: a reader waiting on it is named again.
    Absent,
    Failed,
    /// Past the size, or the object is gone.
    Removed,
}

/// The mappers of one object, one node per space that maps it, in the order
/// a walk visits them. A node links at the tail, seen at
/// [`Object::epoch`] then, and leaves only once its space maps none of the
/// object's frames.
pub trait Mappers {
    type Node: Copy;
    fn head(&self) -> Option<Self::Node>;
    /// The epoch of the walk that last visited `node`, or the one current
    /// when it linked.
    fn seen(&self, node: Self::Node) -> u64;
    /// `node` is visited at `epoch`: it moves to the tail.
    fn visited(&mut self, node: Self::Node, epoch: u64);
}

/// Where an object's walk stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Walk {
    Idle,
    /// A walker visits its mappers; `again` where pages were marked since
    /// its pass began, which another pass covers.
    Running { again: bool },
}

/// What the caller that marked an object's pages does about their mappers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Revoke {
    /// Nothing is revoking.
    Done,
    /// No walk ran: the caller walks it ([`Pages::walk`]) to its end.
    Walk,
    /// The running walk takes the pages in a later pass: they are gone once
    /// [`Object::finished`] answers true of this epoch.
    Queued(u64),
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
    /// Removed, its frame kept until no mapping reaches it.
    revoking: Option<Revoked>,
    prev: u32,
    next: u32,
}

/// One object's part of the table: its size, whether its pager ended, the
/// list of its slots, and its writes under way.
pub struct Object {
    id: u32,
    anonymous: bool,
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
    /// Its slots that are revoking.
    revoking: usize,
    /// The pass of the walk over its mappers, moved at each pass's start.
    epoch: u64,
    walk: Walk,
    /// The epoch whose pass last ended the walk.
    finished: u64,
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

    /// The epoch a mapper linking now is seen at: the walk running passes it
    /// by, since nothing it maps from now on is revoking.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Pages were marked revoking in this hold of the lock: what the caller
    /// does about their mappers.
    pub fn revoke(&mut self) -> Revoke {
        if self.revoking == 0 {
            return Revoke::Done;
        }
        match self.walk {
            Walk::Idle => {
                self.epoch += 1;
                self.walk = Walk::Running { again: false };
                Revoke::Walk
            }
            Walk::Running { .. } => {
                self.walk = Walk::Running { again: true };
                Revoke::Queued(self.epoch + 1)
            }
        }
    }

    /// Whether the walk's pass at `epoch`, or a later one, ended.
    pub fn finished(&self, epoch: u64) -> bool {
        self.finished >= epoch
    }

    /// Whether every page it held is gone, and no walk still owes a frame.
    pub fn empty(&self) -> bool {
        self.pages == 0 && self.walk == Walk::Idle
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
    pub fn object(&mut self, id: u32, size: u64) -> Object {
        assert!(size <= LAST_END, "an object of {size} bytes past the last page");
        Object {
            id,
            anonymous: false,
            size,
            gone: false,
            head: NONE,
            pages: 0,
            version: 0,
            writing: 0,
            held: false,
            revoking: 0,
            epoch: 0,
            walk: Walk::Idle,
            finished: 0,
        }
    }

    /// An anonymous object owns its pages: no backing can supply them again,
    /// so writeback, failure and withdrawal cannot discard present bytes.
    pub fn anonymous(&mut self, id: u32, size: u64) -> Object {
        Object { anonymous: true, ..self.object(id, size) }
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
        self.slots[i as usize] = Some(Slot { object: o.id, page, frame, state, revoking: None, prev: NONE, next });
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
        if s.revoking.is_some() {
            o.revoking -= 1;
        }
        self.free[self.free_len] = i;
        self.free_len += 1;
        s.frame
    }

    /// Whether a holder's read of `o` that began at `since` and waits for
    /// `page` looks again: the page is no longer absent, or a write or resize
    /// moved the version, which the read answers `WouldBlock`.
    pub fn wait_over(&self, o: &Object, page: u64, since: Version) -> bool {
        !o.unchanged(since) || !matches!(self.lookup(o, page), Lookup::Absent)
    }

    /// Remove slot `i` as `becomes` once no mapping reaches it: at once for a
    /// failed page, which no mapping reaches, and as the walk's end for a
    /// frame. A page already revoking becomes failed if either removal fails it.
    fn revoke(&mut self, o: &mut Object, i: u32, becomes: Revoked) {
        let s = self.slot_mut(i);
        if s.frame.is_none() {
            self.remove(o, i);
            return;
        }
        if let Some(was) = s.revoking {
            s.revoking = Some(if was == Revoked::Failed { was } else { becomes });
            return;
        }
        // Its bytes are no longer the file's: no write-back takes them.
        let counted = s.state.counts_dirty();
        s.state = State::CLEAN;
        s.revoking = Some(becomes);
        o.revoking += 1;
        if counted {
            self.dirty_held -= 1;
        }
    }

    /// Visit `o`'s next mapper after pages were marked revoking
    /// ([`Object::revoke`]), or, once every mapper has been visited since the
    /// last marking, give out each revoking page's frame, its page and what it
    /// became, and answer `None`. A page that ends absent was withdrawn,
    /// perhaps after a supply for a waiter's notice, without moving the
    /// version: every want of `waiters` it meets is named again.
    pub fn walk<M: Mappers>(
        &mut self,
        o: &mut Object,
        mappers: &mut M,
        waiters: &mut impl Waiters,
        out: &mut impl FnMut(F, u64, Revoked),
    ) -> Option<M::Node> {
        loop {
            if let Some(node) = mappers.head().filter(|&n| mappers.seen(n) != o.epoch) {
                mappers.visited(node, o.epoch);
                return Some(node);
            }
            match o.walk {
                Walk::Running { again: true } => {
                    o.epoch += 1;
                    o.walk = Walk::Running { again: false };
                }
                Walk::Running { again: false } => break,
                Walk::Idle => panic!("paged: a walk of object {} that never began", o.id),
            }
        }
        let held = !o.gone;
        let mut at = o.head;
        while at != NONE {
            let s = self.slot(at);
            let (next, page, revoking) = (s.next, s.page, s.revoking);
            if let Some(becomes) = revoking {
                let frame = self.remove(o, at).expect("paged: a revoking page holds its frame");
                let becomes = if held && page < o.page_end() { becomes } else { Revoked::Removed };
                match becomes {
                    Revoked::Failed => self.insert(o, page, None, State::CLEAN).expect("paged: the slot just freed"),
                    Revoked::Absent => waiters.each(&mut |want| {
                        want.withdrawn(page, 1);
                    }),
                    Revoked::Removed => {}
                }
                out(frame, page, becomes);
            }
            at = next;
        }
        debug_assert_eq!(o.revoking, 0);
        o.walk = Walk::Idle;
        o.finished = o.epoch;
        None
    }

    pub fn lookup(&self, o: &Object, page: u64) -> Lookup<F> {
        if o.gone {
            return Lookup::Gone;
        }
        if page >= o.page_end() {
            return Lookup::PastEnd;
        }
        match self.find(o.id, page).map(|i| self.slot(i)) {
            None if o.anonymous => Lookup::Zero,
            None => Lookup::Absent,
            Some(s) if s.revoking.is_some() => Lookup::Absent,
            Some(s) => s.frame.map_or(Lookup::Failed, Lookup::Present),
        }
    }

    /// Install `frame` at `page`, clean, or dirty with a slot. A dirty page
    /// extends the size to `end`, where its bytes end past it; a clean one is
    /// what the file holds and moves nothing. A failed page is replaced; a
    /// revoking one is present until its walk ends.
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
            Some(i) if self.slot(i).frame.is_some() || self.slot(i).revoking.is_some() => return Ok(Supplied::Present),
            Some(i) => {
                self.remove(o, i);
            }
            None => {}
        }
        let tracked = dirty && !o.anonymous;
        if tracked && self.dirty_held >= self.dirty_ceiling {
            return Err(Refused::DirtyFull);
        }
        let state = State { dirty: tracked, ..State::CLEAN };
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
        let frame = s.frame.filter(|_| s.revoking.is_none()).ok_or(Refused::InvalidArgument)?;
        let needs_slot = !o.anonymous && !s.state.counts_dirty();
        if needs_slot && self.dirty_held >= self.dirty_ceiling {
            return Err(Refused::DirtyFull);
        }
        let s = self.slot_mut(i);
        s.state.dirty = !o.anonymous;
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
    /// removed meanwhile is revoking, or, supplied again since, holds another
    /// frame or none; there is nothing left to unpin, and the bytes are not
    /// the file's. The caller keeps `frame` pinned until this, so it is not
    /// another page's meanwhile.
    pub fn write_unpin(&mut self, o: &mut Object, page: u64, frame: F, end: u64)
    where
        F: PartialEq,
    {
        assert!(end <= (page + 1) * PAGE && end > page * PAGE, "a write pin's end {end} outside page {page}");
        let Some(i) = self.find(o.id, page) else { return };
        let s = self.slot_mut(i);
        if s.frame != Some(frame) || s.revoking.is_some() {
            return;
        }
        s.state.write_pins = s.state.write_pins.checked_sub(1).expect("a write pin released twice");
        o.size = o.size.max(end);
    }

    /// The end of a [`write_pin`](Self::write_pin) whose bytes did not all
    /// come: released as [`write_unpin`](Self::write_unpin) releases it, the
    /// size left as it was.
    pub fn write_abandon(&mut self, o: &Object, page: u64, frame: F)
    where
        F: PartialEq,
    {
        let Some(i) = self.find(o.id, page) else { return };
        let s = self.slot_mut(i);
        if s.frame != Some(frame) || s.revoking.is_some() {
            return;
        }
        s.state.write_pins = s.state.write_pins.checked_sub(1).expect("a write pin released twice");
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
        if o.anonymous || !Self::range_ok(o, first, count) || overlapping {
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

    /// End a range begun, or its leading part, retaining the unended suffix:
    /// a bit per page still dirty. A page removed meanwhile is clear.
    pub fn writeback_end(&mut self, o: &mut Object, first: u64, count: u64, written: bool) -> Result<[u64; 8], Refused> {
        let at = self.open.iter().position(|r| r.is_some_and(|(id, a, n)| id == o.id && a == first && count > 0 && count <= n))
            .ok_or(Refused::InvalidArgument)?;
        let (_, _, total) = self.open[at].expect("the range found");
        self.open[at] = (count < total).then_some((o.id, first + count, total - count));
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
    /// holds, and the failed ones: how many, and whether a waiter's want is
    /// news its pager must hear again. Each ends absent without moving the
    /// version, perhaps after a supply for a waiter's notice, so every want of
    /// `waiters` it meets is named again: a failed page's now, as it goes at
    /// once, a present page's at its walk's end, as it is revoking until then
    /// ([`Object::revoke`]).
    /// A range longer than the object holds pages walks its pages, not the range.
    pub fn withdraw(&mut self, o: &mut Object, first: u64, count: u64, waiters: &mut impl Waiters) -> (usize, bool) {
        if o.anonymous { return (0, false); }
        let end = first.saturating_add(count);
        let (mut removed, mut news) = (0, false);
        let mut take = |pages: &mut Self, o: &mut Object, i: u32| {
            let s = pages.slot(i);
            if s.revoking.is_some() || s.state.counts_dirty() || s.state.write_pins > 0 {
                return;
            }
            if s.frame.is_none() {
                let page = s.page;
                pages.remove(o, i);
                waiters.each(&mut |want| news |= want.withdrawn(page, 1));
            } else {
                pages.revoke(o, i, Revoked::Absent);
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
        (removed, news)
    }

    /// Mark the range failed: a read of any page of it answers `Failed` until
    /// it is supplied again, a present page's from its walk's end.
    pub fn fail(&mut self, o: &mut Object, first: u64, count: u64) -> Result<(), Refused> {
        if o.anonymous { return Err(Refused::InvalidArgument); }
        let end = first.saturating_add(count).min(o.page_end());
        for page in first..end {
            match self.find(o.id, page) {
                Some(i) if self.slot(i).frame.is_some() => self.revoke(o, i, Revoked::Failed),
                Some(_) => {}
                None => self.insert(o, page, None, State::CLEAN)?,
            }
        }
        Ok(())
    }

    /// Set the size. A shrink removes every page past it, dirty or not, and
    /// answers the frame of a partial last page, whose tail past the size the
    /// caller zeroes.
    pub fn resize(&mut self, o: &mut Object, size: u64) -> Option<F> {
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
                    self.revoke(o, at, Revoked::Absent);
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

    /// The pager ended or let the object go: every page goes, and every
    /// later call answers `Gone`.
    pub fn invalidate(&mut self, o: &mut Object) {
        let mut at = o.head;
        while at != NONE {
            let next = self.slot(at).next;
            self.revoke(o, at, Revoked::Absent);
            at = next;
        }
        o.gone = true;
        for range in &mut self.open {
            if range.is_some_and(|(id, _, _)| id == o.id) {
                *range = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::vec::Vec;

    use super::*;

    /// No space maps anything: a walk ends at its first step.
    struct Unmapped;

    impl Mappers for Unmapped {
        type Node = ();
        fn head(&self) -> Option<()> {
            None
        }
        fn seen(&self, (): ()) -> u64 {
            0
        }
        fn visited(&mut self, (): (), _: u64) {}
    }

    /// The frames of `o`'s revoking pages, once a walk over no mappers ends.
    fn settle(p: &mut Pages<u64>, o: &mut Object) -> Vec<u64> {
        let mut freed = Vec::new();
        match o.revoke() {
            Revoke::Done => {}
            Revoke::Walk => assert_eq!(p.walk(o, &mut Unmapped, &mut Nobody, &mut |f, _, _| freed.push(f)), None),
            Revoke::Queued(_) => panic!("a walk ran with no walker"),
        }
        freed
    }

    /// One object's mappers: each node's space and the epoch it was last
    /// seen at, in visiting order.
    #[derive(Default)]
    struct Ring(std::collections::VecDeque<(u32, u64)>);

    impl Ring {
        fn link(&mut self, space: u32, o: &Object) {
            self.0.push_back((space, o.epoch()));
        }
    }

    impl Mappers for Ring {
        type Node = u32;
        fn head(&self) -> Option<u32> {
            self.0.front().map(|&(n, _)| n)
        }
        fn seen(&self, node: u32) -> u64 {
            self.0.iter().find(|&&(n, _)| n == node).expect("linked").1
        }
        fn visited(&mut self, node: u32, epoch: u64) {
            let at = self.0.iter().position(|&(n, _)| n == node).expect("linked");
            self.0.remove(at);
            self.0.push_back((node, epoch));
        }
    }

    /// A removed page reads as absent, takes no second frame and no write,
    /// and keeps its frame until every mapper of its object was visited;
    /// then its frame goes out once and the page is supplied afresh.
    #[test]
    fn a_revoking_page_keeps_its_one_frame_until_every_mapper_is_visited() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 2 * PAGE);
        let mut ring = Ring::default();
        ring.link(10, &o);
        ring.link(11, &o);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 1);
        assert_eq!(p.lookup(&o, 0), Lookup::Absent);
        assert_eq!(p.supply(&mut o, 0, 8, false, PAGE), Ok(Supplied::Present), "a second frame for the page");
        assert_eq!(p.write_pin(&mut o, 0), Err(Refused::InvalidArgument));
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 0, "withdrawn once");
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut freed = Vec::new();
        let mut out = |f, page, becomes| freed.push((f, page, becomes));
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Some(10));
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Some(11));
        assert!(!o.empty());
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), None);
        assert_eq!(freed, [(7, 0, Revoked::Absent)]);
        assert!(o.finished(o.epoch()));
        assert_eq!(p.supply(&mut o, 0, 8, false, PAGE), Ok(Supplied::Installed));
        assert_eq!(o.revoke(), Revoke::Done, "nothing revoking");
    }

    /// A removal while a walk runs is taken by a second pass over every
    /// mapper, those the first visited and one linked during it, which may
    /// map the newly removed page; its caller knows the pass by its epoch.
    #[test]
    fn a_removal_during_a_walk_is_taken_by_a_second_pass() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 2 * PAGE);
        let mut ring = Ring::default();
        ring.link(10, &o);
        ring.link(11, &o);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.supply(&mut o, 1, 8, false, 2 * PAGE).unwrap();
        p.withdraw(&mut o, 0, 1, &mut Nobody);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut freed = Vec::new();
        let mut out = |f, page, becomes| freed.push((f, page, becomes));
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Some(10));
        ring.link(12, &o);
        p.fail(&mut o, 1, 1).unwrap();
        let Revoke::Queued(second) = o.revoke() else { panic!("a walk runs") };
        assert!(!o.finished(second));
        let visits: Vec<_> = core::iter::from_fn(|| p.walk(&mut o, &mut ring, &mut Nobody, &mut out)).collect();
        assert_eq!(visits, [11, 10, 12, 11], "the first pass ends at 11, the second visits all three");
        assert!(o.finished(second));
        assert_eq!(freed, [(8, 1, Revoked::Failed), (7, 0, Revoked::Absent)]);
        assert_eq!((p.lookup(&o, 0), p.lookup(&o, 1)), (Lookup::Absent, Lookup::Failed));
    }

    /// A mapper linked during a walk that nothing else joins maps nothing
    /// revoking, so the walk passes it by.
    #[test]
    fn a_mapper_linked_during_a_walk_is_passed_by() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, PAGE);
        let mut ring = Ring::default();
        ring.link(10, &o);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.withdraw(&mut o, 0, 1, &mut Nobody);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut out = |_, _, _| {};
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Some(10));
        ring.link(11, &o);
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), None);
    }

    /// A page failed and then shrunk away while revoking ends with no slot,
    /// and one failed and then withdrawn still ends failed.
    #[test]
    fn a_revoking_page_ends_failed_only_where_the_object_still_holds_it() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 2 * PAGE);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.supply(&mut o, 1, 8, false, 2 * PAGE).unwrap();
        p.fail(&mut o, 0, 2).unwrap();
        assert_eq!(p.withdraw(&mut o, 0, 2, &mut Nobody).0, 0, "nothing left to withdraw");
        p.resize(&mut o, PAGE);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut ended = Vec::new();
        assert_eq!(p.walk(&mut o, &mut Unmapped, &mut Nobody, &mut |f, page, becomes| ended.push((f, page, becomes))), None);
        assert_eq!(ended, [(8, 1, Revoked::Removed), (7, 0, Revoked::Failed)]);
        assert_eq!((p.lookup(&o, 0), p.used()), (Lookup::Failed, 1));
    }

    /// No reader waits.
    struct Nobody;

    impl Waiters for Nobody {
        fn each(&mut self, _: &mut dyn FnMut(&mut Want)) {}
    }

    #[test]
    fn a_want_is_named_once_while_waited_and_again_after_a_withdraw() {
        let mut w = Want::NONE;
        assert_eq!(w.report(), None, "nothing asked");
        assert!(w.ask(4, 16));
        assert!(!w.withdrawn(4, 1), "a withdraw before its notice is no news");
        assert_eq!(w.report(), Some((4, 16)));
        assert!(!w.ask(4, 16), "asked again while it waits");
        assert_eq!(w.report(), None, "named twice while it waits");
        assert!(!w.withdrawn(0, 4), "a withdraw before the range");
        assert!(!w.withdrawn(20, 1), "a withdraw past the range");
        assert!(w.withdrawn(19, 8), "a withdraw of the range's last page goes unheard");
        assert!(w.unreported());
        assert!(!w.ask(4, 16), "asked again after the withdraw");
        assert_eq!(w.report(), Some((4, 16)));
        assert!(w.ask(5, 15), "a range that moved");
        assert_eq!(w.report(), Some((5, 15)));
        assert_eq!(w.range(), (5, 15));
    }

    /// XSH 2.9.7: a read that began before a write, or while one was under
    /// way, never counts; one that began after it does.
    #[test]
    fn a_read_counts_only_where_no_write_began_or_was_under_way() {
        let mut p = Pages::<u64>::new(8, 4).unwrap();
        let mut o = p.object(1, PAGE);
        let before = o.read_begin().expect("nothing writes yet");
        o.write_begin();
        assert_eq!(o.read_begin(), None, "a read meets the write in flight");
        assert!(!o.unchanged(before), "a read that began before it meets it");
        o.write_end(true);
        assert!(!o.unchanged(before), "and still does once it ended");
        let after = o.read_begin().expect("the write ended");
        assert!(o.unchanged(after));
        p.resize(&mut o, 0);
        assert!(!o.unchanged(after), "a shrink is a change a read meets");
    }

    /// A pager's write that takes several calls is one write: the first short
    /// call leaves it under way, later ones nest in it, and only its end lets
    /// a read begin.
    #[test]
    fn a_short_call_holds_the_write_until_its_end() {
        let mut p = Pages::<u64>::new(8, 4).unwrap();
        let mut o = p.object(2, PAGE);
        assert_eq!(o.end_write(), Err(Refused::InvalidArgument), "nothing is held");
        o.write_begin();
        o.write_end(false);
        assert_eq!(o.read_begin(), None, "a short call's write is under way");
        o.write_begin();
        o.write_end(false);
        o.write_begin();
        o.write_end(true);
        assert_eq!(o.read_begin(), None, "a later call, short or whole, ends no held write");
        let held = o.version;
        assert_eq!(o.end_write(), Ok(()));
        let v = o.read_begin().expect("the end lets a read begin");
        assert!(o.unchanged(v) && v != Version(held), "the end moved the version");
        assert_eq!(o.end_write(), Err(Refused::InvalidArgument), "a write ends once");
    }

    #[test]
    fn anonymous_bytes_survive_cache_operations_and_release_with_the_object() {
        let mut pages = Pages::new(2, 0).unwrap();
        let mut object = pages.anonymous(1, 0);
        assert_eq!(pages.supply(&mut object, 0, 42, true, PAGE), Ok(Supplied::Installed));
        assert_eq!(pages.dirty_held(), 0);
        assert_eq!(pages.write_pin(&mut object, 0), Ok(42));
        pages.write_unpin(&mut object, 0, 42, PAGE);
        assert_eq!(pages.withdraw(&mut object, 0, 1, &mut Nobody).0, 0);
        assert_eq!(pages.writeback_begin(&mut object, 0, 1), Err(Refused::InvalidArgument));
        assert_eq!(pages.fail(&mut object, 0, 1), Err(Refused::InvalidArgument));
        assert_eq!(pages.lookup(&object, 0), Lookup::Present(42));
        assert_eq!(pages.supply(&mut object, 1, 43, true, 2 * PAGE), Ok(Supplied::Installed));
        assert_eq!(pages.supply(&mut object, 2, 44, true, 3 * PAGE), Err(Refused::NoRoom));
        pages.resize(&mut object, 4 * PAGE);
        assert_eq!(pages.lookup(&object, 3), Lookup::Zero, "a hole reads without any free slot");
        pages.resize(&mut object, PAGE);
        assert_eq!(settle(&mut pages, &mut object), [43]);
        pages.invalidate(&mut object);
        assert_eq!(settle(&mut pages, &mut object), [42]);
        assert_eq!(pages.used(), 0);
        assert!(object.empty());
    }

    #[test]
    fn a_supplied_page_is_found_and_a_second_supply_changes_nothing() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(1, 3 * PAGE);
        assert_eq!(p.lookup(&o, 1), Lookup::Absent);
        assert_eq!(p.supply(&mut o, 1, 100, false, 2 * PAGE), Ok(Supplied::Installed));
        assert_eq!(p.supply(&mut o, 1, 200, false, 2 * PAGE), Ok(Supplied::Present));
        assert_eq!(p.lookup(&o, 1), Lookup::Present(100));
        assert_eq!(p.lookup(&o, 3), Lookup::PastEnd);
    }

    #[test]
    fn a_clean_supply_past_the_size_is_refused_and_a_dirty_one_extends_it() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(2, PAGE);
        assert_eq!(p.supply(&mut o, 1, 1, false, 2 * PAGE), Err(Refused::PastEnd));
        assert_eq!(o.size(), PAGE);
        let mut short = p.object(3, 100);
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
        let mut o = p.object(4, 100);
        p.supply(&mut o, 0, 7, false, 100).unwrap();
        assert_eq!(p.write_pin(&mut o, 0), Ok(7));
        assert_eq!(o.size(), 100, "the copy is in flight: a reader still stops at the old end");
        p.write_unpin(&mut o, 0, 7, 300);
        assert_eq!(o.size(), 300, "the bytes are in, and the size covers them");
        assert_eq!(p.write_pin(&mut o, 1), Err(Refused::InvalidArgument), "an absent page is supplied, not pinned");
    }

    /// A write pin whose copy faulted is released with the size as it was.
    #[test]
    fn an_abandoned_write_pin_grows_nothing_and_holds_nothing() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(6, 100);
        p.supply(&mut o, 0, 7, false, 100).unwrap();
        assert_eq!(p.write_pin(&mut o, 0), Ok(7));
        p.write_abandon(&o, 0, 7);
        assert_eq!(o.size(), 100, "an abandoned write grows nothing");
        let slot = p.find(o.id, 0).expect("the page stays");
        assert_eq!(p.slot(slot).state.write_pins, 0, "an abandoned write holds no pin");
    }

    #[test]
    fn an_unpin_after_a_shrink_removed_the_page_grows_nothing() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(5, PAGE);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.write_pin(&mut o, 0).unwrap();
        p.resize(&mut o, 0);
        settle(&mut p, &mut o);
        p.supply(&mut o, 0, 8, true, 10).unwrap();
        p.write_unpin(&mut o, 0, 7, PAGE);
        assert_eq!(o.size(), 10, "the shrink's size, and the new page's, stand over a copy they overtook");
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 0, "the new page is dirty");
        p.writeback_begin(&mut o, 0, 1).unwrap();
        assert_eq!(p.writeback_end(&mut o, 0, 1, true).unwrap()[0], 0, "and holds no pin of the old one");
    }

    #[test]
    fn the_dirty_ceiling_refuses_and_a_write_back_frees_it() {
        let mut p = Pages::new(8, 2).unwrap();
        let mut o = p.object(6, 4 * PAGE);
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
        let mut o = p.object(7, 8 * PAGE);
        for page in 0..6 {
            p.supply(&mut o, page, page, true, (page + 1) * PAGE).unwrap();
        }
        p.write_pin(&mut o, 0).unwrap(); // pinned across the begin, released inside
        assert_eq!(p.writeback_begin(&mut o, 0, 6).unwrap()[0], 0b11_1111);
        p.write_unpin(&mut o, 0, 0, PAGE);
        p.write_pin(&mut o, 1).unwrap(); // dirtied inside
        p.write_unpin(&mut o, 1, 1, 2 * PAGE);
        p.resize(&mut o, 5 * PAGE); // page 5 removed inside
        p.fail(&mut o, 4, 1).unwrap(); // page 4 failed inside
        assert_eq!(p.dirty_held(), 4, "a revoking page's bytes are no longer written back");
        assert_eq!(settle(&mut p, &mut o), [5, 4]);
        let kept = p.writeback_end(&mut o, 0, 6, true).unwrap()[0];
        assert_eq!(kept, 0b11, "pinned since the begin and dirtied since it; the removed are clear");
        assert_eq!(p.dirty_held(), 2);
        p.writeback_begin(&mut o, 0, 2).unwrap();
        assert_eq!(p.writeback_end(&mut o, 0, 2, false).unwrap()[0], 0b11, "a failed write-back keeps every page");
    }

    #[test]
    fn a_range_ends_once_in_leading_parts_and_keeps_its_unwritten_suffix() {
        let mut p: Pages<u64> = Pages::new(8, 8).unwrap();
        let mut o = p.object(8, PAGE * 1024);
        for page in 0..4 {
            p.supply(&mut o, page, page, true, (page + 1) * PAGE).unwrap();
        }
        p.writeback_begin(&mut o, 0, 4).unwrap();
        assert_eq!(p.writeback_begin(&mut o, 3, 2), Err(Refused::InvalidArgument), "overlapping");
        assert_eq!(p.writeback_end(&mut o, 1, 3, true), Err(Refused::InvalidArgument), "not a leading part");
        assert_eq!(p.writeback_end(&mut o, 0, 0, true), Err(Refused::InvalidArgument), "empty");
        assert_eq!(p.writeback_end(&mut o, 0, 5, true), Err(Refused::InvalidArgument), "past the range");
        assert_eq!(p.writeback_end(&mut o, 0, 2, true), Ok([0; 8]));
        assert_eq!(p.dirty_held(), 2);
        assert_eq!(p.writeback_begin(&mut o, 2, 2), Err(Refused::InvalidArgument), "the suffix remains held");
        assert_eq!(p.writeback_end(&mut o, 0, 2, true), Err(Refused::InvalidArgument), "the prefix already ended");
        assert_eq!(p.writeback_end(&mut o, 2, 2, false).unwrap()[0], 0b11);
        assert_eq!(p.writeback_begin(&mut o, 0, 4).unwrap()[0], 0b1100, "only the unwritten suffix is retried");
        assert_eq!(p.writeback_end(&mut o, 0, 4, true), Ok([0; 8]));
        assert_eq!(p.dirty_held(), 0);
        assert_eq!(p.writeback_end(&mut o, 0, 4, true), Err(Refused::InvalidArgument), "twice");
        assert_eq!(p.writeback_begin(&mut o, 0, RANGE_PAGES + 1), Err(Refused::InvalidArgument));
        assert_eq!(p.writeback_begin(&mut o, 0, 0), Err(Refused::InvalidArgument));
    }

    #[test]
    fn only_clean_unheld_pages_are_withdrawn() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(9, 4 * PAGE);
        p.supply(&mut o, 0, 10, false, PAGE).unwrap();
        p.supply(&mut o, 1, 11, true, 2 * PAGE).unwrap();
        p.supply(&mut o, 2, 12, true, 3 * PAGE).unwrap();
        p.supply(&mut o, 3, 13, false, 4 * PAGE).unwrap();
        p.writeback_begin(&mut o, 2, 1).unwrap(); // clean but under write-back
        p.write_pin(&mut o, 3).unwrap(); // write-pinned
        assert_eq!(p.withdraw(&mut o, 0, 4, &mut Nobody).0, 1);
        assert_eq!(p.lookup(&o, 0), Lookup::Absent, "revoking reads as absent");
        assert_eq!(settle(&mut p, &mut o), [10]);
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
        let mut o = p.object(1, LAST_END);
        p.supply(&mut o, 7, 1, false, 8 * PAGE).unwrap();
        p.supply(&mut o, 1 << 40, 2, false, ((1 << 40) + 1) * PAGE).unwrap();
        p.probes.set(0);
        assert_eq!(p.withdraw(&mut o, 8, 1 << 24, &mut Nobody).0, 0, "neither page lies in 8..8+2^24");
        assert!(p.probes.get() < 8, "{} probes for a range of 2^24 pages", p.probes.get());
        assert_eq!(p.withdraw(&mut o, 8, u64::MAX, &mut Nobody).0, 1);
        assert_eq!(settle(&mut p, &mut o), [2], "only the page inside the range");
        assert_eq!(p.withdraw(&mut o, 0, u64::MAX, &mut Nobody).0, 1);
        settle(&mut p, &mut o);
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
        let mut o = p.object(1, LAST_END);
        let last = LAST_END / PAGE - 1;
        assert_eq!(p.supply(&mut o, last, 1, true, LAST_END), Ok(Supplied::Installed), "the last page");
        assert!(Pages::<u64>::new(Pages::<u64>::MAX_CAPACITY + 1, 0).is_none());
    }

    #[test]
    fn a_shrink_removes_dirty_pages_releases_their_slots_and_names_the_tail() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(11, 3 * PAGE);
        for page in 0..3 {
            p.supply(&mut o, page, page + 100, true, (page + 1) * PAGE).unwrap();
        }
        assert_eq!(p.resize(&mut o, PAGE + 5), Some(101));
        assert_eq!(p.dirty_held(), 2);
        assert_eq!(p.lookup(&o, 2), Lookup::PastEnd);
        assert_eq!(settle(&mut p, &mut o), [102]);
        assert_eq!(p.resize(&mut o, 4 * PAGE), None, "growth removes nothing");
        assert_eq!(p.lookup(&o, 2), Lookup::Absent, "and leaves the new range absent");
    }

    #[test]
    fn a_failed_page_answers_failed_until_supplied_again() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(12, 2 * PAGE);
        p.supply(&mut o, 0, 5, false, PAGE).unwrap();
        p.fail(&mut o, 0, 2).unwrap();
        assert_eq!((p.lookup(&o, 0), p.lookup(&o, 1)), (Lookup::Absent, Lookup::Failed), "page 0 revoking");
        assert_eq!(settle(&mut p, &mut o), [5]);
        assert_eq!((p.lookup(&o, 0), p.lookup(&o, 1)), (Lookup::Failed, Lookup::Failed));
        assert_eq!(p.supply(&mut o, 1, 6, false, 2 * PAGE), Ok(Supplied::Installed));
        assert_eq!(p.lookup(&o, 1), Lookup::Present(6));
    }

    #[test]
    fn an_invalidated_object_answers_gone_and_gives_every_frame_back() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(13, 4 * PAGE);
        p.supply(&mut o, 0, 1, true, PAGE).unwrap();
        p.supply(&mut o, 2, 3, false, 3 * PAGE).unwrap();
        p.invalidate(&mut o);
        assert_eq!(p.lookup(&o, 0), Lookup::Gone);
        let mut freed = settle(&mut p, &mut o);
        freed.sort();
        assert_eq!(freed, [1, 3]);
        assert_eq!((p.used(), p.dirty_held()), (0, 0));
        assert_eq!(p.lookup(&o, 0), Lookup::Gone);
        assert_eq!(p.supply(&mut o, 0, 9, false, PAGE), Err(Refused::Gone));
    }

    #[test]
    fn a_full_table_refuses_and_every_slot_is_reached_after_churn() {
        let mut p = Pages::new(64, 64).unwrap();
        let mut objects: Vec<Object> = (0..4).map(|i| p.object(i, 1 << 30)).collect();
        let mut model: BTreeSet<(usize, u64)> = BTreeSet::new();
        let mut x = 1u64;
        for round in 0..20_000u64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let which = (x >> 33) as usize % 4;
            let page = (x >> 40) % 97;
            let o = &mut objects[which];
            if (x >> 20).is_multiple_of(3) {
                p.withdraw(o, page, 1, &mut Nobody);
                if !settle(&mut p, o).is_empty() {
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

    /// A page made absent again by a shrink and a grow, or by a dirty supply
    /// refused over its failure, moves the version, which ends a holder's
    /// wait; a withdraw does not, and names the waiter's range again instead.
    #[test]
    fn a_holders_wait_ends_where_its_page_or_its_object_changed() {
        let mut p = Pages::<u64>::new(8, 0).unwrap();
        let mut o = p.object(1, PAGE);
        let since = o.read_begin().unwrap();
        assert!(!p.wait_over(&o, 0, since), "absent");
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        assert!(p.wait_over(&o, 0, since), "supplied");
        p.resize(&mut o, 0);
        settle(&mut p, &mut o);
        p.resize(&mut o, PAGE);
        assert_eq!(p.lookup(&o, 0), Lookup::Absent);
        assert!(p.wait_over(&o, 0, since), "shrunk and grown back before the reader looked");

        let mut failed = p.object(2, PAGE);
        let since = failed.read_begin().unwrap();
        p.fail(&mut failed, 0, 1).unwrap();
        failed.write_begin();
        assert_eq!(p.supply(&mut failed, 0, 8, true, PAGE), Err(Refused::DirtyFull));
        failed.write_end(true);
        assert_eq!(p.lookup(&failed, 0), Lookup::Absent, "the refused supply took the failure");
        assert!(p.wait_over(&failed, 0, since), "a write over the failure before the reader looked");

        let mut taken = p.object(3, PAGE);
        let since = taken.read_begin().unwrap();
        p.supply(&mut taken, 0, 9, false, PAGE).unwrap();
        let mut want = Want::NONE;
        want.ask(0, 1);
        assert_eq!(want.report(), Some((0, 1)));
        assert_eq!(p.withdraw(&mut taken, 0, 1, &mut Nobody).0, 1);
        assert_eq!(taken.revoke(), Revoke::Walk);
        assert!(!want.unreported(), "named only once no mapping reaches the page");
        assert_eq!(p.walk(&mut taken, &mut Unmapped, &mut One(&mut want), &mut |_, _, _| ()), None);
        assert!(!p.wait_over(&taken, 0, since), "a withdraw leaves the read waiting");
        assert!(want.unreported(), "and its walk's end names its range again");
    }

    /// One reader's want.
    struct One<'a>(&'a mut Want);

    impl Waiters for One<'_> {
        fn each(&mut self, f: &mut dyn FnMut(&mut Want)) {
            f(self.0);
        }
    }

    /// One holder's read of page 0, waiting on the pager the kernel's read
    /// loop waits on, beside a client that withdraws, fails, shrinks, grows and
    /// writes. The reader looks again only where its wait is over.
    mod wait {
        use super::*;

        #[derive(Clone, Copy, Debug)]
        pub(super) enum Step {
            Withdraw,
            Fail,
            Shrink,
            Grow,
            /// A pager's write whose dirty supply the ceiling refuses.
            Refused,
            /// A supply nobody asked for.
            Supply,
            /// The pager reads its notices.
            Report,
            /// The pager supplies what it read a request for.
            Answer,
            Look,
        }

        pub(super) const STEPS: [Step; 9] =
            [Step::Withdraw, Step::Fail, Step::Shrink, Step::Grow, Step::Refused, Step::Supply, Step::Report, Step::Answer, Step::Look];

        pub(super) struct World {
            p: Pages<u64>,
            o: Object,
            since: Version,
            want: Want,
            /// The read waits; `false` once it answered.
            waits: bool,
            asked: bool,
            next: u64,
        }

        impl World {
            /// The read begins and finds its page absent.
            pub(super) fn new() -> Self {
                let mut p = Pages::new(4, 0).unwrap();
                let o = p.object(1, PAGE);
                let since = o.read_begin().unwrap();
                let mut want = Want::NONE;
                want.ask(0, 1);
                World { p, o, since, want, waits: true, asked: false, next: 10 }
            }

            pub(super) fn step(&mut self, step: Step) -> bool {
                let next = self.next + 1;
                match step {
                    Step::Withdraw => {
                        self.p.withdraw(&mut self.o, 0, 1, &mut One(&mut self.want));
                        self.settle();
                    }
                    Step::Fail => {
                        self.p.fail(&mut self.o, 0, 1).unwrap();
                        self.settle();
                    }
                    Step::Shrink if self.o.size() == PAGE => {
                        self.p.resize(&mut self.o, 0);
                        self.settle();
                    }
                    Step::Grow if self.o.size() == 0 => {
                        self.p.resize(&mut self.o, PAGE);
                        self.settle();
                    }
                    Step::Refused => {
                        self.o.write_begin();
                        let _ = self.p.supply(&mut self.o, 0, next, true, PAGE);
                        self.o.write_end(true);
                    }
                    Step::Supply => {
                        let _ = self.p.supply(&mut self.o, 0, next, false, PAGE);
                    }
                    Step::Report if self.waits && !self.asked && self.want.report().is_some() => self.asked = true,
                    Step::Answer if self.asked => {
                        self.asked = false;
                        // The file server fails a requested range past the
                        // end, which inserts nothing there.
                        if self.p.supply(&mut self.o, 0, next, false, PAGE) == Err(Refused::PastEnd) {
                            self.p.fail(&mut self.o, 0, 1).unwrap();
                        }
                    }
                    Step::Look if self.waits && self.p.wait_over(&self.o, 0, self.since) => {
                        // As the read loop: a moved version answers
                        // `WouldBlock`; else the page decides.
                        let absent = self.o.unchanged(self.since) && matches!(self.p.lookup(&self.o, 0), Lookup::Absent);
                        self.waits = absent;
                        if absent {
                            self.want.ask(0, 1);
                        }
                    }
                    _ => return false,
                }
                self.next = next;
                true
            }

            /// A removal's walk, over no mapper, to its end.
            fn settle(&mut self) {
                if self.o.revoke() == Revoke::Walk {
                    let walked = self.p.walk(&mut self.o, &mut super::Unmapped, &mut One(&mut self.want), &mut |_, _, _| ());
                    assert_eq!(walked, None);
                }
            }

            /// The pager answers what it hears and the reader looks whenever
            /// its wait is over: the read ends.
            pub(super) fn drain(&mut self, steps: &[Step]) {
                for _ in 0..8 {
                    for step in [Step::Look, Step::Report, Step::Answer] {
                        self.step(step);
                    }
                }
                assert!(!self.waits, "after {steps:?}: the read waits for ever, {:?}", self.want);
            }
        }
    }

    /// Every sequence of six of those steps, each then drained: no read is
    /// left waiting on a page nobody is asked for.
    #[test]
    fn no_interleaving_leaves_a_read_waiting_unasked() {
        let mut stack = std::vec![std::vec::Vec::new()];
        let mut played = 0u64;
        while let Some(seq) = stack.pop() {
            played += 1;
            let mut w = wait::World::new();
            assert!(seq.iter().all(|&s| w.step(s)), "a sequence played before");
            w.drain(&seq);
            if seq.len() == 6 {
                continue;
            }
            for &step in &wait::STEPS {
                let mut next = seq.clone();
                next.push(step);
                let mut w = wait::World::new();
                if next.iter().all(|&s| w.step(s)) {
                    stack.push(next);
                }
            }
        }
        assert!(played > 10_000, "only {played} sequences played");
    }

    /// Two spaces mapping one object of two pages, as the kernel drives the
    /// index: a fault writes a leaf only for a present page, a visit clears
    /// every leaf the index no longer holds present as that frame, and an
    /// unmap clears its leaves before its node leaves the ring.
    mod mapped {
        use super::*;

        #[derive(Clone, Copy, Debug)]
        pub(super) enum Step {
            Map(u32),
            Unmap(u32),
            Fault(u32, u64),
            Supply(u64),
            Withdraw(u64),
            Fail(u64),
            Shrink,
            Grow,
            End,
            /// The walker's step under the pager's lock.
            Walk,
            /// The walker's visit of the node it took, under that space's lock.
            Visit,
            /// The server reads its notices: each request not yet named.
            Report,
            /// The server answers the oldest request it read, by a supply.
            Answer,
            /// Every parked fault looks at its page again.
            Retry,
        }

        pub(super) const STEPS: [Step; 21] = [
            Step::Map(0),
            Step::Map(1),
            Step::Unmap(0),
            Step::Unmap(1),
            Step::Fault(0, 0),
            Step::Fault(0, 1),
            Step::Fault(1, 0),
            Step::Fault(1, 1),
            Step::Supply(0),
            Step::Supply(1),
            Step::Withdraw(0),
            Step::Withdraw(1),
            Step::Fail(0),
            Step::Shrink,
            Step::Grow,
            Step::End,
            Step::Walk,
            Step::Visit,
            Step::Report,
            Step::Answer,
            Step::Retry,
        ];

        pub(super) struct World {
            p: Pages<u64>,
            o: Object,
            ring: Ring,
            mapped: [bool; 2],
            leaves: [[Option<u64>; 2]; 2],
            next: u64,
            freed: BTreeSet<u64>,
            walking: bool,
            visiting: Option<u32>,
            /// Faults parked on an absent page: the space, the page, and the
            /// request as the pager hears of it.
            parked: std::vec::Vec<(u32, u64, Want)>,
            /// Pages the server read requests for and has yet to supply.
            asked: std::collections::VecDeque<u64>,
        }

        /// The parked faults' wants, which removals name again.
        struct Parked<'a>(&'a mut std::vec::Vec<(u32, u64, Want)>);

        impl Waiters for Parked<'_> {
            fn each(&mut self, f: &mut dyn FnMut(&mut Want)) {
                for (_, _, want) in self.0.iter_mut() {
                    f(want);
                }
            }
        }

        impl World {
            pub(super) fn new() -> Self {
                let mut p = Pages::new(4, 4).unwrap();
                let o = p.object(1, 2 * PAGE);
                World {
                    p,
                    o,
                    ring: Ring::default(),
                    mapped: [false; 2],
                    leaves: [[None; 2]; 2],
                    next: 100,
                    freed: BTreeSet::new(),
                    walking: false,
                    visiting: None,
                    parked: std::vec::Vec::new(),
                    asked: std::collections::VecDeque::new(),
                }
            }

            /// Whether `step` can be taken now; takes it if so.
            pub(super) fn step(&mut self, step: Step) -> bool {
                match step {
                    Step::Map(s) if !self.mapped[s as usize] => {
                        self.ring.link(s, &self.o);
                        self.mapped[s as usize] = true;
                    }
                    Step::Unmap(s) if self.mapped[s as usize] => {
                        self.leaves[s as usize] = [None; 2];
                        let at = self.ring.0.iter().position(|&(n, _)| n == s).expect("linked while mapped");
                        self.ring.0.remove(at);
                        self.mapped[s as usize] = false;
                    }
                    Step::Fault(s, page) if self.mapped[s as usize] && self.leaves[s as usize][page as usize].is_none() => {
                        if !self.touch(s, page) {
                            if self.p.lookup(&self.o, page) != Lookup::Absent || self.parked.iter().any(|&(t, q, _)| (t, q) == (s, page)) {
                                return false;
                            }
                            let mut want = Want::NONE;
                            want.ask(page, 1);
                            self.parked.push((s, page, want));
                        }
                    }
                    Step::Supply(page) if !self.o.gone() => {
                        self.next += 1;
                        let _ = self.p.supply(&mut self.o, page, self.next, false, (page + 1) * PAGE);
                    }
                    Step::Withdraw(page) if !self.o.gone() => {
                        self.p.withdraw(&mut self.o, page, 1, &mut Parked(&mut self.parked));
                        self.revoke();
                    }
                    Step::Fail(page) if !self.o.gone() => {
                        let _ = self.p.fail(&mut self.o, page, 1);
                        self.revoke();
                    }
                    Step::Shrink if !self.o.gone() && self.o.size() == 2 * PAGE => {
                        self.p.resize(&mut self.o, PAGE);
                        self.revoke();
                    }
                    Step::Grow if !self.o.gone() && self.o.size() == PAGE => {
                        self.p.resize(&mut self.o, 2 * PAGE);
                        self.revoke();
                    }
                    Step::End if !self.o.gone() => {
                        self.p.invalidate(&mut self.o);
                        self.revoke();
                    }
                    Step::Walk if self.walking && self.visiting.is_none() => {
                        let freed = &mut self.freed;
                        let walked = self.p.walk(&mut self.o, &mut self.ring, &mut Parked(&mut self.parked), &mut |f, _, _| {
                            assert!(freed.insert(f), "frame {f} given back twice");
                        });
                        match walked {
                            Some(node) => self.visiting = Some(node),
                            None => self.walking = false,
                        }
                    }
                    Step::Report => {
                        let before = self.asked.len();
                        for (_, _, want) in &mut self.parked {
                            if let Some((first, _)) = want.report() {
                                self.asked.push_back(first);
                            }
                        }
                        if self.asked.len() == before {
                            return false;
                        }
                    }
                    Step::Answer => {
                        let Some(page) = self.asked.pop_front() else { return false };
                        if !self.o.gone() {
                            self.next += 1;
                            let _ = self.p.supply(&mut self.o, page, self.next, false, (page + 1) * PAGE);
                        }
                    }
                    Step::Retry if !self.parked.is_empty() => {
                        let parked = core::mem::take(&mut self.parked);
                        for (s, page, mut want) in parked {
                            if !self.mapped[s as usize] || self.touch(s, page) {
                                continue;
                            }
                            match self.p.lookup(&self.o, page) {
                                Lookup::Absent => {
                                    want.ask(page, 1);
                                    self.parked.push((s, page, want));
                                }
                                // Refused: failed, past the end, or gone.
                                _ => {}
                            }
                        }
                    }
                    Step::Visit if self.visiting.is_some() => {
                        let s = self.visiting.take().unwrap() as usize;
                        for page in 0..2 {
                            if let Some(f) = self.leaves[s][page] {
                                if self.p.lookup(&self.o, page as u64) != Lookup::Present(f) {
                                    self.leaves[s][page] = None;
                                }
                            }
                        }
                    }
                    _ => return false,
                }
                self.check(step);
                true
            }

            /// A fault's hit: `page` and its neighbour written where present.
            fn touch(&mut self, s: u32, page: u64) -> bool {
                let Lookup::Present(_) = self.p.lookup(&self.o, page) else { return false };
                for at in 0..2 {
                    if let (None, Lookup::Present(f)) = (self.leaves[s as usize][at], self.p.lookup(&self.o, at as u64)) {
                        self.leaves[s as usize][at] = Some(f);
                    }
                }
                true
            }

            /// Once nothing is removed any more, the walk ends, the server
            /// answers every request it hears of, and every parked fault
            /// retries: none is left parked.
            pub(super) fn drain(&mut self, steps: &[Step]) {
                let fair = [Step::Walk, Step::Visit, Step::Report, Step::Answer, Step::Retry];
                for _ in 0..64 {
                    if !fair.iter().fold(false, |moved, &step| self.step(step) | moved) {
                        break;
                    }
                }
                assert!(self.parked.is_empty(), "after {steps:?}: faults left parked {:?}", self.parked);
            }

            fn revoke(&mut self) {
                match self.o.revoke() {
                    Revoke::Walk => {
                        assert!(!self.walking, "a second walker");
                        self.walking = true;
                    }
                    Revoke::Queued(_) => assert!(self.walking, "queued behind no walk"),
                    Revoke::Done => {}
                }
            }

            /// Every leaf maps the frame the index holds for its page, present
            /// or revoking, and never one given back.
            fn check(&self, step: Step) {
                for (s, leaves) in self.leaves.iter().enumerate() {
                    for (page, leaf) in leaves.iter().enumerate() {
                        let Some(f) = *leaf else { continue };
                        assert!(!self.freed.contains(&f), "after {step:?}: space {s} page {page} maps given-back frame {f}");
                        let held = self.p.find(self.o.id, page as u64).map(|i| self.p.slot(i).frame);
                        assert_eq!(held, Some(Some(f)), "after {step:?}: space {s} page {page} maps {f}, the index holds {held:?}");
                    }
                }
                assert!(self.walking || self.o.revoking == 0, "after {step:?}: pages revoking with no walk");
            }
        }
    }

    fn play(steps: &[mapped::Step]) -> Option<mapped::World> {
        let mut w = mapped::World::new();
        steps.iter().all(|&s| w.step(s)).then_some(w)
    }

    /// From both spaces mapping both pages, every sequence of five steps of
    /// faults, supplies, removals, mappings and the walk keeps each leaf on
    /// its page's one frame.
    #[test]
    fn every_interleaving_keeps_each_leaf_on_its_pages_one_frame() {
        use mapped::Step;
        let prefix = [Step::Map(0), Step::Map(1), Step::Supply(0), Step::Supply(1), Step::Fault(0, 0), Step::Fault(1, 1)];
        let mut stack = std::vec![prefix.to_vec()];
        let mut played = 0u64;
        while let Some(seq) = stack.pop() {
            played += 1;
            play(&seq).expect("a sequence played before").drain(&seq);
            if seq.len() == prefix.len() + 5 {
                continue;
            }
            for &step in &mapped::STEPS {
                let mut next = seq.clone();
                next.push(step);
                if play(&next).is_some() {
                    stack.push(next);
                }
            }
        }
        assert!(played > 20_000, "only {played} sequences played");
    }

    /// The same over long sequences, seeded: removals queued behind running
    /// walks, ends meeting walks, unmaps while their node is visited.
    #[test]
    fn long_runs_keep_each_leaf_on_its_pages_one_frame() {
        let mut x = 7u64;
        for _ in 0..2_000 {
            let mut w = mapped::World::new();
            for _ in 0..200 {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                w.step(mapped::STEPS[(x >> 33) as usize % mapped::STEPS.len()]);
            }
            w.drain(&[]);
        }
    }
}
