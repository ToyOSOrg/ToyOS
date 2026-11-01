//! Who holds a pipe's two ends, what a watch on one of them is told, how a
//! write lands, and the frames its bytes are kept in.
//!
//! A pipe end is held by counted references, one per object that names it: a
//! handle duplicated or moved to another process is the same object or another
//! reference, so an end is gone only when the last of them is. Every answer
//! here is a function of the two counts and of what the pipe holds, read under
//! the kernel's pipe lock.

#![forbid(unsafe_code)]

use alloc::collections::vec_deque::{Drain, VecDeque};
use core::ops::Range;

use toyos_abi::syscall::{MAX_WHOLE_PIPE_WRITE, PIPE_CAPACITY};

/// The bytes of one frame a pipe's bytes are kept in.
pub const FRAME: usize = crate::frames::FRAME as usize;

/// The most frames a pipe holds: its capacity, the first of them part-read.
pub const MOST_FRAMES: usize = PIPE_CAPACITY / FRAME + 1;

// A whole write fits the one frame an emptied pipe keeps, which is what
// `PIPE_CAPACITY`'s promise to an empty pipe rests on.
const _: () = assert!(PIPE_CAPACITY.is_multiple_of(FRAME) && MAX_WHOLE_PIPE_WRITE <= FRAME);

/// One end of a pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    Read,
    Write,
}

impl End {
    pub fn other(self) -> Self {
        match self {
            Self::Read => Self::Write,
            Self::Write => Self::Read,
        }
    }
}

/// What a reference's release left of its pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use = "a release that left an end unheld owes the other end a post"]
pub enum Released {
    /// Its end has another holder.
    Held,
    /// It was its end's last holder: the other end's watch is owed a post.
    EndGone,
    /// It was the pipe's last holder.
    PipeGone,
}

/// How many references hold each end of one pipe.
#[derive(Default)]
pub struct Holders {
    readers: u32,
    writers: u32,
}

impl Holders {
    pub const fn new() -> Self {
        Self { readers: 0, writers: 0 }
    }

    fn count(&mut self, end: End) -> &mut u32 {
        match end {
            End::Read => &mut self.readers,
            End::Write => &mut self.writers,
        }
    }

    pub fn hold(&mut self, end: End) {
        let count = self.count(end);
        *count = count.checked_add(1).expect("pipe holder overflow");
    }

    pub fn release(&mut self, end: End) -> Released {
        let count = self.count(end);
        *count = count.checked_sub(1).expect("pipe holder underflow");
        match (self.readers, self.writers) {
            (0, 0) => Released::PipeGone,
            _ if !self.held(end) => Released::EndGone,
            _ => Released::Held,
        }
    }

    pub fn held(&self, end: End) -> bool {
        match end {
            End::Read => self.readers > 0,
            End::Write => self.writers > 0,
        }
    }

    /// A read answers now: with bytes, or with the end of a stream no writer
    /// can add to.
    pub fn readable(&self, available: usize) -> bool {
        available > 0 || !self.held(End::Write)
    }

    /// A write waiting for `want` bytes of room answers now: it lands, or is
    /// refused for want of a reader.
    pub fn writable(&self, space: usize, want: usize) -> bool {
        space >= want || !self.held(End::Read)
    }

    /// The end opposite `end` has no holder, whatever the pipe holds.
    pub fn other_end_gone(&self, end: End) -> bool {
        !self.held(end.other())
    }
}

/// The room a pipe write of `len` bytes waits for: it lands as soon as there
/// is this much, and not before. Whole up to [`MAX_WHOLE_PIPE_WRITE`] bytes,
/// and as far as it fits past that.
pub fn room(len: usize) -> usize {
    match len <= MAX_WHOLE_PIPE_WRITE {
        true => len,
        false => 1,
    }
}

/// How many of `len` bytes a pipe write lands with `space` free; `None`
/// until there is [`room`].
pub fn takes(space: usize, len: usize) -> Option<usize> {
    (space >= room(len)).then(|| len.min(space))
}


/// What a pipe holds, in frames taken as it fills and given back as it
/// drains. Its first unread byte is `head` bytes into the first frame, and
/// the frames past its last byte are spare, kept for the next write up to
/// the largest of: the frames holding bytes, the frames the last write
/// spanned, and one. So a pipe holds at most its bytes' frames plus that
/// largest, never more than [`MOST_FRAMES`], and a stream of writes of one
/// size takes no frame from the allocator whether its reader keeps up or
/// lags.
///
/// **A copy runs with no lock held, inside a claim.** A write claims the room
/// past the last byte held and a read the oldest bytes held, one of each at
/// a time; the kernel's lock is held only to claim, to hand out a claim's
/// frames and to end it. Nothing but the claim's holder reaches its bytes: a
/// claimed write's are past every reader's, its frames are kept as used, and
/// only the read's own [`Self::consume`] lets claimed bytes go. A claim's
/// frames keep their contents across another's calls, but not their place in
/// the list, which a read's consume rotates; so a copier takes each frame
/// it needs under the lock, as [`Self::place`] and [`Self::peek`] hand them
/// out, and uses it once the lock is let go.
pub struct Bytes<F> {
    frames: VecDeque<F>,
    head: usize,
    len: usize,
    /// The frames the last write spanned: the working set its next one wants.
    last: usize,
    /// The bytes past the last held a write has claimed.
    writing: Option<usize>,
    /// The oldest bytes held a read has claimed.
    reading: Option<usize>,
}

impl<F> Bytes<F> {
    pub const fn new() -> Self {
        Self { frames: VecDeque::new(), head: 0, len: 0, last: 0, writing: None, reading: None }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Room for a write to claim: none of it claimed.
    pub fn space(&self) -> usize {
        PIPE_CAPACITY - self.len - self.writing.unwrap_or(0)
    }

    /// The frames it holds, spares among them.
    pub fn frames(&self) -> usize {
        self.frames.len()
    }

    /// The frames it can list without growing its list.
    pub fn listed(&self) -> usize {
        self.frames.capacity()
    }

    /// Move the frames into `list`, empty and with room for [`Self::wants`]
    /// more, so that adding them grows nothing; answers the old list, empty,
    /// for the caller to drop.
    pub fn relist(&mut self, mut list: VecDeque<F>) -> VecDeque<F> {
        assert!(list.is_empty(), "pipe: a new list that holds frames");
        assert!(list.capacity() >= self.frames.len() + self.wants(), "pipe: a new list with no room for its claim");
        list.extend(self.frames.drain(..));
        core::mem::replace(&mut self.frames, list)
    }

    /// The frames holding bytes or claimed for them.
    fn used(&self) -> usize {
        (self.head + self.len + self.writing.unwrap_or(0)).div_ceil(FRAME)
    }

    /// Claim `n` bytes of room, at most [`Self::space`], for one write;
    /// `false`, claiming nothing, while another write holds its claim.
    pub fn claim_write(&mut self, n: usize) -> bool {
        if self.writing.is_some() {
            return false;
        }
        assert!(n <= self.space(), "pipe: a {n}-byte write with room for {}", self.space());
        self.writing = Some(n);
        true
    }

    /// Whether a write holds its claim.
    pub fn writing(&self) -> bool {
        self.writing.is_some()
    }

    /// How many frames more the claimed write needs than it holds.
    pub fn wants(&self) -> usize {
        self.used().saturating_sub(self.frames.len())
    }

    /// One frame more for the claimed write, into the room its list has.
    pub fn add(&mut self, frame: F) {
        assert!(self.frames.len() < MOST_FRAMES, "pipe: a frame past the most it holds");
        assert!(self.frames.len() < self.frames.capacity(), "pipe: a frame past its list");
        self.frames.push_back(frame);
    }

    /// Place the `len` claimed bytes `at` into the claim, once it
    /// [`Self::wants`] no more frames: `copy` is handed each run of them as
    /// its frame, where they go in it, and where they start among the `len`.
    /// They are held once [`Self::commit`] counts them.
    #[inline(always)]
    pub fn place(&mut self, at: usize, len: usize, mut copy: impl FnMut(&mut F, Range<usize>, usize)) {
        let claim = self.writing.expect("pipe: bytes placed with no write claimed");
        assert!(at + len <= claim, "pipe: {len} bytes placed at {at} of a {claim}-byte claim");
        assert_eq!(self.wants(), 0, "pipe: bytes placed past the frames it was given");
        let (mut done, mut pos) = (0, self.head + self.len + at);
        while done < len {
            let (frame, off) = (pos / FRAME, pos % FRAME);
            let run = (FRAME - off).min(len - done);
            copy(&mut self.frames[frame], off..off + run, done);
            (done, pos) = (done + run, pos + run);
        }
    }

    /// Count the claimed write's bytes held, ending its claim.
    pub fn commit(&mut self) {
        let n = self.writing.expect("pipe: a commit with no write claimed");
        assert_eq!(self.wants(), 0, "pipe: bytes held past the frames it was given");
        self.last = (self.head + self.len + n).div_ceil(FRAME) - (self.head + self.len) / FRAME;
        (self.len, self.writing) = (self.len + n, None);
    }

    /// End the claimed write holding none of it.
    pub fn abandon_write(&mut self) {
        self.writing = None;
    }

    /// Claim the `n` oldest bytes, at most [`Self::len`], for one read;
    /// `false`, claiming nothing, while another read holds its claim.
    pub fn claim_read(&mut self, n: usize) -> bool {
        if self.reading.is_some() {
            return false;
        }
        assert!(n <= self.len, "pipe: {n} bytes claimed of {} held", self.len);
        self.reading = Some(n);
        true
    }

    /// Whether a read holds its claim.
    pub fn reading(&self) -> bool {
        self.reading.is_some()
    }

    /// Whether either claim is held.
    pub fn claimed(&self) -> bool {
        self.writing.is_some() || self.reading.is_some()
    }

    /// The `len` claimed bytes `at` past the oldest, handed to `copy` as
    /// [`Self::place`] hands them out; [`Self::consume`] lets them go.
    #[inline(always)]
    pub fn peek(&self, at: usize, len: usize, mut copy: impl FnMut(&F, Range<usize>, usize)) {
        let claim = self.reading.expect("pipe: bytes peeked with no read claimed");
        assert!(at + len <= claim, "pipe: {len} bytes peeked at {at} of a {claim}-byte claim");
        let (mut done, mut pos) = (0, self.head + at);
        while done < len {
            let (frame, off) = (pos / FRAME, pos % FRAME);
            let run = (FRAME - off).min(len - done);
            copy(&self.frames[frame], off..off + run, done);
            (done, pos) = (done + run, pos + run);
        }
    }

    /// Let the claimed bytes go, ending the read's claim; a frame it empties
    /// becomes a spare.
    pub fn consume(&mut self) {
        let n = self.reading.take().expect("pipe: a consume with no read claimed");
        let emptied = (self.head + n) / FRAME;
        self.frames.rotate_left(emptied);
        (self.head, self.len) = ((self.head + n) % FRAME, self.len - n);
        if self.len == 0 && self.writing.is_none() {
            self.head = 0;
        }
    }

    /// End the claimed read letting none of it go.
    pub fn abandon_read(&mut self) {
        self.reading = None;
    }

    /// At most `most` of the spares past those it keeps, for the caller to
    /// give back; fewer than `most` once none is left past them.
    pub fn surplus(&mut self, most: usize) -> Drain<'_, F> {
        let used = self.used();
        let keep = used + (self.frames.len() - used).min(used.max(self.last).max(1));
        self.frames.drain(keep.max(self.frames.len().saturating_sub(most))..)
    }

    /// Every frame, for the caller to give back, and with them every byte;
    /// never while a claim is held, whose copy may still reach its frames.
    pub fn clear(&mut self) -> Drain<'_, F> {
        assert!(!self.claimed(), "pipe: bytes cleared under a claim");
        (self.head, self.len, self.last) = (0, 0, 0);
        self.frames.drain(..)
    }
}

impl<F> Default for Bytes<F> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    /// A pipe as `pipe::create` leaves it: one holder of each end.
    fn made() -> Holders {
        let mut pipe = Holders::new();
        pipe.hold(End::Read);
        pipe.hold(End::Write);
        pipe
    }

    #[test]
    fn an_end_whose_other_end_is_held_is_not_told_it_is_gone() {
        let pipe = made();
        assert!(!pipe.other_end_gone(End::Read));
        assert!(!pipe.other_end_gone(End::Write));
    }

    /// **Whatever the ring holds.** `readable` answers for the bytes and says
    /// nothing of the writer; `writable` for the room and nothing of the reader.
    #[test]
    fn an_end_dropped_with_the_ring_neither_empty_nor_full_is_gone() {
        for (available, space) in [(0, 8), (3, 5), (8, 0)] {
            let mut pipe = made();
            assert_eq!(pipe.release(End::Write), Released::EndGone);
            assert!(pipe.other_end_gone(End::Read), "the writer left with {available} byte(s) in the ring");
            assert!(pipe.readable(available));
            assert!(!pipe.other_end_gone(End::Write), "a reader that is held was told gone");

            let mut pipe = made();
            assert_eq!(pipe.release(End::Read), Released::EndGone);
            assert!(pipe.other_end_gone(End::Write), "the reader left with room for {space} byte(s)");
            assert!(pipe.writable(space, 1));
            assert!(!pipe.other_end_gone(End::Read), "a writer that is held was told gone");
        }
    }

    /// The two older conditions do not stand in for it: each is ready with the
    /// other end held.
    #[test]
    fn readable_and_writable_say_nothing_of_the_other_end() {
        let pipe = made();
        assert!(pipe.readable(1) && !pipe.other_end_gone(End::Read));
        assert!(pipe.writable(1, 1) && !pipe.other_end_gone(End::Write));
        assert!(!pipe.readable(0) && !pipe.writable(0, 1));
    }

    /// A handle duplicated, or moved to another process, is a second
    /// reference: the end is gone with the last of them and not before.
    #[test]
    fn an_end_with_a_second_holder_outlives_the_first() {
        for end in [End::Read, End::Write] {
            let mut pipe = made();
            pipe.hold(end);
            assert_eq!(pipe.release(end), Released::Held);
            assert!(!pipe.other_end_gone(end.other()), "{end:?} was told gone with a holder left");
            assert_eq!(pipe.release(end), Released::EndGone);
            assert!(pipe.other_end_gone(end.other()));
        }
    }

    /// The pipe's last reference owes nobody a post: no end is left to watch.
    #[test]
    fn the_last_holder_of_the_pipe_ends_it() {
        let mut pipe = made();
        assert_eq!(pipe.release(End::Read), Released::EndGone);
        assert_eq!(pipe.release(End::Write), Released::PipeGone);
    }

    const MAX: usize = MAX_WHOLE_PIPE_WRITE;

    /// How a model write lands: a pipe end's, or as far as it fits, which no
    /// write makes: the tearing test's control.
    #[derive(Clone, Copy, Debug)]
    enum Landing {
        Pipe,
        Partial,
    }

    impl Landing {
        fn room(self, len: usize) -> usize {
            match self {
                Self::Pipe => room(len),
                Self::Partial => len.min(1),
            }
        }

        fn takes(self, space: usize, len: usize) -> Option<usize> {
            match self {
                Self::Pipe => takes(space, len),
                Self::Partial => (space >= self.room(len)).then(|| len.min(space)),
            }
        }
    }

    /// What a write answers, as `kernel::pipe::write` does.
    #[derive(PartialEq, Eq, Debug)]
    enum Wrote {
        Bytes(usize),
        /// A blocking write waits for [`room`]; a non-blocking one
        /// answers `WouldBlock`.
        NoRoom,
        Broken,
    }

    /// One pipe as the kernel holds it under its lock: the holders and the
    /// bytes in a ring of `cap`.
    struct Model {
        holders: Holders,
        ring: VecDeque<u8>,
        cap: usize,
    }

    impl Model {
        fn new(cap: usize) -> Self {
            Self { holders: made(), ring: VecDeque::new(), cap }
        }

        fn space(&self) -> usize {
            self.cap - self.ring.len()
        }

        fn write(&mut self, bytes: &[u8], landing: Landing) -> Wrote {
            if !self.holders.held(End::Read) {
                return Wrote::Broken;
            }
            match landing.takes(self.space(), bytes.len()) {
                Some(n) => {
                    self.ring.extend(&bytes[..n]);
                    Wrote::Bytes(n)
                }
                None => Wrote::NoRoom,
            }
        }

        /// A blocking writer's wait condition, as `sys_write` asks it.
        fn wakes(&self, len: usize, landing: Landing) -> bool {
            self.holders.writable(self.space(), landing.room(len))
        }

        /// A poll's writable, as `ops::has_space` asks it of a pipe end.
        fn polls_writable(&self) -> bool {
            self.holders.writable(self.space(), MAX)
        }

        fn read(&mut self, want: usize) -> Vec<u8> {
            let n = want.min(self.ring.len());
            self.ring.drain(..n).collect()
        }
    }

    /// A blocking pipe write of at most `MAX` bytes lands whole or not at all
    /// at every amount of room, and a longer one as far as it fits.
    #[test]
    fn a_small_pipe_write_lands_whole_or_waits() {
        for len in [1, 2, 40, MAX - 1, MAX] {
            let bytes = vec![7; len];
            for space in [0, 1, len / 2, len - 1, len, len + 1] {
                let mut pipe = Model::new(4 * MAX);
                pipe.ring.extend(core::iter::repeat_n(0, 4 * MAX - space));
                let answer = pipe.write(&bytes, Landing::Pipe);
                if space >= len {
                    assert_eq!(answer, Wrote::Bytes(len), "a {len}-byte write with room for it");
                } else {
                    assert_eq!(answer, Wrote::NoRoom, "a {len}-byte write split at {space} bytes of room");
                    assert_eq!(pipe.ring.len(), 4 * MAX - space, "a refused write left bytes behind");
                }
            }
        }
        let mut pipe = Model::new(4 * MAX);
        pipe.ring.extend(core::iter::repeat_n(0, 4 * MAX - 1));
        assert_eq!(pipe.write(&[7; MAX + 1], Landing::Pipe), Wrote::Bytes(1), "a long write does not wait for all of it");
    }

    /// The non-blocking form is refused whole, and a poll answers writable
    /// only when a refused write of at most `MAX` bytes would then land: a
    /// retry on readiness is never refused again, so it cannot spin.
    #[test]
    fn a_refused_nonblocking_write_is_answered_by_readiness() {
        let cap = 3 * MAX;
        for len in [1, 40, MAX] {
            let mut pipe = Model::new(cap);
            pipe.ring.extend(core::iter::repeat_n(0, cap - len + 1));
            assert!(!pipe.polls_writable(), "writable with less room than a whole write");
            assert_eq!(pipe.write(&vec![7; len], Landing::Pipe), Wrote::NoRoom);
            while !pipe.polls_writable() {
                pipe.read(97);
            }
            assert_eq!(pipe.write(&vec![7; len], Landing::Pipe), Wrote::Bytes(len), "a {len}-byte retry on readiness refused");
        }
    }

    /// An empty write moves nothing, so it waits for nothing, even on a full
    /// ring, and still answers a missing reader.
    #[test]
    fn an_empty_write_waits_for_nothing() {
        let mut pipe = Model::new(64);
        pipe.ring.extend([0; 64]);
        assert!(pipe.wakes(0, Landing::Pipe), "an empty write waits on a full ring");
        assert_eq!(pipe.write(&[], Landing::Pipe), Wrote::Bytes(0));
        assert_eq!(pipe.holders.release(End::Read), Released::EndGone);
        assert_eq!(pipe.write(&[], Landing::Pipe), Wrote::Broken);
    }

    /// A writer waiting for room is answered when the reader leaves, and its
    /// write is refused rather than landing in a ring nobody reads.
    #[test]
    fn a_reader_leaving_answers_a_waiting_writer() {
        let mut pipe = Model::new(2 * MAX);
        pipe.ring.extend(core::iter::repeat_n(0, 2 * MAX - 10));
        assert!(!pipe.wakes(40, Landing::Pipe));
        assert!(!pipe.polls_writable());
        assert_eq!(pipe.holders.release(End::Read), Released::EndGone);
        assert!(pipe.wakes(40, Landing::Pipe), "a waiting writer was not answered by the reader leaving");
        assert!(pipe.polls_writable());
        assert_eq!(pipe.write(&[7; 40], Landing::Pipe), Wrote::Broken);
    }

    /// The wait condition is exactly the write's: at every amount of room, a
    /// waiting writer wakes if and only if its write would land, so it is
    /// neither woken to be refused nor left asleep with room for it.
    #[test]
    fn a_waiting_writer_wakes_exactly_when_its_write_lands() {
        let cap = 2 * MAX + 3;
        for len in [0, 1, 2, 40, MAX - 1, MAX, MAX + 1, 2 * MAX] {
            for space in 0..=cap {
                let mut pipe = Model::new(cap);
                pipe.ring.extend(core::iter::repeat_n(0, cap - space));
                let wakes = pipe.wakes(len, Landing::Pipe);
                let lands = pipe.write(&vec![7; len], Landing::Pipe) != Wrote::NoRoom;
                assert_eq!(wakes, lands, "{len} bytes, {space} free: woken {wakes}, lands {lands}");
            }
        }
    }

    /// One writer of a run of records, each written whole or in parts as its
    /// write lands, with a `write_all` loop for what is left.
    struct Writer {
        tag: u8,
        lens: Vec<usize>,
        next: usize,
        /// Of the record `next`, already written.
        done: usize,
        parked: bool,
    }

    impl Writer {
        fn pending(&self) -> Option<usize> {
            self.lens.get(self.next).map(|len| len - self.done)
        }
    }

    /// Three writers of records of up to `MAX` bytes share one pipe whose
    /// reader lags, so most writes meet a ring without room for them. A
    /// writer whose write is refused parks until a read's post finds its wait
    /// condition true. Every record reaches the reader in one piece, and every
    /// writer finishes: no wake is missed.
    /// Answers how many writes were refused for room.
    fn compete(landing: Landing) -> Result<usize, String> {
        let mut seed = 0x2545_f491_u64;
        let mut random = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        let mut writers: Vec<Writer> = (0..3u8)
            .map(|tag| Writer {
                tag: b'a' + tag,
                lens: (0..300).map(|_| 1 + random(MAX)).collect(),
                next: 0,
                done: 0,
                parked: false,
            })
            .collect();
        let mut pipe = Model::new(2 * MAX + 13);
        let mut stream = Vec::new();
        let mut refused = 0;
        for _ in 0..1_000_000 {
            if writers.iter().all(|w| w.pending().is_none()) && pipe.ring.is_empty() {
                break;
            }
            let runnable: Vec<usize> = (0..writers.len())
                .filter(|&i| !writers[i].parked && writers[i].pending().is_some())
                .collect();
            if runnable.is_empty() && pipe.ring.is_empty() {
                return Err("every writer parked on an empty ring: a missed wake".into());
            }
            // The reader runs one turn in four, or whenever no writer can.
            if !pipe.ring.is_empty() && (runnable.is_empty() || random(4) == 0) {
                stream.extend(pipe.read(1 + random(MAX / 2)));
                // The read posts the write watch: every parked writer re-reads its condition.
                for w in &mut writers {
                    if let Some(len) = w.pending() {
                        w.parked = w.parked && !pipe.wakes(len, landing);
                    }
                }
                continue;
            }
            let w = &mut writers[runnable[random(runnable.len())]];
            let len = w.pending().expect("a runnable writer has a record");
            match pipe.write(&vec![w.tag; len], landing) {
                Wrote::Bytes(n) => {
                    w.done += n;
                    if w.done == w.lens[w.next] {
                        (w.next, w.done) = (w.next + 1, 0);
                    }
                }
                Wrote::NoRoom => {
                    w.parked = true;
                    refused += 1;
                }
                Wrote::Broken => unreachable!("the reader stays"),
            }
        }
        if writers.iter().any(|w| w.pending().is_some()) {
            return Err("the run did not finish".into());
        }
        let mut at = 0;
        let mut next = [0; 3];
        while at < stream.len() {
            let tag = stream[at];
            let i = (tag - b'a') as usize;
            let len = writers[i].lens[next[i]];
            if let Some(off) = stream[at..at + len].iter().position(|&b| b != tag) {
                return Err(format!("{}'s record {} of {len} bytes torn at {off}", tag as char, next[i]));
            }
            (at, next[i]) = (at + len, next[i] + 1);
        }
        Ok(refused)
    }

    #[test]
    fn competing_small_writes_never_interleave_while_the_reader_lags() {
        let refused = compete(Landing::Pipe).unwrap();
        assert!(refused > 100, "only {refused} writes met a ring without room for them");
    }

    /// The control on the test itself: the same run, writing each record as
    /// far as it fits, does tear.
    #[test]
    fn competing_partial_writes_tear_in_the_same_run() {
        assert!(compete(Landing::Partial).is_err(), "the run never split a record, so it proves nothing");
    }

    /// A frame of the model: its bytes and a name, so a frame handed out twice
    /// or lost is seen.
    struct Frame {
        id: usize,
        bytes: Vec<u8>,
    }

    /// A pipe's bytes with an allocator that counts what it hands out.
    struct Held {
        bytes: Bytes<Frame>,
        taken: usize,
        returned: Vec<usize>,
    }

    impl Held {
        fn new() -> Self {
            Self { bytes: Bytes::new(), taken: 0, returned: Vec::new() }
        }

        /// Claim `n` bytes of room and supply the frames the claim wants, as
        /// `kernel::frame_bytes` does: a list grown first, then the frames.
        fn claim(&mut self, n: usize) {
            assert!(self.bytes.claim_write(n));
            let wants = self.bytes.wants();
            if self.bytes.frames() + wants > self.bytes.listed() {
                let old = self.bytes.relist(VecDeque::with_capacity((self.bytes.frames() + wants).next_power_of_two()));
                assert!(old.is_empty());
            }
            for _ in 0..wants {
                self.bytes.add(Frame { id: self.taken, bytes: vec![0xEE; FRAME] });
                self.taken += 1;
            }
        }

        /// Write `src` as `kernel::pipe` does: claimed, then the copy placed
        /// and committed.
        fn write(&mut self, src: &[u8]) {
            self.claim(src.len());
            self.bytes.place(0, src.len(), |frame, within, at| {
                frame.bytes[within.clone()].copy_from_slice(&src[at..at + within.len()]);
            });
            self.bytes.commit();
        }

        /// Read up to `want` bytes as `kernel::pipe` does: claimed, peeked,
        /// consumed, and the surplus given back.
        fn read(&mut self, want: usize) -> Vec<u8> {
            let n = want.min(self.bytes.len());
            let mut out = vec![0; n];
            assert!(self.bytes.claim_read(n));
            self.bytes.peek(0, n, |frame, within, at| {
                out[at..at + within.len()].copy_from_slice(&frame.bytes[within]);
            });
            self.bytes.consume();
            self.returned.extend(self.bytes.surplus(usize::MAX).map(|frame| frame.id));
            out
        }

        /// Each frame handed out once is held or was given back, never both.
        fn accounted(&self) {
            let mut seen: Vec<usize> = self.bytes.frames.iter().map(|f| f.id).chain(self.returned.iter().copied()).collect();
            seen.sort_unstable();
            assert_eq!(seen, (0..self.taken).collect::<Vec<_>>(), "a frame was lost or given back twice");
        }
    }

    fn pattern(from: usize, len: usize) -> Vec<u8> {
        (from..from + len).map(|i| (i % 251) as u8).collect()
    }

    /// Writes and reads of every size, across frame edges and the whole
    /// capacity, come back as written, and what the pipe keeps stays within
    /// its bound: twice the frames its bytes need and one, at most
    /// `MOST_FRAMES`.
    #[test]
    fn a_stream_comes_back_exact_and_the_frames_stay_bounded() {
        let mut seed = 0x9e37_79b9_u64;
        let mut random = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        let mut pipe = Held::new();
        let (mut written, mut read) = (0, 0);
        for turn in 0..4000 {
            let most = [1, 64, FRAME - 1, FRAME + 1, 64 << 10, PIPE_CAPACITY][turn % 6];
            let n = random(most + 1).min(pipe.bytes.space());
            pipe.write(&pattern(written, n));
            written += n;
            assert!(pipe.bytes.frames() <= MOST_FRAMES);
            let got = pipe.read(random(most + 1));
            assert_eq!(got, pattern(read, got.len()), "turn {turn}: bytes {read}.. came back wrong");
            read += got.len();
            assert!(
                pipe.bytes.frames() <= pipe.bytes.used() + pipe.bytes.used().max(pipe.bytes.last).max(1),
                "turn {turn}: {} frames kept for {} bytes",
                pipe.bytes.frames(),
                pipe.bytes.len()
            );
            pipe.accounted();
        }
        let rest = pipe.read(PIPE_CAPACITY);
        assert_eq!(rest, pattern(read, written - read));
        assert_eq!(pipe.bytes.frames(), pipe.bytes.last.max(1), "a drained pipe keeps its last write's frames");
    }

    /// A write placed in pieces, in any order and cut anywhere, as a user
    /// window's pages cut it, and a read peeked the same way, carry the same
    /// bytes as one copy each would.
    #[test]
    fn pieces_placed_and_peeked_in_any_order_are_one_copy() {
        let mut seed = 0x51ed_270b_u64;
        let mut random = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        let mut pipe = Held::new();
        let (mut written, mut read) = (0, 0);
        for _ in 0..300 {
            let n = random(3 * FRAME + 1).min(pipe.bytes.space());
            pipe.claim(n);
            let src = pattern(written, n);
            let mut cuts: Vec<usize> = (0..random(6)).map(|_| random(n + 1)).chain([0, n]).collect();
            cuts.sort_unstable();
            let mut pieces: Vec<(usize, usize)> = cuts.windows(2).map(|w| (w[0], w[1] - w[0])).collect();
            pieces.reverse();
            for (at, len) in pieces {
                pipe.bytes.place(at, len, |frame, within, from| {
                    frame.bytes[within.clone()].copy_from_slice(&src[at + from..at + from + within.len()]);
                });
            }
            pipe.bytes.commit();
            written += n;

            let m = random(pipe.bytes.len() + 1);
            let mut out = vec![0; m];
            assert!(pipe.bytes.claim_read(m));
            let mut at = 0;
            while at < m {
                let len = (1 + random(2 * FRAME)).min(m - at);
                pipe.bytes.peek(at, len, |frame, within, from| {
                    out[at + from..at + from + within.len()].copy_from_slice(&frame.bytes[within]);
                });
                at += len;
            }
            pipe.bytes.consume();
            pipe.returned.extend(pipe.bytes.surplus(usize::MAX).map(|frame| frame.id));
            assert_eq!(out, pattern(read, m), "bytes {read}.. came back wrong");
            read += m;
            pipe.accounted();
        }
    }

    /// A full pipe whose first frame is part-read holds `MOST_FRAMES`.
    #[test]
    fn a_full_pipe_part_read_holds_the_most_frames() {
        let mut pipe = Held::new();
        pipe.write(&pattern(0, 10));
        assert_eq!(pipe.read(5), pattern(0, 5));
        let rest = pipe.bytes.space();
        pipe.write(&pattern(10, rest));
        assert_eq!((pipe.bytes.len(), pipe.bytes.frames()), (PIPE_CAPACITY, MOST_FRAMES));
        assert_eq!(pipe.read(PIPE_CAPACITY), [pattern(5, 5), pattern(10, rest)].concat());
    }

    /// A stream of one write size takes no frame after its first round,
    /// whether its reader drains the pipe each time, keeps a little backlog
    /// or nearly all of it: the frames a read empties carry the next write.
    #[test]
    fn a_steady_stream_takes_no_frames() {
        const CHUNK: usize = 64 << 10;
        for backlog in [0, CHUNK, PIPE_CAPACITY - CHUNK] {
            let mut pipe = Held::new();
            pipe.write(&vec![1; backlog]);
            pipe.write(&vec![1; CHUNK]);
            pipe.read(CHUNK);
            let taken = pipe.taken;
            for _ in 0..100 {
                pipe.write(&vec![1; CHUNK]);
                pipe.read(CHUNK);
            }
            assert_eq!(pipe.taken, taken, "a backlog of {backlog} bytes took frames each round");
        }
    }

    /// A pipe that held much and is drained keeps what its last write
    /// spanned and gives back the rest, so after a small write and its read
    /// it keeps one frame, which carries the next whole write; `clear` gives
    /// back every one.
    #[test]
    fn a_drained_pipe_gives_its_frames_back() {
        let mut pipe = Held::new();
        pipe.write(&vec![1; PIPE_CAPACITY - 1]);
        pipe.write(&[1]);
        assert_eq!(pipe.bytes.frames(), PIPE_CAPACITY / FRAME);
        pipe.read(PIPE_CAPACITY);
        assert_eq!((pipe.bytes.frames(), pipe.returned.len()), (1, PIPE_CAPACITY / FRAME - 1));
        pipe.write(&[1; MAX_WHOLE_PIPE_WRITE]);
        assert_eq!(pipe.taken, PIPE_CAPACITY / FRAME, "the kept frame did not carry the next whole write");
        assert_eq!(pipe.bytes.clear().count(), 1);
        assert_eq!((pipe.bytes.len(), pipe.bytes.frames()), (0, 0));
    }

    /// **The claims carry the data-race argument.** A write placed a run at a
    /// time with reads claimed, peeked and consumed between its runs, as
    /// another CPU's read runs while a write copies with no lock held: every
    /// frame a run is handed is the one an earlier run of the claim was
    /// handed for those bytes, no read is handed a claimed byte, and none of
    /// the claim's frames is given back as surplus.
    #[test]
    fn a_write_claim_keeps_its_frames_while_reads_drain_beside_it() {
        let mut seed = 0x2545_f491_u64;
        let mut random = move |below: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % below as u64) as usize
        };
        let mut pipe = Held::new();
        let (mut written, mut read) = (0, 0);
        for _ in 0..300 {
            let n = random(4 * FRAME + 1).min(pipe.bytes.space());
            pipe.claim(n);
            let src = pattern(written, n);
            // Where each claimed byte's run was first handed out, by frame id and offset.
            let mut handed: Vec<Option<(usize, usize)>> = vec![None; n];
            let mut at = 0;
            while at < n {
                let len = (1 + random(FRAME)).min(n - at);
                pipe.bytes.place(at, len, |frame, within, from| {
                    frame.bytes[within.clone()].copy_from_slice(&src[at + from..at + from + within.len()]);
                    for (i, off) in within.enumerate() {
                        handed[at + from + i] = Some((frame.id, off));
                    }
                });
                at += len;
                let m = random(pipe.bytes.len() + 1);
                let mut out = vec![0; m];
                assert!(pipe.bytes.claim_read(m));
                pipe.bytes.peek(0, m, |frame, within, from| {
                    out[from..from + within.len()].copy_from_slice(&frame.bytes[within]);
                });
                pipe.bytes.consume();
                pipe.returned.extend(pipe.bytes.surplus(usize::MAX).map(|frame| frame.id));
                assert_eq!(out, pattern(read, m), "a read beside a claim came back wrong");
                read += m;
                pipe.accounted();
                for (byte, place) in handed.iter().enumerate().take(at) {
                    let (id, _) = place.expect("placed");
                    assert!(!pipe.returned.contains(&id), "claimed byte {byte}'s frame {id} was given back");
                }
            }
            // Placed again over what was placed: each byte lands where it did.
            pipe.bytes.place(0, n, |frame, within, from| {
                for (i, off) in within.enumerate() {
                    assert_eq!(handed[from + i], Some((frame.id, off)), "claimed byte {} moved", from + i);
                }
            });
            pipe.bytes.commit();
            written += n;
        }
        let rest = pipe.read(PIPE_CAPACITY);
        assert_eq!(rest, pattern(read, written - read));
    }

    /// One claim of each kind at a time: a second is refused and claims
    /// nothing, and a claim's bytes are not another's.
    #[test]
    fn a_second_claim_is_refused_until_the_first_ends() {
        let mut pipe = Held::new();
        pipe.claim(10);
        assert!(!pipe.bytes.claim_write(1), "a second write claimed beside the first");
        assert_eq!(pipe.bytes.len(), 0, "a claimed write was held before its commit");
        assert_eq!(pipe.bytes.space(), PIPE_CAPACITY - 10, "the claimed room was offered again");
        pipe.bytes.place(0, 10, |frame, within, at| {
            frame.bytes[within.clone()].copy_from_slice(&pattern(at, within.len()));
        });
        pipe.bytes.commit();
        assert!(pipe.bytes.claim_read(4));
        assert!(!pipe.bytes.claim_read(1), "a second read claimed beside the first");
        pipe.bytes.consume();
        assert!(!pipe.bytes.claimed());
        assert_eq!(pipe.read(10), pattern(4, 6));
    }

    /// An abandoned claim holds nothing and lets nothing go.
    #[test]
    fn an_abandoned_claim_changes_nothing() {
        let mut pipe = Held::new();
        pipe.write(&pattern(0, 8));
        pipe.claim(FRAME);
        pipe.bytes.abandon_write();
        assert_eq!((pipe.bytes.len(), pipe.bytes.space()), (8, PIPE_CAPACITY - 8));
        assert!(pipe.bytes.claim_read(8));
        pipe.bytes.abandon_read();
        assert_eq!(pipe.read(8), pattern(0, 8));
    }

    /// A read that empties the pipe while a write holds its claim does not
    /// move the claim's bytes back to the start of the frame they began in.
    #[test]
    fn a_drained_pipe_beside_a_claim_keeps_the_claims_place() {
        let mut pipe = Held::new();
        pipe.write(&pattern(0, 100));
        pipe.claim(50);
        pipe.bytes.place(0, 20, |frame, within, at| {
            frame.bytes[within.clone()].copy_from_slice(&pattern(100 + at, within.len()));
        });
        assert_eq!(pipe.read(100), pattern(0, 100));
        pipe.bytes.place(20, 30, |frame, within, at| {
            frame.bytes[within.clone()].copy_from_slice(&pattern(120 + at, within.len()));
        });
        pipe.bytes.commit();
        assert_eq!(pipe.read(50), pattern(100, 50));
    }

    #[test]
    #[should_panic(expected = "cleared under a claim")]
    fn bytes_are_never_cleared_under_a_claim() {
        let mut pipe = Held::new();
        pipe.claim(1);
        let _ = pipe.bytes.clear();
    }

    /// The surplus given back a batch at a time is the surplus given back
    /// at once.
    #[test]
    fn the_surplus_comes_back_in_batches() {
        let (mut whole, mut batched) = (Held::new(), Held::new());
        for pipe in [&mut whole, &mut batched] {
            pipe.write(&vec![1; PIPE_CAPACITY - 1]);
            pipe.write(&[1]);
            assert!(pipe.bytes.claim_read(PIPE_CAPACITY));
            pipe.bytes.consume();
        }
        let all: Vec<usize> = whole.bytes.surplus(usize::MAX).map(|f| f.id).collect();
        let mut got: Vec<usize> = Vec::new();
        loop {
            let batch: Vec<usize> = batched.bytes.surplus(7).map(|f| f.id).collect();
            got.extend(&batch);
            if batch.len() < 7 {
                break;
            }
        }
        got.sort_unstable();
        let mut all = all;
        all.sort_unstable();
        assert_eq!(got, all);
        assert_eq!(whole.bytes.frames(), batched.bytes.frames());
    }
}
