//! Paged objects: a pager process's memory, one object per file node, which
//! holders read, write and stat through `SYS_PAGED_READ`, `SYS_PAGED_WRITE`
//! and `SYS_PAGED_STAT`, or map read-only.
//!
//! **Two kinds.** A [`PagerObject`] is its process's one pager: its index of
//! `capacity` pages ([`kernel::paged::Pages`]), its objects by id, its
//! readers' requests and its notices. Every owner call names an object by id
//! through it, so a pager holds one handle however many objects it serves. It
//! neither duplicates nor transfers, so its last handle goes with its
//! process, and that ends every object it serves: each reader answers `Gone`.
//! A [`PagedHolderObject`] reads one object, writes it where minted to, and
//! may be narrowed and sent.
//!
//! **One write at a time** ([`kernel::paged::Side`]): a holder's write
//! copies its bytes into frames held aside first, then takes the object's
//! write side in one hold of the lock, refused at once while the side is held
//! or the pager waits for it, and is checked whole and admitted there; its
//! copy into the object's frames is the kernel's own. The pager holds the
//! side from its hold to its unhold, its waits included, and a holder states
//! the size and time of the moment it took them. A holder's read that met
//! [`kernel::paged::WRITES_A_READ_MEETS`] writes asks the pager, whose read
//! holds them off. So the pager waits for the side only on a holder's copy
//! inside the kernel, never on what the holder controls. A holder's write
//! stamps the modification time; the pager sets it for its own changes, and
//! its record of the rest is bytes the kernel keeps and never reads.
//!
//! **Supply copies into fresh frames**, so no frame the pager still maps
//! enters an object. A frame leaves at withdrawal, failure, shrink, the
//! object's release or the pager's end: its page is revoking, keeping the
//! frame, until a walk of the object's mappers ([`MapNode`]) has cleared
//! every space's leaves of it; then it leaves the index under the pager's
//! lock and is freed, which a reader's pin defers to its last unpin.
//!
//! **A mapping shares the index's frames.** A space mapping an object holds
//! one [`MapNode`] on the object's list while it has a range of it, and
//! counts as one of its holders. A fault on an absent page requests it and
//! waits as a reader does, holding nothing.
//!
//! **A reader waits only on its pager and on the kernel's copies.** An
//! absent page is requested in the waiter's own record, linked on its
//! pager's list, and the wait ends on a supply, a failure, a write or resize
//! of the object (after which the read begins again and asks anew), a
//! release, the pager's end or a kill; a wait past the pager's report time is
//! counted and logged overdue and goes on. A pager's own process is refused a
//! holder's calls, so no pager waits on a pager.
//!
//! **Lock order.** A space's lock, then the pager's, then the allocator's.
//! A copy to or from user memory holds pins, never the lock; no wait holds a
//! pin, so such a copy stops at an absent page (`user_ptr::Pinned`), lets its
//! pins go, and waits for the page holding nothing.
//!
//! **What a pager holds** (`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`):
//! its core — `dirty` pages and a read's headroom — committed at its making
//! and kept for its life, every frame of it either promised, in the index or
//! held aside for the next supply; the rest of `capacity` promised a page at
//! a time and given back as each goes. Its index and the frames it holds
//! aside are charged with it. Its objects' slots are made a piece at a time,
//! each piece charged as an object first needs it and kept for the pager's
//! life (`issues/one-client-can-fill-the-tmp-servers-memory-and-deny-every-other-client.md`),
//! so making an object is the one call that allocates.

use alloc::sync::{Arc, Weak};
use core::cell::{Cell, UnsafeCell};
use core::marker::PhantomPinned;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use kernel::chunked::Stack;
use kernel::paged::{
    holder_span, Core, Cursor, Draw, Leave, Lookup, Mappers, Next, Object, Pages, Refused, Revoke, Revoked, Side, Supplied, Version,
    Waiters, Want, Writer, HOLDER_PAGES, PAGE, WRITES_A_READ_MEETS,
};
use kernel::sched::task::WaitClass;
use kernel::slots::Slots;
use toyos_abi::syscall::{
    self as abi, PagedNotice, PagedStat, PagedStatus, SyscallError, NOTICE_DIRTIED, NOTICE_NO_HOLDERS, NOTICE_REQUESTED,
    PAGED_RECORD,
};

use crate::mm::charge::{self, Charge};
use crate::mm::pmm;
use crate::mm::space::Space;
use crate::process::Pid;
use crate::sync::{Lock, LockGuard};
use crate::time::{Deadline, Duration};
use crate::user_ptr::{Pinned, Stopped, UserBytes, UserBytesMut};
use crate::watch::{self, Watch};

mod ranges;
use ranges::Ranges;
use crate::DirectMap;

use super::{KObjectVariant, ObjectCore, ZeroHandles};

// Two pieces of a table and their allocator headers share one superframe.
const _: () = assert!(2 * kernel::chunked::CHUNK_BYTES < crate::mm::MAX_HEAP_ALLOC);

// A piece of slots is one allocation the heap takes.
const _: () = assert!(Slots::<Entry, 2>::PIECE_BYTES <= kernel::chunked::CHUNK_BYTES);

/// The slot table's lists: objects whose last holder went, and objects
/// holders dirtied, each owing its pager a notice.
const NO_HOLDERS: usize = 0;
const DIRTIED: usize = 1;

/// Pages one copy pins, and one supply or write draws, at once.
const CHUNK: usize = 16;

/// The page-in headroom held beside the dirty ceiling: what a read's supply
/// can always draw.
const HEADROOM: u64 = 512;

/// The longest a pager may let a reader wait before it is reported overdue.
const REPORT_CEILING: Duration = Duration::from_secs(10);

/// Notices copied out per hold of the lock.
const NOTICE_BATCH: usize = 32;

/// A reader's request, in its own stack frame for as long as it waits,
/// linked on its pager's list.
struct Request {
    slot: u32,
    want: Want,
    prev: *mut Request,
    next: *mut Request,
}

/// One object of a pager's, at its id's slot.
struct Entry {
    object: Object,
    /// References that keep the slot: it is free again with the last.
    refs: u32,
    /// Holder objects whose handles have not all gone, and spaces mapping it:
    /// a mapping holds the object as a handle does.
    holders: u32,
    /// Not released: owner calls reach it.
    owned: bool,
    /// Holders' writes under way, which outlive the handle they began through:
    /// the last holder is told gone only once none is.
    writers: u32,
    /// The pager's record, which holders read beside the size and time.
    record: [u8; PAGED_RECORD],
    /// The spaces mapping it, one node each ([`MapNode`]).
    mappers: Ends,
}

/// The ends of an object's list of mappers.
struct Ends {
    head: *const MapNode,
    tail: *const MapNode,
}

struct State {
    pages: Pages<u64>,
    /// Every object, at its id's slot, marked on [`NO_HOLDERS`] while its
    /// last holder has gone and on [`DIRTIED`] while holders dirtied its
    /// pages, each until a notice has said so.
    entries: Slots<Entry, 2>,
    /// The heap the slots take, charged to the pager's job as each piece is
    /// made.
    slots_charge: Charge,
    requests: *mut Request,
    held: Held,
    gone: bool,
}

/// The frames the core holds, counted, and those it holds aside for the
/// next supply: `spare` unpinned, `draining` still pinned by a copy that
/// read them.
struct Held {
    core: Core,
    spare: Stack<u64>,
    draining: Stack<u64>,
}

impl Held {
    /// A frame drawn for the index gives up its place: kept aside while the
    /// core would hold less without it, given back otherwise, and always once
    /// the pager has ended. `drawn` where it was in the index.
    fn give_back(&mut self, job: &kernel::sched::job::Job, gone: bool, phys: u64, drawn: bool) {
        if !gone && self.core.leave(drawn) == Leave::Aside {
            // Out of the index, so no copy can pin it from here.
            match pmm::pinned(phys) {
                true => self.draining.push(phys),
                false => self.spare.push(phys),
            }
            return;
        }
        // SAFETY: `phys` is a frame this pager drew, out of its index or never
        // in it, which nothing maps; a reader's pin defers its reuse.
        unsafe { pmm::free_drawn(job, &[phys]) };
    }

    /// Move each held-aside frame no copy pins any more to the spare ones.
    fn settle(&mut self) {
        let mut k = 0;
        while k < self.draining.len() {
            if pmm::pinned(self.draining.get(k)) {
                k += 1;
            } else {
                let phys = self.draining.swap_remove(k);
                self.spare.push(phys);
            }
        }
    }
}

// SAFETY: `requests` and every `Request` linked from it are reached only under
// the pager's lock, and each node is unlinked by its waiter before its frame
// returns (`Waiting`'s drop).
unsafe impl Send for State {}

impl State {
    fn entry(&self, slot: u32) -> &Entry {
        self.entries.get(slot).expect("paged: a slot an object names is held")
    }

    fn entry_mut(&mut self, slot: u32) -> &mut Entry {
        self.entries.get_mut(slot).expect("paged: a slot an object names is held")
    }

    /// The slot of the unreleased object `id` names: the caller's word,
    /// refused `InvalidArgument` where it names none.
    fn live(&self, id: u64) -> Result<u32, SyscallError> {
        let slot = u32::try_from(id).map_err(|_| SyscallError::InvalidArgument)?;
        let entry = self.entries.get(slot).is_some_and(|e| e.owned);
        match entry {
            true if !self.gone => Ok(slot),
            _ if self.gone => Err(SyscallError::Gone),
            _ => Err(SyscallError::InvalidArgument),
        }
    }

    /// The index and the object at `slot`, borrowed apart.
    fn split(&mut self, slot: u32) -> (&mut Pages<u64>, &mut Object) {
        let entry = self.entries.get_mut(slot).expect("paged: a slot an object names is held");
        (&mut self.pages, &mut entry.object)
    }

    /// A free slot for the next object, its piece charged where one is made:
    /// `ResourceExhausted` where the charge is refused or the table is whole.
    fn make_slot(&mut self) -> Result<(), SyscallError> {
        if self.entries.has_free() {
            return Ok(());
        }
        let grown = self.entries.grown_bytes().ok_or(SyscallError::ResourceExhausted)?;
        let more = grown - self.entries.heap_bytes();
        self.slots_charge.grow(more)?;
        if !self.entries.grow() {
            self.slots_charge.shrink(more);
            return Err(SyscallError::ResourceExhausted);
        }
        Ok(())
    }
}

pub struct PagerObject {
    pub(super) core: ObjectCore,
    state: Lock<State>,
    /// Posted when a notice is owed.
    watch: Arc<Watch>,
    /// Posted on every change a waiter reads again: a supply, a failure, a
    /// write's end, a resize, a release, the end.
    changed: Arc<Watch>,
    pid: Pid,
    report: Duration,
    overdue: AtomicU32,
}

/// Why [`PagerObject::create`] refused.
pub enum Unmade {
    Invalid,
    Exhausted,
}

impl PagerObject {
    pub const RIGHTS: toyos_abi::handle::Rights =
        toyos_abi::handle::Rights::READ.union(toyos_abi::handle::Rights::WRITE).union(toyos_abi::handle::Rights::WAIT);

    /// The calling process's pager: `capacity` pages, `dirty` of them dirty at
    /// once, readers overdue past `report_ns`.
    pub fn create(capacity: u64, dirty: u64, report_ns: u64) -> Result<Arc<Self>, Unmade> {
        let Some(slots) = usize::try_from(capacity).ok().filter(|&n| n > 0 && n <= Pages::<u64>::MAX_CAPACITY) else {
            return Err(Unmade::Invalid);
        };
        if dirty > capacity {
            return Err(Unmade::Invalid);
        }
        let core_pages = dirty + (capacity - dirty).min(HEADROOM);
        let held = core_pages as usize;
        let tables = Pages::<u64>::heap_bytes(slots) + 2 * Stack::<u64>::heap_bytes(held);
        let core = Self::new_core(tables).map_err(|_| Unmade::Exhausted)?;
        let pages = Pages::new(slots, dirty as usize).ok_or(Unmade::Exhausted)?;
        let spare = Stack::new(held).ok_or(Unmade::Exhausted)?;
        let draining = Stack::new(held).ok_or(Unmade::Exhausted)?;
        if !pmm::promise(core.job(), core_pages) {
            return Err(Unmade::Exhausted);
        }
        let state = State {
            pages,
            entries: Slots::new(),
            slots_charge: Charge::none(core.job()),
            requests: ptr::null_mut(),
            held: Held { core: Core::new(core_pages), spare, draining },
            gone: false,
        };
        let report = Duration::from_nanos(report_ns.clamp(1, REPORT_CEILING.nanos()));
        Ok(Arc::new(Self {
            core,
            state: Lock::new(state),
            watch: Arc::new(Watch::new()),
            changed: Arc::new(Watch::new()),
            pid: crate::process::current_process(),
            report,
            overdue: AtomicU32::new(0),
        }))
    }

    pub fn watch(&self) -> &Arc<Watch> {
        &self.watch
    }

    pub fn room(&self) -> u64 {
        let state = self.state.lock();
        (state.pages.capacity() - state.pages.used()) as u64
    }

    /// Whether a notice is owed: a request no notice has named, a last
    /// holder gone, or a holder's dirtying.
    pub fn has_notices(&self) -> bool {
        let state = self.state.lock();
        // SAFETY: every node on the list is live while linked, under this lock.
        let unreported = requests(&state).any(|r| unsafe { (*r).want.unreported() });
        unreported || state.entries.any_marked(NO_HOLDERS) || state.entries.any_marked(DIRTIED)
    }

    /// A new object of `size` bytes: its id. `ResourceExhausted` where its
    /// job cannot be charged the slots for another, or the table holds as
    /// many as it can.
    pub fn add(&self, size: u64, anonymous: bool) -> u64 {
        if size > kernel::paged::LAST_END {
            return SyscallError::InvalidArgument.to_u64();
        }
        let mut state = self.state.lock();
        if state.gone {
            return SyscallError::Gone.to_u64();
        }
        if let Err(e) = state.make_slot() {
            return e.to_u64();
        }
        let State { pages, entries, .. } = &mut *state;
        let made = entries.insert_with(|slot| {
            let object = if anonymous { pages.anonymous(slot, size) } else { pages.object(slot, size) };
            let mappers = Ends { head: ptr::null(), tail: ptr::null() };
            Entry { object, refs: 1, holders: 0, owned: true, writers: 0, record: [0; PAGED_RECORD], mappers }
        });
        u64::from(made.expect("paged: a slot was made free"))
    }

    /// `f` over the index, the live object `id` and its waiters, marking
    /// pages revoking, then their walk ([`Self::revoked`]); waiters read again.
    fn removing<R>(&self, id: u64, f: impl FnOnce(&mut Pages<u64>, &mut Object, &mut Linked) -> R) -> Result<R, SyscallError> {
        let mut state = self.state.lock();
        let slot = state.live(id)?;
        let mut waiters = Linked { head: state.requests, slot };
        let (pages, object) = state.split(slot);
        let answer = f(pages, object, &mut waiters);
        let (mut state, queued) = self.revoked(state, slot);
        let Some(epoch) = queued else {
            drop(state);
            self.changed.post();
            return Ok(answer);
        };
        // A walk runs: its next pass takes these pages, and the call returns
        // once it has, with the slot held meanwhile.
        state.entry_mut(slot).refs += 1;
        drop(state);
        self.changed.post();
        let _held = Ref { pager: self, slot };
        let parkable = crate::scheduler::Parkable::at_entry();
        let over = || self.state.lock().entry(slot).object.finished(epoch);
        // A kill ends either wait, and its thread never reads the answer. A
        // walk takes no wait of its own, so one past the report time is a
        // kernel fault worth a line, and the wait goes on.
        let report = Deadline::at(crate::clock::now() + self.report);
        if watch::wait_until(&parkable, &self.changed, 0, WaitClass::Io, report, over).is_ok() && !over() {
            log!("paged: pid {}'s removal waits past {} ms on another's walk", self.pid, self.report.nanos() / 1_000_000);
            let _ = watch::wait_until(&parkable, &self.changed, 0, WaitClass::Io, Deadline::never(), over);
        }
        Ok(answer)
    }

    /// The pages of the object at `slot` just marked revoking leave once no
    /// mapping reaches them and no futex wait is keyed on their frames: this
    /// visits the object's mappers and sweeps the waits on the walk's set as
    /// [`Pages::walk`] directs, the lock dropped for each visit and sweep and
    /// the slot held meanwhile, until the walk's end gives each frame back
    /// and names again every waiter's want a page that ends absent meets.
    /// Answers the lock and, where another's walk runs and takes the pages in
    /// its next pass, that pass's epoch.
    fn revoked<'a>(&'a self, mut state: LockGuard<'a, State>, slot: u32) -> (LockGuard<'a, State>, Option<u64>) {
        match state.entry_mut(slot).object.revoke() {
            Revoke::Done => return (state, None),
            Revoke::Queued(epoch) => return (state, Some(epoch)),
            Revoke::Walk => {}
        }
        let job = self.core.job();
        state.entry_mut(slot).refs += 1;
        let mut absent = false;
        loop {
            let mut waiters = Linked { head: state.requests, slot };
            let State { pages, entries, held, gone, .. } = &mut *state;
            let Entry { object, mappers, .. } = entries.get_mut(slot).expect("paged: a walked slot is held");
            let walked = pages.walk(object, &mut Ring(mappers), &mut waiters, &mut |phys, _, becomes| {
                held.give_back(job, *gone, phys, true);
                absent |= becomes == Revoked::Absent;
            });
            match walked {
                Next::Visit(node) => {
                    // SAFETY: linked, so live, under the lock; a range holds
                    // it, so it upgrades, and the count kept holds it past
                    // its unlinking while the lock is dropped.
                    let node = unsafe { (*node).this.upgrade() }.expect("paged: a linked mapper is held by a range");
                    state = self.visit(state, slot, &node);
                }
                Next::Sweep => state = self.end_futex_waits(state, slot),
                Next::Finished => break,
            }
        }
        self.unref_locked(&mut state, slot);
        if absent {
            self.watch.post();
        }
        (state, None)
    }

    /// A walk's visit of `node`'s space: the object's revoking pages read a
    /// batch at a time under the pager's lock alone, each batch's leaves then
    /// cleared and shot down under the space's lock alone
    /// ([`Space::clear_paged`]). One lock at a time, and O(revoking pages).
    fn visit<'a>(&'a self, mut state: LockGuard<'a, State>, slot: u32, node: &MapNode) -> LockGuard<'a, State> {
        let mut at = Cursor::Start;
        loop {
            let mut batch = [(0u64, 0u64); REVOKE_BATCH];
            let n = state.pages.revoking(&state.entry(slot).object, &mut at, &mut batch);
            drop(state);
            if n > 0 {
                let Some(space) = node.space.upgrade() else { return self.state.lock() };
                space.lock().clear_paged(node, &batch[..n]);
            }
            state = self.state.lock();
            if n < REVOKE_BATCH {
                return state;
            }
        }
    }

    /// Every futex wait on a frame of the walk's set at `slot` ends
    /// ([`Next::Sweep`], asked only of an object a mapper ever linked): the
    /// frames read a batch at a time under the pager's lock, and the waits
    /// ended outside it in every bucket, since a wait in any space that mapped
    /// the object can be keyed on them (`sched::futex::revoke_sorted`).
    /// An unmap, `discard`, `protect` to none or a space's end clears a leaf
    /// without ending the waits on a frame it does not own, so a waiter keyed
    /// on a file's frame through such a leaf is ended here, before the frame
    /// goes back.
    fn end_futex_waits<'a>(&'a self, mut state: LockGuard<'a, State>, slot: u32) -> LockGuard<'a, State> {
        let mut at = Cursor::Start;
        loop {
            let mut batch = [(0u64, 0u64); REVOKE_BATCH];
            let n = state.pages.revoking(&state.entry(slot).object, &mut at, &mut batch);
            drop(state);
            let mut frames = [0u64; REVOKE_BATCH];
            for (frame, &(_, phys)) in frames.iter_mut().zip(&batch[..n]) {
                *frame = phys;
            }
            frames[..n].sort_unstable();
            crate::sched::futex::revoke_sorted(&frames[..n], |&phys| phys);
            state = self.state.lock();
            if n < REVOKE_BATCH {
                return state;
            }
        }
    }

    /// The pager lets object `id` go, in one hold of the lock: it answers
    /// `Gone` from now on, its pages go, and its slot with its last holder. A
    /// second release, or one racing the pager's end, finds it released.
    pub fn release(&self, id: u64) -> u64 {
        let state = self.state.lock();
        let slot = match state.live(id) {
            Ok(slot) => slot,
            Err(e) => return e.to_u64(),
        };
        drop(self.let_go(state, slot));
        self.changed.post();
        0
    }

    /// Under the lock, `state`: the unreleased object at `slot` released.
    fn let_go<'a>(&'a self, mut state: LockGuard<'a, State>, slot: u32) -> LockGuard<'a, State> {
        let State { pages, entries, .. } = &mut *state;
        let entry = entries.get_mut(slot).expect("paged: a slot an object names is held");
        pages.invalidate(&mut entry.object);
        assert!(core::mem::take(&mut entry.owned), "paged: an object released twice");
        entries.unmark(NO_HOLDERS, slot);
        entries.unmark(DIRTIED, slot);
        // A walk already running takes the pages; nothing here waits.
        let (mut state, _) = self.revoked(state, slot);
        self.unref_locked(&mut state, slot);
        state
    }

    /// A holder of object `id`, which writes it where `writes`, and counts
    /// as one once its handle is installed ([`PagedHolderObject::held`]).
    pub fn holder(self: &Arc<Self>, id: u64, writes: bool) -> Result<Arc<PagedHolderObject>, SyscallError> {
        let core = PagedHolderObject::new_core(0)?;
        let mut state = self.state.lock();
        let slot = state.live(id)?;
        state.entry_mut(slot).refs += 1;
        drop(state);
        Ok(Arc::new(PagedHolderObject { core, named: Named { pager: Arc::clone(self), slot }, writes }))
    }

    /// A holder object's handles went.
    fn holder_gone(&self, slot: u32) {
        let mut state = self.state.lock();
        state.entry_mut(slot).holders -= 1;
        self.say_no_holders(&mut state, slot);
    }

    /// Under the lock: the pager is told once that `slot`'s last holder went,
    /// where none is left and no holder's write is under way.
    fn say_no_holders(&self, state: &mut State, slot: u32) {
        let gone = state.gone;
        let entry = state.entry_mut(slot);
        if entry.holders == 0 && entry.writers == 0 && entry.owned && !gone && state.entries.mark(NO_HOLDERS, slot) {
            self.watch.post();
        }
    }

    /// A reference to `slot` went; its entry goes with the last, and the slot
    /// is free for another object.
    fn unref(&self, slot: u32) {
        self.unref_locked(&mut self.state.lock(), slot);
    }

    fn unref_locked(&self, state: &mut State, slot: u32) {
        let entry = state.entry_mut(slot);
        entry.refs -= 1;
        if entry.refs > 0 {
            return;
        }
        let entry = state.entries.remove(slot).expect("paged: a slot an object names is held");
        assert!(entry.object.empty(), "paged: an object's slot freed while it holds pages");
    }

    /// Fill `out` with notices, each answered once.
    pub fn notices(&self, out: &mut UserBytesMut) -> u64 {
        let size = core::mem::size_of::<PagedNotice>();
        let room = out.len() / size;
        let mut filled = 0;
        while filled < room {
            let mut batch = [PagedNotice::default(); NOTICE_BATCH];
            let want = (room - filled).min(NOTICE_BATCH);
            let n = {
                let mut state = self.state.lock();
                let mut n = 0;
                let mut at = state.requests;
                while n < want && !at.is_null() {
                    // SAFETY: linked, so live, under the lock; its waiter
                    // writes it only under the lock too, and nothing forms a
                    // reference to it.
                    let (slot, mut want, next) = unsafe { ((*at).slot, (*at).want, (*at).next) };
                    if let Some((first, count)) = want.report() {
                        // SAFETY: as above.
                        unsafe { (*at).want = want };
                        batch[n] = PagedNotice { object: u64::from(slot), kind: NOTICE_REQUESTED, first, count };
                        n += 1;
                    }
                    at = next;
                }
                while n < want {
                    let Some(slot) = state.entries.take_marked(NO_HOLDERS) else { break };
                    // Said only while still so: a holder made since is held,
                    // and a write under way marks the object again at its end.
                    let entry = state.entry(slot);
                    if entry.holders == 0 && entry.writers == 0 {
                        batch[n] = PagedNotice { object: u64::from(slot), kind: NOTICE_NO_HOLDERS, first: 0, count: 0 };
                        n += 1;
                    }
                }
                while n < want {
                    let Some(slot) = state.entries.take_marked(DIRTIED) else { break };
                    batch[n] = PagedNotice { object: u64::from(slot), kind: NOTICE_DIRTIED, first: 0, count: 0 };
                    n += 1;
                }
                n
            };
            // SAFETY: a `PagedNotice` is `UserSafe`: every byte of it is a
            // field, and the batch outlives the copy.
            if let Err(faulted) = unsafe { out.write_raw(filled * size, batch.as_ptr().cast::<u8>(), n * size) } {
                // The notices taken are lost with the window that could not hold them.
                return if filled > 0 { filled as u64 } else { SyscallError::from(faulted).to_u64() };
            }
            filled += n;
            if n < want {
                break;
            }
        }
        filled as u64
    }

    /// The owner's read of object `id`: `out.len()` bytes from `offset`,
    /// answered `WouldBlock` where the first page is absent.
    pub fn owner_read(&self, id: u64, out: &mut UserBytesMut, offset: u64) -> u64 {
        let live = {
            let mut state = self.state.lock();
            state.live(id).inspect(|&slot| state.entry_mut(slot).refs += 1)
        };
        match live {
            Ok(slot) => {
                // A release by another of the pager's threads leaves the entry
                // to this read until it ends.
                let _held = Ref { pager: self, slot };
                self.read(slot, out, offset, false)
            }
            Err(e) => e.to_u64(),
        }
    }

    /// Read `out.len()` bytes of the object at `slot` from `offset`; a holder
    /// waits for absent pages and for a write under way, and its bytes count
    /// only where no write began meanwhile; one that meets
    /// [`WRITES_A_READ_MEETS`] writes is answered `WouldBlock`, and asks the
    /// pager, whose read holds the writes off. An owner is answered `WouldBlock` for an absent first
    /// page, and orders its read against writes by holding them. The caller
    /// holds a reference on `slot` for the call.
    fn read(&self, slot: u32, out: &mut UserBytesMut, offset: u64, waits: bool) -> u64 {
        let want = out.len();
        let mut done = 0usize;
        let waiting = core::pin::pin!(Waiting::new(self, slot));
        let mut pieces = [Piece::default(); CHUNK];
        // A holder's: what its bytes count against, and the writes it met.
        let mut since: Option<Version> = None;
        let mut met = 0;
        loop {
            let at = offset.saturating_add(done as u64);
            let step = {
                let mut state = self.state.lock();
                let size = state.entry(slot).object.size();
                if state.gone || state.entry(slot).object.gone() {
                    Step::Gone
                } else if done == 0 && (want == 0 || at >= size) {
                    Step::Done
                } else if waits && !Self::unwritten(&state.entry(slot).object, &mut since) {
                    Step::Written
                } else if at >= size {
                    Step::Done
                } else {
                    let end = size.min(offset.saturating_add(want as u64));
                    let waiter = waits.then_some(waiting.as_ref());
                    self.pin_chunk(&mut state, slot, at..end, &mut pieces, waiter)
                }
            };
            match step {
                Step::Copy { count, last } => {
                    let (first, chunk) = (done, pieces[..count].iter().map(|piece| piece.len).sum::<usize>());
                    let mut stopped = None;
                    let pinned = Pinned::begin();
                    for piece in &pieces[..count] {
                        let copied = match piece.phys {
                            // SAFETY: `phys` is pinned by `pin_chunk`, and
                            // `within + len` lies inside its frame.
                            Some(phys) => unsafe { pinned.write(out, done, DirectMap::from_phys(phys).as_ptr::<u8>().add(piece.within), piece.len) },
                            None => pinned.zero(out, done, piece.len),
                        };
                        if let Err(why) = copied {
                            stopped = Some(why);
                            break;
                        }
                        done += piece.len;
                    }
                    drop(pinned);
                    for piece in &pieces[..count] {
                        if let Some(phys) = piece.phys { pmm::unpin_range(phys, PAGE as usize); }
                    }
                    let (gone, written) = {
                        let state = self.state.lock();
                        let object = &state.entry(slot).object;
                        (state.gone || object.gone(), waits && !Self::unwritten(object, &mut since))
                    };
                    // A frame copied as its pager ended is no byte to report.
                    if gone {
                        return SyscallError::Io.to_u64();
                    }
                    if !written {
                        // After the checks: bytes copied before the fault
                        // count only as every copied chunk's do. A page of
                        // the buffer that is absent is waited for with
                        // nothing pinned, and the rest of the chunk read
                        // again.
                        let faulted = match stopped {
                            None => None,
                            Some(Stopped::Absent) => out.wait_in(done, first + chunk - done).err(),
                            Some(Stopped::Faulted(faulted)) => Some(faulted),
                        };
                        if let Some(faulted) = faulted {
                            return if done > 0 { done as u64 } else { SyscallError::from(faulted).to_u64() };
                        }
                        if last && stopped.is_none() {
                            return done as u64;
                        }
                        continue;
                    }
                    // What was copied may be a write in part.
                }
                Step::Written => {}
                Step::Done => return done as u64,
                Step::Gone if done > 0 => return done as u64,
                Step::Gone => return SyscallError::Gone.to_u64(),
                Step::Failed if done > 0 => return done as u64,
                Step::Failed => return SyscallError::Io.to_u64(),
                Step::Absent(_) if !waits && done > 0 => return done as u64,
                Step::Absent(_) if !waits => return SyscallError::WouldBlock.to_u64(),
                Step::Absent(page) => {
                    let since = since.expect("paged: a holder's read waits only once it began");
                    if let Err(e) = self.wait_supplied(slot, page, since, waiting.as_ref()) {
                        return e.to_u64();
                    }
                    continue;
                }
            }
            // A write met the read: it asks the pager, or waits for the
            // write's end and reads again, asking anew.
            met += 1;
            if met >= WRITES_A_READ_MEETS {
                return SyscallError::WouldBlock.to_u64();
            }
            let ready = || {
                let state = self.state.lock();
                let object = &state.entry(slot).object;
                state.gone || object.gone() || object.read_begin().is_some()
            };
            if let Err(e) = self.wait_changed(slot, offset / PAGE, (want as u64).div_ceil(PAGE), ready) {
                return e.to_u64();
            }
            done = 0;
            since = None;
            waiting.as_ref().anew(&mut self.state.lock());
        }
    }

    /// A holder's read that began at `since` waits for `page` of the object
    /// at `slot`, which its `waiting` record asked for, until it is supplied,
    /// failed or cut off, a write or resize begins, or the object or pager
    /// ends ([`Pages::wait_over`]).
    fn wait_supplied(&self, slot: u32, page: u64, since: Version, waiting: Pin<&Waiting<'_>>) -> Result<(), SyscallError> {
        let ready = || {
            let state = self.state.lock();
            state.gone || state.pages.wait_over(&state.entry(slot).object, page, since)
        };
        let (first, count) = waiting.range(&self.state.lock());
        self.wait_changed(slot, first, count, ready)
    }

    /// Under the lock: whether no write met a holder's read of `object` that
    /// began at `since`, or begins now where it is `None`.
    fn unwritten(object: &Object, since: &mut Option<Version>) -> bool {
        match *since {
            Some(v) => object.unchanged(v),
            None => {
                *since = object.read_begin();
                since.is_some()
            }
        }
    }

    /// Wait on `changed` until `ready`, for `count` pages from `first` of
    /// the object at `slot`: past the pager's report time the wait is
    /// counted and logged overdue and goes on. `Gone` only for a wait cut
    /// short, by a kill.
    fn wait_changed(&self, slot: u32, first: u64, count: u64, ready: impl Fn() -> bool) -> Result<(), SyscallError> {
        let parkable = crate::scheduler::Parkable::at_entry();
        let began = crate::clock::now();
        let report = Deadline::at(began + self.report);
        if watch::wait_until(&parkable, &self.changed, 0, WaitClass::Io, report, &ready).is_err() {
            return Err(SyscallError::Gone);
        }
        if !ready() {
            // This wait, and no other, is overdue until it ends.
            let _overdue = Overdue::begin(self, slot, first, count, began);
            if watch::wait_until(&parkable, &self.changed, 0, WaitClass::Io, Deadline::never(), &ready).is_err() {
                return Err(SyscallError::Gone);
            }
        }
        Ok(())
    }

    /// Under the lock: pin the present pages of `range`, at most [`CHUNK`],
    /// into `pieces`, or else say what the first page is, recording a
    /// waiter's request for every page from an absent one to the range's end.
    fn pin_chunk(
        &self,
        state: &mut State,
        slot: u32,
        range: core::ops::Range<u64>,
        pieces: &mut [Piece; CHUNK],
        waiting: Option<Pin<&Waiting<'_>>>,
    ) -> Step {
        let mut count = 0;
        let mut pos = range.start;
        while count < CHUNK && pos < range.end {
            let page = pos / PAGE;
            match state.pages.lookup(&state.entry(slot).object, page) {
                Lookup::Present(phys) if pmm::pin_range(phys, PAGE as usize) => {
                    let within = (pos % PAGE) as usize;
                    let len = ((PAGE - pos % PAGE).min(range.end - pos)) as usize;
                    pieces[count] = Piece { phys: Some(phys), within, len };
                    count += 1;
                    pos += len as u64;
                }
                // A pin count at its limit: copy what is pinned, and pin again.
                Lookup::Present(_) if count > 0 => break,
                Lookup::Present(_) => panic!("paged: a frame's pins overflowed"),
                Lookup::Zero => {
                    let len = ((PAGE - pos % PAGE).min(range.end - pos)) as usize;
                    pieces[count] = Piece { phys: None, within: 0, len };
                    count += 1;
                    pos += len as u64;
                }
                _ if count > 0 => break,
                Lookup::Absent => {
                    if let Some(waiting) = waiting {
                        let last = (range.end - 1) / PAGE;
                        waiting.request(state, page, last - page + 1);
                    }
                    return Step::Absent(page);
                }
                Lookup::Failed => return Step::Failed,
                Lookup::Gone => return Step::Gone,
                Lookup::PastEnd => return Step::Done,
            }
        }
        Step::Copy { count, last: pos == range.end }
    }

    /// The pager holds object `id`'s writes: no other write lands, nor does a
    /// holder's read read half of one, until [`Self::unhold`]; its own writes,
    /// truncations and reads go in between, in as many calls and waits as
    /// they take. It waits for a holder's write under way, work inside the
    /// kernel that waits on nothing. The size, of that moment.
    pub fn hold(&self, id: u64) -> u64 {
        let slot = {
            let mut state = self.state.lock();
            match state.live(id) {
                Ok(slot) if state.entry(slot).object.side() == Side::Pager => return SyscallError::InvalidArgument.to_u64(),
                Ok(slot) => {
                    state.entry_mut(slot).refs += 1;
                    slot
                }
                Err(e) => return e.to_u64(),
            }
        };
        // A release by another of the pager's threads leaves the entry to this wait.
        let _held = Ref { pager: self, slot };
        loop {
            {
                let mut state = self.state.lock();
                if state.gone || !state.entry(slot).owned {
                    return SyscallError::Gone.to_u64();
                }
                let object = &mut state.entry_mut(slot).object;
                if object.take_pager() {
                    return object.size();
                }
            }
            let ready = || {
                let state = self.state.lock();
                let object = &state.entry(slot).object;
                state.gone || object.gone() || object.free_for_pager()
            };
            if let Err(e) = self.wait_changed(slot, 0, 0, ready) {
                return e.to_u64();
            }
        }
    }

    /// The pager gives object `id`'s writes back: `InvalidArgument` where it
    /// holds none.
    pub fn unhold(&self, id: u64) -> u64 {
        let given = {
            let mut state = self.state.lock();
            state.live(id).and_then(|slot| {
                let object = &mut state.entry_mut(slot).object;
                if object.side() != Side::Pager {
                    return Err(SyscallError::InvalidArgument);
                }
                object.give(Side::Pager);
                Ok(())
            })
        };
        match given {
            Ok(()) => {
                self.changed.post();
                0
            }
            Err(e) => e.to_u64(),
        }
    }

    /// Whether the pager holds object `id`'s writes, which every write and
    /// truncation of its needs: `InvalidArgument` where it does not.
    fn holding(&self, id: u64) -> Result<(), SyscallError> {
        let state = self.state.lock();
        let slot = state.live(id)?;
        match state.entry(slot).object.side() {
            Side::Pager => Ok(()),
            _ => Err(SyscallError::InvalidArgument),
        }
    }

    /// Copy `src` into fresh pages of object `id` from page-aligned `offset`,
    /// clean or dirty: the pages installed. A dirty supply is a write, under
    /// the pager's hold, and stops at the first present or refused page. The
    /// span is checked.
    pub fn supply(&self, id: u64, offset: u64, src: &UserBytes, dirty: bool) -> u64 {
        if !offset.is_multiple_of(PAGE) || src.len() == 0 {
            return SyscallError::InvalidArgument.to_u64();
        }
        if let (true, Err(e)) = (dirty, self.holding(id)) {
            return e.to_u64();
        }
        self.supply_pages(id, offset, src, dirty)
    }

    fn supply_pages(&self, id: u64, offset: u64, src: &UserBytes, dirty: bool) -> u64 {
        let mut installed = 0u64;
        let mut done = 0usize;
        while done < src.len() {
            let first = (offset + done as u64) / PAGE;
            let pages = (src.len() - done).div_ceil(PAGE as usize).min(CHUNK);
            // Frames first, under the lock: one held aside, or a promise.
            let mut aside = [None; CHUNK];
            let (slot, reserved, size) = {
                let mut state = self.state.lock();
                let slot = match state.live(id) {
                    Ok(slot) => slot,
                    Err(e) => return refusal(installed, e),
                };
                let reserved = self.draw(&mut state, &mut aside[..pages]);
                (slot, reserved, state.entry(slot).object.size())
            };
            if reserved == 0 {
                return refusal(installed, SyscallError::ResourceExhausted);
            }
            let mut frames = [0u64; CHUNK];
            let mut ends = [0u64; CHUNK];
            // The frames filled from `src`, in order: a fault stops the fill.
            let mut filled = 0;
            let mut faulted = None;
            for k in 0..reserved {
                let page_at = (first + k as u64) * PAGE;
                let from = done + k * PAGE as usize;
                let mut n = (src.len() - from).min(PAGE as usize);
                // A clean page is the file's, whose bytes past its size are zeros.
                if !dirty {
                    n = n.min(size.saturating_sub(page_at) as usize);
                }
                let phys = materialize(aside[k], true);
                frames[k] = phys;
                ends[k] = page_at + (src.len() - from).min(PAGE as usize) as u64;
                // SAFETY: a frame just drawn, which nothing else reaches yet.
                if filled == k {
                    // SAFETY: as above.
                    match unsafe { src.read_raw(from, DirectMap::from_phys(phys).as_mut_ptr::<u8>(), n) } {
                        Ok(()) => filled += 1,
                        Err(why) => faulted = Some(why),
                    }
                }
            }
            let mut refused = None;
            let mut stopped = false;
            {
                let mut state = self.state.lock();
                for k in 0..reserved {
                    // A frame the owner's buffer could not fill goes back as a refused one does.
                    if stopped || k >= filled {
                        let gone = state.gone;
                        state.held.give_back(self.core.job(), gone, frames[k], false);
                        continue;
                    }
                    let answer = match state.live(id) {
                        Ok(_) => {
                            let (pages, object) = state.split(slot);
                            pages.supply(object, first + k as u64, frames[k], dirty, ends[k])
                        }
                        Err(_) => Err(Refused::Gone),
                    };
                    match answer {
                        Ok(Supplied::Installed) => {
                            state.held.core.installed();
                            installed += 1;
                        }
                        Ok(Supplied::Present) => {
                            let gone = state.gone;
                            state.held.give_back(self.core.job(), gone, frames[k], false);
                            stopped = dirty;
                        }
                        Err(why) => {
                            let gone = state.gone;
                            state.held.give_back(self.core.job(), gone, frames[k], false);
                            refused.get_or_insert(word(why));
                            stopped = true;
                        }
                    }
                }
            }
            self.changed.post();
            if let Some(faulted) = faulted.filter(|_| !stopped) {
                refused.get_or_insert(faulted.into());
            }
            if let Some(why) = refused {
                return refusal(installed, why);
            }
            if stopped {
                return installed;
            }
            done += reserved * PAGE as usize;
        }
        installed
    }

    /// Copy `src` into the present pages of object `id` from `offset`, dirty,
    /// under the pager's hold, answering the bytes written, up to the first
    /// absent page. The caller checked the span is in reach.
    pub fn write(&self, id: u64, offset: u64, src: &UserBytes) -> u64 {
        match self.holding(id) {
            Ok(()) => self.write_pages(id, offset, src),
            Err(e) => e.to_u64(),
        }
    }

    fn write_pages(&self, id: u64, offset: u64, src: &UserBytes) -> u64 {
        let mut done = 0usize;
        while done < src.len() {
            let mut pinned = [(0u64, 0u64, 0usize, 0usize); CHUNK];
            let mut count = 0;
            let mut stopped = None;
            let mut stopped_at = None;
            let slot = {
                let mut state = self.state.lock();
                let slot = match state.live(id) {
                    Ok(slot) => slot,
                    Err(e) => return refusal(done as u64, e),
                };
                let mut pos = offset + done as u64;
                while count < CHUNK && (pos - offset) < src.len() as u64 {
                    let page = pos / PAGE;
                    let (pages, object) = state.split(slot);
                    match pages.admit(object, page, Writer::Pager) {
                        Ok(phys) => {
                            assert!(pmm::pin_range(phys, PAGE as usize), "paged: a frame's pins overflowed");
                            let within = (pos % PAGE) as usize;
                            let n = (PAGE as usize - within).min(src.len() - (pos - offset) as usize);
                            pinned[count] = (page, phys, within, n);
                            count += 1;
                            pos += n as u64;
                        }
                        Err(why) => {
                            stopped = Some(why);
                            break;
                        }
                    }
                }
                slot
            };
            let mut from = done;
            // The pages whose bytes all came: a fault in the owner's buffer stops there.
            let mut copied = 0;
            let copies = Pinned::begin();
            for &(_, phys, within, n) in &pinned[..count] {
                // SAFETY: a frame of the object, pinned and write-pinned above.
                if let Err(why) = unsafe { copies.read(src, from, DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), n) } {
                    stopped_at = Some(why);
                    break;
                }
                from += n;
                copied += 1;
            }
            drop(copies);
            let chunk: usize = pinned[..count].iter().map(|&(_, _, _, n)| n).sum();
            {
                let mut state = self.state.lock();
                for (k, &(page, phys, within, n)) in pinned[..count].iter().enumerate() {
                    // A release meanwhile took the object: nothing left to unpin.
                    if state.live(id) != Ok(slot) {
                        break;
                    }
                    let (pages, object) = state.split(slot);
                    if k < copied {
                        pages.write_unpin(object, page, phys, page * PAGE + (within + n) as u64);
                    } else {
                        pages.write_abandon(object, page, phys);
                    }
                }
            }
            for &(_, phys, _, _) in &pinned[..count] {
                pmm::unpin_range(phys, PAGE as usize);
            }
            // A page of the owner's buffer that is absent is waited for with
            // nothing pinned, and the rest of the chunk written again.
            let faulted = match stopped_at {
                None => None,
                Some(Stopped::Absent) => src.wait_in(from, done + chunk - from).err(),
                Some(Stopped::Faulted(faulted)) => Some(faulted),
            };
            done = from;
            if let Some(faulted) = faulted {
                return refusal(done as u64, faulted.into());
            }
            if stopped_at.is_some() {
                continue;
            }
            match stopped {
                None => {}
                Some(Refused::InvalidArgument) => return refusal(done as u64, SyscallError::NotFound),
                Some(why) => return refusal(done as u64, word(why)),
            }
        }
        done as u64
    }

    pub fn withdraw(&self, id: u64, first: u64, count: u64) -> u64 {
        match self.removing(id, |pages, object, waiters| pages.withdraw(object, first, count, waiters)) {
            Ok((n, news)) => {
                if news {
                    self.watch.post();
                }
                n as u64
            }
            Err(e) => e.to_u64(),
        }
    }

    pub fn fail(&self, id: u64, first: u64, count: u64) -> u64 {
        match self.removing(id, |pages, object, _| pages.fail(object, first, count)) {
            Ok(Ok(())) => 0,
            Ok(Err(why)) => word(why).to_u64(),
            Err(e) => e.to_u64(),
        }
    }

    pub fn resize(&self, id: u64, size: u64) -> u64 {
        if size > kernel::paged::LAST_END {
            return SyscallError::InvalidArgument.to_u64();
        }
        if let Err(e) = self.holding(id) {
            return e.to_u64();
        }
        let resized = self.removing(id, |pages, object, _| {
            let tail = pages.resize(object, size)?;
            if let Some(phys) = tail {
                let within = (size % PAGE) as usize;
                // SAFETY: the last page's frame, the object's while the lock is
                // held: zeroed past the size through the direct map, never
                // through a reference, as a reader may copy it meanwhile.
                unsafe { core::ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), 0, PAGE as usize - within) };
            }
            Ok(())
        });
        match resized {
            Ok(Ok(())) => 0,
            Ok(Err(why)) => word(why).to_u64(),
            Err(e) => e.to_u64(),
        }
    }

    pub fn writeback(&self, id: u64, first: u64, count: u64, end: Option<bool>, out: &mut UserBytesMut) -> u64 {
        let bits = {
            let mut state = self.state.lock();
            let slot = match state.live(id) {
                Ok(slot) => slot,
                Err(e) => return e.to_u64(),
            };
            let (pages, object) = state.split(slot);
            match end {
                None => pages.writeback_begin(object, first, count),
                Some(written) => pages.writeback_end(object, first, count, written),
            }
        };
        match bits {
            Ok(bits) => {
                let mut bytes = [0u8; 64];
                for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(bits) {
                    chunk.copy_from_slice(&word.to_le_bytes());
                }
                out.write_at(0, &bytes).map_or_else(|faulted| SyscallError::from(faulted).to_u64(), |()| 0)
            }
            Err(why) => word(why).to_u64(),
        }
    }

    /// `f` over the live object `id`, under the lock.
    fn on_live<R>(&self, id: u64, f: impl FnOnce(&mut State, u32) -> R) -> Result<R, SyscallError> {
        let mut state = self.state.lock();
        let slot = state.live(id)?;
        Ok(f(&mut state, slot))
    }

    /// Holders may turn `pages` more pages of object `id` dirty: the credit now.
    pub fn grant(&self, id: u64, pages: u64) -> Result<u64, SyscallError> {
        self.on_live(id, |state, slot| state.entry_mut(slot).object.grant(pages))
    }

    /// The credit of object `id` holders have not spent, taken back.
    pub fn take_back(&self, id: u64) -> Result<u64, SyscallError> {
        self.on_live(id, |state, slot| state.entry_mut(slot).object.take_back())
    }

    /// Object `id`'s record, where given, and modification time, where given.
    pub fn set_record(&self, id: u64, record: Option<[u8; PAGED_RECORD]>, mtime: Option<u64>) -> Result<(), SyscallError> {
        self.on_live(id, |state, slot| {
            let entry = state.entry_mut(slot);
            if let Some(record) = record {
                entry.record = record;
            }
            if let Some(mtime) = mtime {
                entry.object.stamp(mtime);
            }
        })
    }

    pub fn status(&self, id: u64) -> Result<PagedStatus, SyscallError> {
        self.on_live(id, |state, slot| {
            let object = &state.entry(slot).object;
            PagedStatus { size: object.size(), mtime: object.mtime(), credit: object.credit() }
        })
    }

    /// The pages of object `id` holders dirtied since the last call: a notice
    /// not yet taken of them is owed no more.
    pub fn take_dirtied(&self, id: u64) -> Result<abi::PagedRange, SyscallError> {
        self.on_live(id, |state, slot| {
            state.entries.unmark(DIRTIED, slot);
            let (first, count) = state.entry_mut(slot).object.take_dirtied().unwrap_or((0, 0));
            abi::PagedRange { first, count }
        })
    }

    /// What a holder of the object at `slot` reads of it, of one moment.
    fn stat(&self, slot: u32) -> Result<PagedStat, SyscallError> {
        let state = self.state.lock();
        let entry = state.entry(slot);
        if state.gone || entry.object.gone() {
            return Err(SyscallError::Gone);
        }
        let (size, mtime) = entry.object.stat();
        Ok(PagedStat { size, mtime, record: entry.record })
    }

    /// A holder's write of `src` at `at`, or at the size where `None`: whole
    /// or refused, and never waiting. Its bytes are copied into frames held
    /// aside first, with nothing else held; then, in one hold of the lock, it
    /// takes the write side, refused while anyone holds or waits for it, and
    /// is checked and admitted whole ([`Pages::holder_fits`]). Its copy into
    /// the object's frames is the kernel's own, so whoever waits for the side
    /// waits on no process's memory. The bytes written, or where `at` is
    /// `None` the size after them. The caller holds the holder, so the slot
    /// is the object's for the call.
    fn holder_write(&self, slot: u32, src: &UserBytes, at: Option<u64>) -> u64 {
        let len = src.len();
        if len == 0 {
            let state = self.state.lock();
            let object = &state.entry(slot).object;
            return match state.gone || object.gone() {
                true => SyscallError::Gone.to_u64(),
                false => at.map_or(object.size(), |_| 0),
            };
        }
        let staged_count = (len as u64).div_ceil(PAGE) as usize;
        let mut staged = [0u64; HOLDER];
        let mut fresh = [0u64; HOLDER];
        let mut drawn = [None; 2 * HOLDER];
        let Some(_writing) = HolderWriting::begin(self, slot) else {
            return SyscallError::Gone.to_u64();
        };
        // Frames for the bytes, and for the pages the write would install,
        // as the object stands now: one fewer than it needs then refuses it.
        let fresh_count = {
            let mut state = self.state.lock();
            if state.gone {
                return SyscallError::Gone.to_u64();
            }
            let (pages, object) = state.split(slot);
            let offset = at.unwrap_or(object.size());
            let span = match holder_span(offset, len as u64) {
                Ok(span) => span,
                Err(why) => return holder_word(why).to_u64(),
            };
            let fresh_count = match pages.holder_fits(object, offset / PAGE, span) {
                Ok(fresh) => fresh as usize,
                Err(why) => return holder_word(why).to_u64(),
            };
            let wanted = staged_count + fresh_count;
            let got = self.draw(&mut state, &mut drawn[..wanted]);
            if got < wanted {
                self.give_back_drawn(&mut state, &drawn[..got]);
                return SyscallError::WouldBlock.to_u64();
            }
            fresh_count
        };
        for (k, frame) in staged.iter_mut().take(staged_count).enumerate() {
            *frame = materialize(drawn[k], false);
        }
        for (k, frame) in fresh.iter_mut().take(fresh_count).enumerate() {
            *frame = materialize(drawn[staged_count + k], true);
        }
        let give_up = |e: SyscallError| {
            let mut state = self.state.lock();
            self.give_frames(&mut state, &staged[..staged_count]);
            self.give_frames(&mut state, &fresh[..fresh_count]);
            e.to_u64()
        };
        for (k, &frame) in staged[..staged_count].iter().enumerate() {
            let n = (len - k * PAGE as usize).min(PAGE as usize);
            // SAFETY: a frame this write drew, which nothing else reaches.
            if unsafe { src.read_raw(k * PAGE as usize, DirectMap::from_phys(frame).as_mut_ptr::<u8>(), n) }.is_err() {
                return give_up(SyscallError::BadAddress);
            }
        }
        let mut pinned = [(0u64, 0u64, 0usize, 0usize); HOLDER];
        let (offset, count, used) = {
            let mut state = self.state.lock();
            if state.gone {
                drop(state);
                return give_up(SyscallError::Gone);
            }
            let (pages, object) = state.split(slot);
            if !object.take_holder() {
                drop(state);
                return give_up(SyscallError::WouldBlock);
            }
            let offset = at.unwrap_or(object.size());
            let first = offset / PAGE;
            // An append's end is read here, in this hold of the lock, and
            // checked as a positioned write's is.
            let span = holder_span(offset, len as u64);
            let count = span.unwrap_or(0) as usize;
            let refused = match span.and_then(|span| pages.holder_fits(object, first, span)) {
                Ok(needed) if needed as usize <= fresh_count => None,
                // The object changed since the frames were drawn.
                Ok(_) => Some(SyscallError::WouldBlock),
                Err(why) => Some(holder_word(why)),
            };
            if let Some(e) = refused {
                object.give(Side::Holder);
                drop(state);
                self.changed.post();
                return give_up(e);
            }
            let mut used = 0;
            for (k, entry) in pinned.iter_mut().take(count).enumerate() {
                let page = first + k as u64;
                let phys = match pages.admit(object, page, Writer::Holder) {
                    Ok(phys) => phys,
                    // Checked whole above, in this hold of the lock: it reads as zeros.
                    Err(_) => {
                        let phys = fresh[used];
                        pages.admit_fresh(object, page, phys, Writer::Holder).expect("paged: a holder's checked write was refused a fresh page");
                        used += 1;
                        phys
                    }
                };
                assert!(pmm::pin_range(phys, PAGE as usize), "paged: a frame's pins overflowed");
                let from = (page * PAGE).max(offset);
                let to = ((page + 1) * PAGE).min(offset + len as u64);
                *entry = (page, phys, (from - page * PAGE) as usize, (to - from) as usize);
            }
            for _ in 0..used {
                state.held.core.installed();
            }
            (offset, count, used)
        };
        let mut from = 0;
        for &(_, phys, within, n) in &pinned[..count] {
            copy_staged(&staged[..staged_count], from, phys, within, n);
            from += n;
        }
        {
            let mut state = self.state.lock();
            let (pages, object) = state.split(slot);
            for &(page, phys, within, n) in &pinned[..count] {
                pages.write_unpin(object, page, phys, page * PAGE + (within + n) as u64);
            }
            object.give(Side::Holder);
            object.stamp(now());
            let tell = object.take_tell();
            self.give_frames(&mut state, &staged[..staged_count]);
            self.give_frames(&mut state, &fresh[used..fresh_count]);
            if tell && !state.gone && state.entry(slot).owned && state.entries.mark(DIRTIED, slot) {
                self.watch.post();
            }
        }
        for &(_, phys, _, _) in &pinned[..count] {
            pmm::unpin_range(phys, PAGE as usize);
        }
        // A read or a hold that waited for the side takes it now.
        self.changed.post();
        match at {
            Some(_) => len as u64,
            None => offset + len as u64,
        }
    }

    /// Under the lock: draw a frame for each of `out`'s entries from the
    /// core, as a supply does — one held aside, the core's promise, or a
    /// promise the allocator makes now: how many were drawn, each `Some` for
    /// a frame held aside, which [`materialize`] completes outside the lock.
    fn draw(&self, state: &mut State, out: &mut [Option<u64>]) -> usize {
        state.held.settle();
        let mut drawn = 0;
        while drawn < out.len() {
            let ready = !state.held.spare.is_empty();
            out[drawn] = match state.held.core.draw(ready) {
                Draw::Aside => state.held.spare.pop(),
                Draw::Pool => None,
                Draw::Beyond if pmm::promise(self.core.job(), 1) => {
                    state.held.core.promised();
                    None
                }
                Draw::Beyond => break,
            };
            drawn += 1;
        }
        drawn
    }

    /// Under the lock: give back the first `drawn` of what [`Self::draw`]
    /// drew and nothing used: frames held aside to the spare ones, promises
    /// to the core.
    fn give_back_drawn(&self, state: &mut State, drawn: &[Option<u64>]) {
        let job = self.core.job();
        for d in drawn {
            match *d {
                Some(phys) => {
                    let gone = state.gone;
                    state.held.give_back(job, gone, phys, false);
                }
                None => {
                    // A promise drawn is made a frame and given back, as a
                    // supply's unused frames are.
                    let phys = pmm::alloc_promised().into_raw();
                    let gone = state.gone;
                    state.held.give_back(job, gone, phys, false);
                }
            }
        }
    }

    /// Under the lock: frames drawn and materialized that never entered the
    /// index go back, as a refused supply's do.
    fn give_frames(&self, state: &mut State, frames: &[u64]) {
        let job = self.core.job();
        for &phys in frames {
            let gone = state.gone;
            state.held.give_back(job, gone, phys, false);
        }
    }
}

/// Part of one pinned frame or unwritten anonymous page a read copies.
#[derive(Clone, Copy, Default)]
struct Piece {
    phys: Option<u64>,
    within: usize,
    len: usize,
}

/// What one look at the next pages found.
enum Step {
    /// This many pieces were pinned, `last` where they reach the read's end.
    Copy { count: usize, last: bool },
    Absent(u64),
    /// A write met a holder's read.
    Written,
    Failed,
    Gone,
    Done,
}

/// [`HOLDER_PAGES`] as an array length.
const HOLDER: usize = HOLDER_PAGES as usize;

/// A frame [`PagerObject::draw`] drew: one held aside, zeroed where `zero`, or
/// one the allocator gives on the promise drawn, zeroed already.
fn materialize(drawn: Option<u64>, zero: bool) -> u64 {
    match drawn {
        Some(phys) => {
            if zero {
                // SAFETY: a frame held aside, out of the index and unpinned,
                // which nothing else reaches.
                unsafe { core::ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>(), 0, PAGE as usize) };
            }
            phys
        }
        None => pmm::alloc_promised().into_raw(),
    }
}

/// Copy `n` bytes of `staged`, frames read as one run of bytes, from its byte
/// `at` into `frame` from its byte `within`, never through a reference:
/// another CPU may read the frame meanwhile.
fn copy_staged(staged: &[u64], at: usize, frame: u64, within: usize, n: usize) {
    let mut done = 0;
    while done < n {
        let (k, inside) = ((at + done) / PAGE as usize, (at + done) % PAGE as usize);
        let take = (PAGE as usize - inside).min(n - done);
        // SAFETY: the staged frame is the caller's and the object's frame is
        // pinned by it for the copy; the ranges lie inside them, and they are
        // distinct frames.
        unsafe {
            core::ptr::copy_nonoverlapping(
                DirectMap::from_phys(staged[k]).as_ptr::<u8>().add(inside),
                DirectMap::from_phys(frame).as_mut_ptr::<u8>().add(within + done),
                take,
            );
        }
        done += take;
    }
}

/// A refusal after `done` of the work, answered as the work where there was any.
fn refusal(done: u64, e: SyscallError) -> u64 {
    if done > 0 { done } else { e.to_u64() }
}

/// What a write is stamped with: nanoseconds since the Unix epoch, as the
/// file servers stamp their volumes, or 0 where the machine has no time.
fn now() -> u64 {
    crate::clock::utc_nanos().unwrap_or(0)
}

fn word(why: Refused) -> SyscallError {
    match why {
        Refused::Gone => SyscallError::Gone,
        Refused::PastEnd | Refused::InvalidArgument => SyscallError::InvalidArgument,
        Refused::Absent => SyscallError::NotFound,
        Refused::OutOfReach => SyscallError::InvalidArgument,
        Refused::NoRoom => SyscallError::ResourceExhausted,
        Refused::DirtyFull | Refused::NoCredit => SyscallError::WouldBlock,
        Refused::Sealed => SyscallError::TextBusy,
    }
}

/// A holder's write refused: whatever the pager can do instead of it —
/// supply a page, grant credit, write back or make room — sends the holder
/// to the pager.
fn holder_word(why: Refused) -> SyscallError {
    match why {
        Refused::Gone => SyscallError::Gone,
        Refused::Sealed => SyscallError::TextBusy,
        Refused::Absent | Refused::NoCredit | Refused::DirtyFull | Refused::NoRoom | Refused::OutOfReach => {
            SyscallError::WouldBlock
        }
        Refused::PastEnd | Refused::InvalidArgument => SyscallError::InvalidArgument,
    }
}

/// Every request on the list, as raw nodes.
fn requests(state: &State) -> impl Iterator<Item = *mut Request> + '_ {
    let mut at = state.requests;
    core::iter::from_fn(move || {
        if at.is_null() {
            return None;
        }
        let here = at;
        // SAFETY: linked, so live, under the lock the caller holds.
        at = unsafe { (*here).next };
        Some(here)
    })
}

/// A reader's request, in its own frame for as long as it reads, and on its
/// pager's list from its first absent page until it returns by any path, a
/// cancel included. Its node is reached only through raw pointers, under the
/// pager's lock: by its reader, and by [`PagerObject::notices`].
struct Waiting<'a> {
    pager: &'a PagerObject,
    node: UnsafeCell<Request>,
    linked: Cell<bool>,
    _pinned: PhantomPinned,
}

impl<'a> Waiting<'a> {
    fn new(pager: &'a PagerObject, slot: u32) -> Self {
        let node = Request { slot, want: Want::NONE, prev: ptr::null_mut(), next: ptr::null_mut() };
        Self { pager, node: UnsafeCell::new(node), linked: Cell::new(false), _pinned: PhantomPinned }
    }

    /// Under the pager's lock, `state`: want `count` pages from `first`, on the
    /// list once, and named again in a notice where it is news (`Want::ask`).
    fn request(self: Pin<&Self>, state: &mut State, first: u64, count: u64) {
        let at = self.node.get();
        // SAFETY: the node is this frame's and pinned; under the lock, nothing
        // else reaches it, and no reference to it is formed.
        unsafe {
            let mut want = (*at).want;
            if !want.ask(first, count) {
                return;
            }
            (*at).want = want;
            if !self.linked.get() {
                (*at).prev = ptr::null_mut();
                (*at).next = state.requests;
                if !state.requests.is_null() {
                    (*state.requests).prev = at;
                }
                state.requests = at;
                self.linked.set(true);
            }
        }
        self.pager.watch.post();
    }

    /// Under the pager's lock, `_state`: the read begins again, and asks
    /// anew (`Want::anew`).
    fn anew(self: Pin<&Self>, _state: &mut State) {
        // SAFETY: as in `request`.
        unsafe { (*self.node.get()).want.anew() }
    }

    /// Under the pager's lock, `_state`: the range last requested.
    fn range(&self, _state: &State) -> (u64, u64) {
        // SAFETY: as in `request`.
        unsafe { (*self.node.get()).want.range() }
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        if !self.linked.get() {
            return;
        }
        let mut state = self.pager.state.lock();
        let at = self.node.get();
        // SAFETY: linked, so live in this frame; its neighbours are linked,
        // under the lock.
        unsafe {
            if (*at).prev.is_null() {
                state.requests = (*at).next;
            } else {
                (*(*at).prev).next = (*at).next;
            }
            if !(*at).next.is_null() {
                (*(*at).next).prev = (*at).prev;
            }
        }
    }
}

/// The requests linked on a pager's list for the object at `slot`, under the
/// pager's lock for as long as it lives.
struct Linked {
    head: *mut Request,
    slot: u32,
}

impl Waiters for Linked {
    fn each(&mut self, f: &mut dyn FnMut(&mut Want)) {
        let mut at = self.head;
        while !at.is_null() {
            // SAFETY: linked, so live, under the lock the holder of `self`
            // keeps; its waiter writes it only under the lock too, and
            // nothing forms a reference to it.
            unsafe {
                if (*at).slot == self.slot {
                    let mut want = (*at).want;
                    f(&mut want);
                    (*at).want = want;
                }
                at = (*at).next;
            }
        }
    }
}

/// A reader's wait past its pager's report time, counted until it ends.
struct Overdue<'a> {
    pager: &'a PagerObject,
}

impl<'a> Overdue<'a> {
    fn begin(pager: &'a PagerObject, object: u32, first: u64, count: u64, began: crate::time::Instant) -> Self {
        if pager.overdue.fetch_add(1, Ordering::Relaxed) == 0 {
            log!(
                "paged: pid {}'s pager has a wait overdue: object {object} pages {first}+{count}, waiting {}",
                pager.pid,
                crate::clock::now() - began
            );
        }
        Self { pager }
    }
}

impl Drop for Overdue<'_> {
    fn drop(&mut self) {
        if self.pager.overdue.fetch_sub(1, Ordering::Relaxed) == 1 {
            log!("paged: pid {}'s pager has no wait overdue", self.pager.pid);
        }
    }
}

/// The pager's process ended: every object answers `Gone`, every waiter is
/// woken, and every frame goes back.
impl ZeroHandles for PagerObject {
    fn on_zero_handles(&self) {
        let mut state = self.state.lock();
        state.gone = true;
        for slot in 0..state.entries.len() as u32 {
            if state.entries.get(slot).is_some_and(|e| e.owned) {
                state = self.let_go(state, slot);
            }
        }
        let job = self.core.job();
        while let Some(phys) = state.held.spare.pop().or_else(|| state.held.draining.pop()) {
            // SAFETY: a frame this pager drew and held aside, which nothing
            // maps; a pin defers its free to its last unpin.
            unsafe { pmm::free_drawn(job, &[phys]) };
        }
        let pool = state.held.core.end();
        pmm::unpromise(job, pool);
        drop(state);
        self.changed.post();
    }
}

impl Drop for PagerObject {
    fn drop(&mut self) {
        let state = self.state.lock();
        assert!(state.requests.is_null(), "paged: a pager went with a reader still linked");
        assert!(state.held.spare.is_empty() && state.held.draining.is_empty(), "paged: a pager went holding frames aside");
        pmm::unpromise(self.core.job(), state.held.core.pool());
    }
}

/// One space's mappings of one paged object, on the object's list from its
/// space's first range of the object until its last, which unlinks it only
/// once that range's leaves are cleared and shot down: the space drops a
/// removed range after its shootdown. It holds a reference on the object's
/// slot, so a mapped object outlives its release, and is charged to the
/// mapper's job.
pub struct MapNode {
    pager: Arc<PagerObject>,
    slot: u32,
    space: Weak<Space>,
    /// The ranges holding it ([`PagedRange`]).
    ranges: Ranges,
    /// Under the pager's lock.
    link: UnsafeCell<Link>,
    /// Itself, for a walk to hold while it visits.
    this: Weak<MapNode>,
    _charge: Charge,
}

struct Link {
    /// The epoch of the walk that last visited it, or the one current at
    /// its linking ([`kernel::paged::Mappers`]).
    seen: u64,
    prev: *const MapNode,
    next: *const MapNode,
}

// SAFETY: `link` is reached only under its pager's lock, and the rest is
// immutable or atomic.
unsafe impl Send for MapNode {}
// SAFETY: as for `Send`.
unsafe impl Sync for MapNode {}

/// Why the index holds no frame a fault may map for a page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Missing {
    /// Absent, or revoking, at this version of the object: the fault waits.
    Absent(Version),
    /// A hole of an anonymous object, which only [`Index::fault`] fills.
    Hole,
    /// Its pager cannot let a mapping have it.
    Lost(Lost),
    /// Wholly past the object's end: nothing is there.
    PastEnd(Lost),
}

/// A page a fault was refused, as the record of its end names it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Lost {
    pub pager: Pid,
    pub object: u32,
    pub page: u64,
    pub why: Why,
}

/// Why a pager cannot let a mapping have a page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// The pager failed it.
    Failed,
    /// The pager released the object.
    Released,
    /// The pager ended.
    Ended,
    /// A hole of an anonymous object, and the pager has no room for its page.
    NoRoom,
    /// The page lies wholly past the object's end: nothing is there, so a
    /// touch of it ends the process and a copy from it is refused as a bad
    /// address.
    PastEnd,
}

impl Why {
    pub fn name(self) -> &'static str {
        match self {
            Why::Failed => "failed by its pager",
            Why::Released => "released by its pager",
            Why::Ended => "its pager ended",
            Why::NoRoom => "no room in its pager",
            Why::PastEnd => "past the object's end",
        }
    }
}

/// A mapped object's pages as its index holds them, under the pager's lock.
pub struct Index<'a> {
    pager: &'a PagerObject,
    state: &'a mut State,
    slot: u32,
}

impl Index<'_> {
    /// The frame present for the object's `page`, or why there is none.
    pub fn page(&self, page: u64) -> Result<u64, Missing> {
        let object = &self.state.entry(self.slot).object;
        match self.state.pages.lookup(object, page) {
            _ if self.state.gone => Err(self.lost(page, Why::Ended)),
            Lookup::Present(phys) => Ok(phys),
            Lookup::Absent => Err(Missing::Absent(object.version())),
            Lookup::PastEnd => Err(Missing::PastEnd(self.record(page, Why::PastEnd))),
            Lookup::Zero => Err(Missing::Hole),
            Lookup::Failed => Err(self.lost(page, Why::Failed)),
            Lookup::Gone => Err(self.lost(page, Why::Released)),
        }
    }

    /// As [`Self::page`], for the page a fault touched: a hole of an
    /// anonymous object, a MEMORY volume's, gets a zeroed frame of its
    /// pager's, installed clean and charged as a supply's is, and is refused
    /// where the pager has no room for it (7b's N4).
    pub fn fault(&mut self, page: u64) -> Result<u64, Missing> {
        match self.page(page) {
            Err(Missing::Hole) => self.zeroed(page),
            found => found,
        }
    }

    fn zeroed(&mut self, page: u64) -> Result<u64, Missing> {
        let no_room = self.lost(page, Why::NoRoom);
        let job = self.pager.core.job();
        let State { pages, entries, held, gone, .. } = &mut *self.state;
        held.settle();
        let phys = match held.core.draw(!held.spare.is_empty()) {
            Draw::Aside => {
                let phys = held.spare.pop().expect("paged: a frame drawn aside is held aside");
                // SAFETY: a frame held aside, out of the index and unpinned,
                // which nothing else reaches.
                unsafe { ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>(), 0, PAGE as usize) };
                phys
            }
            Draw::Pool => pmm::alloc_promised().into_raw(),
            Draw::Beyond if pmm::promise(job, 1) => {
                held.core.promised();
                pmm::alloc_promised().into_raw()
            }
            Draw::Beyond => return Err(no_room),
        };
        let object = &mut entries.get_mut(self.slot).expect("paged: a mapped slot is held").object;
        match pages.supply(object, page, phys, false, (page + 1) * PAGE) {
            Ok(Supplied::Installed) => {
                held.core.installed();
                Ok(phys)
            }
            Ok(Supplied::Present) => unreachable!("paged: a hole found under the lock held a page"),
            // Inside the object and the pager alive, as the lookup found it:
            // only a full table refuses.
            Err(_) => {
                held.give_back(job, *gone, phys, false);
                Err(no_room)
            }
        }
    }

    fn lost(&self, page: u64, why: Why) -> Missing {
        Missing::Lost(self.record(page, why))
    }

    fn record(&self, page: u64, why: Why) -> Lost {
        Lost { pager: self.pager.pid, object: self.slot, page, why }
    }
}

/// The revoking pages a walk reads under its pager's lock at once.
/// `tests/guest/mapprobe`'s `shrunk` case revokes 79 pages so that its
/// revocation takes two batches; at 79 or more it would take one, and that
/// case would no longer reach a second.
const REVOKE_BATCH: usize = 64;

/// The pages a fault's request names: the aligned run around its page.
const FAULT_RUN: u64 = kernel::vm::AROUND / PAGE;

impl MapNode {
    /// `f` over the object's pages, under the pager's lock.
    pub fn with_index<R>(&self, f: impl FnOnce(&mut Index<'_>) -> R) -> R {
        let mut state = self.pager.state.lock();
        f(&mut Index { pager: &self.pager, state: &mut state, slot: self.slot })
    }

    /// A fault's wait for the object's `page`, found absent at `since`, its
    /// space's lock not held: the page's aligned run is requested in a fresh
    /// request, so a fault whose last wait a write or resize ended asks
    /// afresh, and the wait lasts until the page is no longer absent, the
    /// version moved, or the pager is gone. Reported overdue past the pager's
    /// report time, as a holder's read is, and never ended by a timeout; a
    /// file's page behaves as memory does (7b's D6). `false` where a kill
    /// ended it.
    pub fn wait(&self, page: u64, since: Version) -> bool {
        let (pager, slot) = (&*self.pager, self.slot);
        let over = |state: &State| state.gone || state.pages.wait_over(&state.entry(slot).object, page, since);
        let waiting = core::pin::pin!(Waiting::new(pager, slot));
        let (first, count) = {
            let mut state = pager.state.lock();
            if over(&state) {
                return true;
            }
            // Absent, so inside the object.
            let end = state.entry(slot).object.size().div_ceil(PAGE);
            let first = page - page % FAULT_RUN;
            let count = (first + FAULT_RUN).min(end) - first;
            waiting.as_ref().request(&mut state, first, count);
            (first, count)
        };
        let ready = || over(&pager.state.lock());
        let parkable = crate::scheduler::Parkable::at_entry();
        let began = crate::clock::now();
        if watch::wait_until(&parkable, &pager.changed, 0, WaitClass::Io, Deadline::at(began + pager.report), ready).is_err() {
            return false;
        }
        if !ready() {
            let _overdue = Overdue::begin(pager, slot, first, count, began);
            if watch::wait_until(&parkable, &pager.changed, 0, WaitClass::Io, Deadline::never(), ready).is_err() {
                return false;
            }
        }
        true
    }
}

/// A range's hold on its space's [`MapNode`]: a clone is another range of
/// the same object in the space, and the last one dropped unlinks the node.
pub struct PagedRange(Arc<MapNode>);

impl PagedRange {
    pub fn node(&self) -> &MapNode {
        &self.0
    }
}

impl Clone for PagedRange {
    fn clone(&self) -> Self {
        self.0.ranges.more();
        Self(Arc::clone(&self.0))
    }
}

impl Drop for PagedRange {
    fn drop(&mut self) {
        if self.0.ranges.less() {
            self.0.pager.unlink(&self.0);
        }
    }
}

impl PagerObject {
    /// The first range of object `slot` in `space`, whose memory is counted
    /// to `job`: its node linked, seen at the object's epoch now, under
    /// the space's lock the caller holds. `Gone` where the object or its
    /// pager is.
    pub fn link(self: &Arc<Self>, slot: u32, space: Weak<Space>, job: &Arc<kernel::sched::job::Job>) -> Result<PagedRange, SyscallError> {
        let charge = Charge::new(job, charge::arc::<MapNode>()).map_err(|_| SyscallError::ResourceExhausted)?;
        let link = UnsafeCell::new(Link { seen: 0, prev: ptr::null(), next: ptr::null() });
        let node = Arc::new_cyclic(|this| MapNode {
            pager: Arc::clone(self),
            slot,
            space,
            ranges: Ranges::one(),
            link,
            this: this.clone(),
            _charge: charge,
        });
        let mut state = self.state.lock();
        let entry = state.entry_mut(slot);
        if entry.object.gone() {
            return Err(SyscallError::Gone);
        }
        let at = Arc::as_ptr(&node);
        // SAFETY: a fresh node no list holds yet, and the tail's, linked,
        // under the lock.
        unsafe {
            *(*at).link.get() = Link { seen: entry.object.link(), prev: entry.mappers.tail, next: ptr::null() };
            match entry.mappers.tail.is_null() {
                true => entry.mappers.head = at,
                false => (*(*entry.mappers.tail).link.get()).next = at,
            }
        }
        entry.mappers.tail = at;
        // A mapping holds the object as a handle does: a file mapped and
        // closed, or unlinked, stays until the last mapping goes.
        entry.refs += 1;
        entry.holders += 1;
        Ok(PagedRange(node))
    }

    /// The last range of `node`'s space went, its leaves cleared and shot
    /// down: the node leaves the list, and its hold and reference on the
    /// slot go.
    fn unlink(&self, node: &MapNode) {
        let mut state = self.state.lock();
        let entry = state.entry_mut(node.slot);
        // SAFETY: linked, as are its neighbours, under the lock.
        unsafe {
            let Link { prev, next, .. } = *node.link.get();
            match prev.is_null() {
                true => entry.mappers.head = next,
                false => (*(*prev).link.get()).next = next,
            }
            match next.is_null() {
                true => entry.mappers.tail = prev,
                false => (*(*next).link.get()).prev = prev,
            }
        }
        state.entry_mut(node.slot).holders -= 1;
        self.say_no_holders(&mut state, node.slot);
        self.unref_locked(&mut state, node.slot);
    }
}

/// An object's list of mappers, as its walk visits them, under the pager's
/// lock.
struct Ring<'a>(&'a mut Ends);

impl Mappers for Ring<'_> {
    type Node = *const MapNode;

    fn head(&self) -> Option<*const MapNode> {
        (!self.0.head.is_null()).then_some(self.0.head)
    }

    fn seen(&self, node: *const MapNode) -> u64 {
        // SAFETY: linked, so live, under the lock.
        unsafe { (*(*node).link.get()).seen }
    }

    fn visited(&mut self, node: *const MapNode, epoch: u64) {
        // SAFETY: linked, as are its neighbours, under the lock; it moves to
        // the tail.
        unsafe {
            let link = &mut *(*node).link.get();
            link.seen = epoch;
            if self.0.tail == node {
                return;
            }
            match link.prev.is_null() {
                true => self.0.head = link.next,
                false => (*(*link.prev).link.get()).next = link.next,
            }
            (*(*link.next).link.get()).prev = link.prev;
            (*(*self.0.tail).link.get()).next = node;
            link.prev = self.0.tail;
            link.next = ptr::null();
            self.0.tail = node;
        }
    }
}

/// A holder's write under way on its pager's object at `slot`, counted
/// until it ends by any path.
struct HolderWriting<'a> {
    pager: &'a PagerObject,
    slot: u32,
}

impl<'a> HolderWriting<'a> {
    /// `None` once the object's last holder went: its pager may have been
    /// told so, and a write now would dirty what it was told no one holds.
    fn begin(pager: &'a PagerObject, slot: u32) -> Option<Self> {
        let mut state = pager.state.lock();
        let entry = state.entry_mut(slot);
        if entry.holders == 0 {
            return None;
        }
        entry.writers += 1;
        Some(Self { pager, slot })
    }
}

impl Drop for HolderWriting<'_> {
    fn drop(&mut self) {
        let mut state = self.pager.state.lock();
        state.entry_mut(self.slot).writers -= 1;
        self.pager.say_no_holders(&mut state, self.slot);
    }
}

/// One call's reference to its pager's object.
struct Ref<'a> {
    pager: &'a PagerObject,
    slot: u32,
}

impl Drop for Ref<'_> {
    fn drop(&mut self) {
        self.pager.unref(self.slot);
    }
}

/// A holder's reference to its pager's object.
struct Named {
    pager: Arc<PagerObject>,
    slot: u32,
}

impl Drop for Named {
    fn drop(&mut self) {
        self.pager.unref(self.slot);
    }
}

pub struct PagedHolderObject {
    pub(super) core: ObjectCore,
    named: Named,
    /// Minted to write: its first handle carries `WRITE`.
    writes: bool,
}

impl PagedHolderObject {
    const READS: toyos_abi::handle::Rights =
        toyos_abi::handle::Rights::READ.union(toyos_abi::handle::Rights::DUP).union(toyos_abi::handle::Rights::TRANSFER);

    /// What its first handle carries.
    pub fn rights(&self) -> toyos_abi::handle::Rights {
        match self.writes {
            true => Self::READS.union(toyos_abi::handle::Rights::WRITE),
            false => Self::READS,
        }
    }

    /// Its handle is installed: it counts as a holder until its last goes.
    pub fn held(&self) {
        self.named.pager.state.lock().entry_mut(self.named.slot).holders += 1;
    }

    pub fn read(&self, out: &mut UserBytesMut, offset: u64) -> u64 {
        self.named.pager.read(self.named.slot, out, offset, true)
    }

    /// Whether `range` maps this holder's object.
    pub fn maps(&self, range: &PagedRange) -> bool {
        ptr::eq(Arc::as_ptr(&range.0.pager), Arc::as_ptr(&self.named.pager)) && range.0.slot == self.named.slot
    }

    /// The first range of its object in `space`, whose memory is counted to
    /// `job` ([`PagerObject::link`]).
    pub fn link(&self, space: Weak<Space>, job: &Arc<kernel::sched::job::Job>) -> Result<PagedRange, SyscallError> {
        self.named.pager.link(self.named.slot, space, job)
    }

    /// A write of `src` at `at`, or at the size where `None`.
    pub fn write(&self, src: &UserBytes, at: Option<u64>) -> u64 {
        self.named.pager.holder_write(self.named.slot, src, at)
    }

    pub fn stat(&self) -> Result<PagedStat, SyscallError> {
        self.named.pager.stat(self.named.slot)
    }
}

impl ZeroHandles for PagedHolderObject {
    fn on_zero_handles(&self) {
        self.named.pager.holder_gone(self.named.slot);
    }
}
