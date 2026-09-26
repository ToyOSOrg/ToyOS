//! The kernel side of an [inbox](toyos_abi::inbox) — shared-memory submission
//! and completion rings. `inbox_setup` creates one; `inbox_submit` submits
//! and waits. The rings and submission array live in one 2 MiB page mapped
//! into both kernel and userspace. `OP_WATCH` fires once; userspace re-submits to re-arm.
//!
//! **A watch is a poll registered on the watched object's own
//! [`Watch`](crate::watch::Watch)**, one entry per direction it asked for, and
//! the object's post completes it. There is no table of sources here: what a
//! handle watches is `ops::read_watch`/`ops::write_watch`'s answer, and the
//! poll holds no reference to the object at all.
//!
//! **A completion is a trust boundary, and the kernel is the only writer of
//! its position.** `completion_tail` lives here, never in the page; the head
//! is the process's and is read once per post, so a head the process lies
//! about makes the kernel drop the completion and count it, never write
//! outside the ring. What a completion says comes from the kernel — the
//! caller's own `token`, and a result that is either the direction the object
//! posted or the refusal — so a process cannot make one appear in another
//! process's ring or say something no object said. A post writes at most one
//! entry, and a ring holds at most [`MAX_PENDING_WATCHES`] polls.
//!
//! **Lock order: an object's watch, then a ring's own lock, then the watch
//! its submitters park on.** A ring's lock takes nothing under it, and a
//! ring's watch holds only threads, because no handle names a ring as a thing
//! to watch.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use toyos_sched::task::WaitClass;
use toyos_sched::watch::{Fire, Ring};

use crate::object::shm::SharedMemObject;
use crate::object::{ops, KObjectRef};
use crate::process::{self, Pid};
use crate::scheduler;
use crate::sync::Lock;
use crate::time::{Deadline, Duration};
use crate::watch::Watch;
use crate::DirectMap;

use toyos_abi::inbox::{
    Completion, RingLayout, RingHeader, Submission,
    SUBMISSION_RING_OFF, COMPLETION_RING_OFF, SUBMISSIONS_OFF,
};
use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::SyscallError;

mod once;

use once::Once;

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
        // Taken out under the lock and let go of outside it: the unmap flushes.
        let Some(mut state) = self.0.state.lock().take() else {
            unreachable!("an inbox is torn down by its one reference, once");
        };
        for poll in state.pending.drain(..) {
            poll.withdraw();
        }
        // `Unmapped`'s drop flushes; the `Arc` drop after it frees the pages.
        drop(state.shm.unmap_from(state.owner_pid));
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
    /// hand-copied and unchecked, for the reason `arch/syscall/vm.rs`'s
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
}

/// One `OP_WATCH` a ring is waiting on: one-shot across every watch it is
/// registered on and against its own registrant's recheck.
pub struct Poll {
    inbox: Arc<Inbox>,
    user_data: u64,
    /// The handle the poll was submitted against; the dedup key.
    handle: RawHandle,
    /// Taken by exactly one of a fire and a withdrawal.
    state: Once,
}

impl Poll {
    /// Post this poll's completion if nothing has answered it yet.
    fn complete(&self, result: i32) {
        if self.state.fire() {
            self.inbox.complete(self.user_data, result);
        }
    }

    /// Answer nothing: a newer poll on the same handle replaced it, or its
    /// ring went away.
    fn withdraw(&self) {
        let _ = self.state.withdraw();
    }

    fn armed(&self) -> bool {
        self.state.armed()
    }
}

/// A poll as one watch holds it: the poll, and which of its directions this
/// watch is.
pub struct PollEntry {
    poll: Arc<Poll>,
    direction: Readiness,
}

impl Ring for PollEntry {
    fn fire(&self, how: Fire) {
        self.poll.complete(match how {
            // The direction this watch is: its object posted it.
            Fire::Ready => self.direction.result_flags() as i32,
            Fire::Gone => -(SyscallError::NotFound as i32),
        });
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
    /// Threads parked in `submit`; never a poll — see the lock order above.
    watch: Watch,
}

struct RingState {
    shm_phys: DirectMap,
    /// A ring's page has no lifetime of its own; it goes with the last handle to the ring.
    shm: Arc<SharedMemObject>,
    submission_size: u32,
    completion_size: u32,
    /// The kernel's own copy of the completion tail, the only one it reads.
    completion_tail: u32,
    /// Polls still armed as of the last registration, which sweeps the rest.
    pending: Vec<Arc<Poll>>,
    owner_pid: Pid,
}

impl RingState {
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
    /// A full ring is not fatal here: a poll completes on the poster's thread,
    /// which belongs to a different process.
    fn post_completion(&mut self, user_data: u64, result: i32, flags: u32) {
        let tail = self.completion_tail;
        if tail.wrapping_sub(self.completion_head().load(Ordering::Acquire)) >= self.completion_size {
            self.completion_dropped().fetch_add(1, Ordering::Relaxed);
            return;
        }
        let idx = tail & (self.completion_size - 1);
        // SAFETY: `idx` is masked to ring size; the ring's lock serializes kernel writers.
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

    /// Cumulative, never cleared.
    fn dropped(&self) -> u32 {
        self.completion_dropped().load(Ordering::Relaxed)
    }
}

impl Inbox {
    /// Post one completion and wake whoever waits in `submit`. A ring already
    /// torn down takes nothing and wakes nobody.
    fn complete(&self, user_data: u64, result: i32) {
        let posted = self.with_state(|state| state.post_completion(user_data, result, 0));
        if posted.is_ok() {
            self.watch.post();
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut RingState) -> R) -> Result<R, SyscallError> {
        self.state.lock().as_mut().map(f).ok_or(SyscallError::NotFound)
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
            shm,
            submission_size,
            completion_size,
            completion_tail: 0,
            pending: Vec::new(),
            owner_pid: pid,
        })),
        watch: Watch::new(),
    });
    Ok((InboxRef(inbox), shm_vaddr))
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
        let (count, dropped) = inbox.with_state(|s| (s.completion_count(), s.dropped()))?;

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
            || inbox.with_state(|s| s.completion_count()).map_or(true, |n| n >= min_complete),
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

/// Registers an `OP_WATCH`, or answers it immediately; every refusal posts a completion rather than going silent.
fn process_watch(inbox: &Arc<Inbox>, submission: &Submission) {
    let handle = submission.handle;
    let user_data = submission.token;
    let flags = match WatchFlags::from_raw(submission.op_flags) {
        Ok(flags) => flags,
        Err(e) => {
            inbox.complete(user_data, -(e as i32));
            return;
        }
    };

    // Readiness is checked on the process's table, not the thread's: a ring is process-wide.
    // The object is cloned out so the registration below holds no process lock.
    let resolved = process::with_process_data(|data| {
        data.handles.get_ref(handle, Rights::WAIT).cloned()
    });
    let object = match resolved {
        Ok(object) => object,
        // Nothing is held here: `with_process_data` has given the guard up.
        Err(e) => {
            let refusal = e.refuse_as_error();
            inbox.complete(user_data, -(refusal as i32));
            return;
        }
    };

    let readiness = readiness_of(&object, flags);
    if readiness.readable || readiness.writable {
        // Ready already: complete now, one-shot, with the directions that fired.
        inbox.complete(user_data, readiness.result_flags() as i32);
        return;
    }

    let read = if flags.readable() { ops::read_watch(&object) } else { None };
    let write = if flags.writable() { ops::write_watch(&object) } else { None };
    // No readiness in either direction: nothing could ever complete this poll, so it is refused, not registered.
    if read.is_none() && write.is_none() {
        inbox.complete(user_data, -(SyscallError::NotSupported as i32));
        return;
    }

    let poll = Arc::new(Poll {
        inbox: inbox.clone(),
        user_data,
        handle,
        state: Once::new(),
    });
    // The cap is checked before registering: registering first would leave a
    // watch holding a poll the ring never counted.
    let admitted = inbox.with_state(|state| {
        // The old poll on this handle answers nothing once this one replaces it.
        if let Some(at) = state.pending.iter().position(|p| p.handle == handle && p.armed()) {
            state.pending.swap_remove(at).withdraw();
        }
        state.pending.retain(|p| p.armed());
        if state.pending.len() >= MAX_PENDING_WATCHES {
            return false;
        }
        state.pending.push(poll.clone());
        true
    });
    match admitted {
        Ok(true) => {}
        Ok(false) => {
            inbox.complete(user_data, -(SyscallError::ResourceExhausted as i32));
            return;
        }
        // The ring's last handle closed under this submit.
        Err(_) => return,
    }

    // Registered with no ring lock held, then rechecked: a post either ran
    // before the registration — and the recheck sees what it changed — or
    // finds the entry. Whichever answers first answers alone.
    if let Some(watch) = &read {
        watch.add_poll(PollEntry { poll: poll.clone(), direction: Readiness { readable: true, writable: false } });
    }
    if let Some(watch) = &write {
        watch.add_poll(PollEntry { poll: poll.clone(), direction: Readiness { readable: false, writable: true } });
    }
    let now = readiness_of(&object, flags);
    if now.readable || now.writable {
        poll.complete(now.result_flags() as i32);
    }
}

/// Per-direction readiness of the object, restricted to what was asked for.
fn readiness_of(object: &KObjectRef, flags: WatchFlags) -> Readiness {
    Readiness {
        readable: flags.readable() && ops::has_data(object),
        writable: flags.writable() && ops::has_space(object),
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
