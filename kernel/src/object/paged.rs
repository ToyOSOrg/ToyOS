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
//! supply, a failure, a shrink, a release, the pager's end or a kill; a wait
//! past the pager's report time is counted and logged overdue and goes on.
//! A pager's own process is refused a holder's read, so no pager waits on a
//! pager.
//!
//! **Lock order.** The pager's lock is innermost but for the allocator's. A
//! copy to or from user memory holds pins, never the lock; no wait holds a
//! pin.
//!
//! **What a pager holds** (`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`):
//! its core — `dirty` pages and a read's headroom — committed at its making
//! and kept for its life, every frame of it either promised, in the index or
//! held aside for the next supply; the rest of `capacity` promised a page at
//! a time and given back as each goes. Its index, objects and the frames it
//! holds aside are charged with it, so no call on it allocates.

use alloc::sync::Arc;
use core::cell::{Cell, UnsafeCell};
use core::marker::PhantomPinned;
use core::pin::Pin;
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use kernel::chunked::{Chunked, Stack};
use kernel::paged::{Core, Draw, Leave, Lookup, Object, Pages, Refused, Supplied, PAGE};
use kernel::sched::task::WaitClass;
use toyos_abi::syscall::{PagedNotice, SyscallError, NOTICE_NO_HOLDERS, NOTICE_REQUESTED};

use crate::mm::pmm;
use crate::process::Pid;
use crate::sync::Lock;
use crate::time::{Deadline, Duration};
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::{self, Watch};
use crate::DirectMap;

use super::{KObjectVariant, ObjectCore, ZeroHandles};

// Two pieces of a table and their allocator headers share one superframe.
const _: () = assert!(2 * kernel::chunked::CHUNK_BYTES < crate::mm::MAX_HEAP_ALLOC);

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
    first: u64,
    count: u64,
    reported: bool,
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
    /// The last holder went and no notice has said so.
    no_holders: bool,
}

struct State {
    pages: Pages<u64>,
    /// Every object's slot, made with the pager.
    entries: Chunked<Option<Entry>>,
    free: Stack<u32>,
    requests: *mut Request,
    /// Entries whose `no_holders` is set.
    no_holders: usize,
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
        unsafe { pmm::free_drawn(job, [phys]) };
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
        self.entries[slot as usize].as_ref().expect("paged: a slot an object names is held")
    }

    fn entry_mut(&mut self, slot: u32) -> &mut Entry {
        self.entries[slot as usize].as_mut().expect("paged: a slot an object names is held")
    }

    /// The slot of the unreleased object `id` names: the caller's word,
    /// refused `InvalidArgument` where it names none.
    fn live(&self, id: u64) -> Result<u32, SyscallError> {
        let slot = u32::try_from(id).map_err(|_| SyscallError::InvalidArgument)?;
        let entry = (slot as usize) < self.entries.len() && self.entries[slot as usize].as_ref().is_some_and(|e| e.owned);
        match entry {
            true if !self.gone => Ok(slot),
            _ if self.gone => Err(SyscallError::Gone),
            _ => Err(SyscallError::InvalidArgument),
        }
    }

    /// The index and the object at `slot`, borrowed apart.
    fn split(&mut self, slot: u32) -> (&mut Pages<u64>, &mut Object) {
        let entry = self.entries[slot as usize].as_mut().expect("paged: a slot an object names is held");
        (&mut self.pages, &mut entry.object)
    }

}

pub struct PagerObject {
    pub(super) core: ObjectCore,
    state: Lock<State>,
    /// Posted when a notice is owed.
    watch: Arc<Watch>,
    /// Posted on every change a waiter reads again: a supply, a failure, a
    /// shrink, a release, the end.
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
    /// once, at most `objects` objects, readers overdue past `report_ns`.
    pub fn create(capacity: u64, dirty: u64, objects: u64, report_ns: u64) -> Result<Arc<Self>, Unmade> {
        let fits = |n: u64| usize::try_from(n).ok().filter(|&n| n > 0 && n <= Pages::<u64>::MAX_CAPACITY);
        let (Some(slots), Some(count)) = (fits(capacity), fits(objects)) else { return Err(Unmade::Invalid) };
        if dirty > capacity {
            return Err(Unmade::Invalid);
        }
        let core_pages = dirty + (capacity - dirty).min(HEADROOM);
        let held = core_pages as usize;
        let tables = Pages::<u64>::heap_bytes(slots)
            + Chunked::<Option<Entry>>::heap_bytes(count)
            + Stack::<u32>::heap_bytes(count)
            + 2 * Stack::<u64>::heap_bytes(held);
        let core = Self::new_core(tables).map_err(|_| Unmade::Exhausted)?;
        let pages = Pages::new(slots, dirty as usize).ok_or(Unmade::Exhausted)?;
        let entries = Chunked::new(count, || None).ok_or(Unmade::Exhausted)?;
        let mut free = Stack::new(count).ok_or(Unmade::Exhausted)?;
        for slot in (0..count as u32).rev() {
            free.push(slot);
        }
        let spare = Stack::new(held).ok_or(Unmade::Exhausted)?;
        let draining = Stack::new(held).ok_or(Unmade::Exhausted)?;
        if !pmm::promise(core.job(), core_pages) {
            return Err(Unmade::Exhausted);
        }
        let state = State {
            pages,
            entries,
            free,
            requests: ptr::null_mut(),
            no_holders: 0,
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

    /// Whether a notice is owed: a request no notice has named, or a last
    /// holder gone.
    pub fn has_notices(&self) -> bool {
        let state = self.state.lock();
        // SAFETY: every node on the list is live while linked, under this lock.
        let unreported = requests(&state).any(|r| unsafe { !(*r).reported });
        unreported || state.no_holders > 0
    }

    /// A new object of `size` bytes: its id. `ResourceExhausted` where every
    /// slot holds one.
    pub fn add(&self, size: u64) -> u64 {
        if size > kernel::paged::LAST_END {
            return SyscallError::InvalidArgument.to_u64();
        }
        let mut state = self.state.lock();
        if state.gone {
            return SyscallError::Gone.to_u64();
        }
        let Some(slot) = state.free.pop() else { return SyscallError::ResourceExhausted.to_u64() };
        let object = state.pages.object(slot, size);
        state.entries[slot as usize] = Some(Entry { object, refs: 1, holders: 0, owned: true, no_holders: false });
        u64::from(slot)
    }

    /// `f` over the index and the live object `id`, each frame it removes
    /// given back as it goes; waiters read again.
    fn removing<R>(&self, id: u64, f: impl FnOnce(&mut Pages<u64>, &mut Object, &mut dyn FnMut(u64)) -> R) -> Result<R, SyscallError> {
        let mut state = self.state.lock();
        let slot = state.live(id)?;
        let job = self.core.job();
        let State { pages, entries, held, gone, .. } = &mut *state;
        let object = &mut entries[slot as usize].as_mut().expect("live").object;
        let answer = f(pages, object, &mut |phys| held.give_back(job, *gone, phys, true));
        drop(state);
        self.changed.post();
        Ok(answer)
    }

    /// The pager lets object `id` go, in one hold of the lock: it answers
    /// `Gone` from now on, its pages go, and its slot with its last holder. A
    /// second release, or one racing the pager's end, finds it released.
    pub fn release(&self, id: u64) -> u64 {
        let mut state = self.state.lock();
        let slot = match state.live(id) {
            Ok(slot) => slot,
            Err(e) => return e.to_u64(),
        };
        self.let_go(&mut state, slot);
        drop(state);
        self.changed.post();
        0
    }

    /// Under the lock, `state`: the unreleased object at `slot` released.
    fn let_go(&self, state: &mut State, slot: u32) {
        let job = self.core.job();
        let State { pages, entries, held, gone, no_holders, .. } = state;
        let entry = entries[slot as usize].as_mut().expect("paged: a slot an object names is held");
        pages.invalidate(&mut entry.object, &mut |phys| held.give_back(job, *gone, phys, true));
        assert!(core::mem::take(&mut entry.owned), "paged: an object released twice");
        if core::mem::take(&mut entry.no_holders) {
            *no_holders -= 1;
        }
        self.unref_locked(state, slot);
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
        if entry.holders == 0 && entry.owned && !gone && !entry.no_holders {
            entry.no_holders = true;
            state.no_holders += 1;
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
        let entry = state.entries[slot as usize].take().expect("paged: a slot an object names is held");
        state.no_holders -= usize::from(entry.no_holders);
        let job = self.core.job();
        let State { pages, held, gone, .. } = state;
        pages.release(entry.object, &mut |phys| held.give_back(job, *gone, phys, true));
        state.free.push(slot);
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
                    let (slot, first, count, reported, next) =
                        unsafe { ((*at).slot, (*at).first, (*at).count, (*at).reported, (*at).next) };
                    if !reported {
                        // SAFETY: as above.
                        unsafe { (*at).reported = true };
                        batch[n] = PagedNotice { object: u64::from(slot), kind: NOTICE_REQUESTED, first, count };
                        n += 1;
                    }
                    at = next;
                }
                let mut owed = state.no_holders;
                for slot in 0..state.entries.len() {
                    if n == want || owed == 0 {
                        break;
                    }
                    let Some(entry) = state.entries[slot].as_mut() else { continue };
                    if entry.no_holders {
                        entry.no_holders = false;
                        owed -= 1;
                        // Said only while still so: a holder made since is held.
                        if entry.holders == 0 {
                            batch[n] = PagedNotice { object: slot as u64, kind: NOTICE_NO_HOLDERS, first: 0, count: 0 };
                            n += 1;
                        }
                    }
                }
                state.no_holders = owed;
                n
            };
            // SAFETY: a `PagedNotice` is `UserSafe`: every byte of it is a
            // field, and the batch outlives the copy.
            unsafe { out.write_raw(filled * size, batch.as_ptr().cast::<u8>(), n * size) };
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
        loop {
            let at = offset.saturating_add(done as u64);
            let step = {
                let mut state = self.state.lock();
                if state.gone || state.entry(slot).object.gone() {
                    Step::Gone
                } else {
                    let size = state.entry(slot).object.size();
                    if done == want || at >= size {
                        Step::Done
                    } else {
                        let end = size.min(offset.saturating_add(want as u64));
                        let waiter = waits.then_some(waiting.as_ref());
                        self.pin_chunk(&mut state, slot, at..end, &mut pieces, waiter)
                    }
                }
            };
            match step {
                Step::Copy(count) => {
                    for piece in &pieces[..count] {
                        // SAFETY: `phys` is a frame of the object, pinned by
                        // `pin_chunk`, and `within + len` lies inside it.
                        unsafe { out.write_raw(done, DirectMap::from_phys(piece.phys).as_ptr::<u8>().add(piece.within), piece.len) };
                        done += piece.len;
                    }
                    for piece in &pieces[..count] {
                        pmm::unpin_range(piece.phys, PAGE as usize);
                    }
                    let gone = {
                        let state = self.state.lock();
                        state.gone || state.entry(slot).object.gone()
                    };
                    // A frame copied as its pager ended is no byte to report.
                    if gone {
                        return SyscallError::Io.to_u64();
                    }
                }
                Step::Done => return done as u64,
                Step::Gone if done > 0 => return done as u64,
                Step::Gone => return SyscallError::Gone.to_u64(),
                Step::Failed if done > 0 => return done as u64,
                Step::Failed => return SyscallError::Io.to_u64(),
                Step::Absent(_) if !waits && done > 0 => return done as u64,
                Step::Absent(_) if !waits => return SyscallError::WouldBlock.to_u64(),
                Step::Absent(page) => {
                    let ready = || {
                        let state = self.state.lock();
                        state.gone || !matches!(state.pages.lookup(&state.entry(slot).object, page), Lookup::Absent)
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
                    pieces[count] = Piece { phys, within, len };
                    count += 1;
                    pos += len as u64;
                }
                // A pin count at its limit: copy what is pinned, and pin again.
                Lookup::Present(_) if count > 0 => break,
                Lookup::Present(_) => panic!("paged: a frame's pins overflowed"),
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
        Step::Copy(count)
    }

    /// Copy `src` into fresh pages of object `id` from page-aligned `offset`,
    /// clean or dirty: the pages installed. The caller checked the span is in
    /// reach.
    pub fn supply(&self, id: u64, offset: u64, src: &UserBytes, dirty: bool) -> u64 {
        if !offset.is_multiple_of(PAGE) || src.len() == 0 {
            return SyscallError::InvalidArgument.to_u64();
        }
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
                // SAFETY: a frame just drawn, which nothing else reaches yet.
                unsafe { src.read_raw(from, DirectMap::from_phys(phys).as_mut_ptr::<u8>(), n) };
                frames[k] = phys;
                ends[k] = page_at + (src.len() - from).min(PAGE as usize) as u64;
            }
            let mut refused = None;
            {
                let mut state = self.state.lock();
                for k in 0..reserved {
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
                        }
                        Err(why) => {
                            let gone = state.gone;
                            state.held.give_back(self.core.job(), gone, frames[k], false);
                            refused.get_or_insert(why);
                        }
                    }
                }
            }
            self.changed.post();
            if let Some(why) = refused {
                return refusal(installed, word(why));
            }
            done += reserved * PAGE as usize;
        }
        installed
    }

    /// Copy `src` into the present pages of object `id` from `offset`, dirty:
    /// the bytes written, up to the first absent page. The caller checked the
    /// span is in reach.
    pub fn write(&self, id: u64, offset: u64, src: &UserBytes) -> u64 {
        let mut done = 0usize;
        while done < src.len() {
            let mut pinned = [(0u64, 0u64, 0usize, 0usize); CHUNK];
            let mut count = 0;
            let mut stopped = None;
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
            for &(_, phys, within, n) in &pinned[..count] {
                // SAFETY: a frame of the object, pinned and write-pinned above.
                unsafe { src.read_raw(from, DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), n) };
                from += n;
            }
            {
                let mut state = self.state.lock();
                for &(page, phys, within, n) in &pinned[..count] {
                    // A release meanwhile took the object: nothing left to unpin.
                    if state.live(id) != Ok(slot) {
                        break;
                    }
                    let (pages, object) = state.split(slot);
                    pages.write_unpin(object, page, phys, page * PAGE + (within + n) as u64);
                }
            }
            for &(_, phys, _, _) in &pinned[..count] {
                pmm::unpin_range(phys, PAGE as usize);
            }
            done = from;
            match stopped {
                None => {}
                Some(Refused::InvalidArgument) => return refusal(done as u64, SyscallError::NotFound),
                Some(why) => return refusal(done as u64, word(why)),
            }
        }
        done as u64
    }

    pub fn withdraw(&self, id: u64, first: u64, count: u64) -> u64 {
        match self.removing(id, |pages, object, out| pages.withdraw(object, first, count, &mut |f| out(f))) {
            Ok(n) => n as u64,
            Err(e) => e.to_u64(),
        }
    }

    pub fn fail(&self, id: u64, first: u64, count: u64) -> u64 {
        match self.removing(id, |pages, object, out| pages.fail(object, first, count, &mut |f| out(f))) {
            Ok(Ok(())) => 0,
            Ok(Err(why)) => word(why).to_u64(),
            Err(e) => e.to_u64(),
        }
    }

    pub fn resize(&self, id: u64, size: u64) -> u64 {
        if size > kernel::paged::LAST_END {
            return SyscallError::InvalidArgument.to_u64();
        }
        let resized = self.removing(id, |pages, object, out| {
            if let Some(phys) = pages.resize(object, size, &mut |f| out(f)) {
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
                out.write_at(0, &bytes);
                0
            }
            Err(why) => word(why).to_u64(),
        }
    }
}

/// Part of one pinned frame a read copies.
#[derive(Clone, Copy, Default)]
struct Piece {
    phys: u64,
    within: usize,
    len: usize,
}

/// What one look at the next pages found.
enum Step {
    /// This many pieces were pinned.
    Copy(usize),
    Absent(u64),
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
        let node = Request { slot, first: 0, count: 0, reported: false, prev: ptr::null_mut(), next: ptr::null_mut() };
        Self { pager, node: UnsafeCell::new(node), linked: Cell::new(false), _pinned: PhantomPinned }
    }

    /// Under the pager's lock, `state`: want `count` pages from `first`, on the
    /// list once, and named again in a notice where the range moved.
    fn request(self: Pin<&Self>, state: &mut State, first: u64, count: u64) {
        let at = self.node.get();
        // SAFETY: the node is this frame's and pinned; under the lock, nothing
        // else reaches it, and no reference to it is formed.
        unsafe {
            if self.linked.get() && (*at).first == first && (*at).count == count {
                return;
            }
            (*at).first = first;
            (*at).count = count;
            (*at).reported = false;
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
        unsafe { ((*self.node.get()).first, (*self.node.get()).count) }
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
            let Some(entry) = state.entries[slot as usize].as_mut() else { continue };
            entry.no_holders = false;
            if entry.owned {
                self.let_go(&mut state, slot);
            }
        }
        state.no_holders = 0;
        let job = self.core.job();
        while let Some(phys) = state.held.spare.pop().or_else(|| state.held.draining.pop()) {
            // SAFETY: a frame this pager drew and held aside, which nothing
            // maps; a pin defers its free to its last unpin.
            unsafe { pmm::free_drawn(job, [phys]) };
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
}

impl ZeroHandles for PagedHolderObject {
    fn on_zero_handles(&self) {
        self.named.pager.holder_gone(self.named.slot);
    }
}
