//! The kernel side of an [inbox](toyos_abi::inbox) — shared-memory submission
//! and completion rings. `inbox_setup` creates one; `inbox_submit` submits
//! and waits. The rings and submission array live in one 2 MiB page mapped
//! into both kernel and userspace. `OP_WATCH` fires once; userspace re-submits to re-arm.
//!
//! **A watch is a poll registered on the watched object's own
//! [`Watch`](crate::watch::Watch)**, one entry per direction it asked for, and
//! the object's post fires it. There is no table of sources here: what a
//! handle watches is `ops::read_watch`/`ops::write_watch`'s answer, and the
//! poll holds no reference to the object at all.
//!
//! **Only the ring's submitter writes a watch's answer, after a look**
//! ([`polls`]): a post owes the poll a look, and `submit` looks at the object
//! again before it writes anything, so an answer is never older than the wait
//! that returned it.
//!
//! **A completion is a trust boundary, and the kernel is the only writer of
//! its position.** `completion_tail` lives here, never in the page; the head
//! is the process's and is read once per write, so a head the process lies
//! about makes the kernel drop the completion and count it, never write
//! outside the ring. What a completion says comes from the kernel — the
//! caller's own `token`, and a result that is either the directions the object
//! was ready in or the refusal — so a process cannot make one appear in another
//! process's ring or say something no object said. A ring holds at most
//! [`MAX_PENDING_WATCHES`] polls.
//!
//! **Locks.** What a completion writes, and the page it is written into, sit
//! behind an [`IrqLock`] of their own; nothing is taken under it. The rest of a
//! ring, its submissions and its polls, is its `Lock`'s, which no post
//! reaches. A ring's own watch is an [`IrqWatch`] and holds only threads,
//! because no handle names a ring as a thing to watch.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use toyos_sched::sync::CellLock;
use toyos_sched::task::WaitClass;
use toyos_sched::watch::{Fire, Ring};

use crate::object::shm::SharedMemObject;
use crate::object::{ops, HandleError, KObjectRef};
use crate::process::{self, Pid};
use crate::scheduler;
use crate::sync::Lock;
use crate::time::{Deadline, Duration};
use crate::watch::{IrqLock, IrqWatch};
use crate::DirectMap;

use toyos_abi::inbox::{
    Completion, RingLayout, RingHeader, Submission,
    SUBMISSION_RING_OFF, COMPLETION_RING_OFF, SUBMISSIONS_OFF,
};
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;

mod once;
mod polls;

use polls::{Look, Polls, Submitter};

/// The one owned reference to a ring, held by its handle's object; dropping it
/// tears the ring down.
pub struct InboxRef(Arc<Inbox>);

impl InboxRef {
    pub fn inbox(&self) -> Arc<Inbox> {
        self.0.clone()
    }
}

impl Drop for InboxRef {
    fn drop(&mut self) {
        // The page is the completions', so it goes only once nothing can reach
        // them. Both halves are taken out under their locks and let go of
        // outside them: the unmap flushes.
        let Some(completions) = self.0.completions.with(Option::take) else {
            unreachable!("an inbox is torn down by its one reference, once");
        };
        let Some(mut state) = self.0.state.lock().take() else {
            unreachable!("an inbox is torn down by its one reference, once");
        };
        state.polls.withdraw_all();
        // `Unmapped`'s drop flushes; the `Arc` drop after it frees the pages.
        drop(completions.shm.unmap_from(state.owner_pid));
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
    pub const READABLE: Self = Self(toyos_abi::inbox::READABLE);
    pub const WRITABLE: Self = Self(toyos_abi::inbox::WRITABLE);
    /// Every bit `toyos_abi::inbox` defines for `Submission::op_flags`;
    /// hand-copied and unchecked, for the reason `syscall/vm.rs`'s
    /// `MMAP_PROT_KNOWN` gives for all four of these masks.
    const KNOWN: u32 = Self::READABLE.0 | Self::WRITABLE.0;

    /// A bit outside [`Self::KNOWN`] is an interest this kernel would register
    /// for neither direction, so the whole watch is refused rather than served.
    fn from_raw(raw: u32) -> Result<Self, SyscallError> {
        if raw & !Self::KNOWN != 0 {
            return Err(SyscallError::InvalidArgument);
        }
        Ok(Self(raw))
    }
    pub fn readable(self) -> bool { self.0 & Self::READABLE.0 != 0 }
    pub fn writable(self) -> bool { self.0 & Self::WRITABLE.0 != 0 }
    pub fn raw(self) -> u32 { self.0 }
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

    fn any(self) -> bool {
        self.readable || self.writable
    }
}

type Poll = polls::Poll<Arc<Inbox>>;

impl polls::Wake for Arc<Inbox> {
    fn owe(&self) {
        // Before the post, so the waiter it wakes finds the debt; in place,
        // because a device's interrupt handler fires polls.
        self.owed.store(true, Ordering::Release);
        self.watch.post_in_place();
    }
}

/// A poll as one watch holds it: the poll, and which of its directions this
/// watch is.
pub struct PollEntry {
    poll: Arc<Poll>,
    direction: WatchFlags,
}

impl Ring for PollEntry {
    fn fire(&self, how: Fire) {
        match how {
            Fire::Ready => self.poll.fire(self.direction.raw()),
            Fire::Gone => self.poll.end(),
        }
    }

    fn live(&self) -> bool {
        self.poll.armed()
    }
}

/// Hard cap on pending polls per ring.
const MAX_PENDING_WATCHES: usize = 1024;

/// A ring: what a poll posts into, and what `submit` parks on.
pub struct Inbox {
    /// `None` once the ring's one reference let go of it.
    state: Lock<Option<RingState>>,
    /// `None` from the moment that reference starts letting go of it.
    completions: IrqLock<Option<Completions>>,
    /// Threads parked in `submit`; never a poll — see the module header.
    watch: IrqWatch,
    /// A poll has been taken since `submit` last looked.
    owed: AtomicBool,
}

struct RingState {
    /// The page's address. The page is [`Completions`]'s, which the teardown
    /// lets go of only after it has taken this.
    shm_phys: DirectMap,
    submission_size: u32,
    polls: Polls<Arc<Inbox>>,
    owner_pid: Pid,
}

// No accessor below returns a Rust reference into a ring's page — the process
// maps it writable, so only atomics or `read_volatile` are sound here.

/// One atomic word of one ring header; never `&RingHeader` — see the block above.
fn ring_word(page: &DirectMap, ring_off: u64, field_off: usize) -> &core::sync::atomic::AtomicU32 {
    let ptr = page.as_mut_ptr::<u8>();
    // SAFETY: offset is in-bounds and 4-aligned within the 2 MiB page, which outlives both of the ring's halves that name it; `AtomicU32` is sound over memory the process also writes.
    unsafe {
        core::sync::atomic::AtomicU32::from_ptr(
            ptr.add(ring_off as usize + field_off) as *mut u32,
        )
    }
}

impl RingState {
    fn submission_head(&self) -> &core::sync::atomic::AtomicU32 {
        ring_word(&self.shm_phys, SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, head))
    }

    fn submission_tail(&self) -> &core::sync::atomic::AtomicU32 {
        ring_word(&self.shm_phys, SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, tail))
    }

    /// One submission entry, copied out by value via `read_volatile` — never a `&Submission`.
    fn submission_at(&self, index: u32) -> Submission {
        let ptr = self.shm_phys.as_mut_ptr::<u8>();
        // SAFETY: `index` is masked by `submission_size` (≤256), keeping the read in-bounds and aligned within the page.
        unsafe { (ptr.add(SUBMISSIONS_OFF as usize + index as usize * core::mem::size_of::<Submission>()) as *const Submission).read_volatile() }
    }
}

/// What a poll's completion writes, and the ring's page, which goes only
/// with these.
struct Completions {
    /// A ring's page has no lifetime of its own; it goes with the last handle to the ring.
    shm: Arc<SharedMemObject>,
    page: DirectMap,
    completion_size: u32,
    /// The kernel's own copy of the completion tail, the only one it reads.
    completion_tail: u32,
}

impl Completions {
    fn completion_head(&self) -> &core::sync::atomic::AtomicU32 {
        ring_word(&self.page, COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, head))
    }

    fn completion_tail_word(&self) -> &core::sync::atomic::AtomicU32 {
        ring_word(&self.page, COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, tail))
    }

    fn completion_dropped(&self) -> &core::sync::atomic::AtomicU32 {
        ring_word(&self.page, COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, dropped))
    }

    /// The address of one completion entry — a pointer, never a `&mut` minted from a shared borrow.
    fn completion_at(&self, index: u32) -> *mut Completion {
        let ptr = self.page.as_mut_ptr::<u8>();
        // SAFETY: `index` is masked by `completion_size` (≤512), keeping the offset inside the page.
        unsafe { ptr.add(COMPLETION_RING_OFF as usize + core::mem::size_of::<RingHeader>() + index as usize * core::mem::size_of::<Completion>()) as *mut Completion }
    }

    /// Posts a completion, or records a drop if the ring reports itself full.
    fn post_completion(&mut self, user_data: u64, result: i32, flags: u32) {
        let tail = self.completion_tail;
        if tail.wrapping_sub(self.completion_head().load(Ordering::Acquire)) >= self.completion_size {
            self.completion_dropped().fetch_add(1, Ordering::Relaxed);
            return;
        }
        let idx = tail & (self.completion_size - 1);
        // SAFETY: `idx` is masked to ring size; the completions' lock serializes kernel writers.
        unsafe { self.completion_at(idx).write(Completion { token: user_data, result, flags }) };
        self.completion_tail = tail.wrapping_add(1);
        self.completion_tail_word().store(tail.wrapping_add(1), Ordering::Release);
    }

    /// Available completions, measured against the kernel's own tail.
    /// A process that rewrites its own `head` can only mislead itself, never the kernel, about completions waiting.
    fn completion_count(&self) -> u32 {
        let head = self.completion_head().load(Ordering::Acquire);
        self.completion_tail.wrapping_sub(head)
    }

    fn room(&self) -> bool {
        self.completion_count() < self.completion_size
    }

    /// Cumulative, never cleared.
    fn dropped(&self) -> u32 {
        self.completion_dropped().load(Ordering::Relaxed)
    }
}

impl Inbox {
    /// Write one completion and wake whoever waits in `submit`. A ring already
    /// torn down takes nothing and wakes nobody.
    fn complete(&self, user_data: u64, result: i32) {
        let posted = self.completions.with(|c| {
            // Inside the section whatever lock it is, so `handler-post` reds
            // on one that leaves interrupts open.
            #[cfg(feature = "boot-actuators")]
            crate::watch::handler_post::raise_if_staged();
            c.as_mut().map(|c| c.post_completion(user_data, result, 0))
        });
        if posted.is_some() {
            // In place: an interrupt handler's post reaches here through the
            // poll it fires.
            self.watch.post_in_place();
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut RingState) -> R) -> Result<R, SyscallError> {
        self.state.lock().as_mut().map(f).ok_or(SyscallError::NotFound)
    }

    fn with_completions<R>(&self, f: impl FnOnce(&Completions) -> R) -> Result<R, SyscallError> {
        self.completions.with(|c| c.as_ref().map(f)).ok_or(SyscallError::NotFound)
    }
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

    let inbox = Arc::new(Inbox {
        state: Lock::new(Some(RingState {
            shm_phys,
            submission_size,
            polls: Polls::new(),
            owner_pid: pid,
        })),
        completions: IrqLock::new(Some(Completions {
            shm,
            page: shm_phys,
            completion_size,
            completion_tail: 0,
        })),
        watch: IrqWatch::new(),
        owed: AtomicBool::new(false),
    });
    Ok((InboxRef(inbox), shm_vaddr))
}

/// `handler-post`'s ring: the kernel's own, mapped into no process and
/// submitted to by nobody, which polls a watch and completes as a submission
/// does.
#[cfg(feature = "boot-actuators")]
pub(crate) struct Staged(Arc<Inbox>);

#[cfg(feature = "boot-actuators")]
impl Staged {
    pub(crate) fn new() -> Self {
        let depth = 2 * toyos_sched::watch::handler_post::HOLDS;
        let shm = SharedMemObject::create(crate::mm::PAGE_2M).expect("handler-post: a ring's page");
        let page = shm.phys_before_mapping();
        write_ring_page(page, depth, depth * 2);
        Self(Arc::new(Inbox {
            state: Lock::new(None),
            completions: IrqLock::new(Some(Completions {
                shm,
                page,
                completion_size: depth * 2,
                completion_tail: 0,
            })),
            watch: IrqWatch::new(),
            owed: AtomicBool::new(false),
        }))
    }

    /// A poll of this ring on `watch`, which that watch's next post fires.
    pub(crate) fn poll(&self, watch: &IrqWatch) {
        let poll = Arc::new(Poll::new(self.0.clone(), 0, RawHandle(0), WatchFlags::READABLE.raw()));
        watch.add_poll(PollEntry { poll, direction: WatchFlags::READABLE });
    }

    pub(crate) fn complete(&self) {
        self.0.complete(0, 0);
    }

    /// Run `f` holding this ring's own watch's list lock, as a registration
    /// in `submit` holds it.
    pub(crate) fn holding_its_watch(&self, f: impl FnOnce()) {
        self.0.watch.holding(f);
    }
}

/// Processes submissions and waits for completions; called from the syscall handler.
pub fn submit(
    inbox: &Arc<Inbox>,
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
        submit_submissions(inbox, to_submit)?;
    }

    loop {
        // Before the debt is read again: a fire after this swap sets it anew.
        // `AcqRel`, so the polls the swap clears the debt of are seen taken.
        inbox.owed.swap(false, Ordering::AcqRel);
        polls::deliver(|| inbox.with_state(|s| s.polls.take_owed()).ok().flatten(), inbox);
        let (count, dropped) = inbox.with_completions(|c| (c.completion_count(), c.dropped()))?;

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
        if crate::watch::wait_until(
            &parkable,
            &inbox.watch,
            0,
            WaitClass::Io,
            deadline,
            || {
                inbox.owed.load(Ordering::Acquire)
                    || inbox.with_completions(|c| c.completion_count()).map_or(true, |n| n >= min_complete)
            },
        )
        .is_err()
        {
            return Err(SyscallError::Gone);
        }
    }
}

/// Reads and processes submissions from the submission ring; unclamped inputs are refused, not silently clamped.
fn submit_submissions(inbox: &Arc<Inbox>, count: u32) -> Result<(), SyscallError> {
    if count > inbox.with_state(|s| s.submission_size)? {
        return Err(SyscallError::InvalidArgument);
    }
    for _ in 0..count {
        let Some(submission) = claim_submission(inbox)? else { break };
        process_submission(inbox, &submission);
    }
    Ok(())
}

/// Takes the submission at the ring head, advancing it; `None` when the ring is empty.
/// Submissions are claimed one at a time under the lock, never batched into a `Vec` sized by userland's own count.
fn claim_submission(inbox: &Inbox) -> Result<Option<Submission>, SyscallError> {
    inbox.with_state(|state| {
        let head = state.submission_head().load(Ordering::Acquire);
        let tail = state.submission_tail().load(Ordering::Acquire);
        let available = tail.wrapping_sub(head);
        if available == 0 {
            return Ok(None);
        }
        if available > state.submission_size {
            return Err(SyscallError::InvalidArgument);
        }
        let submission = state.submission_at(head & (state.submission_size - 1));
        state.submission_head().store(head.wrapping_add(1), Ordering::Release);
        Ok(Some(submission))
    })?
}

/// Process a single submission.
fn process_submission(inbox: &Arc<Inbox>, submission: &Submission) {
    // `Submission::flags` is declared and read by nothing, so a caller setting
    // it is asking for a behaviour that does not exist.
    if submission.flags != 0 {
        inbox.complete(submission.token, -(SyscallError::InvalidArgument as i32));
        return;
    }
    let op = match Op::from_raw(submission.op) {
        Ok(op) => op,
        Err(_) => {
            inbox.complete(submission.token, -(SyscallError::InvalidArgument as i32));
            return;
        }
    };

    match op {
        Op::Nop => inbox.complete(submission.token, 0),
        Op::Watch => process_watch(inbox, submission),
        Op::Accept => process_accept(inbox, submission),
    }
}

/// Takes an `OP_WATCH` as its handle's one poll; every refusal writes a completion rather than going silent.
fn process_watch(inbox: &Arc<Inbox>, submission: &Submission) {
    let user_data = submission.token;
    let flags = match WatchFlags::from_raw(submission.op_flags) {
        Ok(flags) => flags,
        Err(e) => {
            inbox.complete(user_data, -(e as i32));
            return;
        }
    };
    // Nothing is held here: `resolve` has given the guard up.
    let refused = resolve(submission.handle).map_err(HandleError::refuse_as_error).and_then(|object| {
        arm(inbox, Poll::new(inbox.clone(), user_data, submission.handle, flags.raw()), &object)
    });
    if let Err(refusal) = refused {
        inbox.complete(user_data, -(refusal as i32));
    }
}

/// The object `handle` names in this process's table, not the thread's: a ring is process-wide.
/// Cloned out, so what follows holds no process lock.
fn resolve(handle: RawHandle) -> Result<KObjectRef, HandleError> {
    process::with_process_data(|data| data.handles.get_ref(handle, Rights::WAIT).cloned())
}

/// Keep `poll` as its handle's one poll, and fire it if `object` is ready or
/// arm it on the object's watches if not; the refusal, when nothing could ever
/// answer it.
fn arm(inbox: &Arc<Inbox>, poll: Poll, object: &KObjectRef) -> Result<(), SyscallError> {
    let flags = WatchFlags(poll.flags);
    let ready = readiness_of(object, flags).any();
    let read = if flags.readable() { ops::read_watch(object) } else { None };
    let write = if flags.writable() { ops::write_watch(object) } else { None };
    // No readiness in either direction: nothing could ever answer this poll, so it is refused, not registered.
    if !ready && read.is_none() && write.is_none() {
        return Err(SyscallError::NotSupported);
    }

    let poll = Arc::new(poll);
    // The cap is checked before registering: registering first would leave a
    // watch holding a poll the ring never counted.
    match inbox.with_state(|state| state.polls.admit(poll.clone(), MAX_PENDING_WATCHES)) {
        Ok(true) => {}
        Ok(false) => return Err(SyscallError::ResourceExhausted),
        // The ring's last handle closed under this submit.
        Err(_) => return Ok(()),
    }
    if ready {
        poll.fire(0);
        return Ok(());
    }

    // Registered with no ring lock held, then rechecked: a post either ran
    // before the registration — and the recheck sees what it changed — or
    // finds the entry. Whichever fires first fires alone.
    if let Some(watch) = &read {
        watch.add_poll(PollEntry { poll: poll.clone(), direction: WatchFlags::READABLE });
    }
    if let Some(watch) = &write {
        watch.add_poll(PollEntry { poll: poll.clone(), direction: WatchFlags::WRITABLE });
    }
    if readiness_of(object, flags).any() {
        poll.fire(0);
    }
    Ok(())
}

/// Per-direction readiness of the object, restricted to what was asked for.
fn readiness_of(object: &KObjectRef, flags: WatchFlags) -> Readiness {
    Readiness {
        readable: flags.readable() && ops::has_data(object),
        writable: flags.writable() && ops::has_space(object),
    }
}

impl Submitter<Arc<Inbox>> for Arc<Inbox> {
    fn room(&self) -> bool {
        self.with_completions(Completions::room).unwrap_or(false)
    }

    fn answer(&self, user_data: u64, result: i32) {
        self.complete(user_data, result);
    }

    fn look(&self, poll: &Poll) -> Look {
        let object = match resolve(poll.handle) {
            Ok(object) => object,
            // Closed since it was watched, which is no bug of the process's:
            // the poll is over, as a close that ends it says.
            Err(HandleError::BadHandle | HandleError::Stale | HandleError::WrongType { .. }) => {
                return Look::Refused(SyscallError::NotFound);
            }
            Err(HandleError::Rights { .. }) => return Look::Refused(SyscallError::PermissionDenied),
            Err(HandleError::TableFull) => return Look::Refused(SyscallError::ResourceExhausted),
        };
        let flags = WatchFlags(poll.flags);
        let mut now = readiness_of(&object, flags);
        // A post on its read watch is the object's readability, and nothing
        // in the kernel could be looked at instead.
        now.readable |= flags.readable()
            && poll.posted() & WatchFlags::READABLE.raw() != 0
            && ops::read_posts_are_readiness(&object);
        if now.any() {
            return Look::Ready(now.result_flags());
        }
        match arm(self, Poll::new(self.clone(), poll.user_data, poll.handle, poll.flags), &object) {
            Ok(()) => Look::Armed,
            Err(refusal) => Look::Refused(refusal),
        }
    }
}

/// The submission form of `SYS_ACCEPT`; refusals fold into one `-InvalidArgument` completion instead of ending the process.
fn process_accept(inbox: &Inbox, submission: &Submission) {
    let user_data = submission.token;

    let acceptor = process::with_process_data(|data| {
        data.handles.get::<crate::object::port::Acceptor>(submission.handle, Rights::READ)
    });

    let acceptor = match acceptor {
        Ok(a) => a,
        // Nothing held: `with_process_data` has given the guard up.
        Err(e) => {
            let refusal = e.refuse_as_error();
            inbox.complete(user_data, -(refusal as i32));
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
                Ok(h) => inbox.complete(user_data, h.0 as i32),
                Err(e) => inbox.complete(user_data, -(e as i32)),
            }
        }
        None => inbox.complete(user_data, -(SyscallError::WouldBlock as i32)),
    }
}
