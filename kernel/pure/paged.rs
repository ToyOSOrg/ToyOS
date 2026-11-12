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
//! walk running is taken by that walk's next pass; after the last pass and,
//! for an object a mapper ever linked, a sweep of the futex waits on them,
//! the walk's end gives every revoking page's frame out ([`Pages::walk`]).
//!
//! **Every write is admitted page by page** ([`Pages::admit`]): refused on a
//! gone or sealed object, refused to a holder whose page would become dirty
//! without a credit the pager granted, and refused at the dirty ceiling. A
//! holder's write is checked whole first ([`Pages::holder_fits`]), in the same
//! hold of the caller's lock as its admission. A page a holder's credit paid
//! for gives the credit back where the pager writes it, or where it goes
//! before its write-back.
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
//! **One write at a time** (POSIX XSH 2.9.7): every write takes the object's
//! write side ([`Side`]) for its whole: a holder's, which never waits for it
//! and is refused while it is taken or the pager waits for it; and the
//! pager's, from its hold to its unhold, its truncations and its own reads
//! among them. A holder's read begins only while no write is under way
//! ([`Object::read_begin`]), and its bytes count only where none has begun
//! since ([`Object::unchanged`]); one that met [`WRITES_A_READ_MEETS`]
//! writes asks the pager, whose read holds the writes off. While the pager
//! holds them, a holder states the size and time of the moment it took them
//! ([`Object::stat`]).
//!
//! **A holder's dirtying is told once** ([`Object::take_tell`]): the first page
//! a holder dirties after the pager last took the range
//! ([`Object::take_dirtied`]) owes the pager a notice; the rest widen the range.
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
/// object's version, which ends the wait ([`Pages::wait_over`]), and the read
/// that begins again asks anew ([`Want::anew`]).
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

    /// The reader begins its read again, after a write: its next range is
    /// news, as what was supplied for its last notice may have gone since.
    pub fn anew(&mut self) {
        *self = Self::NONE;
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
    /// The object is sealed: something runs from it, so nothing writes it.
    Sealed,
    /// A holder's write would dirty a page past the credit its pager granted.
    NoCredit,
    /// A holder's write meets a page inside the size that is not present: the
    /// pager supplies it.
    Absent,
    /// A holder's write spans more pages than one call takes whole, or ends
    /// past [`HOLDER_END`]: the pager writes it.
    OutOfReach,
    /// A range that is empty, too long, overlaps an open write-back, or names
    /// none that was begun.
    InvalidArgument,
}

/// Who asks a page to be written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Writer {
    /// The pager, which reserved the space for its own writes.
    Pager,
    /// A holder, writing against the credit the pager granted the object.
    Holder,
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

/// What a walk's step asks of its caller, or that it ended ([`Pages::walk`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Next<N> {
    /// Visit this mapper: clear its leaves of the walk's set.
    Visit(N),
    /// End every futex wait on a frame of the walk's set: only of an object
    /// a mapper ever linked ([`Object::link`]).
    Sweep,
    /// The walk ended: the set's frames were given out.
    Finished,
}

/// Where a reader of an object's revoking pages has read to
/// ([`Pages::revoking`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Start,
    After(u32),
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
/// a walk visits them. A node links at the tail, seen at the epoch
/// [`Object::link`] answers, and leaves only once its space maps none of the
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
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
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
    /// A holder's credit paid for its dirtying, and the pager owes the room.
    credited: bool,
    /// The write-back under way spends what a holder's credit paid for.
    credit_in_flight: bool,
}

impl State {
    const CLEAN: Self =
        Self { dirty: false, writeback: false, write_pins: 0, pinned_since_begin: false, credited: false, credit_in_flight: false };

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
    /// The next of its object's revoking slots, in the order they were marked.
    revoke_next: u32,
}

/// One object's part of the table: its size, whether its pager ended, the
/// list of its slots, and its writes under way.
#[cfg_attr(test, derive(Clone, PartialEq, Eq, Hash))]
pub struct Object {
    id: u32,
    anonymous: bool,
    size: u64,
    gone: bool,
    head: u32,
    pages: usize,
    /// Moved at every writer's take and give of the write side, and at every
    /// resize.
    version: u64,
    side: Side,
    /// The pager waits for the write side, which no holder takes first.
    pager_waiting: bool,
    /// The size and time when the pager took the side, what a holder states
    /// until it gives it back.
    held: Option<(u64, u64)>,
    /// Image holders and the ranges sourced from them: sealed while above 0.
    seals: u32,
    /// Pages a holder's write may still turn dirty.
    credit: u64,
    /// When it was last written, as the caller's clock says.
    mtime: u64,
    /// The pages holders dirtied since the pager last took them: `lo..hi`.
    dirtied: Option<(u64, u64)>,
    /// The pager is owed, or was given, a notice of `dirtied`.
    told: bool,
    /// A notice is owed and not yet taken.
    tell: bool,
    /// Its slots that are revoking, and the walk's set of them: a list in
    /// the order they were marked, which only the walk's end empties.
    revoking: usize,
    revoke_head: u32,
    revoke_tail: u32,
    /// The pass of the walk over its mappers, moved at each pass's start.
    epoch: u64,
    walk: Walk,
    /// The epoch whose pass last ended the walk.
    finished: u64,
    /// The epoch at which a walk was last told to sweep ([`Next::Sweep`]):
    /// 0, which no walk's pass is, before the first.
    swept: u64,
    /// A mapper ever linked. Only a mapping's leaf keys a futex wait on one
    /// of its frames, and an unmap clears its leaves without ending such a
    /// wait, so this never resets.
    linked: bool,
}

/// What a holder's read began at ([`Object::read_begin`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Version(u64);

impl Object {
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn gone(&self) -> bool {
        self.gone
    }

    /// Pages a holder's write may still turn dirty.
    pub fn credit(&self) -> u64 {
        self.credit
    }

    /// The pager grants `pages` more pages a holder may turn dirty: the
    /// credit now.
    pub fn grant(&mut self, pages: u64) -> u64 {
        self.credit = self.credit.saturating_add(pages);
        self.credit
    }

    /// The pager takes back what holders have not spent: how much.
    pub fn take_back(&mut self) -> u64 {
        core::mem::take(&mut self.credit)
    }

    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    /// The object was written, or set, at `mtime`.
    pub fn stamp(&mut self, mtime: u64) {
        self.mtime = mtime;
    }

    /// Whether a notice of holders' dirtying is owed and not yet taken; it is
    /// taken now.
    pub fn take_tell(&mut self) -> bool {
        core::mem::take(&mut self.tell)
    }

    /// The pages holders dirtied since the last call, as `(first, count)`:
    /// the next page a holder dirties owes the pager a notice again.
    pub fn take_dirtied(&mut self) -> Option<(u64, u64)> {
        self.told = false;
        self.tell = false;
        self.dirtied.take().map(|(lo, hi)| (lo, hi - lo))
    }

    /// A holder turned `page` dirty.
    fn holder_dirtied(&mut self, page: u64) {
        let (lo, hi) = self.dirtied.unwrap_or((page, page + 1));
        self.dirtied = Some((lo.min(page), hi.max(page + 1)));
        if !self.told {
            self.told = true;
            self.tell = true;
        }
    }

    fn sealed(&self) -> bool {
        self.seals > 0
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
        (self.side == Side::Free).then_some(Version(self.version))
    }

    /// The size and modification time a holder states: while the pager holds
    /// the writes, those of the moment it took them, so a statement of them
    /// comes whole before the pager's write as a read does.
    pub fn stat(&self) -> (u64, u64) {
        self.held.unwrap_or((self.size, self.mtime))
    }

    /// Where a fault's wait for one of its pages begins: a write or resize
    /// since ends it ([`Pages::wait_over`]), and the fault asks afresh
    /// ([`Want::anew`]).
    pub fn version(&self) -> Version {
        Version(self.version)
    }

    /// Whether no write or resize has begun since the read began at `v`.
    pub fn unchanged(&self, v: Version) -> bool {
        self.version == v.0
    }

    pub fn side(&self) -> Side {
        self.side
    }

    /// Whether [`take_pager`](Self::take_pager) would take the side now: what
    /// a waiting pager waits for.
    pub fn free_for_pager(&self) -> bool {
        self.side == Side::Free
    }

    /// A holder's write takes the write side, or is refused while anyone
    /// holds it or the pager waits for it: it never waits, and its writer asks
    /// the pager instead.
    pub fn take_holder(&mut self) -> bool {
        let free = self.side == Side::Free && !self.pager_waiting;
        if free {
            self.side = Side::Holder;
            self.version += 1;
        }
        free
    }

    /// A mapper links: the epoch it is seen at, which the walk running
    /// passes by, since nothing it maps from now on is revoking.
    pub fn link(&mut self) -> u64 {
        self.linked = true;
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

    /// The pager takes the write side, or waits, and asks again, while a
    /// holder's write holds it: work inside the kernel that ends without
    /// waiting on anything. No holder's write begins meanwhile.
    pub fn take_pager(&mut self) -> bool {
        let free = self.free_for_pager();
        self.pager_waiting = !free;
        if free {
            self.side = Side::Pager;
            self.held = Some((self.size, self.mtime));
            self.version += 1;
        }
        free
    }

    /// `who` gives the write side back.
    pub fn give(&mut self, who: Side) {
        assert_eq!(self.side, who, "the write side given back by another than its taker");
        self.version += 1;
        self.side = Side::Free;
        self.held = None;
    }
}

/// Who holds an object's write side, which every write takes for its whole.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Side {
    Free,
    /// A holder's write: its bytes already in the kernel, admitted and copied
    /// in without waiting on anything.
    Holder,
    /// The pager's, from its hold to its unhold: its writes, truncations and
    /// own reads, however many calls and waits they take.
    Pager,
}

/// The writes a holder's read meets, each waited out but the last, which
/// sends it to the pager.
pub const WRITES_A_READ_MEETS: u32 = 3;

/// The most pages one holder's write takes, whole.
pub const HOLDER_PAGES: u64 = 64;

/// The furthest a holder's write ends, the kernel's reach for holders: one
/// past it is refused and is the pager's to write, and a pager that mints
/// write holders accepts that no holder grows an object past it.
pub const HOLDER_END: u64 = 1 << 44;

/// The pages a holder's write of `len` bytes at `offset` spans, which one
/// call takes whole: `OutOfReach` for one past [`HOLDER_PAGES`] or ending
/// past [`HOLDER_END`], which the holder asks the pager to write.
pub fn holder_span(offset: u64, len: u64) -> Result<u64, Refused> {
    if !offset.checked_add(len).is_some_and(|end| end <= HOLDER_END) {
        return Err(Refused::OutOfReach);
    }
    let pages = (offset % PAGE + len).div_ceil(PAGE);
    if pages > HOLDER_PAGES {
        return Err(Refused::OutOfReach);
    }
    Ok(pages)
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
    /// live object of this table has. A released object's may be, once it is
    /// [`empty`](Object::empty): [`invalidate`](Self::invalidate) marks every
    /// page it held, and its walk's end takes them.
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
            side: Side::Free,
            pager_waiting: false,
            held: None,
            seals: 0,
            credit: 0,
            mtime: 0,
            dirtied: None,
            told: false,
            tell: false,
            revoking: 0,
            revoke_head: NONE,
            revoke_tail: NONE,
            swept: 0,
            linked: false,
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
        self.slots[i as usize] = Some(Slot { object: o.id, page, frame, state, revoking: None, prev: NONE, next, revoke_next: NONE });
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
        // Gone before its write-back, it needs none of the room it was paid for.
        if s.state.credited {
            o.credit += 1;
        }
        self.free[self.free_len] = i;
        self.free_len += 1;
        s.frame
    }

    /// Whether a holder's read of `o` that began at `since` and waits for
    /// `page` looks again: the page is no longer absent, or a write or resize
    /// moved the version, and the read begins again.
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
        // Its bytes are no longer the file's: no write-back takes them, and
        // it needs none of the room a holder's credit paid for.
        let (counted, credited) = (s.state.counts_dirty(), s.state.credited);
        s.state = State::CLEAN;
        s.revoking = Some(becomes);
        s.revoke_next = NONE;
        o.revoking += 1;
        if counted {
            self.dirty_held -= 1;
        }
        if credited {
            o.credit += 1;
        }
        match o.revoke_tail {
            NONE => o.revoke_head = i,
            tail => self.slot_mut(tail).revoke_next = i,
        }
        o.revoke_tail = i;
    }

    /// The next step of `o`'s walk, after pages were marked revoking
    /// ([`Object::revoke`]): visit a mapper not visited in this pass, a new
    /// pass begun where a removal queued more pages meanwhile; once every
    /// mapper has been visited, where a mapper ever linked, have every futex
    /// wait on the walk's set ended, again whenever a pass has begun since it
    /// was last asked, since a page marked during a sweep may be waited on
    /// through a leaf an unmap already cleared; and once both are done, end
    /// the walk here ([`Next::Finished`]): each revoking page leaves, its
    /// frame given out to `out` with its page and what it became.
    pub fn walk<M: Mappers>(
        &mut self,
        o: &mut Object,
        mappers: &mut M,
        waiters: &mut impl Waiters,
        out: &mut impl FnMut(F, u64, Revoked),
    ) -> Next<M::Node> {
        loop {
            if let Some(node) = mappers.head().filter(|&n| mappers.seen(n) != o.epoch) {
                mappers.visited(node, o.epoch);
                return Next::Visit(node);
            }
            match o.walk {
                Walk::Running { again: true } => {
                    o.epoch += 1;
                    o.walk = Walk::Running { again: false };
                }
                Walk::Running { again: false } if o.linked && o.swept != o.epoch => {
                    o.swept = o.epoch;
                    return Next::Sweep;
                }
                Walk::Running { again: false } => {
                    self.finish(o, waiters, out);
                    return Next::Finished;
                }
                Walk::Idle => panic!("paged: a walk of object {} that never began", o.id),
            }
        }
    }

    /// The walk's end, which only [`Self::walk`] reaches: each revoking page
    /// leaves, its frame given out with its page and what it became. A page
    /// that ends absent was withdrawn, perhaps after a supply for a waiter's
    /// notice, without moving the version: every want of `waiters` it meets
    /// is named again.
    fn finish(&mut self, o: &mut Object, waiters: &mut impl Waiters, out: &mut impl FnMut(F, u64, Revoked)) {
        let held = !o.gone;
        let mut at = core::mem::replace(&mut o.revoke_head, NONE);
        o.revoke_tail = NONE;
        while at != NONE {
            let s = self.slot(at);
            let (next, page, becomes) = (s.revoke_next, s.page, s.revoking.expect("paged: the walk's set holds revoking slots"));
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
            at = next;
        }
        debug_assert_eq!(o.revoking, 0);
        o.walk = Walk::Idle;
        o.finished = o.epoch;
    }

    /// `o`'s revoking pages from `at` on, in the order they were marked, each
    /// with the frame it keeps until the walk's end: as many as fit in `out`,
    /// whose count this answers, `at` moved past them. Only the walk's end
    /// takes a slot off the set, so a cursor stays good across the drops of
    /// the pager's lock between batches, and sees pages marked since.
    pub fn revoking(&self, o: &Object, at: &mut Cursor, out: &mut [(u64, F)]) -> usize {
        let mut next = match *at {
            Cursor::Start => o.revoke_head,
            Cursor::After(i) => self.slot(i).revoke_next,
        };
        let mut n = 0;
        while n < out.len() && next != NONE {
            let s = self.slot(next);
            out[n] = (s.page, s.frame.expect("paged: a revoking page holds its frame"));
            *at = Cursor::After(next);
            next = s.revoke_next;
            n += 1;
        }
        n
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
        if dirty && o.sealed() {
            return Err(Refused::Sealed);
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

    /// Whether `by` may turn a page whose state is `dirty` dirty: the
    /// refusals every admission checks, in order, before it changes anything.
    fn admissible(&self, o: &Object, dirty: Option<State>, by: Writer) -> Result<(), Refused> {
        if o.gone {
            return Err(Refused::Gone);
        }
        if o.sealed() {
            return Err(Refused::Sealed);
        }
        if o.anonymous {
            return Ok(());
        }
        // A page under write-back is clean again for the space it takes: on a
        // copy-on-write volume its next write needs room of its own.
        let becomes_dirty = dirty.is_none_or(|s| !s.dirty);
        if by == Writer::Holder && becomes_dirty && o.credit == 0 {
            return Err(Refused::NoCredit);
        }
        if dirty.is_none_or(|s| !s.counts_dirty()) && self.dirty_held >= self.dirty_ceiling {
            return Err(Refused::DirtyFull);
        }
        Ok(())
    }

    /// Admit a write into present `page` and pin it for a copy that runs
    /// outside the object's lock: the page is dirty from here, and a
    /// write-back that begins or ends before [`write_unpin`](Self::write_unpin)
    /// keeps it so. A holder's admission that turns the page dirty spends a
    /// credit. The size is not moved until the unpin, so no reader sees bytes
    /// past it that the copy has not written yet. An absent page answers
    /// `InvalidArgument`: it is supplied, or [`admit_fresh`](Self::admit_fresh) made.
    pub fn admit(&mut self, o: &mut Object, page: u64, by: Writer) -> Result<F, Refused> {
        if o.gone {
            return Err(Refused::Gone);
        }
        let i = self.find(o.id, page).ok_or(Refused::InvalidArgument)?;
        let s = self.slot(i);
        let frame = s.frame.filter(|_| s.revoking.is_none()).ok_or(Refused::InvalidArgument)?;
        let state = s.state;
        self.admissible(o, Some(state), by)?;
        let s = self.slot_mut(i);
        s.state.dirty = !o.anonymous;
        s.state.write_pins = s.state.write_pins.checked_add(1).expect("a page's write pins overflowed");
        if s.state.writeback {
            s.state.pinned_since_begin = true;
        }
        if !o.anonymous && !state.counts_dirty() {
            self.dirty_held += 1;
        }
        match by {
            Writer::Holder if !o.anonymous && !state.dirty => {
                o.credit -= 1;
                self.slot_mut(i).state.credited = true;
                o.holder_dirtied(page);
            }
            // The pager's write brings its own room, so what a holder's credit
            // paid for this page is credit again.
            Writer::Pager if state.credited => {
                self.slot_mut(i).state.credited = false;
                o.credit += 1;
            }
            _ => {}
        }
        Ok(frame)
    }

    /// Admit a write into `page`, which holds no slot and reads as zeros — it
    /// lies past every page the size reaches, or is a hole of an anonymous
    /// object — with `frame`, zeroed, installed dirty and pinned as
    /// [`admit`](Self::admit) pins. Nothing needs supplying first.
    pub fn admit_fresh(&mut self, o: &mut Object, page: u64, frame: F, by: Writer) -> Result<(), Refused> {
        if o.gone {
            return Err(Refused::Gone);
        }
        if (page < o.page_end() && !o.anonymous) || self.find(o.id, page).is_some() {
            return Err(Refused::InvalidArgument);
        }
        self.admissible(o, None, by)?;
        let credited = by == Writer::Holder && !o.anonymous;
        let state = State { dirty: !o.anonymous, write_pins: 1, credited, ..State::CLEAN };
        self.insert(o, page, Some(frame), state)?;
        if credited {
            o.credit -= 1;
            o.holder_dirtied(page);
        }
        Ok(())
    }

    /// Whether a holder may write `count` pages from `first` whole: every
    /// page is present, or reads as zeros without a slot, and the credit and
    /// the dirty ceiling cover each it would turn dirty. The fresh pages it
    /// would need. The same refusals as [`admit`](Self::admit), and `Absent`
    /// for a page the pager must supply first, or one that failed.
    pub fn holder_fits(&self, o: &Object, first: u64, count: u64) -> Result<u64, Refused> {
        self.admissible(o, Some(State { dirty: true, ..State::CLEAN }), Writer::Holder)?;
        let (mut fresh, mut dirtying, mut held) = (0u64, 0u64, 0u64);
        for page in first..first.saturating_add(count) {
            match self.find(o.id, page) {
                Some(i) => {
                    let s = self.slot(i);
                    if s.frame.is_none() || s.revoking.is_some() {
                        return Err(Refused::Absent);
                    }
                    dirtying += u64::from(!s.state.dirty);
                    held += u64::from(!s.state.counts_dirty());
                }
                None if page >= o.page_end() || o.anonymous => {
                    fresh += 1;
                    dirtying += 1;
                    held += 1;
                }
                None => return Err(Refused::Absent),
            }
        }
        if o.anonymous {
            return if self.free_len as u64 >= fresh { Ok(fresh) } else { Err(Refused::NoRoom) };
        }
        if dirtying > o.credit {
            return Err(Refused::NoCredit);
        }
        if self.dirty_held as u64 + held > self.dirty_ceiling as u64 {
            return Err(Refused::DirtyFull);
        }
        if (self.free_len as u64) < fresh {
            return Err(Refused::NoRoom);
        }
        Ok(fresh)
    }

    /// The end of a [`admit`](Self::admit) of `frame`, once its
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

    /// The end of a [`admit`](Self::admit) whose bytes did not all
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
                s.state.credit_in_flight = core::mem::take(&mut s.state.credited);
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
            // Unwritten, the page still owes the room its credit paid for.
            let in_flight = core::mem::take(&mut s.state.credit_in_flight);
            s.state.credited |= in_flight && !written;
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
    pub fn resize(&mut self, o: &mut Object, size: u64) -> Result<Option<F>, Refused> {
        if o.sealed() {
            return Err(Refused::Sealed);
        }
        o.version += 1;
        // A holder dirtied nothing past the size that the pager owes a write-back.
        if let Some((lo, hi)) = o.dirtied {
            let hi = hi.min(size.div_ceil(PAGE));
            o.dirtied = (lo < hi).then_some((lo, hi));
        }
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
            return Ok(None);
        }
        Ok(match self.lookup(o, size / PAGE) {
            Lookup::Present(frame) => Some(frame),
            _ => None,
        })
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

    /// No space maps anything.
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

    /// The frames of `o`'s revoking pages, once a walk over no mappers ends:
    /// none ever linked, so it sweeps nothing.
    fn settle(p: &mut Pages<u64>, o: &mut Object) -> Vec<u64> {
        let mut freed = Vec::new();
        match o.revoke() {
            Revoke::Done => {}
            Revoke::Walk => {
                assert_eq!(p.walk(o, &mut Unmapped, &mut Nobody, &mut |f, _, _| freed.push(f)), Next::Finished);
            }
            Revoke::Queued(_) => panic!("a walk ran with no walker"),
        }
        freed
    }

    /// One object's mappers: each node's space and the epoch it was last
    /// seen at, in visiting order.
    #[derive(Default)]
    struct Ring(std::collections::VecDeque<(u32, u64)>);

    impl Ring {
        fn link(&mut self, space: u32, o: &mut Object) {
            self.0.push_back((space, o.link()));
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

    /// A page marked while the sweep has its lock dropped is swept before the
    /// walk gives its frame back, though the pass its marking begins finds no
    /// mapper: an unmap may have cleared the leaf a waiter keyed on it went
    /// through, and only the sweep ends that wait.
    #[test]
    fn a_page_marked_during_the_sweep_is_swept_before_its_frame_goes_back() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 2 * PAGE);
        // A mapper linked, and its unmap since cleared its leaves.
        o.link();
        p.supply(&mut o, 0, 100, false, PAGE).unwrap();
        p.supply(&mut o, 1, 101, false, 2 * PAGE).unwrap();
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 1);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut swept = Vec::new();
        let mut sweep = |p: &Pages<u64>, o: &Object| {
            let mut batch = [(0, 0); 4];
            let n = p.revoking(o, &mut Cursor::Start, &mut batch);
            swept.extend(batch[..n].iter().map(|&(_, frame)| frame));
        };
        let mut freed = Vec::new();
        assert_eq!(p.walk(&mut o, &mut Unmapped, &mut Nobody, &mut |frame, _, _| freed.push(frame)), Next::Sweep);
        sweep(&p, &o);
        // Between the sweep's last read and its relock, another removal.
        assert_eq!(p.withdraw(&mut o, 1, 1, &mut Nobody).0, 1);
        assert!(matches!(o.revoke(), Revoke::Queued(_)));
        assert_eq!(p.walk(&mut o, &mut Unmapped, &mut Nobody, &mut |frame, _, _| freed.push(frame)), Next::Sweep, "a pass began since the sweep");
        sweep(&p, &o);
        assert_eq!(p.walk(&mut o, &mut Unmapped, &mut Nobody, &mut |frame, _, _| freed.push(frame)), Next::Finished);
        assert_eq!(freed, [100, 101]);
        assert!(freed.iter().all(|frame| swept.contains(frame)), "a frame went back unswept: swept {swept:?}");
    }

    /// The walk's set holds each revoking page once, in the order they were
    /// marked; a cursor reads it a batch at a time and sees pages marked
    /// between batches; the walk's end empties it.
    #[test]
    fn the_walks_set_is_read_a_batch_at_a_time_in_marking_order() {
        let mut p = Pages::new(16, 4).unwrap();
        let mut o = p.object(1, 16 * PAGE);
        for page in [9, 2, 14, 5, 0, 11, 7] {
            p.supply(&mut o, page, 100 + page, false, (page + 1) * PAGE).unwrap();
        }
        for page in [7, 2, 11] {
            assert_eq!(p.withdraw(&mut o, page, 1, &mut Nobody).0, 1);
        }
        let mut at = Cursor::Start;
        let mut batch = [(0, 0); 2];
        assert_eq!(p.revoking(&o, &mut at, &mut batch), 2);
        assert_eq!(batch, [(7, 107), (2, 102)]);
        assert_eq!(p.withdraw(&mut o, 2, 1, &mut Nobody).0, 0, "a revoking page is marked once");
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 1);
        assert_eq!(p.revoking(&o, &mut at, &mut batch), 2, "a page marked between batches is read");
        assert_eq!(batch, [(11, 111), (0, 100)]);
        assert_eq!(p.revoking(&o, &mut at, &mut batch), 0);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut freed = Vec::new();
        assert_eq!(p.walk(&mut o, &mut Unmapped, &mut Nobody, &mut |f, _, _| freed.push(f)), Next::Finished);
        assert_eq!(freed, [107, 102, 111, 100]);
        assert_eq!(p.revoking(&o, &mut Cursor::Start, &mut batch), 0, "the walk's end empties its set");
    }

    /// A removed page reads as absent, takes no second frame and no write,
    /// and keeps its frame until every mapper of its object was visited;
    /// then its frame goes out once and the page is supplied afresh.
    #[test]
    fn a_revoking_page_keeps_its_one_frame_until_every_mapper_is_visited() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 2 * PAGE);
        let mut ring = Ring::default();
        ring.link(10, &mut o);
        ring.link(11, &mut o);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 1);
        assert_eq!(p.lookup(&o, 0), Lookup::Absent);
        assert_eq!(p.supply(&mut o, 0, 8, false, PAGE), Ok(Supplied::Present), "a second frame for the page");
        assert_eq!(p.admit(&mut o, 0, Writer::Pager), Err(Refused::InvalidArgument));
        assert_eq!(p.withdraw(&mut o, 0, 1, &mut Nobody).0, 0, "withdrawn once");
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut freed = Vec::new();
        let mut out = |f, page, becomes| freed.push((f, page, becomes));
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Visit(10));
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Visit(11));
        assert!(!o.empty());
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Sweep);
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Finished);
        assert_eq!(freed, [(7, 0, Revoked::Absent)]);
        assert!(o.empty());
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
        ring.link(10, &mut o);
        ring.link(11, &mut o);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.supply(&mut o, 1, 8, false, 2 * PAGE).unwrap();
        p.withdraw(&mut o, 0, 1, &mut Nobody);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut freed = Vec::new();
        let mut out = |f, page, becomes| freed.push((f, page, becomes));
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Visit(10));
        ring.link(12, &mut o);
        p.fail(&mut o, 1, 1).unwrap();
        let Revoke::Queued(second) = o.revoke() else { panic!("a walk runs") };
        assert!(!o.finished(second));
        let visits: Vec<_> = core::iter::from_fn(|| match p.walk(&mut o, &mut ring, &mut Nobody, &mut out) {
            Next::Visit(node) => Some(node),
            Next::Sweep | Next::Finished => None,
        })
        .collect();
        assert_eq!(visits, [11, 10, 12, 11], "the first pass ends at 11, the second visits all three");
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Finished, "the sweep, then the end");
        assert!(o.finished(second));
        assert_eq!(freed, [(7, 0, Revoked::Absent), (8, 1, Revoked::Failed)], "in marking order");
        assert_eq!((p.lookup(&o, 0), p.lookup(&o, 1)), (Lookup::Absent, Lookup::Failed));
    }

    /// A mapper linked during a walk that nothing else joins maps nothing
    /// revoking, so the walk passes it by.
    #[test]
    fn a_mapper_linked_during_a_walk_is_passed_by() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, PAGE);
        let mut ring = Ring::default();
        ring.link(10, &mut o);
        p.supply(&mut o, 0, 7, false, PAGE).unwrap();
        p.withdraw(&mut o, 0, 1, &mut Nobody);
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut out = |_, _, _| {};
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Visit(10));
        ring.link(11, &mut o);
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Sweep);
        assert_eq!(p.walk(&mut o, &mut ring, &mut Nobody, &mut out), Next::Finished);
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
        p.resize(&mut o, PAGE).unwrap();
        assert_eq!(o.revoke(), Revoke::Walk);
        let mut ended = Vec::new();
        assert_eq!(p.walk(&mut o, &mut Unmapped, &mut Nobody, &mut |f, page, becomes| ended.push((f, page, becomes))), Next::Finished);
        assert_eq!(ended, [(7, 0, Revoked::Failed), (8, 1, Revoked::Removed)]);
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
        assert!(o.take_pager());
        assert_eq!(o.read_begin(), None, "a read meets the write in flight");
        assert!(!o.unchanged(before), "a read that began before it meets it");
        o.give(Side::Pager);
        assert!(!o.unchanged(before), "and still does once it ended");
        let after = o.read_begin().expect("the write ended");
        assert!(o.unchanged(after));
        p.resize(&mut o, 0).unwrap();
        assert!(!o.unchanged(after), "a shrink is a change a read meets");
    }

    /// One write at a time: a holder's takes the side or is refused at once,
    /// never waiting; the pager waits for a holder's and is owed the side
    /// before any later holder's.
    #[test]
    fn the_write_side_is_one_writers_and_a_holder_never_waits_for_it() {
        let mut p = Pages::<u64>::new(8, 4).unwrap();
        let mut o = p.object(1, PAGE);
        assert!(o.take_holder());
        assert!(!o.take_holder(), "a second holder's write is refused");
        assert!(!o.take_pager(), "the pager waits for the holder's");
        assert_eq!(o.read_begin(), None);
        o.give(Side::Holder);
        assert!(!o.take_holder(), "the waiting pager is owed the side first");
        assert!(o.take_pager());
        assert!(!o.take_holder() && o.read_begin().is_none(), "the pager's hold excludes every other write and read");
        o.give(Side::Pager);
        assert!(o.take_holder(), "free again");
    }

    /// While the pager holds the writes, a holder states the size and time of
    /// the moment it took them, however its writes move them; its unhold
    /// shows what they left.
    #[test]
    fn a_holder_states_the_moment_the_pager_took_the_writes() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 100);
        o.stamp(5);
        assert_eq!(o.stat(), (100, 5));
        assert!(o.take_pager());
        p.supply(&mut o, 1, 1, true, 2 * PAGE).unwrap();
        o.stamp(9);
        assert_eq!((o.size(), o.mtime()), (2 * PAGE, 9), "the pager sees its own");
        assert_eq!(o.stat(), (100, 5), "a holder sees the moment the hold began");
        p.resize(&mut o, 0).unwrap();
        assert_eq!(o.stat(), (100, 5), "and a truncation under it");
        p.resize(&mut o, 3 * PAGE).unwrap();
        o.give(Side::Pager);
        assert_eq!(o.stat(), (3 * PAGE, 9), "the unhold shows what the writes left");
        assert!(o.take_holder());
        o.stamp(11);
        assert_eq!(o.stat(), (3 * PAGE, 11), "a holder's write holds no statement back");
    }

    /// A holder's write is taken whole, or refused for the pager to write:
    /// spanning more pages than one call takes, wherever it starts, or ending
    /// past a holder's reach, up to the last offsets a `u64` holds.
    #[test]
    fn a_holders_span_is_whole_or_refused_up_to_its_reach() {
        assert_eq!(holder_span(0, HOLDER_PAGES * PAGE), Ok(HOLDER_PAGES));
        assert_eq!(holder_span(1, HOLDER_PAGES * PAGE), Err(Refused::OutOfReach), "one byte into a page more");
        assert_eq!(holder_span(PAGE - 1, 2), Ok(2));
        assert_eq!(holder_span(100, 1 << 30), Err(Refused::OutOfReach));
        for k in 1..=2 * HOLDER_PAGES {
            let at = HOLDER_END - k * PAGE;
            assert_eq!(holder_span(at, 1), Ok(1), "the page {k} below the reach");
            assert_eq!(holder_span(at + 7, k * PAGE - 7).is_ok(), k <= HOLDER_PAGES);
        }
        assert_eq!(holder_span(HOLDER_END - 1, 1), Ok(1), "a size at the reach leaves an append its last byte");
        assert_eq!(holder_span(HOLDER_END, 1), Err(Refused::OutOfReach), "an append at a size at the reach");
        assert_eq!(holder_span(HOLDER_END - PAGE + 1, PAGE), Err(Refused::OutOfReach));
        for at in [LAST_END - PAGE, LAST_END - 1, u64::MAX] {
            assert_eq!(holder_span(at, 1), Err(Refused::OutOfReach), "{at:#x}");
        }
        assert_eq!(holder_span(u64::MAX - 3, 8), Err(Refused::OutOfReach), "an end that wraps");
    }

    /// The room a holder's credit paid for a page is credit again where the
    /// pager writes the page itself or the page goes before its write-back,
    /// and spent where its write-back is written; a refused write-back keeps
    /// it owed.
    #[test]
    fn a_holders_paid_credit_comes_back_unless_its_write_back_spends_it() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 3 * PAGE);
        for page in 0..3 {
            p.supply(&mut o, page, page, false, (page + 1) * PAGE).unwrap();
        }
        o.grant(3);
        for page in 0..3 {
            p.admit(&mut o, page, Writer::Holder).unwrap();
            p.write_unpin(&mut o, page, page, (page + 1) * PAGE);
        }
        assert_eq!(o.credit(), 0);
        p.admit(&mut o, 0, Writer::Pager).unwrap();
        p.write_unpin(&mut o, 0, 0, PAGE);
        assert_eq!(o.credit(), 1, "the pager's write of page 0 brought its own room");
        p.resize(&mut o, 2 * PAGE).unwrap();
        assert_eq!(o.credit(), 2, "page 2 went unwritten");
        p.writeback_begin(&mut o, 1, 1).unwrap();
        p.writeback_end(&mut o, 1, 1, false).unwrap();
        p.admit(&mut o, 1, Writer::Pager).unwrap();
        p.write_unpin(&mut o, 1, 1, 2 * PAGE);
        assert_eq!(o.credit(), 3, "a refused write-back left it owed, and the pager's write gave it back");
        p.writeback_begin(&mut o, 0, 1).unwrap();
        p.writeback_end(&mut o, 0, 1, true).unwrap();
        p.admit(&mut o, 0, Writer::Holder).unwrap();
        p.write_unpin(&mut o, 0, 0, PAGE);
        p.writeback_begin(&mut o, 0, 1).unwrap();
        p.writeback_end(&mut o, 0, 1, true).unwrap();
        p.admit(&mut o, 0, Writer::Pager).unwrap();
        p.write_unpin(&mut o, 0, 0, PAGE);
        assert_eq!(o.credit(), 2, "a written write-back spent it");
    }

    #[test]
    fn anonymous_bytes_survive_cache_operations_and_release_with_the_object() {
        let mut pages = Pages::new(2, 0).unwrap();
        let mut object = pages.anonymous(1, 0);
        assert_eq!(pages.supply(&mut object, 0, 42, true, PAGE), Ok(Supplied::Installed));
        assert_eq!(pages.dirty_held(), 0);
        assert_eq!(pages.admit(&mut object, 0, Writer::Pager), Ok(42));
        pages.write_unpin(&mut object, 0, 42, PAGE);
        assert_eq!(pages.withdraw(&mut object, 0, 1, &mut Nobody).0, 0);
        assert_eq!(pages.writeback_begin(&mut object, 0, 1), Err(Refused::InvalidArgument));
        assert_eq!(pages.fail(&mut object, 0, 1), Err(Refused::InvalidArgument));
        assert_eq!(pages.lookup(&object, 0), Lookup::Present(42));
        assert_eq!(pages.supply(&mut object, 1, 43, true, 2 * PAGE), Ok(Supplied::Installed));
        assert_eq!(pages.supply(&mut object, 2, 44, true, 3 * PAGE), Err(Refused::NoRoom));
        pages.resize(&mut object, 4 * PAGE).unwrap();
        assert_eq!(pages.lookup(&object, 3), Lookup::Zero, "a hole reads without any free slot");
        pages.resize(&mut object, PAGE).unwrap();
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
        assert_eq!(p.admit(&mut o, 0, Writer::Pager), Ok(7));
        assert_eq!(o.size(), 100, "the copy is in flight: a reader still stops at the old end");
        p.write_unpin(&mut o, 0, 7, 300);
        assert_eq!(o.size(), 300, "the bytes are in, and the size covers them");
        assert_eq!(p.admit(&mut o, 1, Writer::Pager), Err(Refused::InvalidArgument), "an absent page is supplied, not pinned");
    }

    /// A write pin whose copy faulted is released with the size as it was.
    #[test]
    fn an_abandoned_write_pin_grows_nothing_and_holds_nothing() {
        let mut p = Pages::new(8, 4).unwrap();
        let mut o = p.object(6, 100);
        p.supply(&mut o, 0, 7, false, 100).unwrap();
        assert_eq!(p.admit(&mut o, 0, Writer::Pager), Ok(7));
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
        p.admit(&mut o, 0, Writer::Pager).unwrap();
        p.resize(&mut o, 0).unwrap();
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
        p.admit(&mut o, 0, Writer::Pager).unwrap(); // pinned across the begin, released inside
        assert_eq!(p.writeback_begin(&mut o, 0, 6).unwrap()[0], 0b11_1111);
        p.write_unpin(&mut o, 0, 0, PAGE);
        p.admit(&mut o, 1, Writer::Pager).unwrap(); // dirtied inside
        p.write_unpin(&mut o, 1, 1, 2 * PAGE);
        p.resize(&mut o, 5 * PAGE).unwrap(); // page 5 removed inside
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
        p.admit(&mut o, 3, Writer::Pager).unwrap(); // write-pinned
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
        assert_eq!(p.resize(&mut o, PAGE + 5), Ok(Some(101)));
        assert_eq!(p.dirty_held(), 2);
        assert_eq!(p.lookup(&o, 2), Lookup::PastEnd);
        assert_eq!(settle(&mut p, &mut o), [102]);
        assert_eq!(p.resize(&mut o, 4 * PAGE), Ok(None), "growth removes nothing");
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

    /// A holder's write spends a credit for each page it turns dirty and
    /// none for a page already dirty; at 0 it is refused with nothing
    /// changed. A page under write-back costs a credit again, as a
    /// copy-on-write volume needs new room for it. The pager's writes spend
    /// none. Granting 0 takes the credit back.
    #[test]
    fn a_holder_spends_a_credit_for_each_page_it_turns_dirty() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 3 * PAGE);
        for page in 0..3 {
            p.supply(&mut o, page, page, false, (page + 1) * PAGE).unwrap();
        }
        assert_eq!(p.admit(&mut o, 0, Writer::Holder), Err(Refused::NoCredit), "no credit granted");
        assert_eq!(p.dirty_held(), 0, "a refusal changes nothing");
        assert_eq!(o.grant(2), 2);
        assert_eq!(p.admit(&mut o, 0, Writer::Holder), Ok(0));
        p.write_unpin(&mut o, 0, 0, PAGE);
        assert_eq!(o.credit(), 1);
        assert_eq!(p.admit(&mut o, 0, Writer::Holder), Ok(0), "already dirty");
        p.write_unpin(&mut o, 0, 0, PAGE);
        assert_eq!(o.credit(), 1, "a dirty page costs nothing again");
        p.writeback_begin(&mut o, 0, 1).unwrap();
        assert_eq!(p.admit(&mut o, 0, Writer::Holder), Ok(0), "under write-back");
        p.write_unpin(&mut o, 0, 0, PAGE);
        assert_eq!(o.credit(), 0, "a page under write-back needs room again");
        assert_eq!(p.admit(&mut o, 1, Writer::Holder), Err(Refused::NoCredit));
        assert_eq!(p.admit(&mut o, 1, Writer::Pager), Ok(1), "the pager reserved its own");
        p.write_unpin(&mut o, 1, 1, 2 * PAGE);
        assert_eq!(o.credit(), 0);
        o.grant(5);
        assert_eq!(o.take_back(), 5, "what was not spent is taken back");
        assert_eq!(o.credit(), 0);
    }

    /// A write past every page the size reaches needs no supply: a fresh
    /// page is installed dirty and pinned, against the same admission.
    #[test]
    fn a_holder_appends_into_a_fresh_page_against_the_same_admission() {
        let mut p = Pages::new(8, 1).unwrap();
        let mut o = p.object(2, 100);
        assert_eq!(p.admit_fresh(&mut o, 0, 9, Writer::Holder), Err(Refused::InvalidArgument), "page 0 holds bytes");
        assert_eq!(p.admit_fresh(&mut o, 1, 9, Writer::Holder), Err(Refused::NoCredit));
        o.grant(1);
        assert_eq!(p.admit_fresh(&mut o, 1, 9, Writer::Holder), Ok(()));
        assert_eq!(o.size(), 100, "the size waits for the unpin");
        assert_eq!(p.admit_fresh(&mut o, 1, 10, Writer::Holder), Err(Refused::InvalidArgument), "present now");
        p.write_unpin(&mut o, 1, 9, PAGE + 10);
        assert_eq!(o.size(), PAGE + 10);
        assert_eq!(p.lookup(&o, 1), Lookup::Present(9));
        o.grant(1);
        assert_eq!(p.admit_fresh(&mut o, 2, 11, Writer::Holder), Err(Refused::DirtyFull), "the ceiling holds");
        assert_eq!(o.credit(), 1, "and spends nothing");
    }

    /// A sealed object refuses every write, the pager's included, and every
    /// size change; reads go on.
    #[test]
    fn a_sealed_object_refuses_every_write_and_resize() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(3, PAGE);
        p.supply(&mut o, 0, 1, false, PAGE).unwrap();
        o.grant(4);
        o.seals = 1;
        assert_eq!(p.admit(&mut o, 0, Writer::Holder), Err(Refused::Sealed));
        assert_eq!(p.admit(&mut o, 0, Writer::Pager), Err(Refused::Sealed));
        assert_eq!(p.admit_fresh(&mut o, 1, 2, Writer::Holder), Err(Refused::Sealed));
        assert_eq!(p.supply(&mut o, 1, 2, true, 2 * PAGE), Err(Refused::Sealed));
        assert_eq!(p.resize(&mut o, 0), Err(Refused::Sealed));
        assert_eq!(p.resize(&mut o, 4 * PAGE), Err(Refused::Sealed));
        assert_eq!((o.size(), o.credit(), p.dirty_held()), (PAGE, 4, 0), "nothing changed");
        assert_eq!(p.lookup(&o, 0), Lookup::Present(1));
        o.seals = 0;
        assert_eq!(p.admit(&mut o, 0, Writer::Holder), Ok(1));
    }

    /// A holder's write is refused whole, with nothing admitted, where any
    /// page of it would be: an unsupplied or failed page, credit or the dirty
    /// ceiling short for the pages it turns dirty, or no slot for its fresh
    /// ones. Pages already dirty cost nothing; pages past the size are fresh.
    #[test]
    fn a_holders_write_is_checked_whole_before_any_page_is_admitted() {
        let mut p = Pages::new(8, 3).unwrap();
        let mut o = p.object(1, 3 * PAGE);
        p.supply(&mut o, 0, 10, false, PAGE).unwrap();
        p.supply(&mut o, 1, 11, true, 2 * PAGE).unwrap();
        assert_eq!(p.holder_fits(&o, 0, 3), Err(Refused::Absent), "page 2 lies inside the size, unsupplied");
        assert_eq!(p.holder_fits(&o, 0, 2), Err(Refused::NoCredit), "page 0 turns dirty with no credit");
        o.grant(1);
        assert_eq!(p.holder_fits(&o, 0, 2), Ok(0), "page 1 is dirty already");
        assert_eq!(p.holder_fits(&o, 1, 3), Err(Refused::Absent));
        p.supply(&mut o, 2, 12, false, 3 * PAGE).unwrap();
        assert_eq!(p.holder_fits(&o, 2, 3), Err(Refused::NoCredit), "pages 3 and 4 are fresh and need credit too");
        o.grant(2);
        assert_eq!(p.holder_fits(&o, 2, 3), Err(Refused::DirtyFull), "1 dirty and 3 more pass a ceiling of 3");
        assert_eq!(p.holder_fits(&o, 2, 2), Ok(1), "page 3 is fresh");
        assert_eq!((o.credit(), p.dirty_held()), (3, 1), "a check changes nothing");
        p.fail(&mut o, 0, 1).unwrap();
        assert_eq!(p.holder_fits(&o, 0, 1), Err(Refused::Absent), "a failed page is the pager's to supply again");
        o.seals = 1;
        assert_eq!(p.holder_fits(&o, 1, 1), Err(Refused::Sealed));
        let mut small = Pages::new(1, 1).unwrap();
        let mut m = small.anonymous(2, 0);
        assert_eq!(small.holder_fits(&m, 0, 1), Ok(1), "anonymous memory needs no credit");
        assert_eq!(small.holder_fits(&m, 0, 2), Err(Refused::NoRoom), "but a slot for each fresh page");
        small.resize(&mut m, 2 * PAGE).unwrap();
        assert_eq!(small.admit_fresh(&mut m, 1, 7, Writer::Holder), Ok(()), "a hole inside the size reads as zeros");
        assert_eq!(small.holder_fits(&m, 0, 2), Err(Refused::NoRoom), "page 1 holds the only slot");
    }

    /// A revoking page takes no write: a holder's check finds it absent, and
    /// neither an admission nor a fresh page lands on it. The room a holder's
    /// credit paid for it is credit again as it is marked, as for a page gone
    /// before its write-back, and its walk's end gives back nothing twice.
    #[test]
    fn a_revoking_page_takes_no_write_and_gives_its_credit_back_once() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, 2 * PAGE);
        p.supply(&mut o, 0, 10, false, PAGE).unwrap();
        p.supply(&mut o, 1, 11, false, 2 * PAGE).unwrap();
        o.grant(1);
        p.admit(&mut o, 1, Writer::Holder).unwrap();
        p.write_unpin(&mut o, 1, 11, 2 * PAGE);
        assert_eq!(o.credit(), 0);
        p.resize(&mut o, PAGE).unwrap();
        assert_eq!(o.credit(), 1, "marked revoking, the page needs none of its room");
        assert_eq!(p.holder_fits(&o, 1, 1), Err(Refused::Absent));
        assert_eq!(p.admit(&mut o, 1, Writer::Pager), Err(Refused::InvalidArgument));
        assert_eq!(p.admit_fresh(&mut o, 1, 12, Writer::Holder), Err(Refused::InvalidArgument));
        assert_eq!(settle(&mut p, &mut o), [11]);
        assert_eq!(o.credit(), 1, "the walk's end gives nothing twice");
        assert_eq!(p.holder_fits(&o, 1, 1), Ok(1), "past the size, a fresh page once the walk ended");
    }

    /// The first page a holder dirties owes the pager one notice; later ones
    /// widen the range without another, until the pager takes it. A shrink
    /// keeps only what lies inside the size.
    #[test]
    fn a_holders_dirtying_is_told_once_until_the_pager_takes_it() {
        let mut p = Pages::new(8, 8).unwrap();
        let mut o = p.object(1, PAGE);
        p.supply(&mut o, 0, 10, false, PAGE).unwrap();
        o.grant(8);
        assert!(!o.take_tell());
        p.admit(&mut o, 0, Writer::Pager).unwrap();
        p.write_unpin(&mut o, 0, 10, PAGE);
        assert!(!o.take_tell(), "the pager's own writes are its to know");
        p.writeback_begin(&mut o, 0, 1).unwrap();
        p.writeback_end(&mut o, 0, 1, true).unwrap();
        p.admit(&mut o, 0, Writer::Holder).unwrap();
        p.write_unpin(&mut o, 0, 10, PAGE);
        assert!(o.take_tell(), "the first page a holder dirties");
        p.admit_fresh(&mut o, 3, 13, Writer::Holder).unwrap();
        p.write_unpin(&mut o, 3, 13, 4 * PAGE);
        assert!(!o.take_tell(), "told once");
        assert_eq!(o.take_dirtied(), Some((0, 4)));
        assert_eq!(o.take_dirtied(), None);
        p.admit_fresh(&mut o, 5, 15, Writer::Holder).unwrap();
        p.write_unpin(&mut o, 5, 15, 5 * PAGE + 1);
        assert!(o.take_tell(), "owed again once taken");
        p.resize(&mut o, 2 * PAGE).unwrap();
        assert_eq!(o.take_dirtied(), None, "a shrink takes what lay past it");
    }

    /// **A model of the kernel's protocol, not its code**: a holder's read of
    /// two pages, two holders' writes (one of them twice) and a write the
    /// pager makes, each step an atomic action on the object under its
    /// pager's lock, driven through every interleaving. They follow
    /// `kernel/src/object/paged.rs` and the file server: a read that meets a
    /// write, begun or under way, waits for its end and reads again, and at
    /// the [`WRITES_A_READ_MEETS`]th asks the pager, which reads under its
    /// hold; a holder's write that cannot take the side asks the pager, which
    /// writes it after what it was asked before. Every read is one write's
    /// generation, no two writes are under way at once, nothing deadlocks,
    /// and no holder's write takes the side while the pager waits for it.
    #[test]
    fn every_interleaving_reads_whole_writes_one_at_a_time_and_the_pager_is_not_overtaken() {
        use std::collections::HashSet;
        use std::vec;

        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        enum Ask {
            Write(u64),
            Read,
        }
        #[derive(Clone, PartialEq, Eq, Hash)]
        enum Reader {
            Begin { met: u32 },
            Waiting { met: u32 },
            Copy { v: Version, at: usize, got: [u64; 2], met: u32 },
            Asked,
            Done,
        }
        #[derive(Clone, PartialEq, Eq, Hash)]
        enum Writer {
            Take(Vec<u64>),
            Write(Vec<u64>, usize),
            Gone,
        }
        #[derive(Clone, PartialEq, Eq, Hash)]
        enum Pager {
            Idle,
            Taking(Ask),
            Write(u64, usize),
            Read(usize, [u64; 2]),
        }
        #[derive(Clone, PartialEq, Eq, Hash)]
        struct World {
            o: Object,
            page: [u64; 2],
            reader: Reader,
            writers: [Writer; 2],
            pager: Pager,
            asked: Vec<Ask>,
        }

        /// The read met one more write, once: it waits for its end, or asks
        /// the pager.
        fn meets(asked: &mut Vec<Ask>, met: u32) -> Reader {
            if met + 1 >= WRITES_A_READ_MEETS {
                asked.push(Ask::Read);
                Reader::Asked
            } else {
                Reader::Waiting { met: met + 1 }
            }
        }

        fn steps(w: &World) -> Vec<World> {
            let mut next = Vec::new();
            let mut r = w.clone();
            match w.reader.clone() {
                Reader::Begin { met } => {
                    r.reader = match r.o.read_begin() {
                        Some(v) => Reader::Copy { v, at: 0, got: [0; 2], met },
                        None => meets(&mut r.asked, met),
                    };
                    next.push(r);
                }
                Reader::Waiting { met } if w.o.read_begin().is_some() => {
                    r.reader = Reader::Begin { met };
                    next.push(r);
                }
                Reader::Copy { v, at, mut got, met } if at < 2 => {
                    got[at] = w.page[at];
                    r.reader = Reader::Copy { v, at: at + 1, got, met };
                    next.push(r);
                }
                Reader::Copy { v, got, met, .. } => {
                    if r.o.unchanged(v) {
                        assert_eq!(got[0], got[1], "a read met half a write");
                        r.reader = Reader::Done;
                    } else {
                        r.reader = meets(&mut r.asked, met);
                    }
                    next.push(r);
                }
                _ => {}
            }
            for k in 0..2 {
                let mut n = w.clone();
                match w.writers[k].clone() {
                    Writer::Take(gens) if !gens.is_empty() => {
                        let pager_waits = n.o.pager_waiting;
                        if n.o.take_holder() {
                            assert!(!pager_waits, "a holder's write took the side the pager waits for");
                            n.writers[k] = Writer::Write(gens, 0);
                        } else {
                            // Refused, the holder asks the pager to write it.
                            n.asked.push(Ask::Write(gens[0]));
                            n.writers[k] = if gens.len() > 1 { Writer::Take(gens[1..].to_vec()) } else { Writer::Gone };
                        }
                        next.push(n);
                    }
                    Writer::Write(gens, at) if at < 2 => {
                        assert_eq!(n.o.side(), Side::Holder, "a holder wrote without the side");
                        n.page[at] = gens[0];
                        n.writers[k] = Writer::Write(gens, at + 1);
                        next.push(n);
                    }
                    Writer::Write(gens, _) => {
                        n.o.give(Side::Holder);
                        n.writers[k] = if gens.len() > 1 { Writer::Take(gens[1..].to_vec()) } else { Writer::Gone };
                        next.push(n);
                    }
                    _ => {}
                }
            }
            let mut p = w.clone();
            match w.pager {
                Pager::Idle if !w.asked.is_empty() => {
                    let ask = p.asked.remove(0);
                    p.pager = Pager::Taking(ask);
                    next.push(p);
                }
                Pager::Taking(ask) => {
                    let before = p.o.clone();
                    if p.o.take_pager() {
                        p.pager = match ask {
                            Ask::Write(g) => Pager::Write(g, 0),
                            Ask::Read => Pager::Read(0, [0; 2]),
                        };
                        next.push(p);
                    } else if p.o != before {
                        // The first refused take marks the pager waiting.
                        next.push(p);
                    }
                }
                Pager::Write(g, at) if at < 2 => {
                    assert_eq!(p.o.side(), Side::Pager, "the pager wrote without the side");
                    p.page[at] = g;
                    p.pager = Pager::Write(g, at + 1);
                    next.push(p);
                }
                Pager::Read(at, mut got) if at < 2 => {
                    got[at] = p.page[at];
                    p.pager = Pager::Read(at + 1, got);
                    next.push(p);
                }
                Pager::Read(_, got) => {
                    assert_eq!(got[0], got[1], "the pager's read for the reader met half a write");
                    p.o.give(Side::Pager);
                    p.reader = Reader::Done;
                    p.pager = Pager::Idle;
                    next.push(p);
                }
                Pager::Write(..) => {
                    p.o.give(Side::Pager);
                    p.pager = Pager::Idle;
                    next.push(p);
                }
                _ => {}
            }
            next
        }

        let mut p = Pages::<u64>::new(4, 4).unwrap();
        let start = World {
            o: p.object(1, 2 * PAGE),
            page: [0; 2],
            reader: Reader::Begin { met: 0 },
            writers: [Writer::Take(vec![1, 2]), Writer::Take(vec![3])],
            pager: Pager::Idle,
            asked: vec![Ask::Write(4)],
        };
        let (mut seen, mut stack, mut ends, mut asked_reads) = (HashSet::new(), vec![start], 0, 0);
        while let Some(w) = stack.pop() {
            if !seen.insert(w.clone()) {
                continue;
            }
            asked_reads += usize::from(w.reader == Reader::Asked);
            let next = steps(&w);
            if next.is_empty() {
                let finished = w.reader == Reader::Done
                    && w.writers.iter().all(|x| *x == Writer::Gone)
                    && w.pager == Pager::Idle
                    && w.asked.is_empty();
                assert!(finished, "an interleaving deadlocked");
                ends += 1;
            }
            stack.extend(next);
        }
        assert!(ends > 0 && seen.len() > 1000, "{} states, {ends} ends: the model explored too little", seen.len());
        assert!(asked_reads > 0, "no interleaving sent the read to the pager");
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
        p.resize(&mut o, 0).unwrap();
        settle(&mut p, &mut o);
        p.resize(&mut o, PAGE).unwrap();
        assert_eq!(p.lookup(&o, 0), Lookup::Absent);
        assert!(p.wait_over(&o, 0, since), "shrunk and grown back before the reader looked");

        let mut failed = p.object(2, PAGE);
        let since = failed.read_begin().unwrap();
        p.fail(&mut failed, 0, 1).unwrap();
        assert!(failed.take_pager());
        assert_eq!(p.supply(&mut failed, 0, 8, true, PAGE), Err(Refused::DirtyFull));
        failed.give(Side::Pager);
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
        assert_eq!(p.walk(&mut taken, &mut Unmapped, &mut One(&mut want), &mut |_, _, _| ()), Next::Finished);
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
            /// The writes the read met.
            met: u32,
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
                World { p, o, since, want, waits: true, met: 0, asked: false, next: 10 }
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
                        self.p.resize(&mut self.o, 0).unwrap();
                        self.settle();
                    }
                    Step::Grow if self.o.size() == 0 => {
                        self.p.resize(&mut self.o, PAGE).unwrap();
                        self.settle();
                    }
                    Step::Refused => {
                        assert!(self.o.take_pager());
                        let _ = self.p.supply(&mut self.o, 0, next, true, PAGE);
                        self.o.give(Side::Pager);
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
                        // As the read loop: a moved version begins the read
                        // again, asking anew, until it met
                        // `WRITES_A_READ_MEETS` writes and asks the pager;
                        // else the page decides.
                        if !self.o.unchanged(self.since) {
                            self.met += 1;
                            if self.met >= WRITES_A_READ_MEETS {
                                self.waits = false;
                                return true;
                            }
                            self.since = self.o.read_begin().unwrap();
                            self.want.anew();
                        }
                        let absent = matches!(self.p.lookup(&self.o, 0), Lookup::Absent);
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

            /// A removal's walk, over no mapper ever linked, to its end.
            fn settle(&mut self) {
                if self.o.revoke() == Revoke::Walk {
                    assert_eq!(self.p.walk(&mut self.o, &mut super::Unmapped, &mut One(&mut self.want), &mut |_, _, _| ()), Next::Finished);
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
            /// The walker's sweep of the futex waits on the walk's set, a
            /// frame at a time.
            Sweep,
            /// The server reads its notices: each request not yet named.
            Report,
            /// The server answers the oldest request it read, by a supply.
            Answer,
            /// Every parked fault looks at its page again.
            Retry,
            /// A thread of space `s` begins a futex wait on a word in `page`:
            /// it translates the word to the frame its leaf maps, under the
            /// space's lock, and notes the space's count of changes.
            Translate(u32, u64),
            /// That thread arms its wait, keyed on the frame: where the count
            /// has not moved it trusts the translation, and otherwise looks
            /// at the leaf again. Only a sweep ends the wait, since a visit or
            /// an unmap clears a leaf and revokes no frame it does not own.
            Arm,
        }

        pub(super) const STEPS: [Step; 25] = [
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
            Step::Sweep,
            Step::Report,
            Step::Answer,
            Step::Retry,
            Step::Translate(0, 0),
            Step::Translate(1, 1),
            Step::Arm,
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
            /// The space a walk visits, and where its visit has read the
            /// object's revoking pages to.
            visiting: Option<(u32, Cursor)>,
            /// Where the walker's sweep has read the walk's set to.
            sweeping: Option<Cursor>,
            /// The frames a futex waiter is keyed on.
            futex: BTreeSet<u64>,
            /// Each space's count of the leaf changes it shot down.
            counted: [u64; 2],
            /// A futex wait translated and not yet armed: its space, page,
            /// frame and the count it read.
            translated: Option<(u32, u64, u64, u64)>,
            /// Faults parked on an absent page: the space, the page, and the
            /// request as the pager hears of it.
            parked: std::vec::Vec<(u32, u64, Want, Version)>,
            /// Pages the server read requests for and has yet to supply.
            asked: std::collections::VecDeque<u64>,
        }

        /// The parked faults' wants, which removals name again.
        struct Parked<'a>(&'a mut std::vec::Vec<(u32, u64, Want, Version)>);

        impl Waiters for Parked<'_> {
            fn each(&mut self, f: &mut dyn FnMut(&mut Want)) {
                for (_, _, want, _) in self.0.iter_mut() {
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
                    sweeping: None,
                    futex: BTreeSet::new(),
                    counted: [0; 2],
                    translated: None,
                    parked: std::vec::Vec::new(),
                    asked: std::collections::VecDeque::new(),
                }
            }

            /// Whether `step` can be taken now; takes it if so.
            pub(super) fn step(&mut self, step: Step) -> bool {
                match step {
                    Step::Map(s) if !self.mapped[s as usize] => {
                        self.ring.link(s, &mut self.o);
                        self.mapped[s as usize] = true;
                    }
                    Step::Unmap(s) if self.mapped[s as usize] => {
                        self.leaves[s as usize] = [None; 2];
                        self.counted[s as usize] += 1;
                        let at = self.ring.0.iter().position(|&(n, _)| n == s).expect("linked while mapped");
                        self.ring.0.remove(at);
                        self.mapped[s as usize] = false;
                    }
                    Step::Fault(s, page) if self.mapped[s as usize] && self.leaves[s as usize][page as usize].is_none() => {
                        if !self.touch(s, page) {
                            if self.p.lookup(&self.o, page) != Lookup::Absent || self.parked.iter().any(|&(t, q, _, _)| (t, q) == (s, page)) {
                                return false;
                            }
                            let mut want = Want::NONE;
                            want.ask(page, 1);
                            self.parked.push((s, page, want, self.o.version()));
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
                        self.p.resize(&mut self.o, PAGE).unwrap();
                        self.revoke();
                    }
                    Step::Grow if !self.o.gone() && self.o.size() == PAGE => {
                        self.p.resize(&mut self.o, 2 * PAGE).unwrap();
                        self.revoke();
                    }
                    Step::End if !self.o.gone() => {
                        self.p.invalidate(&mut self.o);
                        self.revoke();
                    }
                    Step::Walk if self.walking && self.visiting.is_none() && self.sweeping.is_none() => {
                        let (freed, futex) = (&mut self.freed, &self.futex);
                        let walked = self.p.walk(&mut self.o, &mut self.ring, &mut Parked(&mut self.parked), &mut |f, _, _| {
                            assert!(!futex.contains(&f), "frame {f} given back with a futex waiter keyed on it");
                            assert!(freed.insert(f), "frame {f} given back twice");
                        });
                        match walked {
                            Next::Visit(node) => self.visiting = Some((node, Cursor::Start)),
                            Next::Sweep => self.sweeping = Some(Cursor::Start),
                            Next::Finished => self.walking = false,
                        }
                    }
                    // The sweep, a frame at a time: anything may run between.
                    Step::Sweep if self.sweeping.is_some() => {
                        let mut at = self.sweeping.take().unwrap();
                        let mut batch = [(0, 0)];
                        if self.p.revoking(&self.o, &mut at, &mut batch) == 1 {
                            self.futex.remove(&batch[0].1);
                            self.sweeping = Some(at);
                        }
                    }
                    Step::Translate(s, page) if self.translated.is_none() && self.mapped[s as usize] && self.leaves[s as usize][page as usize].is_some() => {
                        let f = self.leaves[s as usize][page as usize].unwrap();
                        self.translated = Some((s, page, f, self.counted[s as usize]));
                    }
                    Step::Arm if self.translated.is_some() => {
                        let (s, page, f, at) = self.translated.take().unwrap();
                        let holds = self.counted[s as usize] == at || self.leaves[s as usize][page as usize] == Some(f);
                        if holds {
                            self.futex.insert(f);
                        }
                    }
                    Step::Report => {
                        let before = self.asked.len();
                        for (_, _, want, _) in &mut self.parked {
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
                        // A wake may be stale: each looks whether or not its
                        // wait is over, as a thread woken for another's page.
                        for (s, page, mut want, since) in parked {
                            if !self.mapped[s as usize] || self.touch(s, page) {
                                continue;
                            }
                            match self.p.lookup(&self.o, page) {
                                // A write or resize ended the wait: ask afresh.
                                Lookup::Absent if !self.o.unchanged(since) => {
                                    want.anew();
                                    want.ask(page, 1);
                                    self.parked.push((s, page, want, self.o.version()));
                                }
                                Lookup::Absent => {
                                    want.ask(page, 1);
                                    self.parked.push((s, page, want, since));
                                }
                                // Refused: failed, past the end, or gone.
                                _ => {}
                            }
                        }
                    }
                    // The kernel's visit, a batch of one: the next revoking
                    // page read, and the space's leaf of it cleared where it
                    // maps that frame; anything may run between batches.
                    Step::Visit if self.visiting.is_some() => {
                        let (s, mut at) = self.visiting.take().unwrap();
                        let mut batch = [(0, 0)];
                        if self.p.revoking(&self.o, &mut at, &mut batch) == 1 {
                            let (page, f) = batch[0];
                            if self.leaves[s as usize][page as usize] == Some(f) {
                                self.leaves[s as usize][page as usize] = None;
                                // The visit's shootdown is counted, as an
                                // unmap's is.
                                self.counted[s as usize] += 1;
                            }
                            self.visiting = Some((s, at));
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
                let fair = [Step::Walk, Step::Visit, Step::Sweep, Step::Report, Step::Answer, Step::Retry];
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
                assert!(self.futex.is_disjoint(&self.freed), "after {step:?}: a futex waiter keyed on a given-back frame");
            }
        }
    }

    fn play(steps: &[mapped::Step]) -> Option<mapped::World> {
        let mut w = mapped::World::new();
        steps.iter().all(|&s| w.step(s)).then_some(w)
    }

    /// Every sequence of `depth` steps after `prefix`, each then drained: how
    /// many were played.
    fn explore(prefix: &[mapped::Step], depth: usize) -> u64 {
        let mut stack = std::vec![prefix.to_vec()];
        let mut played = 0u64;
        while let Some(seq) = stack.pop() {
            played += 1;
            play(&seq).expect("a sequence played before").drain(&seq);
            if seq.len() == prefix.len() + depth {
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
        played
    }

    /// From both spaces mapping both pages, every sequence of five steps of
    /// faults, supplies, removals, mappings and the walk keeps each leaf on
    /// its page's one frame, and leaves no fault parked once drained.
    #[test]
    fn every_interleaving_keeps_each_leaf_on_its_pages_one_frame() {
        use mapped::Step;
        let played = explore(&[Step::Map(0), Step::Map(1), Step::Supply(0), Step::Supply(1), Step::Fault(0, 0), Step::Fault(1, 1)], 5);
        assert!(played > 20_000, "only {played} sequences played");
    }

    /// A futex wait that translated its word before a walk's visit cleared
    /// the leaf, and arms only after the walk's sweep, does not arm on the
    /// frame the walk then gives back: the visit's shootdown is counted, so
    /// the arm looks at the leaf again.
    #[test]
    fn a_wait_translated_before_a_visit_does_not_arm_on_a_frame_given_back() {
        use mapped::Step;
        let steps = [
            Step::Map(0),
            Step::Supply(0),
            Step::Supply(1),
            Step::Fault(0, 0),
            Step::Translate(0, 0),
            Step::Withdraw(0),
            Step::Walk,
            Step::Visit,
            Step::Visit,
            Step::Walk,
            Step::Sweep,
            Step::Sweep,
            Step::Arm,
            Step::Walk,
        ];
        play(&steps).expect("every step is taken").drain(&steps);
    }

    /// From a futex wait translated before its page was withdrawn and a walk
    /// begun, every sequence of six steps arms no waiter on a frame the walk
    /// gives back, whenever it arms: a visit's clear is counted, so the arm
    /// looks at the leaf again.
    #[test]
    fn a_wait_translated_before_a_removal_never_arms_on_a_frame_given_back() {
        use mapped::Step;
        let prefix = [
            Step::Map(0),
            Step::Supply(0),
            Step::Supply(1),
            Step::Fault(0, 0),
            Step::Translate(0, 0),
            Step::Withdraw(0),
            Step::Walk,
        ];
        let played = explore(&prefix, 6);
        assert!(played > 100_000, "only {played} sequences played");
    }

    /// From a futex waiter an unmap left keyed on a frame and a walk begun
    /// with no mapper left, every sequence of five steps gives back no frame
    /// a waiter is keyed on: a page marked while the sweep has its lock
    /// dropped is swept before the walk ends.
    #[test]
    fn a_waiter_an_unmap_left_is_never_on_a_frame_given_back() {
        use mapped::Step;
        let prefix = [
            Step::Map(0),
            Step::Supply(0),
            Step::Supply(1),
            Step::Fault(0, 0),
            Step::Translate(0, 0),
            Step::Arm,
            Step::Unmap(0),
            Step::Withdraw(1),
            Step::Walk,
        ];
        let played = explore(&prefix, 5);
        assert!(played > 1_000, "only {played} sequences played");
    }

    /// From a walk begun on an object no mapper has linked, which ends with
    /// no sweep, every sequence of five steps gives back no frame a futex
    /// waiter is keyed on: a mapper linked during the walk maps nothing
    /// revoking, and the walk sweeps the pages removed once it has linked.
    #[test]
    fn a_walk_begun_before_any_mapping_gives_back_no_frame_a_waiter_is_keyed_on() {
        use mapped::Step;
        let played = explore(&[Step::Supply(0), Step::Supply(1), Step::Withdraw(0)], 5);
        assert!(played > 10_000, "only {played} sequences played");
    }

    /// A fault whose request the pager read and answered, before the fault
    /// looked again, meets a shrink and a grow: the version moved, so it
    /// asks afresh and is heard.
    #[test]
    fn a_fault_answered_then_shrunk_and_grown_asks_afresh() {
        use mapped::Step;
        let steps = [
            Step::Map(0),
            Step::Fault(0, 1),
            Step::Report,
            Step::Answer,
            Step::Shrink,
            Step::Walk,
            // A batch of one: the revoked page, then the read that finds none.
            Step::Visit,
            Step::Visit,
            Step::Walk,
            // The sweep: the revoked page's frame, then the read that finds none.
            Step::Sweep,
            Step::Sweep,
            Step::Walk,
            Step::Grow,
            Step::Retry,
        ];
        play(&steps).expect("every step is taken").drain(&steps);
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
