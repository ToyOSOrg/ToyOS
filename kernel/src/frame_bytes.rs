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

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;

use kernel::pipe::{Bytes, FRAME, MOST_FRAMES};
use kernel::sched::job::Job;

use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::{pmm, DirectMap};
use crate::sync::Lock;
use crate::user_ptr::{UserBytes, UserBytesMut};

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

    /// Claim `n` bytes of room, at most [`Self::space`], for one write;
    /// `false` while another write holds its claim.
    pub fn claim_write(&mut self, n: usize) -> bool {
        self.bytes.claim_write(n)
    }

    pub fn writing(&self) -> bool {
        self.bytes.writing()
    }

    /// Hold the claimed write's bytes, ending its claim.
    pub fn commit(&mut self) {
        self.bytes.commit();
    }

    pub fn abandon_write(&mut self) {
        self.bytes.abandon_write();
    }

    /// Claim the `n` oldest bytes, at most [`Self::len`], for one read;
    /// `false` while another read holds its claim.
    pub fn claim_read(&mut self, n: usize) -> bool {
        self.bytes.claim_read(n)
    }

    pub fn reading(&self) -> bool {
        self.bytes.reading()
    }

    /// Let the claimed read's bytes go, ending its claim; answers the first
    /// of the spares it leaves past those kept, for [`give_back`].
    pub fn consume(&mut self) -> Spares {
        self.bytes.consume();
        Spares::take(self)
    }

    pub fn abandon_read(&mut self) {
        self.bytes.abandon_read();
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
        let old = list.and_then(|(charge, list)| {
            (held.bytes.frames() + held.bytes.wants() > held.bytes.listed())
                .then(|| (core::mem::replace(&mut held.list, charge), held.bytes.relist(list)))
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
pub fn fill<S>(lock: &Lock<S>, of: impl Fn(&mut S) -> &mut FrameBytes, buf: &UserBytes, n: usize, first: Runs) {
    let mut runs = first;
    let mut done = 0;
    while done < n {
        if !runs.holds(done) {
            runs = of(&mut lock.lock()).placed(done, n - done);
        }
        let (base, len) = (done - runs.at, (runs.at + runs.len - done).min(n - done));
        buf.read_pieces(done, len, |from, at, piece| {
            let mut gather = Gather::NONE;
            runs.each(base + at, piece, |to, off, run| {
                // SAFETY: `from` is valid for `piece` bytes until this walk's
                // callback returns, and `off + run` is at most `piece`; `to` is
                // a run of the claim's frames, which no process maps and only
                // this claim's holder reaches.
                unsafe { gather.add(from.add(off), to, run) }
            });
            // SAFETY: as above, before the callback returns.
            unsafe { gather.flush() };
        });
        done += len;
    }
}

/// Copy the `n` claimed bytes `at` past the claimed read's first into the
/// start of `buf`, as [`fill`] copies, from `runs` where they hold them.
pub fn drain<S>(lock: &Lock<S>, of: impl Fn(&mut S) -> &mut FrameBytes, at: usize, buf: &mut UserBytesMut, n: usize, runs: Runs) {
    let mut runs = runs;
    let mut done = 0;
    while done < n {
        let pos = at + done;
        if !runs.holds(pos) {
            runs = of(&mut lock.lock()).peeked(pos, n - done);
        }
        let (base, len) = (pos - runs.at, (runs.at + runs.len - pos).min(n - done));
        buf.write_pieces(done, len, |to, at, piece| {
            let mut gather = Gather::NONE;
            runs.each(base + at, piece, |from, off, run| {
                // SAFETY: `to` is valid for `piece` bytes until this walk's
                // callback returns, and `off + run` is at most `piece`; the
                // frames are as in `fill`.
                unsafe { gather.add(from, to.add(off), run) }
            });
            // SAFETY: as above, before the callback returns.
            unsafe { gather.flush() };
        });
        done += len;
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

/// One copy gathered from runs that continue each other in both source and
/// destination, so a user page's piece over two physically adjacent frames
/// is one copy, not two.
struct Gather {
    from: *const u8,
    to: *mut u8,
    len: usize,
}

impl Gather {
    const NONE: Self = Self { from: core::ptr::null(), to: core::ptr::null_mut(), len: 0 };

    /// Add `len` bytes from `from` to `to`, copying what was gathered first
    /// unless these continue it.
    ///
    /// # Safety
    /// `from` is readable and `to` writable for `len` bytes, and stay so
    /// until the next [`Self::flush`]; the two never overlap.
    #[inline(always)]
    unsafe fn add(&mut self, from: *const u8, to: *mut u8, len: usize) {
        if self.len > 0 && self.from.wrapping_add(self.len) == from && self.to.wrapping_add(self.len) == to {
            self.len += len;
            return;
        }
        // SAFETY: what was gathered meets this function's contract.
        unsafe { self.flush() };
        *self = Self { from, to, len };
    }

    /// Copy what was gathered.
    ///
    /// # Safety
    /// As [`Self::add`] for every run gathered.
    #[inline(always)]
    unsafe fn flush(&mut self) {
        if self.len > 0 {
            // SAFETY: the runs gathered are contiguous on both sides and
            // each met `add`'s contract.
            unsafe { core::ptr::copy_nonoverlapping(self.from, self.to, self.len) };
        }
        *self = Self::NONE;
    }
}

#[cold]
#[inline(never)]
fn past_a_frame(within: core::ops::Range<usize>) -> ! {
    panic!("frame_bytes: bytes {within:?} past a frame")
}
