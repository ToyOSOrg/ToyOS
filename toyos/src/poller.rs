//! Event-driven I/O polling on an [inbox](toyos_abi::inbox).

use core::cell::RefCell;
use core::sync::atomic::{AtomicU32, Ordering};
use toyos_abi::RawHandle;
use toyos_abi::{clock, syscall};
use toyos_abi::inbox::{
    Submission, Completion, RingHeader, RingLayout,
    OP_WATCH, SUBMISSION_RING_OFF, COMPLETION_RING_OFF, SUBMISSIONS_OFF,
};
use crate::AsHandle;

pub use toyos_abi::inbox::{READABLE, WRITABLE};

/// The inbox page, and the only thing in this crate that touches it.
///
/// **The kernel is the second writer of every byte below, so no Rust reference
/// covers any of it.** `inbox::post_completion` stores a whole [`Completion`]
/// and publishes the tail; `claim_submission` reads a submission slot and
/// advances the submission head. A `&T` carries `dereferenceable` into LLVM —
/// and for a `T` with no interior mutability `noalias` and `readonly` too — so
/// a `&RingHeader` here is a data race on `ring_size` whatever is then read
/// through it, and `&mut Submission`/`&Completion` (all integers, therefore
/// `Freeze`) are borrows the compiler is entitled to fold, hoist or split
/// against a kernel that is writing the same bytes.
///
/// So a ring header is reached one atomic word at a time (`AtomicU32::from_ptr`,
/// which is the only way to do an atomic operation on memory Rust does not own
/// and is *sound* over shared memory: an atomic's `UnsafeCell` is what withdraws
/// `noalias`/`readonly`), and an entry is one `ptr::write` or one
/// `read_volatile` of the whole struct — one access, not one the compiler may
/// split, fold or repeat.
///
/// This is the mirror of `kernel/src/inbox.rs`'s accessor block: the field
/// offsets come from `offset_of!` at both ends and `toyos_abi::inbox`'s
/// `RING_*_OFF` constants are what a reordering meets. `ring_size` has no
/// accessor at either end — both hold the sizes themselves rather than reading
/// them back out of a page the other side can write.
struct Rings {
    base: *mut u8,
    submission_ring_size: u32,
    completion_ring_size: u32,
}

impl Rings {
    /// Read the layout the kernel wrote at offset 0, and take the ring sizes
    /// from it.
    ///
    /// One `read_volatile` of the whole struct rather than a `&RingLayout`.
    /// The kernel writes this before the page is mapped and never again
    /// (`SharedMemObject::phys_before_mapping` is what enforces the order), so
    /// there is no race to lose here — but a rule with an exception "for the
    /// field nobody rewrites" is a rule that has to be re-derived every time
    /// somebody adds a field.
    ///
    /// # Safety
    ///
    /// `base` must be the address of a live inbox page, laid out by the kernel
    /// and mapped for this process's lifetime — which is what
    /// [`syscall::inbox_setup`] answers with.
    unsafe fn over(base: *mut u8) -> Self {
        // SAFETY: the caller guarantees `base` is a live, kernel-laid-out inbox
        // page. `RingLayout` is `#[repr(C)]` over integers, so every bit
        // pattern is a value, and offset 0 of a 2 MiB page is aligned for it.
        let layout = unsafe { (base as *const RingLayout).read_volatile() };
        Self {
            base,
            submission_ring_size: layout.submission_ring_size,
            completion_ring_size: layout.completion_ring_size,
        }
    }

    /// One atomic word of one ring header.
    ///
    /// `&AtomicU32` and never `&RingHeader` — the type's own header says why.
    fn ring_word(&self, ring_off: u64, field_off: usize) -> &AtomicU32 {
        // SAFETY: `base` is the whole 2 MiB inbox page, live for this
        // `Poller`'s lifetime, and both ring offsets (0x1000 and 0x2000) plus
        // `size_of::<RingHeader>()` are far inside it. The offsets are
        // page-aligned and `field_off` is `offset_of!` over a `#[repr(C)]`
        // struct of `u32`-sized fields, so the result is 4-aligned, which is
        // what `AtomicU32` needs. The `&AtomicU32` is sound over a page the
        // kernel writes because an atomic is exactly the type that says so:
        // its `UnsafeCell` withdraws `noalias`/`readonly`, and every access
        // through it is an atomic operation.
        //
        // Irreducible: `AtomicU32::from_ptr` is the only way to perform an
        // atomic operation on memory Rust does not own, and a shared ring is
        // memory Rust cannot own.
        unsafe { AtomicU32::from_ptr(self.base.add(ring_off as usize + field_off) as *mut u32) }
    }

    fn submission_head(&self) -> &AtomicU32 {
        self.ring_word(SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, head))
    }

    fn submission_tail(&self) -> &AtomicU32 {
        self.ring_word(SUBMISSION_RING_OFF, core::mem::offset_of!(RingHeader, tail))
    }

    fn completion_head(&self) -> &AtomicU32 {
        self.ring_word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, head))
    }

    fn completion_tail(&self) -> &AtomicU32 {
        self.ring_word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, tail))
    }

    fn completion_dropped(&self) -> &AtomicU32 {
        self.ring_word(COMPLETION_RING_OFF, core::mem::offset_of!(RingHeader, dropped))
    }

    /// Put one whole submission in the slot `index` names.
    ///
    /// One store of the whole entry, before the tail publishes it: a
    /// `&mut Submission` is a borrow the compiler may assume exclusive over a
    /// page the kernel also maps, and field-by-field assignment is several
    /// stores it is free to reorder against each other.
    fn write_submission(&self, index: u32, entry: Submission) {
        // SAFETY: `index` is masked by `submission_ring_size` at the one call
        // site, and that size is a power of two no greater than
        // `MAX_HANDLES` (256), so the furthest entry ends at `SUBMISSIONS_OFF`
        // (0x4000) + 256 * `size_of::<Submission>()`, inside the 2 MiB page.
        // `SUBMISSIONS_OFF` is page-aligned and `Submission` is 8-aligned with
        // a size that is a multiple of 8, so every entry is aligned. Nothing
        // else in this process writes the page — `Poller` owns it — and the
        // kernel only reads a slot the tail below has published.
        //
        // Irreducible: the entry is at a fixed offset in shared memory and the
        // safe spelling of a store into one is a reference, which is the
        // borrow this type refuses.
        unsafe {
            (self.base.add(
                SUBMISSIONS_OFF as usize + index as usize * core::mem::size_of::<Submission>(),
            ) as *mut Submission)
                .write(entry);
        }
    }

    /// One completion entry, copied out.
    ///
    /// By value and by `read_volatile`, so what the caller goes on to decide
    /// with is a snapshot it took once — the mirror of the kernel's
    /// `submission_at`.
    fn completion_at(&self, index: u32) -> Completion {
        // SAFETY: `index` is masked by `completion_ring_size` at the one call
        // site, which is twice the submission ring and so at most 512 entries
        // past `COMPLETION_RING_OFF` + `size_of::<RingHeader>()` — inside the
        // 2 MiB page, and 8-aligned because that offset is 16 past a page
        // boundary and `Completion` is 16 bytes. `Completion` is all integers,
        // so every bit pattern the kernel could have left is a value.
        unsafe {
            (self.base.add(
                COMPLETION_RING_OFF as usize
                    + core::mem::size_of::<RingHeader>()
                    + index as usize * core::mem::size_of::<Completion>(),
            ) as *const Completion)
                .read_volatile()
        }
    }

    /// Number of pending submissions (not yet flushed to the kernel).
    fn pending(&self) -> u32 {
        let head = self.submission_head().load(Ordering::Acquire);
        let tail = self.submission_tail().load(Ordering::Acquire);
        tail.wrapping_sub(head)
    }
}

/// The registrations that can still answer, at most one per handle.
///
/// **What the kernel hands back is the registration's number, never the
/// caller's token.** Watching a handle again replaces its registration — the
/// key `process_watch` withdraws an armed poll by — but an answer the replaced
/// one has posted stays in the ring, and so does one it posts after a
/// replacement that answered at once. Under the caller's token either reads as
/// news about the handle, of bytes already read or already announced. Under a
/// number, an answer that is not its handle's latest registration's names
/// nothing here and is dropped.
struct Registry {
    live: [Registration; MAX_LIVE],
    len: usize,
    /// Twice the poller's capacity: the handles it watches, and as many again
    /// closed since the last wait whose end the ring has not yet handed out.
    limit: usize,
    next: u64,
}

#[derive(Clone, Copy)]
struct Registration {
    handle: RawHandle,
    token: u64,
    number: u64,
}

const VACANT: Registration = Registration { handle: RawHandle(0), token: 0, number: 0 };

/// The widest registry, for a poller of [`Poller::MAX_HANDLES`].
const MAX_LIVE: usize = 2 * Poller::MAX_HANDLES as usize;

impl Registry {
    fn new(limit: usize) -> Self {
        Self { live: [VACANT; MAX_LIVE], len: 0, limit, next: 0 }
    }

    /// The number a registration of `handle` is submitted under; whatever the
    /// handle's earlier registration posts answers nothing from here on.
    fn register(&mut self, handle: RawHandle, token: u64) -> u64 {
        let registration = Registration { handle, token, number: self.next };
        self.next += 1;
        match self.live[..self.len].iter_mut().find(|r| r.handle == handle) {
            Some(replaced) => *replaced = registration,
            None => {
                assert!(
                    self.len < self.limit,
                    "Poller: {} handles hold a registration that has not answered, the most \
                     a poller of capacity {} keeps: it watches past its declared set",
                    self.len,
                    self.limit / 2,
                );
                self.live[self.len] = registration;
                self.len += 1;
            }
        }
        registration.number
    }

    /// The caller's token for the answer posted under `number`, which ends its
    /// registration, or `None` for one a later registration replaced.
    fn answer(&mut self, number: u64) -> Option<u64> {
        let at = self.live[..self.len].iter().position(|r| r.number == number)?;
        let token = self.live[at].token;
        self.len -= 1;
        self.live[at] = self.live[self.len];
        Some(token)
    }
}

/// An inbox, for watching handles for readiness.
///
/// Owns the inbox handle and shared memory mapping. Submissions are batched
/// and flushed on [`wait`](Self::wait).
///
/// **Deliberately not called `Inbox`**: the kernel already carries two objects
/// under that word, and `Poller` is what this type does for its caller.
///
/// **A poller has a declared capacity and cannot lose a completion inside it.**
/// [`new`](Self::new) takes the number of handles the caller will watch at once
/// and sizes both rings from it: the submission ring holds them all, so no
/// batch is ever flushed mid-registration, and the kernel's completion ring —
/// always twice the submission ring — holds the most completions that can exist
/// between two [`wait`](Self::wait) calls, which is two per watched handle (a
/// registration left over from the previous round firing, and this round's
/// registration finding the handle ready).
///
/// Going past the capacity is a contract violation and panics, because it is
/// the caller's own bug and the alternative is the failure this replaced: the
/// kernel silently dropping a completion and the caller blocking forever on
/// readiness that was thrown away. The capacity is the number of handles, not
/// the number of calls — re-registering the same handle within a round is
/// deduplicated by the kernel but still counts here, so declare the set.
///
/// [`wait`](Self::wait) reads the kernel's drop counter on every call — an
/// assert that should be unreachable, kept because that is the shape a
/// fail-fast check is supposed to have.
///
/// **A token [`wait`](Self::wait) hands out is the answer of its handle's
/// latest registration, and a registration answers once.** Watching a handle
/// replaces its earlier registration, and whatever that one posts, before the
/// replacement or after it, is dropped (`Registry`). So a caller that watches
/// a handle before every wait is never told twice of one arrival, nor of bytes
/// it read before that watch: what it is told of is there to read. A
/// registration left standing across waits answers whenever its handle turns
/// ready, so a caller that reads such a handle untold watches it again before
/// it waits.
pub struct Poller {
    inbox: RawHandle,
    rings: Rings,
    capacity: u32,
    registry: RefCell<Registry>,
}

// Safety: the base pointer is process-local shared memory mapped from the
// kernel. It is only ever reached through `Rings`, which takes no reference
// over it: atomics for the shared words, whole-value volatile copies for
// everything else. Not `Sync`: a watch moves the submission tail in two steps,
// and the registry is a `RefCell`.
unsafe impl Send for Poller {}

impl Poller {
    /// Widest handle set one poller can carry — the kernel's deepest
    /// submission ring, `MAX_SUBMISSION_DEPTH` in `kernel/src/inbox.rs`. A
    /// caller that must bound its own watched set has to bound it below this.
    pub const MAX_HANDLES: u32 = 256;

    /// Create a poller for `capacity` simultaneously watched handles.
    ///
    /// `capacity` is a declaration, not a hint: the rings are rounded up to the
    /// power of two that holds it, and registering past it panics. A capacity
    /// above [`MAX_HANDLES`] is refused rather than clamped: a clamp hands the
    /// caller a ring smaller than the set it just declared, which makes the
    /// loss reachable while looking like a success.
    pub fn new(capacity: u32) -> Self {
        assert!(
            capacity >= 1 && capacity <= Self::MAX_HANDLES,
            "Poller::new: {capacity} handles is outside 1..={}; \
             bound the watched set below the kernel's deepest ring",
            Self::MAX_HANDLES,
        );
        let entries = capacity.next_power_of_two();
        // The inbox owns its page and the kernel maps it: one call, and no
        // second lifetime for a mapping that is only ever this inbox's.
        let (inbox, base) = unsafe { syscall::inbox_setup(entries) }
            .expect("Poller::new: inbox_setup failed");
        // SAFETY: `base` is what `inbox_setup` just answered with — the
        // address of this process's own inbox page, laid out by the kernel
        // before it was mapped and unmapped only when the handle closes, which
        // is this `Poller`'s `Drop`.
        let rings = unsafe { Rings::over(base) };
        // The whole point of the sizing: `capacity` registrations fit the
        // submission ring with no mid-batch flush, and the completions they can
        // produce fit the completion ring.
        assert!(
            rings.submission_ring_size >= capacity && rings.completion_ring_size >= 2 * capacity,
            "Poller::new: kernel built {}/{} rings for {capacity} handles",
            rings.submission_ring_size,
            rings.completion_ring_size,
        );
        Self::over(inbox, rings, capacity)
    }

    fn over(inbox: RawHandle, rings: Rings, capacity: u32) -> Self {
        let registry = RefCell::new(Registry::new(2 * capacity as usize));
        Self { inbox, rings, capacity, registry }
    }

    /// Watch the given handle for readiness.
    ///
    /// `flags` are [`READABLE`] / [`WRITABLE`].
    /// `token` is returned in completions to identify which handle is ready.
    pub fn watch(&self, handle: &impl AsHandle, flags: u32, token: u64) {
        self.watch_raw(handle.as_handle(), flags, token);
    }

    /// Watch a raw handle for readiness.
    ///
    /// Prefer [`watch`](Self::watch) when you have a typed handle.
    pub fn watch_raw(&self, handle: RawHandle, flags: u32, token: u64) {
        // A panic, because this is first-party code exceeding a bound it
        // declared itself. A mid-batch flush here instead would make
        // completions reachable while the caller is still registering, and
        // past the completion ring the kernel drops them and the caller blocks
        // forever on readiness it was told about. With the ring sized for
        // `capacity` this is unreachable.
        assert!(
            self.pending() < self.capacity,
            "Poller: {} handles registered since the last wait(), capacity is {}",
            self.pending(),
            self.capacity,
        );
        let number = self.registry.borrow_mut().register(handle, token);
        let tail = self.rings.submission_tail().load(Ordering::Acquire);
        let idx = tail & (self.rings.submission_ring_size - 1);
        self.rings.write_submission(
            idx,
            Submission {
                op: OP_WATCH,
                handle,
                op_flags: flags,
                token: number,
                ..Submission::default()
            },
        );
        self.rings.submission_tail().store(tail.wrapping_add(1), Ordering::Release);
    }

    /// Number of pending submissions (not yet flushed to the kernel).
    pub fn pending(&self) -> u32 {
        self.rings.pending()
    }

    /// Hand the queued submissions to the kernel.
    ///
    /// The `expect` is sound because every error `inbox_submit` can report is
    /// about an argument this type owns — an over-deep batch, or a handle that
    /// is not this poller's inbox. Nothing a peer process does reaches it: a
    /// timeout or an empty completion ring is `Ok`.
    fn submit(&self, min_complete: u32, timeout_nanos: u64) {
        let to_submit = self.pending();
        syscall::inbox_submit(self.inbox, to_submit, min_complete, timeout_nanos)
            .expect("Poller::submit: inbox_submit rejected the batch");
    }

    /// Submit pending entries and hand `f` the token of every answer, until at
    /// least `min_complete` have been handed out or `timeout_nanos` has passed
    /// — `0` looks once, `u64::MAX` never passes.
    pub fn wait(&self, min_complete: u32, timeout_nanos: u64, mut f: impl FnMut(u64)) {
        self.wait_on(
            min_complete,
            timeout_nanos,
            &mut f,
            |min, nanos| self.submit(min, nanos),
            clock::nanos_since_boot,
        );
    }

    /// [`wait`](Self::wait) over the kernel's half and a clock it is handed,
    /// so a host test can hand it fakes.
    ///
    /// The kernel counts a dropped answer towards `min_complete` and returns
    /// for it, so the wait goes on, for what is left of its time, until the
    /// answers handed out make up the count.
    fn wait_on(
        &self,
        min_complete: u32,
        timeout_nanos: u64,
        f: &mut impl FnMut(u64),
        mut submit: impl FnMut(u32, u64),
        now: impl Fn() -> u64,
    ) {
        let deadline = now().saturating_add(timeout_nanos);
        let mut nanos = timeout_nanos;
        let mut handed = 0;
        loop {
            submit(min_complete - handed, nanos);
            handed += self.drain(f);
            if handed >= min_complete {
                return;
            }
            nanos = match timeout_nanos {
                0 => return,
                u64::MAX => u64::MAX,
                _ => match deadline.checked_sub(now()) {
                    Some(left) if left > 0 => left,
                    _ => return,
                },
            };
        }
    }

    /// Read every completion the kernel has published, oldest first, and hand
    /// `f` the token of each that answers a live registration; how many did.
    ///
    /// Split from [`wait`](Self::wait) because it is the half that is a pure
    /// function of the page: a host test can hand it a fake one.
    fn drain(&self, f: &mut impl FnMut(u64)) -> u32 {
        // Unreachable, and kept for that reason: `capacity` bounds the
        // registrations and the rings are sized from `capacity`, so nothing a
        // conforming caller does can make the kernel drop a completion here.
        // The counter is cumulative and never cleared, so if the reasoning is
        // wrong this fires and stays fired instead of turning into a caller
        // blocked forever on readiness that was thrown away.
        let dropped = self.rings.completion_dropped().load(Ordering::Relaxed);
        assert_eq!(
            dropped, 0,
            "Poller: the kernel dropped {dropped} completion(s) with capacity {} \
             and rings {}/{} — the sizing rule is wrong, not the caller.",
            self.capacity, self.rings.submission_ring_size, self.rings.completion_ring_size,
        );

        let mut handed = 0;
        loop {
            let head = self.rings.completion_head().load(Ordering::Acquire);
            let tail = self.rings.completion_tail().load(Ordering::Acquire);
            if head == tail {
                return handed;
            }
            let idx = head & (self.rings.completion_ring_size - 1);
            let completion = self.rings.completion_at(idx);
            self.rings.completion_head().store(head.wrapping_add(1), Ordering::Release);
            // Do not filter on `completion.result`. A negative result is the
            // kernel saying the registration is over and will never fire
            // (a watched handle's close answers every poll on a watch it ends
            // with `-NotFound`, i.e. on any peer disconnect), and the caller must react
            // to that exactly as to readiness — by looking at the handle again.
            // A zero result is meaningful too: `OP_ACCEPT` reports handle 0
            // that way.
            let Some(token) = self.registry.borrow_mut().answer(completion.token) else {
                continue;
            };
            f(token);
            handed += 1;
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        syscall::close(self.inbox);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use core::mem::ManuallyDrop;
    use toyos_abi::inbox::{
        RING_DROPPED_OFF, RING_HEAD_OFF, RING_SIZE_OFF, RING_TAIL_OFF,
    };

    /// A page the "kernel" and the test both write, standing in for the one
    /// `SYS_INBOX_SETUP` maps. `u64` cells so it is 8-aligned, which is what
    /// `Submission`, `Completion` and `RingLayout` need.
    struct FakePage(Vec<u64>);

    /// Big enough for the layout, both ring headers and a submission array of
    /// `MAX_HANDLES` entries: 0x4000 + 256 * 40, rounded up.
    const PAGE_BYTES: usize = 0x8000;

    impl FakePage {
        fn new(submission_ring_size: u32, completion_ring_size: u32) -> Self {
            let mut page = Self(vec![0u64; PAGE_BYTES / 8]);
            // The kernel's `write_ring_page`, in the test's own words: the
            // layout at 0 and a `ring_size` in each header.
            let base = page.base();
            // SAFETY: `base` is this `Vec`'s own storage, 8-aligned and
            // `PAGE_BYTES` long, and nothing else refers to it here.
            unsafe {
                (base as *mut RingLayout).write(RingLayout {
                    submission_ring_off: SUBMISSION_RING_OFF,
                    completion_ring_off: COMPLETION_RING_OFF,
                    submissions_off: SUBMISSIONS_OFF,
                    submission_ring_size,
                    completion_ring_size,
                    features: 0,
                    _pad: 0,
                });
            }
            page.put(SUBMISSION_RING_OFF as usize + RING_SIZE_OFF, submission_ring_size);
            page.put(COMPLETION_RING_OFF as usize + RING_SIZE_OFF, completion_ring_size);
            page
        }

        fn base(&mut self) -> *mut u8 {
            self.0.as_mut_ptr() as *mut u8
        }

        /// Write one `u32` at a byte offset, the way the kernel would.
        fn put(&mut self, at: usize, value: u32) {
            let base = self.base();
            // SAFETY: `at` is inside `PAGE_BYTES` at every call site below and
            // 4-aligned, and the storage is this `Vec`'s.
            unsafe { (base.add(at) as *mut u32).write_volatile(value) };
        }

        /// Read one `u32` at a byte offset.
        fn get(&mut self, at: usize) -> u32 {
            let base = self.base();
            // SAFETY: as `put`.
            unsafe { (base.add(at) as *const u32).read_volatile() }
        }

        /// Post a completion the way `inbox::post_completion` does: the whole
        /// entry, then a release store of the tail.
        fn post(&mut self, index: u32, entry: Completion) {
            let base = self.base();
            // SAFETY: `index` is under the ring size at every call site, so
            // the entry is inside `PAGE_BYTES`; `COMPLETION_RING_OFF` is
            // 8-aligned and `Completion` is 16 bytes.
            unsafe {
                (base.add(
                    COMPLETION_RING_OFF as usize
                        + core::mem::size_of::<RingHeader>()
                        + index as usize * core::mem::size_of::<Completion>(),
                ) as *mut Completion)
                    .write(entry);
            }
        }

        /// The submission in slot `index`, copied out as `submission_at` does.
        fn submission(&mut self, index: u32) -> Submission {
            let base = self.base();
            // SAFETY: `index` is under the submission ring size, so the slot
            // is inside `PAGE_BYTES`; `SUBMISSIONS_OFF` is page-aligned and
            // `Submission` is 40 bytes, 8-aligned.
            unsafe {
                (base.add(SUBMISSIONS_OFF as usize + index as usize * core::mem::size_of::<Submission>())
                    as *const Submission)
                    .read_volatile()
            }
        }
    }

    /// The kernel's half of a watch, as `process_watch` and an object's post
    /// do it, over a [`FakePage`]: a submission on a handle already ready is
    /// answered at once and leaves any earlier poll on it armed; one on a
    /// handle not ready withdraws the earlier poll and arms; a post answers
    /// every poll armed on its handle. Every answer carries its submission's
    /// token, as the kernel's do.
    struct FakeKernel {
        page: FakePage,
        ready: Vec<RawHandle>,
        armed: Vec<(RawHandle, u64)>,
    }

    /// A poller of `capacity` and the kernel behind its page. The poller is
    /// never dropped: its `Drop` is a syscall.
    fn pair(capacity: u32) -> (FakeKernel, ManuallyDrop<Poller>) {
        let entries = capacity.next_power_of_two();
        let mut page = FakePage::new(entries, 2 * entries);
        let poller = ManuallyDrop::new(Poller::over(RawHandle(0), rings(&mut page), capacity));
        (FakeKernel { page, ready: Vec::new(), armed: Vec::new() }, poller)
    }

    impl FakeKernel {
        /// `inbox_submit`'s first half: every queued submission, registered.
        fn submit(&mut self) {
            let head_at = SUBMISSION_RING_OFF as usize + RING_HEAD_OFF;
            let size = self.page.get(SUBMISSION_RING_OFF as usize + RING_SIZE_OFF);
            loop {
                let head = self.page.get(head_at);
                if head == self.page.get(SUBMISSION_RING_OFF as usize + RING_TAIL_OFF) {
                    return;
                }
                let s = self.page.submission(head & (size - 1));
                self.page.put(head_at, head.wrapping_add(1));
                if self.ready.contains(&s.handle) {
                    self.answer(s.token);
                } else {
                    self.armed.retain(|&(h, _)| h != s.handle);
                    self.armed.push((s.handle, s.token));
                }
            }
        }

        /// Bytes reach `handle`, and its post has not run yet.
        fn fill(&mut self, handle: RawHandle) {
            self.ready.push(handle);
        }

        /// `handle`'s post: every poll armed on it answers.
        fn post(&mut self, handle: RawHandle) {
            let (fired, armed): (Vec<_>, Vec<_>) =
                core::mem::take(&mut self.armed).into_iter().partition(|&(h, _)| h == handle);
            self.armed = armed;
            for (_, token) in fired {
                self.answer(token);
            }
        }

        /// Bytes reach `handle` and its post runs.
        fn arrive(&mut self, handle: RawHandle) {
            self.fill(handle);
            self.post(handle);
        }

        /// `handle`'s bytes are read by a call that did not ask the poller.
        fn take(&mut self, handle: RawHandle) {
            self.ready.retain(|&h| h != handle);
        }

        /// Answers posted and not yet drained.
        fn posted(&mut self) -> u32 {
            let head = self.page.get(COMPLETION_RING_OFF as usize + RING_HEAD_OFF);
            self.page.get(COMPLETION_RING_OFF as usize + RING_TAIL_OFF).wrapping_sub(head)
        }

        fn answer(&mut self, token: u64) {
            let tail_at = COMPLETION_RING_OFF as usize + RING_TAIL_OFF;
            let tail = self.page.get(tail_at);
            let size = self.page.get(COMPLETION_RING_OFF as usize + RING_SIZE_OFF);
            self.page.post(tail & (size - 1), Completion { token, result: READABLE as i32, flags: 0 });
            self.page.put(tail_at, tail.wrapping_add(1));
        }
    }

    /// Every token one drain hands out.
    fn drained(poller: &Poller) -> Vec<u64> {
        let mut seen = Vec::new();
        poller.drain(&mut |token| seen.push(token));
        seen
    }

    fn rings(page: &mut FakePage) -> Rings {
        // SAFETY: the page is laid out exactly as the kernel lays one out and
        // outlives the `Rings` at every call site.
        unsafe { Rings::over(page.base()) }
    }

    /// The accessors land on the offsets `toyos_abi` states, which is what the
    /// kernel's own `offset_of!`s resolve to.
    ///
    /// The two ends never meet in one binary — the kernel is not in the host
    /// workspace — so this is the SDK half of that agreement: the constants
    /// are the claim, `toyos_abi::inbox`'s `const _`s hold the kernel's
    /// `offset_of!` to them, and this holds the SDK's accessors to them.
    #[test]
    fn every_accessor_lands_on_the_abi_offset() {
        let mut page = FakePage::new(4, 8);
        let base = page.base() as usize;
        let r = rings(&mut page);
        let at = |w: &AtomicU32| w as *const AtomicU32 as usize - base;
        assert_eq!(at(r.submission_head()), SUBMISSION_RING_OFF as usize + RING_HEAD_OFF);
        assert_eq!(at(r.submission_tail()), SUBMISSION_RING_OFF as usize + RING_TAIL_OFF);
        assert_eq!(at(r.completion_head()), COMPLETION_RING_OFF as usize + RING_HEAD_OFF);
        assert_eq!(at(r.completion_tail()), COMPLETION_RING_OFF as usize + RING_TAIL_OFF);
        assert_eq!(at(r.completion_dropped()), COMPLETION_RING_OFF as usize + RING_DROPPED_OFF);
    }

    /// A submission is one whole entry at the slot the tail names, in the
    /// bytes the kernel's `submission_at` reads.
    #[test]
    fn a_submission_is_the_whole_entry_where_the_kernel_reads_it() {
        let mut page = FakePage::new(4, 8);
        {
            let r = rings(&mut page);
            r.write_submission(
                2,
                Submission {
                    op: OP_WATCH,
                    handle: RawHandle(9),
                    op_flags: READABLE,
                    token: 0xfeed,
                    ..Submission::default()
                },
            );
            r.submission_tail().store(3, Ordering::Release);
        }
        let at = SUBMISSIONS_OFF as usize + 2 * core::mem::size_of::<Submission>();
        let base = page.base();
        // SAFETY: the slot is inside `PAGE_BYTES` and 8-aligned.
        let entry = unsafe { (base.add(at) as *const Submission).read_volatile() };
        assert_eq!(entry.op, OP_WATCH);
        assert_eq!(entry.handle, RawHandle(9));
        assert_eq!(entry.op_flags, READABLE);
        assert_eq!(entry.token, 0xfeed);
        assert_eq!(page.get(SUBMISSION_RING_OFF as usize + RING_TAIL_OFF), 3);
    }

    /// **The negative control for the borrow this file refuses.**
    ///
    /// A "kernel" writes the completion tail *between* the two reads a drain
    /// makes, which is exactly what `post_completion` does on another CPU. The
    /// header is reached one `&AtomicU32` at a time, so the second read sees
    /// the new value and the second batch is drained. Write it instead as
    /// `&*(base.add(COMPLETION_RING_OFF) as *const RingHeader)` and the drain
    /// holds one snapshot of the header across the loop: the borrow is
    /// `Freeze`, LLVM is entitled to keep the first `tail` in a register, and
    /// the second batch is never seen.
    #[test]
    fn a_tail_the_kernel_publishes_mid_drain_is_observed() {
        let mut page = FakePage::new(4, 8);
        page.post(0, Completion { token: 11, result: 1, flags: 0 });
        page.put(COMPLETION_RING_OFF as usize + RING_TAIL_OFF, 1);

        let mut seen: Vec<u64> = Vec::new();
        {
            let r = rings(&mut page);
            loop {
                let head = r.completion_head().load(Ordering::Acquire);
                let tail = r.completion_tail().load(Ordering::Acquire);
                if head == tail {
                    break;
                }
                let idx = head & (r.completion_ring_size - 1);
                seen.push(r.completion_at(idx).token);
                r.completion_head().store(head.wrapping_add(1), Ordering::Release);
                // The kernel, on another CPU, one completion later.
                if seen.len() == 1 {
                    // SAFETY: slot 1 is inside the 8-entry ring.
                    unsafe {
                        (r.base.add(
                            COMPLETION_RING_OFF as usize
                                + core::mem::size_of::<RingHeader>()
                                + core::mem::size_of::<Completion>(),
                        ) as *mut Completion)
                            .write(Completion { token: 22, result: 1, flags: 0 });
                    }
                    r.completion_tail().store(2, Ordering::Release);
                }
            }
        }
        assert_eq!(seen, vec![11, 22]);
        // The head the kernel reads back is the one the drain published.
        assert_eq!(page.get(COMPLETION_RING_OFF as usize + RING_HEAD_OFF), 2);
    }

    /// The drop counter is where the kernel says it dropped one, and it is
    /// read out of the page rather than out of a snapshot of the header.
    #[test]
    fn the_drop_counter_is_read_from_the_page() {
        let mut page = FakePage::new(4, 8);
        page.put(COMPLETION_RING_OFF as usize + RING_DROPPED_OFF, 3);
        let r = rings(&mut page);
        assert_eq!(r.completion_dropped().load(Ordering::Relaxed), 3);
    }

    /// `pending` is the submission ring's own arithmetic, and it wraps.
    #[test]
    fn pending_counts_what_the_kernel_has_not_claimed() {
        let mut page = FakePage::new(4, 8);
        page.put(SUBMISSION_RING_OFF as usize + RING_HEAD_OFF, u32::MAX - 1);
        page.put(SUBMISSION_RING_OFF as usize + RING_TAIL_OFF, 1);
        let r = rings(&mut page);
        assert_eq!(r.pending(), 3);
    }

    const H: RawHandle = RawHandle(5);
    const G: RawHandle = RawHandle(6);

    /// A registration a wait did not see answer, answered by bytes a call that
    /// did not ask the poller read; the next watch finds the handle empty. The
    /// answer still in the ring announces bytes that are gone, and a reader
    /// that took it for news would block in its read.
    #[test]
    fn an_answer_for_bytes_already_read_is_not_handed_out() {
        let (mut kernel, poller) = pair(1);
        poller.watch_raw(H, READABLE, 7);
        kernel.submit();
        assert!(drained(&poller).is_empty());
        kernel.arrive(H);
        kernel.take(H);
        poller.watch_raw(H, READABLE, 7);
        kernel.submit();
        assert!(drained(&poller).is_empty());
        kernel.arrive(H);
        assert_eq!(drained(&poller), [7]);
    }

    /// A watch that finds its handle ready is answered at once and leaves the
    /// poll it replaced armed, which answers again when the post that made the
    /// handle ready runs.
    #[test]
    fn an_answer_the_replaced_registration_posts_late_is_not_handed_out() {
        let (mut kernel, poller) = pair(1);
        poller.watch_raw(H, READABLE, 1);
        kernel.submit();
        kernel.fill(H);
        poller.watch_raw(H, READABLE, 2);
        kernel.submit();
        kernel.post(H);
        assert_eq!(drained(&poller), [2]);
    }

    /// epoll(7): "Does an operation on a file descriptor affect the already
    /// collected but not yet reported events? … Modify will reread available
    /// I/O." An answer collected before its handle is watched again is
    /// reported once, under the new watch's token.
    #[test]
    fn a_handle_watched_again_is_answered_once_under_its_new_token() {
        let (mut kernel, poller) = pair(1);
        poller.watch_raw(H, READABLE, 1);
        kernel.submit();
        kernel.arrive(H);
        poller.watch_raw(H, READABLE, 2);
        kernel.submit();
        assert_eq!(drained(&poller), [2]);
    }

    /// A registration the caller does not renew is not replaced: it answers in
    /// whichever later wait its handle turns ready.
    #[test]
    fn a_registration_left_standing_still_answers() {
        let (mut kernel, poller) = pair(2);
        poller.watch_raw(H, READABLE, 3);
        poller.watch_raw(G, READABLE, 4);
        kernel.submit();
        assert!(drained(&poller).is_empty());
        poller.watch_raw(G, READABLE, 4);
        kernel.submit();
        kernel.arrive(H);
        assert_eq!(drained(&poller), [3]);
    }

    /// The kernel leaves a wait for an answer the poller then drops, as
    /// `an_answer_for_bytes_already_read_is_not_handed_out` sets up, and the
    /// wait goes on to the live one rather than returning with none.
    #[test]
    fn a_wait_sleeps_past_a_dropped_answer_to_the_live_one() {
        let (mut kernel, poller) = pair(1);
        poller.watch_raw(H, READABLE, 7);
        kernel.submit();
        kernel.arrive(H);
        kernel.take(H);
        poller.watch_raw(H, READABLE, 7);
        let mut submits = 0;
        let mut seen = Vec::new();
        let submit = |_, _| {
            kernel.submit();
            submits += 1;
            if submits == 2 {
                kernel.arrive(H);
            }
        };
        poller.wait_on(1, u64::MAX, &mut |token| seen.push(token), submit, || 0);
        assert_eq!((seen, submits), (vec![7], 2));
    }

    /// A wait woken only by answers it drops ends at its deadline, and sleeps
    /// only what is left of it.
    #[test]
    fn a_wait_woken_only_by_dropped_answers_ends_at_its_deadline() {
        let (mut kernel, poller) = pair(1);
        poller.watch_raw(H, READABLE, 7);
        kernel.submit();
        kernel.arrive(H);
        kernel.take(H);
        poller.watch_raw(H, READABLE, 7);
        let clock = Cell::new(100);
        let mut slept = Vec::new();
        let mut seen = Vec::new();
        let submit = |min, nanos| {
            kernel.submit();
            slept.push(nanos);
            // Back 300 ns later for what is posted, and at its deadline for nothing.
            clock.set(clock.get() + if kernel.posted() >= min { 300 } else { nanos });
        };
        poller.wait_on(1, 1_000, &mut |token| seen.push(token), submit, || clock.get());
        assert_eq!((seen, slept), (vec![], vec![1_000, 700]));
    }

    /// A wait of zero looks once, whatever it drops.
    #[test]
    fn a_wait_of_zero_looks_once() {
        let (mut kernel, poller) = pair(1);
        poller.watch_raw(H, READABLE, 7);
        kernel.submit();
        kernel.arrive(H);
        kernel.take(H);
        poller.watch_raw(H, READABLE, 7);
        let mut submits = 0;
        let submit = |_, _| {
            kernel.submit();
            submits += 1;
        };
        poller.wait_on(1, 0, &mut |token| panic!("handed out {token}"), submit, || 0);
        assert_eq!(submits, 1);
    }

    /// Past the handles it declared and as many again closed and unreported,
    /// a registration is refused by name.
    #[test]
    #[should_panic(expected = "watches past its declared set")]
    fn a_registration_past_twice_the_capacity_panics() {
        let (mut kernel, poller) = pair(1);
        for handle in 1..=3 {
            poller.watch_raw(RawHandle(handle), READABLE, 0);
            kernel.submit();
        }
    }
}
