//! Bytes a kernel object holds for a reader, in 4 KiB frames counted to the
//! job that made the object and taken as it fills (`kernel::pipe::Bytes`): a
//! pipe's stream and a connection direction's messages.
//!
//! The frames are kernel-only and written before they are read, so they are
//! never zeroed. Every call is made under its object's lock, which is the
//! only thing that reaches them.

use core::ops::Range;

use alloc::sync::Arc;

use kernel::pipe::{Bytes, MOST_FRAMES};
use kernel::sched::job::Job;

use crate::mm::charge::{self, Charge, Exhausted};
use crate::mm::{pmm, DirectMap};
use crate::user_ptr::{UserBytes, UserBytesMut};

/// The frames and the heap their list takes, charged to one job; the frames
/// go back when it drops.
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

    /// Room for more bytes, up to the capacity.
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

    /// Append the first `n` bytes of `buf`, taking the frames they need, all
    /// of them or none: a refusal appends nothing. `n` is at most
    /// [`Self::space`].
    pub fn write(&mut self, buf: &UserBytes, n: usize) -> Result<(), Exhausted> {
        self.take_frames(n)?;
        let bytes = &mut self.bytes;
        // One walk of the user window, each pinned piece copied into the
        // frames it covers.
        buf.read_pieces(0, n, |from, at, piece| {
            let mut gather = Gather::NONE;
            bytes.place(at, piece, |frame, within, off| {
                // SAFETY: `from` is valid for `piece` bytes until this walk's
                // callback returns, and `off + within.len()` is at most
                // `piece`; the frame is this object's alone, reached only
                // under its lock, and no process maps it.
                unsafe { gather.add(from.add(off), frame_bytes(frame, within.clone()), within.len()) }
            });
            // SAFETY: as above, before the callback returns.
            unsafe { gather.flush() };
        });
        bytes.commit(n);
        Ok(())
    }

    /// Move the first `n` bytes held into the start of `buf`, giving back the
    /// spares the read leaves past those kept. `n` is at most [`Self::len`].
    pub fn read(&mut self, buf: &mut UserBytesMut, n: usize) {
        let bytes = &mut self.bytes;
        buf.write_pieces(0, n, |to, at, piece| {
            let mut gather = Gather::NONE;
            bytes.peek(at, piece, |frame, within, off| {
                // SAFETY: `to` is valid for `piece` bytes until this walk's
                // callback returns, and `off + within.len()` is at most
                // `piece`; the frame is as in `write`.
                unsafe { gather.add(frame_bytes(frame, within.clone()), to.add(off), within.len()) }
            });
            // SAFETY: as above, before the callback returns.
            unsafe { gather.flush() };
        });
        bytes.consume(n);
        // SAFETY: a spare holds no byte, and nothing but this list names it.
        unsafe { pmm::free_held(self.list.job(), self.bytes.surplus()) };
    }

    fn take_frames(&mut self, n: usize) -> Result<(), Exhausted> {
        let wants = self.bytes.wants(n);
        if wants == 0 {
            return Ok(());
        }
        let frames = self.bytes.frames() + wants;
        if frames > self.bytes.listed() {
            let list = frames.next_power_of_two().min(MOST_FRAMES);
            self.list.resize(charge::array::<u64>(list))?;
            self.bytes.list(list);
        }
        let bytes = &mut self.bytes;
        pmm::alloc_held(self.list.job(), wants, |frame| bytes.add(frame)).then_some(()).ok_or(Exhausted)
    }
}

impl Drop for FrameBytes {
    fn drop(&mut self) {
        // SAFETY: nothing but this list names its frames.
        unsafe { pmm::free_held(self.list.job(), self.bytes.clear()) };
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

/// `within` of a frame an object holds, which no process maps and only that
/// object's lock reaches.
#[inline(always)]
fn frame_bytes(frame: &u64, within: Range<usize>) -> *mut u8 {
    if within.end > kernel::pipe::FRAME {
        past_a_frame(within);
    }
    // SAFETY: inside the frame, which the direct map covers.
    unsafe { DirectMap::from_phys(*frame).as_mut_ptr::<u8>().add(within.start) }
}

#[cold]
#[inline(never)]
fn past_a_frame(within: Range<usize>) -> ! {
    panic!("frame_bytes: bytes {within:?} past a frame")
}
