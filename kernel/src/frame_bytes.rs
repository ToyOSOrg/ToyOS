//! Bytes a kernel object holds for a reader, in 4 KiB frames counted to the
//! job that made the object and taken as it fills (`kernel::pipe::Bytes`): a
//! pipe's stream and a connection direction's messages.
//!
//! The frames are kernel-only and written before they are read, so they are
//! never zeroed. **The object's lock is never held across a user copy or the
//! frame allocator**: under it a write or a read claims its bytes and is handed
//! its first chunk's frames ([`FrameBytes::first_fill`],
//! [`FrameBytes::first_drain`]), and ends its claim; [`supply`], [`fill`],
//! [`drain`] and [`give_back`] take frames, copy and give frames back between
//! its holds. The claims are the data-race argument (`kernel::pipe::Bytes`).
//!
//! One claim of each kind at a time: another copy waits for its [`turn`], on
//! its end's watch, and each claim's end is answered by [`wake`].
//!
//! A small transfer is not claimed across a copy: it is copied between its
//! caller and a [`Bounce`] on the caller's stack with no lock held, and
//! between the bounce and the frames in one hold ([`FrameBytes::put`],
//! [`FrameBytes::take`]). One hold, where a claim takes two or more.

use alloc::collections::VecDeque;
use core::mem::MaybeUninit;
use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel::pipe::{Bytes, End, Ended, FRAME, MOST_FRAMES};
use kernel::sched::job::Job;
use kernel::sched::task::WaitClass;

use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::{pmm, DirectMap};
use crate::scheduler::Parkable;
use crate::sync::{Lock, LockGuard};
use crate::time::Deadline;
use crate::user_ptr::{Faulted, UserBytes, UserBytesMut};
use crate::watch::{self, Cancelled, Watch};

/// The frames and the heap their list takes, charged to one job; the frames
/// go back when it drops, which no claim may be held across.
pub struct FrameBytes {
    bytes: Bytes<u64>,
    list: Charge,
}

impl FrameBytes {
    pub fn new(job: &Arc<Job>) -> Self {
        Self { bytes: Bytes::new(), list: Charge::none(job) }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Room for a write to claim, up to the capacity.
    pub fn space(&self) -> usize {
        self.bytes.space()
    }

    /// Frames taken, spares included.
    pub fn frames(&self) -> usize {
        self.bytes.frames()
    }

    pub fn job(&self) -> &Arc<Job> {
        self.list.job()
    }

    /// Whether `end`'s claim is held, for a copy that will not wait for it.
    pub fn claim_held(&self, end: End) -> bool {
        self.bytes.claim_held(end)
    }

    /// Whether a copy at `end` may claim in this hold; `false` asks the
    /// claim held to wake it as it ends (`kernel::pipe::Bytes::turn`).
    pub fn turn(&mut self, end: End) -> bool {
        self.bytes.turn(end)
    }

    /// Claim `n` bytes of room, at most [`Self::space`], for one write, in
    /// its [`turn`].
    pub fn claim_write(&mut self, n: usize) {
        self.bytes.claim_write(n);
    }

    /// Hold the claimed write's bytes, ending its claim.
    pub fn commit(&mut self) -> Ended {
        self.bytes.commit()
    }

    pub fn abandon_write(&mut self) -> Ended {
        self.bytes.abandon_write()
    }

    /// Write `src`, at most [`Self::space`], whole in this hold, in its
    /// [`turn`]: `false`, claiming nothing, where it would want frames, which
    /// a claim takes ([`supply`]). It owes no wake.
    pub fn put(&mut self, src: &[u8]) -> bool {
        self.bytes.put(src.len(), |frame, within, at| {
            // SAFETY: `within` is inside the frame, which the direct map covers
            // and only this hold's write reaches; `src` holds `at + within.len()`.
            unsafe { copy_to_frame(*frame, within, &src[at..]) }
        })
    }

    /// Copy `src` into the claimed write, whose frames are all supplied, and
    /// commit it.
    pub fn place_all(&mut self, src: &[u8]) -> Ended {
        self.bytes.place(0, src.len(), |frame, within, at| {
            // SAFETY: as in `put`, under the claim this hold ends.
            unsafe { copy_to_frame(*frame, within, &src[at..]) }
        });
        self.bytes.commit()
    }

    /// Read the `n` oldest bytes, at most [`Self::len`] and `N`, into `into`
    /// whole in this hold, in its [`turn`]; answers the spares it leaves, for
    /// [`give_back`]. It owes no wake.
    pub fn take<const N: usize>(&mut self, n: usize, into: &mut Bounce<N>) -> Spares {
        assert!(n <= N, "frame_bytes: a {n}-byte take past a {N}-byte bounce");
        let to = into.bytes.as_mut_ptr().cast::<u8>();
        self.bytes.take(n, |frame, within, at| {
            // SAFETY: `within` is inside the frame, which the direct map
            // covers; `at + within.len()` is at most `n`, inside `into`.
            unsafe {
                let from = DirectMap::from_phys(*frame + within.start as u64).as_mut_ptr::<u8>();
                core::ptr::copy_nonoverlapping(from, to.add(at), within.len());
            }
        });
        into.len = n;
        Spares::take(self)
    }

    /// Claim the `n` oldest bytes, at most [`Self::len`], for one read, in
    /// its [`turn`].
    pub fn claim_read(&mut self, n: usize) {
        self.bytes.claim_read(n);
    }

    /// Let the claimed read's bytes go, ending its claim; answers the first
    /// of the spares it leaves past those kept, for [`give_back`].
    pub fn consume(&mut self) -> (Spares, Ended) {
        let ended = self.bytes.consume();
        (Spares::take(self), ended)
    }

    /// Whether a copy may still reach these frames: while it does, they are
    /// not given back.
    pub fn claimed(&self) -> bool {
        self.bytes.claimed()
    }

    /// The claimed write's first `n` bytes, at most a chunk, as runs to copy
    /// into, or the frames to take first ([`supply`]).
    pub fn first_fill(&mut self, n: usize) -> Fill {
        match self.bytes.wants() {
            0 => Fill::Runs(self.placed(0, n)),
            wants => {
                let frames = self.bytes.frames() + wants;
                let list = (frames > self.bytes.listed()).then(|| frames.next_power_of_two().min(MOST_FRAMES));
                Fill::Supply(Needs { wants, list, job: Arc::clone(self.job()) })
            }
        }
    }

    /// The claimed read's first `n` bytes from `at`, at most a chunk, as runs
    /// to copy out of.
    pub fn first_drain(&self, at: usize, n: usize) -> Runs {
        self.peeked(at, n)
    }

    fn placed(&mut self, at: usize, len: usize) -> Runs {
        let mut runs = Runs::new(at, len.min(CHUNK));
        self.bytes.place(at, runs.len, |frame, within, _| runs.add(*frame, within));
        runs
    }

    fn peeked(&self, at: usize, len: usize) -> Runs {
        let mut runs = Runs::new(at, len.min(CHUNK));
        self.bytes.peek(at, runs.len, |frame, within, _| runs.add(*frame, within));
        runs
    }
}

impl Drop for FrameBytes {
    fn drop(&mut self) {
        // SAFETY: nothing but this list names its frames, and no claim's copy
        // reaches them: `clear` refuses under one.
        unsafe { pmm::free_held(self.list.job(), self.bytes.clear()) };
    }
}

/// The most bytes a pipe's read or write bounces through its caller's stack
/// rather than claiming them across its copy.
pub const BOUNCE: usize = 4096;

/// Up to `N` bytes of one transfer, on its caller's stack: filled from the
/// caller or the frames, and read back only as far as it was filled.
pub struct Bounce<const N: usize> {
    bytes: [MaybeUninit<u8>; N],
    len: usize,
}

impl<const N: usize> Bounce<N> {
    pub const fn new() -> Self {
        Self { bytes: [MaybeUninit::uninit(); N], len: 0 }
    }

    /// Copy `buf`'s bytes, at most `N`, in with no lock held. Filled in
    /// place: a bounce is never moved, which would copy all of it.
    pub fn read(&mut self, buf: &UserBytes) -> Result<&Self, Faulted> {
        assert!(buf.len() <= N, "frame_bytes: a {}-byte bounce", buf.len());
        // SAFETY: the buffer is writable for `N` bytes and `buf.len()` is at most that.
        unsafe { buf.read_raw(0, self.bytes.as_mut_ptr().cast(), buf.len())? };
        self.len = buf.len();
        Ok(self)
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: the first `len` bytes were written by `read` or `FrameBytes::take`.
        unsafe { core::slice::from_raw_parts(self.bytes.as_ptr().cast(), self.len) }
    }
}

/// Copy `src`'s first `within.len()` bytes into `frame` at `within`.
///
/// # Safety
/// `within` lies inside the frame, which nothing else reaches meanwhile.
unsafe fn copy_to_frame(frame: u64, within: core::ops::Range<usize>, src: &[u8]) {
    let src = &src[..within.len()];
    // SAFETY: the caller's contract; the direct map covers every frame.
    unsafe {
        let to = DirectMap::from_phys(frame + within.start as u64).as_mut_ptr::<u8>();
        core::ptr::copy_nonoverlapping(src.as_ptr(), to, src.len());
    }
}

/// The token a copy waits under for its end's claim to end: a claim's end
/// wakes it and no poll, and any post on its watch wakes it to look again.
const TURN: u64 = 1;

/// `lock`, held once no copy holds `end`'s claim on the bytes `of` names,
/// parking on `watch`, `end`'s, while one does.
pub fn turn<'l, S>(
    p: &Parkable,
    lock: &'l Lock<S>,
    of: impl Fn(&mut S) -> &mut FrameBytes,
    end: End,
    watch: &Watch,
    class: WaitClass,
) -> Result<LockGuard<'l, S>, Cancelled> {
    loop {
        let mut state = lock.lock();
        if of(&mut state).turn(end) {
            return Ok(state);
        }
        drop(state);
        // Each look that finds the claim held asks to be woken again, so a
        // claim made between a wake and its look wakes this wait as it ends.
        watch::wait_until(p, watch, TURN, class, Deadline::never(), || of(&mut lock.lock()).turn(end))?;
    }
}

/// Do what a claim's end owes, with its object's lock let go, on its end's
/// `watch`.
pub fn wake(watch: &Watch, ended: Ended) {
    if ended.room {
        watch.post();
    } else if ended.turn {
        watch.post_n(TURN, usize::MAX);
    }
}

/// What a claimed write's first chunk is: its runs, or the frames it needs.
#[expect(clippy::large_enum_variant, reason = "one lives on its writer's stack for a claim; boxed, every write would allocate")]
pub enum Fill {
    Runs(Runs),
    Supply(Needs),
}

/// The frames a claimed write wants and the longer list they need, if any.
pub struct Needs {
    wants: usize,
    list: Option<usize>,
    job: Arc<Job>,
}

/// Take what `needs` names with `lock` let go, then hand them to the claimed
/// write and answer its first `n` bytes' runs, as [`FrameBytes::first_fill`]
/// would; refused, the claim holds none of them and is the caller's to
/// abandon. Only the claim's holder adds frames, and a read meanwhile only
/// lowers what it wants.
pub fn supply<S>(lock: &Lock<S>, of: impl Fn(&mut S) -> &mut FrameBytes, needs: Needs, n: usize) -> Result<Runs, Exhausted> {
    let Needs { wants, list, job } = needs;
    let list = match list {
        Some(len) => Some((Charge::new(&job, charge::array::<u64>(len))?, VecDeque::with_capacity(len))),
        None => None,
    };
    let mut frames = Vec::new();
    frames.try_reserve_exact(wants).map_err(|_| Exhausted)?;
    if !pmm::alloc_held(&job, wants, |frame| frames.push(frame)) {
        return Err(Exhausted);
    }
    let (runs, old) = {
        let mut state = lock.lock();
        let held = of(&mut state);
        // A read meanwhile may have made the longer list unnecessary: the
        // unused one goes back as the old one does, with the lock let go.
        let old = list.map(|(charge, list)| {
            if held.bytes.frames() + held.bytes.wants() > held.bytes.listed() {
                (core::mem::replace(&mut held.list, charge), held.bytes.relist(list))
            } else {
                (charge, list)
            }
        });
        for frame in frames.drain(..held.bytes.wants()) {
            held.bytes.add(frame);
        }
        (held.placed(0, n), old)
    };
    // SAFETY: taken above and named by nothing since.
    unsafe { pmm::free_held(&job, frames.drain(..)) };
    drop(old);
    Ok(runs)
}

/// Bytes of a claim one hold of the lock hands out frames for.
const CHUNK: usize = 16 * FRAME;

/// One chunk of a claim's bytes, `len` of them from `at`, as runs inside its
/// frames: each run's direct-map address and length, in order and continuing
/// each other in the claim. Its frames are the claim's, which only its holder
/// reaches.
#[derive(Clone, Copy)]
pub struct Runs {
    at: usize,
    len: usize,
    runs: [(u64, usize); CHUNK / FRAME + 1],
    count: usize,
}

impl Runs {
    fn new(at: usize, len: usize) -> Self {
        Self { at, len, runs: [(0, 0); CHUNK / FRAME + 1], count: 0 }
    }

    /// Whether claim byte `pos` is one of these.
    fn holds(&self, pos: usize) -> bool {
        (self.at..self.at + self.len).contains(&pos)
    }

    fn add(&mut self, frame: u64, within: core::ops::Range<usize>) {
        if within.end > FRAME {
            past_a_frame(within);
        }
        self.runs[self.count] = (frame + within.start as u64, within.len());
        self.count += 1;
    }

    /// Hand `copy` each run's part of the chunk's `[at, at + len)`: its
    /// direct-map pointer, where it starts among the `len`, and its length.
    #[inline(always)]
    fn each(&self, at: usize, len: usize, mut copy: impl FnMut(*mut u8, usize, usize)) {
        let mut start = 0;
        for &(phys, run) in &self.runs[..self.count] {
            let (lo, hi) = (start.max(at), (start + run).min(at + len));
            if lo < hi {
                // SAFETY: inside the run's frame, which the direct map covers.
                let to = unsafe { DirectMap::from_phys(phys).as_mut_ptr::<u8>().add(lo - start) };
                copy(to, lo - at, hi - lo);
            }
            start += run;
            if start >= at + len {
                break;
            }
        }
    }
}

/// Copy the first `n` bytes of `buf` into the claimed write, `n` at most its
/// claim, from its `first` chunk's runs and then a chunk at a time: each
/// chunk's frames handed out under `lock`, its copy made with the lock let go.
/// A user byte its process cannot reach stops it.
pub fn fill<S>(lock: &Lock<S>, of: impl Fn(&mut S) -> &mut FrameBytes, buf: &UserBytes, n: usize, first: Runs) -> Result<(), Faulted> {
    let mut runs = first;
    let mut done = 0;
    while done < n {
        if !runs.holds(done) {
            runs = of(&mut lock.lock()).placed(done, n - done);
        }
        let (base, len) = (done - runs.at, (runs.at + runs.len - done).min(n - done));
        // SAFETY: each run is the claim's frames, which no process maps and
        // only this claim's holder reaches, and `at + n` stays inside `buf`.
        Span::copy(&runs, base, len, done, |frame, at, n| unsafe { buf.read_raw(at, frame, n) })?;
        done += len;
    }
    Ok(())
}

/// Copy the `n` claimed bytes `at` past the claimed read's first into the
/// start of `buf`, as [`fill`] copies, from `runs` where they hold them.
pub fn drain<S>(lock: &Lock<S>, of: impl Fn(&mut S) -> &mut FrameBytes, at: usize, buf: &mut UserBytesMut, n: usize, runs: Runs) -> Result<(), Faulted> {
    let mut runs = runs;
    let mut done = 0;
    while done < n {
        let pos = at + done;
        if !runs.holds(pos) {
            runs = of(&mut lock.lock()).peeked(pos, n - done);
        }
        let (base, len) = (pos - runs.at, (runs.at + runs.len - pos).min(n - done));
        // SAFETY: as in `fill`.
        Span::copy(&runs, base, len, done, |frame, at, n| unsafe { buf.write_raw(at, frame, n) })?;
        done += len;
    }
    Ok(())
}

/// Runs of a chunk gathered while each continues the last in the direct map,
/// so a run over two physically adjacent frames is one user copy, not two.
struct Span {
    frame: *mut u8,
    at: usize,
    len: usize,
}

impl Span {
    /// Hand `copy` the chunk's `[base, base + len)` as gathered spans: a
    /// frame pointer, the user offset `user` plus where the span starts among
    /// the `len`, and the span's length.
    fn copy(runs: &Runs, base: usize, len: usize, user: usize, mut copy: impl FnMut(*mut u8, usize, usize) -> Result<(), Faulted>) -> Result<(), Faulted> {
        let mut span = Span { frame: core::ptr::null_mut(), at: 0, len: 0 };
        let mut stopped = Ok(());
        runs.each(base, len, |frame, off, run| {
            if stopped.is_err() {
                return;
            }
            if span.len > 0 && span.frame.wrapping_add(span.len) == frame {
                span.len += run;
                return;
            }
            if span.len > 0 {
                stopped = copy(span.frame, span.at, span.len);
            }
            span = Span { frame, at: user + off, len: run };
        });
        stopped?;
        if span.len > 0 {
            copy(span.frame, span.at, span.len)?;
        }
        Ok(())
    }
}

/// Frames past those a pipe keeps, listed under its lock to give back with
/// it let go: at most a batch.
pub struct Spares {
    frames: [u64; Spares::BATCH],
    count: usize,
    job: Option<Arc<Job>>,
}

impl Spares {
    const BATCH: usize = 32;

    pub const NONE: Self = Self { frames: [0; Self::BATCH], count: 0, job: None };

    fn take(held: &mut FrameBytes) -> Self {
        let mut spares = Self::NONE;
        for (slot, frame) in spares.frames.iter_mut().zip(held.bytes.surplus(Self::BATCH)) {
            (*slot, spares.count) = (frame, spares.count + 1);
        }
        if spares.count > 0 {
            spares.job = Some(Arc::clone(held.job()));
        }
        spares
    }
}

impl Drop for Spares {
    fn drop(&mut self) {
        if let Some(job) = &self.job {
            // SAFETY: a spare holds no byte, and the list that named it no longer does.
            unsafe { pmm::free_held(job, self.frames[..self.count].iter().copied()) };
        }
    }
}

/// Give back `first`, the spares a read's consume listed, and any left past
/// them a batch per hold of `lock`, each with it let go.
pub fn give_back<S>(lock: &Lock<S>, of: impl Fn(&mut S) -> &mut FrameBytes, first: Spares) {
    let mut spares = first;
    while spares.count == Spares::BATCH {
        drop(spares);
        spares = Spares::take(of(&mut lock.lock()));
    }
}

#[cold]
#[inline(never)]
fn past_a_frame(within: core::ops::Range<usize>) -> ! {
    panic!("frame_bytes: bytes {within:?} past a frame")
}
