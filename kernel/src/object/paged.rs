//! Paged objects (memory design §7, stage 7a): a pager process's memory,
//! one object per file node, which holders read only through
//! `SYS_PAGED_READ`. Nothing here is mapped.
//!
//! **Three kinds.** A [`PagerObject`] is its process's one pager: its page
//! table of `capacity` slots ([`kernel::paged::Pages`]), its readers'
//! requests and its notices. It neither duplicates nor transfers, so its last
//! handle goes with its process, and that ends every object it serves: each
//! reader answers `Gone`. A [`PagedOwnerObject`] is the pager's handle on one
//! object, which supplies, writes and writes back. A [`PagedHolderObject`]
//! reads it, and may be narrowed and sent.
//!
//! **Supply copies into fresh frames**, so no frame the pager still maps
//! enters an object. A frame leaves at withdrawal, failure, shrink or the
//! object's end: out of the table under the pager's lock, then freed, which a
//! reader's pin defers to its last unpin.
//!
//! **A reader waits only on its pager.** An absent page is requested in the
//! waiter's own record, linked on its pager's list, and the wait ends on a
//! supply, a failure, a shrink, the pager's end or a kill; past the pager's
//! report time it is counted and logged overdue and goes on waiting. A pager's
//! own process is refused a holder's read, so no pager waits on a pager.
//!
//! **Lock order.** The pager's lock is innermost but for the allocator's. A
//! copy to or from user memory holds pins, never the lock; no wait holds a
//! pin.
//!
//! What a pager holds: `dirty` pages and a read's headroom promised at once,
//! the rest of `capacity` promised a page at a time and given back as each
//! goes (`issues/a-pager-holds-a-fixed-capacity-until-the-memory-service-reclaims-its-clean-pages.md`).

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use kernel::paged::{Lookup, Object, Pages, Refused, Supplied, PAGE};
use kernel::sched::task::WaitClass;
use toyos_abi::syscall::{PagedNotice, SyscallError, NOTICE_NO_HOLDERS, NOTICE_REQUESTED};

use crate::mm::charge;
use crate::mm::pmm;
use crate::process::Pid;
use crate::sync::Lock;
use crate::time::{Deadline, Duration};
use crate::user_ptr::{UserBytes, UserBytesMut};
use crate::watch::{self, Watch};
use crate::DirectMap;

use super::{KObjectVariant, ObjectCore, ZeroHandles};

/// Pages one copy pins, and one supply or write draws, at once.
const CHUNK: usize = 16;

/// The page-in headroom held back beside the dirty ceiling: what a read's
/// supply can always draw.
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

/// One object of a pager's.
struct Entry {
    object: Object,
    cookie: u64,
    /// Owner and holder objects that name this entry: it goes with the last.
    refs: u32,
    /// Holder objects whose handles have not all gone.
    holders: u32,
    /// The owner's handle has not gone.
    owned: bool,
    /// The last holder went and no notice has said so.
    no_holders: bool,
    /// Posted on every change a waiter re-reads: supply, failure, shrink, end.
    watch: Arc<Watch>,
}

struct State {
    pages: Pages<u64>,
    entries: Vec<Option<Entry>>,
    free: Vec<u32>,
    requests: *mut Request,
    /// Frames promised and not yet drawn.
    pool: u64,
    /// Frames the table holds.
    drawn: u64,
    gone: bool,
}

// SAFETY: `requests` and every `Request` linked from it are reached only under
// the pager's lock, and each node is unlinked by its waiter before its frame
// returns (`Linked`'s drop).
unsafe impl Send for State {}

impl State {
    fn entry(&self, slot: u32) -> &Entry {
        self.entries[slot as usize].as_ref().expect("paged: a slot an object names is held")
    }

    fn entry_mut(&mut self, slot: u32) -> &mut Entry {
        self.entries[slot as usize].as_mut().expect("paged: a slot an object names is held")
    }

    /// The table and the object at `slot`, borrowed apart.
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
    pid: Pid,
    report: Duration,
    /// Frames held whatever the table holds.
    core_pages: u64,
    overdue: AtomicU32,
}

/// Why [`PagerObject::create`] refused.
pub enum Unmade {
    Invalid,
    Exhausted,
}

impl PagerObject {
    pub const RIGHTS: toyos_abi::handle::Rights = toyos_abi::handle::Rights::READ.union(toyos_abi::handle::Rights::WAIT);

    /// The calling process's pager: `capacity` pages, `dirty` of them dirty at
    /// once, readers overdue past `report_ns`.
    pub fn create(capacity: u64, dirty: u64, report_ns: u64) -> Result<Arc<Self>, Unmade> {
        if capacity == 0 || capacity > toyos_abi::syscall::PAGER_MAX_PAGES || dirty > capacity {
            return Err(Unmade::Invalid);
        }
        let slots = capacity as usize;
        let core = Self::new_core(Pages::<u64>::heap_bytes(slots)).map_err(|_| Unmade::Exhausted)?;
        let core_pages = dirty + (capacity - dirty).min(HEADROOM);
        if !pmm::promise(core.job(), core_pages) {
            return Err(Unmade::Exhausted);
        }
        let state = State {
            pages: Pages::new(slots, dirty as usize),
            entries: Vec::new(),
            free: Vec::new(),
            requests: core::ptr::null_mut(),
            pool: core_pages,
            drawn: 0,
            gone: false,
        };
        let report = Duration::from_nanos(report_ns.clamp(1, REPORT_CEILING.nanos()));
        Ok(Arc::new(Self {
            core,
            state: Lock::new(state),
            watch: Arc::new(Watch::new()),
            pid: crate::process::current_process(),
            report,
            core_pages,
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
        unreported || state.entries.iter().flatten().any(|e| e.no_holders)
    }

    /// A new object of `size` bytes named by `cookie`, with the one reference
    /// its owner holds.
    fn add(&self, size: u64, cookie: u64) -> Result<u32, SyscallError> {
        let watch = Arc::new(Watch::new());
        let mut state = self.state.lock();
        if state.gone {
            return Err(SyscallError::Gone);
        }
        let object = state.pages.object(size);
        let entry = Entry { object, cookie, refs: 1, holders: 0, owned: true, no_holders: false, watch };
        let slot = match state.free.pop() {
            Some(slot) => {
                state.entries[slot as usize] = Some(entry);
                slot
            }
            None => {
                state.entries.push(Some(entry));
                (state.entries.len() - 1) as u32
            }
        };
        Ok(slot)
    }

    /// Give a frame drawn for the table back, and refill the promise toward
    /// the pager's core.
    fn give_back(&self, state: &mut State, phys: u64, drawn: bool) {
        // SAFETY: `phys` is a frame this pager drew, out of its table or never
        // in it, which nothing maps; a reader's pin defers its reuse.
        unsafe { pmm::free_drawn(self.core.job(), &[phys]) };
        if drawn {
            state.drawn -= 1;
        }
        let short = self.core_pages.saturating_sub(state.drawn + state.pool);
        if short > 0 && pmm::promise(self.core.job(), short) {
            state.pool += short;
        }
    }

    /// Frames the table answered as removed, each given back.
    fn removed(&self, state: &mut State, frames: &[u64]) {
        for &phys in frames {
            self.give_back(state, phys, true);
        }
    }

    /// `f` over the table and the object at `slot`, collecting each frame it
    /// removes, which goes back after.
    fn removing<R>(&self, slot: u32, f: impl FnOnce(&mut Pages<u64>, &mut Object, &mut dyn FnMut(u64)) -> R) -> R {
        let mut state = self.state.lock();
        let mut frames = Vec::new();
        let answer = {
            let (pages, object) = state.split(slot);
            f(pages, object, &mut |phys| frames.push(phys))
        };
        self.removed(&mut state, &frames);
        state.entry(slot).watch.post();
        answer
    }

    /// The owner's handle went: the object answers `Gone` from now on.
    fn owner_gone(&self, slot: u32) {
        self.state.lock().entry_mut(slot).owned = false;
        self.removing(slot, |pages, object, out| pages.invalidate(object, &mut |f| out(f)));
    }

    /// A holder object's handles went.
    fn holder_gone(&self, slot: u32) {
        let mut state = self.state.lock();
        let gone = state.gone;
        let entry = state.entry_mut(slot);
        entry.holders -= 1;
        if entry.holders == 0 && entry.owned && !gone {
            entry.no_holders = true;
            self.watch.post();
        }
    }

    /// An object naming `slot` went; the entry goes with the last.
    fn unref(&self, slot: u32) {
        let mut state = self.state.lock();
        let entry = state.entry_mut(slot);
        entry.refs -= 1;
        if entry.refs > 0 {
            return;
        }
        let entry = state.entries[slot as usize].take().expect("paged: a slot an object names is held");
        let mut frames = Vec::new();
        state.pages.release(entry.object, &mut |phys| frames.push(phys));
        self.removed(&mut state, &frames);
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
                        batch[n] = PagedNotice { cookie: state.entry(slot).cookie, kind: NOTICE_REQUESTED, first, count };
                        n += 1;
                    }
                    at = next;
                }
                for entry in state.entries.iter_mut().flatten() {
                    if n == want {
                        break;
                    }
                    if entry.no_holders {
                        entry.no_holders = false;
                        batch[n] = PagedNotice { cookie: entry.cookie, kind: NOTICE_NO_HOLDERS, first: 0, count: 0 };
                        n += 1;
                    }
                }
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

    /// Read `out.len()` bytes of the object at `slot` from `offset`; a holder
    /// waits for absent pages, an owner is answered `WouldBlock`.
    fn read(&self, slot: u32, out: &mut UserBytesMut, offset: u64, waits: bool) -> u64 {
        let want = out.len();
        let mut done = 0usize;
        let mut node = Request { slot, first: 0, count: 0, reported: false, prev: core::ptr::null_mut(), next: core::ptr::null_mut() };
        let mut linked: Option<Linked<'_>> = None;
        let mut overdue: Option<Overdue<'_>> = None;
        let began = crate::clock::now();
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
                        self.pin_chunk(&mut state, slot, at, end, &mut node, &mut linked, waits)
                    }
                }
            };
            match step {
                Step::Copy(pieces, count) => {
                    for &(phys, within, n) in &pieces[..count] {
                        // SAFETY: `phys` is a frame of the object, pinned by
                        // `pin_chunk`, and `within + n` lies inside it.
                        unsafe { out.write_raw(done, DirectMap::from_phys(phys).as_ptr::<u8>().add(within), n) };
                        done += n;
                    }
                    for &(phys, _, _) in &pieces[..count] {
                        pmm::unpin_range(phys, PAGE as usize);
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
                    let watch = Arc::clone(&self.state.lock().entry(slot).watch);
                    let ready = || {
                        let state = self.state.lock();
                        state.gone || !matches!(state.pages.lookup(&state.entry(slot).object, page), Lookup::Absent)
                    };
                    let parkable = crate::scheduler::Parkable::at_entry();
                    let deadline = match overdue {
                        Some(_) => Deadline::never(),
                        None => Deadline::at(began + self.report),
                    };
                    // Never read: a killed thread exits before it reaches its process again.
                    if watch::wait_until(&parkable, &watch, 0, WaitClass::Io, deadline, ready).is_err() {
                        return SyscallError::Gone.to_u64();
                    }
                    if overdue.is_none() && !ready() && deadline.reached(crate::clock::now()) {
                        let (cookie, first, count) = {
                            let state = self.state.lock();
                            (state.entry(slot).cookie, node.first, node.count)
                        };
                        overdue = Some(Overdue::begin(self, cookie, first, count, began));
                    }
                }
            }
        }
    }

    /// Under the lock: pin the present pages from `at` up to `end`, at most
    /// [`CHUNK`], or else say what the first page is, recording a waiter's
    /// request for every absent page from it.
    #[allow(clippy::too_many_arguments)]
    fn pin_chunk<'a>(
        &'a self,
        state: &mut State,
        slot: u32,
        at: u64,
        end: u64,
        node: &mut Request,
        linked: &mut Option<Linked<'a>>,
        waits: bool,
    ) -> Step {
        let mut pieces = [(0u64, 0usize, 0usize); CHUNK];
        let mut count = 0;
        let mut pos = at;
        while count < CHUNK && pos < end {
            let page = pos / PAGE;
            match state.pages.lookup(&state.entry(slot).object, page) {
                Lookup::Present(phys) if pmm::pin_range(phys, PAGE as usize) => {
                    let within = (pos % PAGE) as usize;
                    let n = ((PAGE - pos % PAGE).min(end - pos)) as usize;
                    pieces[count] = (phys, within, n);
                    count += 1;
                    pos += n as u64;
                }
                // A pin count at its limit: copy what is pinned, and pin again.
                Lookup::Present(_) if count > 0 => break,
                Lookup::Present(_) => panic!("paged: a frame's pins overflowed"),
                _ if count > 0 => break,
                Lookup::Absent => {
                    if waits {
                        let last = (end - 1) / PAGE;
                        self.request(state, node, linked, page, last - page + 1);
                    }
                    return Step::Absent(page);
                }
                Lookup::Failed => return Step::Failed,
                Lookup::Gone => return Step::Gone,
                Lookup::PastEnd => return Step::Done,
            }
        }
        Step::Copy(pieces, count)
    }

    /// Record that the waiter wants `count` pages from `first`: on the list
    /// once, and named again in a notice where the range moved.
    fn request<'a>(&'a self, state: &mut State, node: &mut Request, linked: &mut Option<Linked<'a>>, first: u64, count: u64) {
        if linked.is_some() && node.first == first && node.count == count {
            return;
        }
        node.first = first;
        node.count = count;
        node.reported = false;
        if linked.is_none() {
            let at: *mut Request = node;
            node.next = state.requests;
            node.prev = core::ptr::null_mut();
            if !state.requests.is_null() {
                // SAFETY: the head is linked, so live, under the lock.
                unsafe { (*state.requests).prev = at };
            }
            state.requests = at;
            *linked = Some(Linked { pager: self, node: at });
        }
        self.watch.post();
    }

    /// Copy `src` into fresh pages from page-aligned `offset`, clean or
    /// dirty: the pages installed.
    fn supply(&self, slot: u32, offset: u64, src: &UserBytes, dirty: bool) -> u64 {
        if !offset.is_multiple_of(PAGE) || src.len() == 0 {
            return SyscallError::InvalidArgument.to_u64();
        }
        let mut installed = 0u64;
        let mut done = 0usize;
        while done < src.len() {
            let first = (offset + done as u64) / PAGE;
            let pages = (src.len() - done).div_ceil(PAGE as usize).min(CHUNK);
            // Promises first, under the lock: what a page drawn below may take.
            let (reserved, size) = {
                let mut state = self.state.lock();
                if state.gone || state.entry(slot).object.gone() {
                    return refusal(installed, SyscallError::Gone);
                }
                let mut reserved = 0;
                while reserved < pages {
                    if state.pool > 0 {
                        state.pool -= 1;
                    } else if !pmm::promise(self.core.job(), 1) {
                        break;
                    }
                    reserved += 1;
                }
                (reserved, state.entry(slot).object.size())
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
                let frame = pmm::alloc_promised();
                // SAFETY: a frame just drawn, which nothing else reaches yet.
                unsafe { src.read_raw(from, frame.direct_map().as_mut_ptr::<u8>(), n) };
                frames[k] = frame.into_raw();
                ends[k] = page_at + (src.len() - from).min(PAGE as usize) as u64;
            }
            let mut refused = None;
            {
                let mut state = self.state.lock();
                for k in 0..reserved {
                    let answer = if state.gone {
                        Err(Refused::Gone)
                    } else {
                        let (pages, object) = state.split(slot);
                        pages.supply(object, first + k as u64, frames[k], dirty, ends[k])
                    };
                    match answer {
                        Ok(Supplied::Installed) => {
                            state.drawn += 1;
                            installed += 1;
                        }
                        Ok(Supplied::Present) => self.give_back(&mut state, frames[k], false),
                        Err(why) => {
                            self.give_back(&mut state, frames[k], false);
                            refused.get_or_insert(why);
                        }
                    }
                }
                state.entry(slot).watch.post();
            }
            if let Some(why) = refused {
                return refusal(installed, word(why));
            }
            done += reserved * PAGE as usize;
        }
        installed
    }

    /// Copy `src` into the present pages from `offset`, dirty: the bytes
    /// written, up to the first absent page.
    fn write(&self, slot: u32, offset: u64, src: &UserBytes) -> u64 {
        let mut done = 0usize;
        while done < src.len() {
            let mut pinned = [(0u64, 0u64, 0usize, 0usize); CHUNK];
            let mut count = 0;
            let mut stopped = None;
            {
                let mut state = self.state.lock();
                if state.gone {
                    return refusal(done as u64, SyscallError::Gone);
                }
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
            }
            let mut from = done;
            for &(_, phys, within, n) in &pinned[..count] {
                // SAFETY: a frame of the object, pinned and write-pinned above.
                unsafe { src.read_raw(from, DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), n) };
                from += n;
            }
            {
                let mut state = self.state.lock();
                for &(page, phys, within, n) in &pinned[..count] {
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

    fn withdraw(&self, slot: u32, first: u64, count: u64) -> u64 {
        self.removing(slot, |pages, object, out| pages.withdraw(object, first, count, &mut |f| out(f))) as u64
    }

    fn fail(&self, slot: u32, first: u64, count: u64) -> u64 {
        match self.removing(slot, |pages, object, out| pages.fail(object, first, count, &mut |f| out(f))) {
            Ok(()) => 0,
            Err(why) => word(why).to_u64(),
        }
    }

    fn resize(&self, slot: u32, size: u64) -> u64 {
        let resized = self.removing(slot, |pages, object, out| {
            if object.gone() {
                return Err(SyscallError::Gone);
            }
            if let Some(phys) = pages.resize(object, size, &mut |f| out(f)) {
                let within = (size % PAGE) as usize;
                // SAFETY: the last page's frame, the object's while the lock is
                // held: zeroed past the size through the direct map, never
                // through a reference, as a reader may copy it meanwhile.
                unsafe { core::ptr::write_bytes(DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(within), 0, PAGE as usize - within) };
            }
            Ok(())
        });
        match resized {
            Ok(()) => 0,
            Err(e) => e.to_u64(),
        }
    }

    fn writeback(&self, slot: u32, first: u64, count: u64, end: Option<bool>, out: &mut UserBytesMut) -> u64 {
        let bits = {
            let mut state = self.state.lock();
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

/// What one look at the next pages found: a value of one frame, never stored.
#[allow(clippy::large_enum_variant)]
enum Step {
    Copy([(u64, usize, usize); CHUNK], usize),
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

/// A waiter's request on its pager's list, taken off as the read returns
/// by any path, a cancel included.
struct Linked<'a> {
    pager: &'a PagerObject,
    node: *mut Request,
}

impl Drop for Linked<'_> {
    fn drop(&mut self) {
        let mut state = self.pager.state.lock();
        // SAFETY: the node is in the reader's frame, which outlives this, and
        // linked; its neighbours are linked, under the lock.
        unsafe {
            let node = &*self.node;
            if node.prev.is_null() {
                state.requests = node.next;
            } else {
                (*node.prev).next = node.next;
            }
            if !node.next.is_null() {
                (*node.next).prev = node.prev;
            }
        }
    }
}

/// A reader past its pager's report time, counted until its wait ends.
struct Overdue<'a> {
    pager: &'a PagerObject,
}

impl<'a> Overdue<'a> {
    fn begin(pager: &'a PagerObject, cookie: u64, first: u64, count: u64, began: crate::time::Instant) -> Self {
        if pager.overdue.fetch_add(1, Ordering::Relaxed) == 0 {
            log!(
                "paged: pid {}'s pager has a read overdue: object {cookie} pages {first}+{count}, waiting {}",
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
/// woken, every frame goes back.
impl ZeroHandles for PagerObject {
    fn on_zero_handles(&self) {
        let mut state = self.state.lock();
        state.gone = true;
        let mut frames = Vec::new();
        let State { pages, entries, .. } = &mut *state;
        for entry in entries.iter_mut().flatten() {
            pages.invalidate(&mut entry.object, &mut |phys| frames.push(phys));
            entry.no_holders = false;
            entry.watch.post();
        }
        self.removed(&mut state, &frames);
        pmm::unpromise(self.core.job(), state.pool);
        state.pool = 0;
    }
}

impl Drop for PagerObject {
    fn drop(&mut self) {
        let state = self.state.lock();
        assert!(state.requests.is_null(), "paged: a pager went with a reader still linked");
        pmm::unpromise(self.core.job(), state.pool);
    }
}

/// What owner and holder objects share: the entry they name.
struct Named {
    pager: Arc<PagerObject>,
    slot: u32,
}

impl Drop for Named {
    fn drop(&mut self) {
        self.pager.unref(self.slot);
    }
}

pub struct PagedOwnerObject {
    pub(super) core: ObjectCore,
    named: Named,
}

impl PagedOwnerObject {
    /// Neither `DUP` nor `TRANSFER`: its handle is its pager's.
    pub const RIGHTS: toyos_abi::handle::Rights = toyos_abi::handle::Rights::READ.union(toyos_abi::handle::Rights::WRITE);

    /// A new object of `pager`'s.
    pub fn create(pager: &Arc<PagerObject>, size: u64, cookie: u64) -> Result<Arc<Self>, SyscallError> {
        let core = Self::new_core(charge::arc::<Watch>() + 2 * core::mem::size_of::<Option<Entry>>())?;
        let slot = pager.add(size, cookie)?;
        Ok(Arc::new(Self { core, named: Named { pager: Arc::clone(pager), slot } }))
    }

    /// A holder of the same object, which counts as one once its handle is
    /// installed ([`PagedHolderObject::held`]).
    pub fn holder(&self) -> Result<Arc<PagedHolderObject>, SyscallError> {
        let core = PagedHolderObject::new_core(0)?;
        let Named { pager, slot } = &self.named;
        let mut state = pager.state.lock();
        if state.gone || !state.entry(*slot).owned {
            return Err(SyscallError::Gone);
        }
        state.entry_mut(*slot).refs += 1;
        drop(state);
        Ok(Arc::new(PagedHolderObject { core, named: Named { pager: Arc::clone(pager), slot: *slot } }))
    }

    pub fn read(&self, out: &mut UserBytesMut, offset: u64) -> u64 {
        self.named.pager.read(self.named.slot, out, offset, false)
    }

    pub fn supply(&self, offset: u64, src: &UserBytes, dirty: bool) -> u64 {
        self.named.pager.supply(self.named.slot, offset, src, dirty)
    }

    pub fn write(&self, offset: u64, src: &UserBytes) -> u64 {
        self.named.pager.write(self.named.slot, offset, src)
    }

    pub fn withdraw(&self, first: u64, count: u64) -> u64 {
        self.named.pager.withdraw(self.named.slot, first, count)
    }

    pub fn fail(&self, first: u64, count: u64) -> u64 {
        self.named.pager.fail(self.named.slot, first, count)
    }

    pub fn resize(&self, size: u64) -> u64 {
        self.named.pager.resize(self.named.slot, size)
    }

    pub fn writeback(&self, first: u64, count: u64, end: Option<bool>, out: &mut UserBytesMut) -> u64 {
        self.named.pager.writeback(self.named.slot, first, count, end, out)
    }
}

impl ZeroHandles for PagedOwnerObject {
    fn on_zero_handles(&self) {
        self.named.pager.owner_gone(self.named.slot);
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
