//! The kernel side of an [inbox](toyos_abi::inbox) — shared-memory submission
//! and completion rings. `inbox_setup` creates one; `inbox_submit` submits
//! and waits. The rings and submission array live in one 2 MiB page mapped
//! into both kernel and userspace. `OP_WATCH` fires once; userspace re-submits to re-arm.
//!
//! Not `completion::Inbox` (a task's own record ring); the handle held is
//! `object::inbox::InboxObject`, wrapping this file's `Inbox`. Lock order:
//! source locks release before INBOXES acquires; never both held.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use toyos_sched::task::WaitClass;

use crate::object::shm::SharedMemObject;
use crate::object::{ops, KObjectRef};
use crate::id_map::{IdKey, IdMap};
use crate::pipe::{self, PipeId};
use crate::process::{self, Pid};
use crate::scheduler;
use crate::sync::Lock;
use crate::completion::{self, Watch};
use crate::time::{Deadline, Duration};
use crate::DirectMap;

use toyos_abi::inbox::{
    Completion, RingLayout, RingHeader, Submission,
    SUBMISSION_RING_OFF, COMPLETION_RING_OFF, SUBMISSIONS_OFF,
};
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct InboxId(usize);

impl InboxId {
}

impl core::ops::Add for InboxId {
    type Output = Self;
    fn add(self, rhs: Self) -> Self { InboxId(self.0 + rhs.0) }
}

impl IdKey for InboxId {
    const ZERO: Self = InboxId(0);
    const ONE: Self = InboxId(1);
}

/// Owned reference to a ring; `Clone` bumps the refcount, `Drop` tears down at zero.
pub struct InboxRef(InboxId);

impl InboxRef {
    pub fn id(&self) -> InboxId { self.0 }
}

impl Clone for InboxRef {
    fn clone(&self) -> Self {
        let mut guard = INBOXES.lock();
        let map = guard.as_mut().expect("inbox not initialized");
        // The instance is removed only when the last `InboxRef` drops.
        map.get_mut(self.0).expect("InboxRef outlived its ring").refs += 1;
        Self(self.0)
    }
}

impl Drop for InboxRef {
    fn drop(&mut self) {
        let instance = {
            let mut guard = INBOXES.lock();
            let map = guard.as_mut().expect("inbox not initialized");
            let instance = map.get_mut(self.0).expect("InboxRef outlived its ring");
            instance.refs -= 1;
            if instance.refs > 0 {
                return;
            }
            map.remove(self.0)
        };
        if let Some(mut instance) = instance {
            for poll in instance.pending_watches.drain(..) {
                for source in poll.sources.iter() {
                    source.remove_watcher(self.0);
                }
            }
            // `Unmapped`'s drop flushes; the `Arc` drop after it frees the pages.
            drop(instance.shm.unmap_from(instance.owner_pid));
        }
    }
}


#[derive(Clone, Copy)]
pub enum Op {
    Nop,
    Watch,
    Accept,
}

impl Op {
    fn from_raw(raw: u8) -> Result<Self, SyscallError> {
        // 2 is retired (formerly IORING_OP_POLL_REMOVE); it refuses like any undeclared op.
        match raw {
            0 => Ok(Self::Nop),
            1 => Ok(Self::Watch),
            3 => Ok(Self::Accept),
            _ => Err(SyscallError::InvalidArgument),
        }
    }
}


#[derive(Clone, Copy)]
pub struct WatchFlags(u32);

impl WatchFlags {
    pub const READABLE: Self = Self(1);
    pub const WRITABLE: Self = Self(4);

    pub fn from_raw(raw: u32) -> Self { Self(raw) }
    pub fn readable(self) -> bool { self.0 & 1 != 0 }
    pub fn writable(self) -> bool { self.0 & 4 != 0 }
    pub fn raw(self) -> u32 { self.0 }
}

/// What an `OP_WATCH` is registered on — the object naming what it watches.
#[derive(Clone)]
pub enum Source {
    Keyboard,
    Mouse,
    Network,
    /// Source::Port holds the shared `PortShared`, never either endpoint, because a server's Acceptor poll and a client's Connector completion agree on only that object.
    Port(Arc<crate::object::port::PortShared>),
    PipeReadable(PipeId),
    PipeWritable(PipeId),
    VirtioSound,
    Hda,
    /// Edge-triggered: [`Source::is_ready`] is always `false` here; completions come only from `log::user::post_readiness`.
    Log,
}

/// A source whose whole lifetime is one object's; [`cancel_by_source`] takes only these.
pub struct EndedSource(Source);

impl Source {
    /// This source, if the last handle naming it ends it; `None` for `Log` and `Keyboard`, which the machine ends on its own.
    pub fn ended_by_its_last_handle(self) -> Option<EndedSource> {
        // Keyboard is matched separately: a keyboard *claim* closing is the stimulus, not a `SysCap`.
        let ends = match self {
            Self::Log => crate::actuator::log_close_cancels_any_syscap(),
            Self::Keyboard => crate::actuator::keyboard_close_cancels_every_console(),
            Self::Mouse
            | Self::Network
            | Self::VirtioSound
            | Self::Hda
            | Self::Port(_)
            | Self::PipeReadable(_)
            | Self::PipeWritable(_) => true,
        };
        ends.then_some(EndedSource(self))
    }
}

impl PartialEq for Source {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Keyboard, Self::Keyboard)
            | (Self::Mouse, Self::Mouse)
            | (Self::Network, Self::Network)
            | (Self::VirtioSound, Self::VirtioSound)
            | (Self::Log, Self::Log)
            | (Self::Hda, Self::Hda) => true,
            (Self::Port(a), Self::Port(b)) => Arc::ptr_eq(a, b),
            (Self::PipeReadable(a), Self::PipeReadable(b)) => a == b,
            (Self::PipeWritable(a), Self::PipeWritable(b)) => a == b,
            _ => false,
        }
    }
}


/// The sources a pending poll is registered on — never both `None`; [`Watched::of`] is the only constructor.
struct Watched {
    read: Option<Source>,
    write: Option<Source>,
}

impl Watched {
    /// `None` when neither direction has readiness to watch; the caller must answer that with a completion.
    fn of(read: Option<Source>, write: Option<Source>) -> Option<Self> {
        (read.is_some() || write.is_some()).then_some(Self { read, write })
    }

    fn iter(&self) -> impl Iterator<Item = &Source> {
        [&self.read, &self.write].into_iter().flatten()
    }

    fn is_ready(&self) -> bool {
        self.iter().any(Source::is_ready)
    }

    fn watches(&self, source: &Source) -> bool {
        self.iter().any(|s| s == source)
    }

    /// Per-direction readiness, rechecked from object state under the INBOXES lock.
    fn readiness(&self) -> Readiness {
        Readiness {
            readable: self.read.as_ref().is_some_and(Source::is_ready),
            writable: self.write.as_ref().is_some_and(Source::is_ready),
        }
    }

    /// [`Self::readiness`], but the direction `fired` names is asserted, not
    /// rechecked: an edge source (`Source::Log`) or a concurrent drain rechecks
    /// to false, yet the event that woke this poll proves that direction fired.
    fn readiness_for(&self, fired: &Source) -> Readiness {
        let mut r = self.readiness();
        r.readable |= self.read.as_ref() == Some(fired);
        r.writable |= self.write.as_ref() == Some(fired);
        r
    }
}

/// The readiness a watch completion reports — computed from object state, never
/// from the request. [`Self::result_flags`] is the only source of a completion's
/// positive result word, so no site can rebuild it from the interest mask.
#[derive(Clone, Copy)]
struct Readiness {
    readable: bool,
    writable: bool,
}

impl Readiness {
    fn result_flags(self) -> u32 {
        let mut flags = 0u32;
        if self.readable { flags |= WatchFlags::READABLE.raw(); }
        if self.writable { flags |= WatchFlags::WRITABLE.raw(); }
        flags
    }
}

struct PendingWatch {
    user_data: u64,
    /// The handle the poll was submitted against; the dedup key.
    handle: RawHandle,
    sources: Watched,
}

impl PendingWatch {
    fn watches(&self, source: &Source) -> bool {
        self.sources.watches(source)
    }
}

/// Takes the poll at `index` out, unregistering the ring only from sources no other poll of it still names.
fn take_poll(instance: &mut Inbox, index: usize) -> PendingWatch {
    let poll = instance.pending_watches.swap_remove(index);
    for source in poll.sources.iter() {
        if !instance.pending_watches.iter().any(|p| p.watches(source)) {
            source.remove_watcher(instance.id);
        }
    }
    poll
}

/// Hard cap on pending polls per ring.
const MAX_PENDING_WATCHES: usize = 1024;

struct Inbox {
    id: InboxId,
    shm_phys: DirectMap,
    /// A ring's page has no lifetime of its own; it goes with the last handle to the ring.
    shm: alloc::sync::Arc<SharedMemObject>,
    /// Never zero while this entry is in the map.
    refs: u32,
    submission_size: u32,
    completion_size: u32,
    pending_watches: Vec<PendingWatch>,
    /// Cloned out of the table because `submit` holds it across its park.
    /// Exactly one `Watch`, never a pair, so no wake site can drop half of one.
    watch: Arc<Watch>,
    /// Touched only under `INBOXES`.
    completion_tail: core::cell::Cell<u32>,
    owner_pid: Pid,
}

impl Inbox {
    // No accessor below returns a Rust reference into this page — the process
    // maps it writable, so only atomics or `read_volatile` are sound here.

    /// One atomic word of one ring header; never `&RingHeader` — see the block above.
    fn ring_word(&self, ring_off: u64, field_off: usize) -> &core::sync::atomic::AtomicU32 {
        let ptr = self.shm_phys.as_mut_ptr::<u8>();
        // SAFETY: offset is in-bounds and 4-aligned within the 2 MiB page; `AtomicU32` is sound over memory the process also writes.
        unsafe {
            core::sync::atomic::AtomicU32::from_ptr(
                ptr.add(ring_off as usize + field_off) as *mut u32,
            )
        }
    }

    fn submission_head(&self) -> &core::sync::atomic::AtomicU32 {
        self.ring_word(SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, head))
    }

    fn submission_tail(&self) -> &core::sync::atomic::AtomicU32 {
        self.ring_word(SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, tail))
    }

    fn completion_head(&self) -> &core::sync::atomic::AtomicU32 {
        self.ring_word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, head))
    }

    fn completion_tail_word(&self) -> &core::sync::atomic::AtomicU32 {
        self.ring_word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, tail))
    }

    fn completion_dropped(&self) -> &core::sync::atomic::AtomicU32 {
        self.ring_word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, dropped))
    }

    /// One submission entry, copied out by value via `read_volatile` — never a `&Submission`.
    fn submission_at(&self, index: u32) -> Submission {
        let ptr = self.shm_phys.as_mut_ptr::<u8>();
        // SAFETY: `index` is masked by `submission_size` (≤256), keeping the read in-bounds and aligned within the page.
        unsafe { (ptr.add(SUBMISSIONS_OFF as usize + index as usize * core::mem::size_of::<Submission>()) as *const Submission).read_volatile() }
    }

    /// The address of one completion entry — a pointer, never a `&mut` minted from a shared borrow.
    fn completion_at(&self, index: u32) -> *mut Completion {
        let ptr = self.shm_phys.as_mut_ptr::<u8>();
        // SAFETY: `index` is masked by `completion_size` (≤512), keeping the offset inside the page.
        unsafe { ptr.add(COMPLETION_RING_OFF as usize + core::mem::size_of::<RingHeader>() + index as usize * core::mem::size_of::<Completion>()) as *mut Completion }
    }

    /// Posts a completion, or records a drop if the ring reports itself full.
    /// A full ring is not fatal here: `complete_pending_for_event` can call this on the waker's thread, which belongs to a different process.
    fn post_completion(&self, user_data: u64, result: i32, flags: u32) {
        let tail = self.completion_tail.get();
        if tail.wrapping_sub(self.completion_head().load(Ordering::Acquire)) >= self.completion_size {
            self.completion_dropped().fetch_add(1, Ordering::Relaxed);
            return;
        }
        let idx = tail & (self.completion_size - 1);
        // SAFETY: `idx` is masked to ring size; `INBOXES` serializes kernel writers.
        unsafe { self.completion_at(idx).write(Completion { token: user_data, result, flags }) };
        self.completion_tail.set(tail.wrapping_add(1));
        self.completion_tail_word().store(tail.wrapping_add(1), Ordering::Release);
    }

    /// Available completions, measured against the kernel's own tail.
    /// A process that rewrites its own `head` can only mislead itself, never the kernel, about completions waiting.
    fn completion_count(&self) -> u32 {
        let head = self.completion_head().load(Ordering::Acquire);
        self.completion_tail.get().wrapping_sub(head)
    }

    /// Cumulative, never cleared.
    fn dropped(&self) -> u32 {
        self.completion_dropped().load(Ordering::Relaxed)
    }
}

static INBOXES: Lock<Option<IdMap<InboxId, Inbox>>> = Lock::new(None);

pub fn init() {
    *INBOXES.lock() = Some(IdMap::new());
}

/// Largest submission ring a process may ask for.
const MAX_SUBMISSION_DEPTH: u32 = 256;

/// Lays out a freshly allocated inbox page before `map_into` makes it shared.
fn write_ring_page(base: DirectMap, submission_size: u32, completion_size: u32) {
    use core::sync::atomic::AtomicU32;

    let base = base.as_mut_ptr::<u8>();
    // SAFETY: `base` is a freshly allocated page not yet mapped anywhere; these are exclusive writes.
    unsafe {
        (base as *mut RingLayout).write(RingLayout {
            submission_ring_off: SUBMISSION_RING_OFF,
            completion_ring_off: COMPLETION_RING_OFF,
            submissions_off: SUBMISSIONS_OFF,
            submission_ring_size: submission_size,
            completion_ring_size: completion_size,
            features: 0,
            _pad: 0,
        });
        for (off, ring_size) in
            [(SUBMISSION_RING_OFF, submission_size), (COMPLETION_RING_OFF, completion_size)]
        {
            (base.add(off as usize) as *mut RingHeader).write(RingHeader {
                head: AtomicU32::new(0),
                tail: AtomicU32::new(0),
                ring_size,
                dropped: AtomicU32::new(0),
            });
        }
    }
}

/// Creates an inbox and maps its rings into the caller.
pub fn create(depth: u32) -> Result<(InboxRef, u64), SyscallError> {
    if depth == 0 || depth > MAX_SUBMISSION_DEPTH || !depth.is_power_of_two() {
        return Err(SyscallError::InvalidArgument);
    }

    let submission_size = depth;
    let completion_size = depth * 2;

    let pid = process::current_process();
    let addr_space = process::current_address_space();
    let shm = SharedMemObject::create(crate::mm::PAGE_2M)?;

    // Built before mapped: mapping first lets a sibling thread write the page while the kernel initializes it.
    write_ring_page(shm.phys_before_mapping(), submission_size, completion_size);

    let shm_vaddr = shm.map_into(pid, &addr_space)?;
    let shm_phys = shm.phys();

    let inbox_id = {
        let mut guard = INBOXES.lock();
        let map = guard.as_mut().expect("inbox not initialized");
        map.insert_with(|id| Inbox {
            id,
            shm_phys,
            shm,
            refs: 1,
            submission_size,
            completion_size,
            pending_watches: Vec::new(),
            watch: Arc::new(Watch::new()),
            completion_tail: core::cell::Cell::new(0),
            owner_pid: pid,
        })
    };

    Ok((InboxRef(inbox_id), shm_vaddr))
}


fn completion_count(inbox_id: InboxId) -> Result<u32, SyscallError> {
    with_instance(inbox_id, |inst| inst.completion_count())
}

/// What `submit` sees before deciding to park: readable count and whether any completion was dropped.
fn completion_state(inbox_id: InboxId) -> Result<(u32, u32), SyscallError> {
    with_instance(inbox_id, |inst| (inst.completion_count(), inst.dropped()))
}

/// Processes submissions and waits for completions; called from the syscall handler.
pub fn submit(
    inbox_id: InboxId,
    to_submit: u32,
    min_complete: u32,
    timeout_nanos: u64,
) -> Result<u32, SyscallError> {
    // `timeout_nanos` becomes a typed `Deadline` here; a bare absolute `u64` can't distinguish `0` from "no timeout".
    let non_blocking = timeout_nanos == 0;
    let deadline = if non_blocking {
        Deadline::passed()
    } else if timeout_nanos == u64::MAX {
        Deadline::never()
    } else {
        Deadline::at(crate::clock::now() + Duration::from_nanos(timeout_nanos))
    };

    if to_submit > 0 {
        submit_submissions(inbox_id, to_submit)?;
    }

    // The queue is cloned out of the table so the park can borrow it without holding the table.
    let queue = waiters_of(inbox_id)?;
    loop {
        let (count, dropped) = completion_state(inbox_id)?;

        if count >= min_complete || min_complete == 0 {
            return Ok(count);
        }

        if non_blocking {
            return Ok(count);
        }

        // A ring that has dropped a completion must not be slept on: the one this thread awaits may be it.
        if dropped > 0 {
            return Ok(count);
        }

        if deadline.reached(crate::clock::now()) {
            return Ok(count);
        }

        // The recheck closure is this ring's own condition, not mere readiness — else a waiter for `min_complete` spins.
        let parkable = scheduler::Parkable::at_entry();
        if completion::wait_until(
            &parkable,
            completion::Subject::of(&queue),
            completion::Token::new(inbox_id.0 as u64),
            WaitClass::Io,
            deadline,
            || completion_count(inbox_id).map_or(true, |n| n >= min_complete),
        )
        .is_err()
        {
            return Err(SyscallError::Gone);
        }
    }
}

/// This ring's completion waiter set, cloned out of the table.
fn waiters_of(inbox_id: InboxId) -> Result<Arc<Watch>, SyscallError> {
    with_instance(inbox_id, |inst| inst.watch.clone())
}

/// Reads and processes submissions from the submission ring; unclamped inputs are refused, not silently clamped.
fn submit_submissions(inbox_id: InboxId, count: u32) -> Result<(), SyscallError> {
    if count > with_instance(inbox_id, |inst| inst.submission_size)? {
        return Err(SyscallError::InvalidArgument);
    }
    for _ in 0..count {
        let Some(submission) = claim_submission(inbox_id)? else { break };
        process_submission(inbox_id, &submission);
    }
    Ok(())
}

/// Takes the submission at the ring head, advancing it; `None` when the ring is empty.
/// Submissions are claimed one at a time under the lock, never batched into a `Vec` sized by userland's own count.
fn claim_submission(inbox_id: InboxId) -> Result<Option<Submission>, SyscallError> {
    with_instance(inbox_id, |instance| {
        let head = instance.submission_head().load(Ordering::Acquire);
        let tail = instance.submission_tail().load(Ordering::Acquire);
        let available = tail.wrapping_sub(head);
        if available == 0 {
            return Ok(None);
        }
        if available > instance.submission_size {
            return Err(SyscallError::InvalidArgument);
        }
        let submission = instance.submission_at(head & (instance.submission_size - 1));
        instance.submission_head().store(head.wrapping_add(1), Ordering::Release);
        Ok(Some(submission))
    })?
}

fn with_instance<R>(inbox_id: InboxId, f: impl FnOnce(&Inbox) -> R) -> Result<R, SyscallError> {
    let guard = INBOXES.lock();
    let map = guard.as_ref().expect("inbox not initialized");
    Ok(f(map.get(inbox_id).ok_or(SyscallError::NotFound)?))
}

/// Process a single submission.
fn process_submission(inbox_id: InboxId, submission: &Submission) {
    let op = match Op::from_raw(submission.op) {
        Ok(op) => op,
        Err(_) => {
            post_completion_locked(inbox_id, submission.token, -(SyscallError::InvalidArgument as i32), 0);
            return;
        }
    };

    match op {
        Op::Nop => {
            post_completion_locked(inbox_id, submission.token, 0, 0);
        }
        Op::Watch => {
            process_watch(inbox_id, submission);
        }
        Op::Accept => {
            process_accept(inbox_id, submission);
        }
    }
}

/// Registers an `OP_WATCH`, or answers it immediately; every refusal posts a completion rather than going silent.
fn process_watch(inbox_id: InboxId, submission: &Submission) {
    let handle = submission.handle;
    let flags = WatchFlags::from_raw(submission.op_flags);
    let user_data = submission.token;

    // Readiness is checked on the process's table, not the thread's: a ring is process-wide.
    let resolved = process::with_process_data(|data| {
        let object = data.handles.get_ref(handle, Rights::WAIT)?;
        let readiness = Readiness {
            readable: flags.readable() && ops::has_data(object),
            writable: flags.writable() && ops::has_space(object),
        };
        let rsrc = if flags.readable() { ops::read_source(object) } else { None };
        let wsrc = if flags.writable() { ops::write_source(object) } else { None };
        Ok::<_, crate::object::HandleError>((readiness, rsrc, wsrc))
    });
    let (readiness, read_source, write_source) = match resolved {
        Ok(seen) => seen,
        // Nothing is held here: `with_process_data` has given the guard up.
        Err(e) => {
            let refusal = e.refuse_as_error();
            post_completion_locked(inbox_id, user_data, -(refusal as i32), 0);
            return;
        }
    };

    if readiness.readable || readiness.writable {
        // Ready already: complete now, one-shot, with the directions that fired.
        post_completion_locked(inbox_id, user_data, readiness.result_flags() as i32, 0);
        return;
    }

    // No readiness in either direction: nothing could ever complete this poll, so it is refused, not registered.
    let Some(sources) = Watched::of(read_source, write_source) else {
        post_completion_locked(inbox_id, user_data, -(SyscallError::NotSupported as i32), 0);
        return;
    };

    // The old poll on this handle is taken first, so its unregistration can't undo this one's registration.
    let mut woken: Option<Arc<Watch>> = None;
    let mut guard = INBOXES.lock();
    let map = guard.as_mut().expect("inbox not initialized");
    if let Some(instance) = map.get_mut(inbox_id) {
        if let Some(pos) = instance.pending_watches.iter().position(|pp| pp.handle == handle) {
            take_poll(instance, pos);
        }

        // The cap is checked before registering: registering first would leave watcher lists naming a poll that was never pushed.
        if instance.pending_watches.len() >= MAX_PENDING_WATCHES {
            instance.post_completion(user_data, -(SyscallError::ResourceExhausted as i32), 0);
            let watch = instance.watch.clone();
            drop(guard);
            completion::post(completion::Subject::of(&watch), completion::Outcome::Ready);
            return;
        }

        for src in sources.iter() {
            src.add_watcher(inbox_id);
        }
        instance.pending_watches.push(PendingWatch { user_data, handle, sources });

        // Recheck closes the TOCTOU window: a concurrent wake either ran already (caught here) or is blocked on INBOXES (finds the poll after release).
        let became_ready =
            instance.pending_watches.last().expect("the poll just pushed").sources.is_ready();
        if became_ready {
            if let Some(pos) = instance.pending_watches.iter().position(|pp| pp.handle == handle) {
                let pp = take_poll(instance, pos);
                instance.post_completion(pp.user_data, pp.sources.readiness().result_flags() as i32, 0);
                woken = Some(instance.watch.clone());
            }
        }
    }
    drop(guard);
    if let Some(watch) = woken {
        completion::post(completion::Subject::of(&watch), completion::Outcome::Ready);
    }
}

/// The submission form of `SYS_ACCEPT`; refusals fold into one `-InvalidArgument` completion instead of ending the process.
fn process_accept(inbox_id: InboxId, submission: &Submission) {
    let user_data = submission.token;

    let acceptor = process::with_process_data(|data| {
        data.handles.get::<crate::object::port::Acceptor>(submission.handle, Rights::READ)
    });

    let acceptor = match acceptor {
        Ok(a) => a,
        // Nothing held: `with_process_data` has given the guard up.
        Err(e) => {
            let refusal = e.refuse_as_error();
            post_completion_locked(inbox_id, user_data, -(refusal as i32), 0);
            return;
        }
    };

    match acceptor.pop() {
        Some(conn) => {
            let installed = process::with_process_data(|data| {
                ops::install(
                    &mut data.handles,
                    KObjectRef::Connection(crate::object::service::ConnectionEnd::new(
                        conn.rx,
                        conn.tx,
                        conn.inbox,
                        conn.outbox,
                    )),
                )
            });
            match installed {
                Ok(h) => post_completion_locked(inbox_id, user_data, h.0 as i32, 0),
                Err(e) => post_completion_locked(inbox_id, user_data, -(e as i32), 0),
            }
        }
        None => {
            post_completion_locked(inbox_id, user_data, -(SyscallError::WouldBlock as i32), 0);
        }
    }
}

/// Posts a completion and wakes this ring's waiters; a sibling thread parked in `submit` needs the announcement.
fn post_completion_locked(inbox_id: InboxId, user_data: u64, result: i32, flags: u32) {
    let guard = INBOXES.lock();
    let map = guard.as_ref().expect("inbox not initialized");
    let woken = map.get(inbox_id).map(|instance| {
        instance.post_completion(user_data, result, flags);
        instance.watch.clone()
    });
    drop(guard);
    if let Some(watch) = woken {
        completion::post(completion::Subject::of(&watch), completion::Outcome::Ready);
    }
}


/// Completes pending polls registered on `event`; callers must have released
/// source locks first. Private: every event reaches it through [`Source::wake`],
/// which cannot fire it without also waking the direct blocker.
fn complete_pending_for_event(watchers: &[InboxId], event: &Source) {
    if watchers.is_empty() { return; }

    // Wake happens after the table lock releases: a wake may send an IPI, which doesn't need INBOXES.
    let mut to_wake: Vec<Arc<Watch>> = Vec::new();
    let mut guard = INBOXES.lock();
    let map = guard.as_mut().expect("inbox not initialized");

    for &inbox_id in watchers {
        let Some(instance) = map.get_mut(inbox_id) else { continue };

        let mut i = 0;
        while i < instance.pending_watches.len() {
            if instance.pending_watches[i].watches(event) {
                let pp = take_poll(instance, i);
                // The direction `event` proves fired, OR-ed with a recheck of the
                // other registered direction — never the request mask.
                let result = pp.sources.readiness_for(event).result_flags();
                instance.post_completion(pp.user_data, result as i32, 0);
            } else {
                i += 1;
            }
        }

        to_wake.push(instance.watch.clone());
    }
    drop(guard);
    for watch in to_wake {
        completion::post(completion::Subject::of(&watch), completion::Outcome::Ready);
    }
}

/// Cancels every pending poll on a source going away, across every ring watching it — matched by source, never by handle.
/// Every cancelled poll must wake its ring, or a `u64::MAX` waiter parks forever.
pub fn cancel_by_source(sources: &[Option<EndedSource>]) {
    let mut affected: Vec<InboxId> = Vec::new();
    for EndedSource(source) in sources.iter().flatten() {
        for &id in source.watchers().iter() {
            if !affected.contains(&id) {
                affected.push(id);
            }
        }
    }

    if affected.is_empty() { return; }

    let watches_a_closing_source =
        |pp: &PendingWatch| sources.iter().flatten().any(|EndedSource(s)| pp.watches(s));

    let mut to_wake: Vec<Arc<Watch>> = Vec::new();
    let mut guard = INBOXES.lock();
    let map = guard.as_mut().expect("inbox not initialized");
    for inbox_id in affected {
        if let Some(instance) = map.get_mut(inbox_id) {
            let mut i = 0;
            let mut cancelled = false;
            while i < instance.pending_watches.len() {
                if watches_a_closing_source(&instance.pending_watches[i]) {
                    let pp = take_poll(instance, i);
                    instance.post_completion(pp.user_data, -(SyscallError::NotFound as i32), 0);
                    cancelled = true;
                } else {
                    i += 1;
                }
            }
            if cancelled {
                to_wake.push(instance.watch.clone());
            }
        }
    }
    drop(guard);
    for watch in to_wake {
        completion::post(completion::Subject::of(&watch), completion::Outcome::Ready);
    }
}


impl Source {
    /// Is the object ready right now? Called under the INBOXES lock, during the TOCTOU recheck in `process_watch`.
    fn is_ready(&self) -> bool {
        match self {
            Self::PipeReadable(id) => pipe::has_data(*id),
            Self::PipeWritable(id) => pipe::has_space(*id),
            Self::Port(p) => p.has_pending(),
            Self::Keyboard => crate::keyboard::has_data(),
            Self::Mouse => crate::mouse::has_data(),
            Self::Network => crate::net::has_packet(),
            Self::VirtioSound => crate::drivers::virtio_sound::has_pending(),
            Self::Hda => crate::drivers::hda::has_pending(),
            // Always false: the kernel holds no reader cursor to answer readiness with.
            Self::Log => false,
        }
    }

    fn add_watcher(&self, inbox_id: InboxId) {
        match self {
            Self::PipeReadable(pipe_id) | Self::PipeWritable(pipe_id) => {
                pipe::add_inbox_watcher(*pipe_id, inbox_id);
            }
            Self::Keyboard => crate::keyboard::add_inbox_watcher(inbox_id),
            Self::Mouse => crate::mouse::add_inbox_watcher(inbox_id),
            Self::Network => crate::net::add_inbox_watcher(inbox_id),
            Self::VirtioSound => crate::drivers::virtio_sound::add_inbox_watcher(inbox_id),
            Self::Hda => crate::drivers::hda::add_inbox_watcher(inbox_id),
            Self::Log => crate::log::user::add_inbox_watcher(inbox_id),
            Self::Port(p) => p.add_watcher(inbox_id),
        }
    }

    fn remove_watcher(&self, inbox_id: InboxId) {
        match self {
            Self::PipeReadable(pipe_id) | Self::PipeWritable(pipe_id) => {
                pipe::remove_inbox_watcher(*pipe_id, inbox_id);
            }
            Self::Keyboard => crate::keyboard::remove_inbox_watcher(inbox_id),
            Self::Mouse => crate::mouse::remove_inbox_watcher(inbox_id),
            Self::Network => crate::net::remove_inbox_watcher(inbox_id),
            Self::VirtioSound => crate::drivers::virtio_sound::remove_inbox_watcher(inbox_id),
            Self::Hda => crate::drivers::hda::remove_inbox_watcher(inbox_id),
            Self::Log => crate::log::user::remove_inbox_watcher(inbox_id),
            Self::Port(p) => p.remove_watcher(inbox_id),
        }
    }

    fn watchers(&self) -> Vec<InboxId> {
        match self {
            Self::PipeReadable(pipe_id) | Self::PipeWritable(pipe_id) => {
                pipe::inbox_watchers(*pipe_id)
            }
            Self::Keyboard => crate::keyboard::inbox_watchers(),
            Self::Mouse => crate::mouse::inbox_watchers(),
            Self::Network => crate::net::inbox_watchers(),
            Self::VirtioSound => crate::drivers::virtio_sound::inbox_watchers(),
            Self::Hda => crate::drivers::hda::inbox_watchers(),
            Self::Log => crate::log::user::inbox_watchers(),
            Self::Port(p) => p.watchers(),
        }
    }

    /// The subject a thread blocked in a plain syscall on this source parks on.
    /// Exhaustive on purpose: a new source cannot be added without deciding it,
    /// which is the half the 7a cutover deleted and nothing caught for months.
    fn wake_direct_blocker(&self) {
        match self {
            Self::Keyboard => crate::keyboard::wake_waiters(),
            Self::VirtioSound | Self::Hda => {
                crate::sched::waitqs::wake_device(&crate::sched::waitqs::AUDIO_WATCH)
            }
            Self::PipeReadable(id) => scheduler::wake_pipe_readers(*id),
            Self::PipeWritable(id) => scheduler::wake_pipe_writers(*id),
            Self::Port(p) => {
                completion::post(completion::Subject::of(p.watch()), completion::Outcome::Ready)
            }
            // No blocking-syscall queue: an empty read answers `NotFound` and never parks
            // (mouse, network); the log is edge-triggered on the reader's own cursor.
            Self::Mouse | Self::Network | Self::Log => {}
        }
    }

    /// Both wakes an event on this source owes, as one act: the thread blocked
    /// directly in a syscall, and every io_uring ring that registered a
    /// `POLL_ADD`. Neither half is reachable without the other — the pairing
    /// that was a hand-kept invariant before this.
    pub fn wake(&self) {
        self.wake_direct_blocker();
        let watchers = self.watchers();
        if !watchers.is_empty() {
            complete_pending_for_event(&watchers, self);
        }
    }
}
