//! Paged objects (memory design §7, stage 7a): a pager process's memory,
//! one object per file node, which holders read only through
//! `SYS_PAGED_READ`. Nothing here is mapped.
//!
//! **Two kinds.** A [`PagerObject`] is its process's one pager: its index of
//! `capacity` pages ([`kernel::paged::Pages`]), its objects by id, its
//! readers' requests and its notices. Every owner call names an object by id
//! through it, so a pager holds one handle however many objects it serves. It
//! neither duplicates nor transfers, so its last handle goes with its
//! process, and that ends every object it serves: each reader answers `Gone`.
//! A [`PagedHolderObject`] reads one object, and may be narrowed and sent.
//!
//! **Supply copies into fresh frames**, so no frame the pager still maps
//! enters an object. A frame leaves at withdrawal, failure, shrink or the
//! object's release: out of the index under the pager's lock, then freed,
//! which a reader's pin defers to its last unpin.
//!
//! **A reader waits only on its pager.** An absent page is requested in the
//! waiter's own record, linked on its pager's list, and the wait ends on a
//! supply, a failure, a write or resize of the object (which the read answers
//! `WouldBlock`), a release, the pager's end or a kill; a wait past the
//! pager's report time is counted and logged overdue and goes on.
//! A pager's own process is refused a holder's read, so no pager waits on a
//! pager.
//!
//! **A holder's read is atomic against its pager's writes**
//! ([`kernel::paged::Object::read_begin`]): one that meets a write is answered
//! `WouldBlock`, and the holder asks the pager, whose own reads its writes
//! order.
//!
//! **Lock order.** The pager's lock is innermost but for the allocator's. A
//! copy to or from user memory holds pins, never the lock; no wait holds a
//! pin.
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
use kernel::paged::{Core, Draw, Leave, Lookup, Mappers, Object, Pages, Refused, Revoke, Revoked, Supplied, Version, Waiters, Want, PAGE};
use kernel::sched::task::WaitClass;
use kernel::slots::Slots;
use toyos_abi::syscall::{PagedNotice, SyscallError, NOTICE_NO_HOLDERS, NOTICE_REQUESTED};

use crate::mm::charge::{self, Charge};
use crate::mm::pmm;
use crate::mm::space::Space;
use crate::process::Pid;
use crate::sync::{Lock, LockGuard};
use crate::time::{Deadline, Duration};
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::{self, Watch};
use crate::DirectMap;

use super::{KObjectVariant, ObjectCore, ZeroHandles};

// Two pieces of a table and their allocator headers share one superframe.
const _: () = assert!(2 * kernel::chunked::CHUNK_BYTES < crate::mm::MAX_HEAP_ALLOC);

// A piece of slots is one allocation the heap takes.
const _: () = assert!(Slots::<Entry>::PIECE_BYTES <= kernel::chunked::CHUNK_BYTES);

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
    /// The pager's own reference, while unreleased, each holder object's, and
    /// each owner read's, while it runs: the slot is free again with the last.
    refs: u32,
    /// Holder objects whose handles have not all gone.
    holders: u32,
    /// Not released: owner calls reach it.
    owned: bool,
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
    /// Every object, at its id's slot, marked while its last holder has gone
    /// and no notice has said so.
    entries: Slots<Entry>,
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

    /// Whether a notice is owed: a request no notice has named, or a last
    /// holder gone.
    pub fn has_notices(&self) -> bool {
        let state = self.state.lock();
        // SAFETY: every node on the list is live while linked, under this lock.
        let unreported = requests(&state).any(|r| unsafe { (*r).want.unreported() });
        unreported || state.entries.any_marked()
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
            Entry { object, refs: 1, holders: 0, owned: true, mappers }
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
    /// mapping reaches them: this walks the object's mappers, the lock dropped
    /// for each space's visit and the slot held meanwhile, gives each frame
    /// back, and names again every waiter's want a page that ends absent meets
    /// (`Pages::walk`). Answers the lock and, where another's walk runs and
    /// takes the pages in its next pass, that pass's epoch.
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
            let next = pages.walk(object, &mut Ring(mappers), &mut waiters, &mut |phys, _, becomes| {
                held.give_back(job, *gone, phys, true);
                absent |= becomes == Revoked::Absent;
            });
            let Some(node) = next else { break };
            // SAFETY: linked, so live, under the lock; a range holds it, so
            // it upgrades, and the count kept holds it past its unlinking
            // while the lock is dropped.
            let node = unsafe { (*node).this.upgrade() }.expect("paged: a linked mapper is held by a range");
            drop(state);
            if let Some(space) = node.space.upgrade() {
                space.lock().revoke_paged(&node);
            }
            state = self.state.lock();
        }
        self.unref_locked(&mut state, slot);
        if absent {
            self.watch.post();
        }
        (state, None)
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
        entries.unmark(slot);
        // A walk already running takes the pages; nothing here waits.
        let (mut state, _) = self.revoked(state, slot);
        self.unref_locked(&mut state, slot);
        state
    }

    /// A holder of object `id`, which counts as one once its handle is
    /// installed ([`PagedHolderObject::held`]).
    pub fn holder(self: &Arc<Self>, id: u64) -> Result<Arc<PagedHolderObject>, SyscallError> {
        let core = PagedHolderObject::new_core(0)?;
        let mut state = self.state.lock();
        let slot = state.live(id)?;
        state.entry_mut(slot).refs += 1;
        drop(state);
        Ok(Arc::new(PagedHolderObject { core, named: Named { pager: Arc::clone(self), slot } }))
    }

    /// A holder object's handles went.
    fn holder_gone(&self, slot: u32) {
        let mut state = self.state.lock();
        let gone = state.gone;
        let entry = state.entry_mut(slot);
        entry.holders -= 1;
        if entry.holders == 0 && entry.owned && !gone && state.entries.mark(slot) {
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
                    let Some(slot) = state.entries.take_marked() else { break };
                    // Said only while still so: a holder made since is held.
                    if state.entry(slot).holders == 0 {
                        batch[n] = PagedNotice { object: u64::from(slot), kind: NOTICE_NO_HOLDERS, first: 0, count: 0 };
                        n += 1;
                    }
                }
                n
            };
            // SAFETY: a `PagedNotice` is `UserSafe`: every byte of it is a
            // field, and the batch outlives the copy.
            if unsafe { out.write_raw(filled * size, batch.as_ptr().cast::<u8>(), n * size) }.is_err() {
                // The notices taken are lost with the window that could not hold them.
                return if filled > 0 { filled as u64 } else { SyscallError::BadAddress.to_u64() };
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
    /// waits for absent pages, an owner is answered `WouldBlock`. The caller
    /// holds a reference on `slot` for the call.
    fn read(&self, slot: u32, out: &mut UserBytesMut, offset: u64, waits: bool) -> u64 {
        let want = out.len();
        let mut done = 0usize;
        let waiting = core::pin::pin!(Waiting::new(self, slot));
        let mut pieces = [Piece::default(); CHUNK];
        // A holder's: what its bytes count against.
        let mut since: Option<Version> = None;
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
                    let mut faulted = false;
                    for piece in &pieces[..count] {
                        let copied = match piece.phys {
                            // SAFETY: `phys` is pinned by `pin_chunk`, and
                            // `within + len` lies inside its frame.
                            Some(phys) => unsafe { out.write_raw(done, DirectMap::from_phys(phys).as_ptr::<u8>().add(piece.within), piece.len) },
                            None => out.fill_zero(done, piece.len),
                        };
                        if copied.is_err() {
                            faulted = true;
                            break;
                        }
                        done += piece.len;
                    }
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
                    if written {
                        return SyscallError::WouldBlock.to_u64();
                    }
                    // After the checks: bytes copied before the fault count only
                    // as every copied chunk's do.
                    if faulted {
                        return if done > 0 { done as u64 } else { SyscallError::BadAddress.to_u64() };
                    }
                    if last {
                        return done as u64;
                    }
                }
                Step::Written => return SyscallError::WouldBlock.to_u64(),
                Step::Done => return done as u64,
                Step::Gone if done > 0 => return done as u64,
                Step::Gone => return SyscallError::Gone.to_u64(),
                Step::Failed if done > 0 => return done as u64,
                Step::Failed => return SyscallError::Io.to_u64(),
                Step::Absent(_) if !waits && done > 0 => return done as u64,
                Step::Absent(_) if !waits => return SyscallError::WouldBlock.to_u64(),
                Step::Absent(page) => {
                    let since = since.expect("paged: a holder's read waits only once it began");
                    let ready = || {
                        let state = self.state.lock();
                        state.gone || state.pages.wait_over(&state.entry(slot).object, page, since)
                    };
                    let parkable = crate::scheduler::Parkable::at_entry();
                    let began = crate::clock::now();
                    // Never read: a killed thread exits before it reaches its process again.
                    let report = Deadline::at(began + self.report);
                    if watch::wait_until(&parkable, &self.changed, 0, WaitClass::Io, report, ready).is_err() {
                        return SyscallError::Gone.to_u64();
                    }
                    if !ready() {
                        // This wait, and no other, is overdue until it ends.
                        let (first, count) = waiting.range(&self.state.lock());
                        let _overdue = Overdue::begin(self, slot, first, count, began);
                        if watch::wait_until(&parkable, &self.changed, 0, WaitClass::Io, Deadline::never(), ready).is_err() {
                            return SyscallError::Gone.to_u64();
                        }
                    }
                }
            }
        }
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

    /// A write call on object `id` begins: its slot is held by a reference
    /// until [`Self::write_end`], so no release
    /// can hand it to another object.
    fn write_begin(&self, id: u64) -> Result<u32, SyscallError> {
        let mut state = self.state.lock();
        let slot = state.live(id)?;
        let entry = state.entry_mut(slot);
        entry.refs += 1;
        entry.object.write_begin();
        Ok(slot)
    }

    /// The write call [`Self::write_begin`] began ends, `whole` where it took
    /// everything it was given: its reference goes.
    fn write_end(&self, slot: u32, whole: bool) {
        {
            let mut state = self.state.lock();
            state.entry_mut(slot).object.write_end(whole);
            self.unref_locked(&mut state, slot);
        }
        // The version moved, which ends a holder's wait.
        self.changed.post();
    }

    /// The pager ends the write of object `id` a short call left under way.
    pub fn end_write(&self, id: u64) -> u64 {
        let mut state = self.state.lock();
        let ended = state.live(id).and_then(|slot| state.entry_mut(slot).object.end_write().map_err(word));
        drop(state);
        match ended {
            Ok(()) => {
                // As at `write_end`.
                self.changed.post();
                0
            }
            Err(e) => e.to_u64(),
        }
    }

    /// Copy `src` into fresh pages of object `id` from page-aligned `offset`,
    /// clean or dirty: the pages installed. A dirty supply is a write and
    /// stops at the first present or refused page. The span is checked.
    pub fn supply(&self, id: u64, offset: u64, src: &UserBytes, dirty: bool) -> u64 {
        if !offset.is_multiple_of(PAGE) || src.len() == 0 {
            return SyscallError::InvalidArgument.to_u64();
        }
        if !dirty {
            return self.supply_pages(id, offset, src, false);
        }
        let slot = match self.write_begin(id) {
            Ok(slot) => slot,
            Err(e) => return e.to_u64(),
        };
        let answer = self.supply_pages(id, offset, src, true);
        self.write_end(slot, answer == (src.len() as u64).div_ceil(PAGE));
        answer
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
                state.held.settle();
                let mut reserved = 0;
                while reserved < pages {
                    let ready = !state.held.spare.is_empty();
                    match state.held.core.draw(ready) {
                        Draw::Aside => aside[reserved] = state.held.spare.pop(),
                        Draw::Pool => {}
                        Draw::Beyond if pmm::promise(self.core.job(), 1) => state.held.core.promised(),
                        Draw::Beyond => break,
                    }
                    reserved += 1;
                }
                (slot, reserved, state.entry(slot).object.size())
            };
            if reserved == 0 {
                return refusal(installed, SyscallError::ResourceExhausted);
            }
            let mut frames = [0u64; CHUNK];
            let mut ends = [0u64; CHUNK];
            // The frames filled from `src`, in order: a fault stops the fill.
            let mut filled = 0;
            for k in 0..reserved {
                let page_at = (first + k as u64) * PAGE;
                let from = done + k * PAGE as usize;
                let mut n = (src.len() - from).min(PAGE as usize);
                // A clean page is the file's, whose bytes past its size are zeros.
                if !dirty {
                    n = n.min(size.saturating_sub(page_at) as usize);
                }
                let phys = match aside[k] {
                    Some(phys) => {
                        // SAFETY: a frame held aside, out of the index and
                        // unpinned, which nothing else reaches.
                        unsafe { core::ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>(), 0, PAGE as usize) };
                        phys
                    }
                    None => pmm::alloc_promised().into_raw(),
                };
                frames[k] = phys;
                ends[k] = page_at + (src.len() - from).min(PAGE as usize) as u64;
                // SAFETY: a frame just drawn, which nothing else reaches yet.
                if filled == k && unsafe { src.read_raw(from, DirectMap::from_phys(phys).as_mut_ptr::<u8>(), n) }.is_ok() {
                    filled += 1;
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
            if filled < reserved && !stopped {
                refused.get_or_insert(SyscallError::BadAddress);
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
    /// answering the bytes written, up to the first absent page.
    /// The caller checked the span is in reach.
    pub fn write(&self, id: u64, offset: u64, src: &UserBytes) -> u64 {
        let slot = match self.write_begin(id) {
            Ok(slot) => slot,
            Err(e) => return e.to_u64(),
        };
        let answer = self.write_pages(id, offset, src);
        self.write_end(slot, answer == src.len() as u64);
        answer
    }

    fn write_pages(&self, id: u64, offset: u64, src: &UserBytes) -> u64 {
        let mut done = 0usize;
        while done < src.len() {
            let mut pinned = [(0u64, 0u64, 0usize, 0usize); CHUNK];
            let mut count = 0;
            let mut stopped = None;
            let mut faulted = false;
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
                    match pages.write_pin(object, page) {
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
            for &(_, phys, within, n) in &pinned[..count] {
                // SAFETY: a frame of the object, pinned and write-pinned above.
                if unsafe { src.read_raw(from, DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), n) }.is_err() {
                    faulted = true;
                    break;
                }
                from += n;
                copied += 1;
            }
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
            done = from;
            if faulted {
                return refusal(done as u64, SyscallError::BadAddress);
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
        let resized = self.removing(id, |pages, object, _| {
            if let Some(phys) = pages.resize(object, size) {
                let within = (size % PAGE) as usize;
                // SAFETY: the last page's frame, the object's while the lock is
                // held: zeroed past the size through the direct map, never
                // through a reference, as a reader may copy it meanwhile.
                unsafe { core::ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), 0, PAGE as usize - within) };
            }
        });
        match resized {
            Ok(()) => 0,
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

/// A refusal after `done` of the work, answered as the work where there was any.
fn refusal(done: u64, e: SyscallError) -> u64 {
    if done > 0 { done } else { e.to_u64() }
}

fn word(why: Refused) -> SyscallError {
    match why {
        Refused::Gone => SyscallError::Gone,
        Refused::PastEnd | Refused::InvalidArgument => SyscallError::InvalidArgument,
        Refused::NoRoom => SyscallError::ResourceExhausted,
        Refused::DirtyFull => SyscallError::WouldBlock,
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
                "paged: pid {}'s pager has a read overdue: object {object} pages {first}+{count}, waiting {}",
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
            log!("paged: pid {}'s pager has no read overdue", self.pager.pid);
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
    /// The ranges holding it ([`PagedRange`]), each under its space's lock.
    ranges: AtomicU32,
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
    /// Failed, past the end, a hole of an anonymous object, or the pager is
    /// gone: the fault is refused.
    Refused,
}

/// A mapped object's pages as its index holds them, under the pager's lock.
pub struct Index<'a> {
    state: &'a State,
    slot: u32,
}

impl Index<'_> {
    /// The frame present for the object's `page`, or why there is none.
    pub fn page(&self, page: u64) -> Result<u64, Missing> {
        let object = &self.state.entry(self.slot).object;
        match self.state.pages.lookup(object, page) {
            _ if self.state.gone => Err(Missing::Refused),
            Lookup::Present(phys) => Ok(phys),
            Lookup::Absent => Err(Missing::Absent(object.version())),
            Lookup::Zero | Lookup::Failed | Lookup::PastEnd | Lookup::Gone => Err(Missing::Refused),
        }
    }
}

/// The pages a fault's request names: the aligned run around its page.
const FAULT_RUN: u64 = 16;

impl MapNode {
    /// `f` over the object's pages, under the pager's lock.
    pub fn with_present<R>(&self, f: impl FnOnce(&Index<'_>) -> R) -> R {
        let state = self.pager.state.lock();
        f(&Index { state: &state, slot: self.slot })
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
        self.0.ranges.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(&self.0))
    }
}

impl Drop for PagedRange {
    fn drop(&mut self) {
        if self.0.ranges.fetch_sub(1, Ordering::Relaxed) == 1 {
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
            ranges: AtomicU32::new(1),
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
            *(*at).link.get() = Link { seen: entry.object.epoch(), prev: entry.mappers.tail, next: ptr::null() };
            match entry.mappers.tail.is_null() {
                true => entry.mappers.head = at,
                false => (*(*entry.mappers.tail).link.get()).next = at,
            }
        }
        entry.mappers.tail = at;
        entry.refs += 1;
        Ok(PagedRange(node))
    }

    /// The last range of `node`'s space went, its leaves cleared and shot
    /// down: the node leaves the list, and its reference on the slot goes.
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
}

impl PagedHolderObject {
    pub const RIGHTS: toyos_abi::handle::Rights =
        toyos_abi::handle::Rights::READ.union(toyos_abi::handle::Rights::DUP).union(toyos_abi::handle::Rights::TRANSFER);

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
}

impl ZeroHandles for PagedHolderObject {
    fn on_zero_handles(&self) {
        self.named.pager.holder_gone(self.named.slot);
    }
}
