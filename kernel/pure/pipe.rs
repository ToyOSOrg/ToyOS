//! Who holds a pipe's two ends, and what a watch on one of them is told.
//!
//! A pipe end is held by counted references, one per object that names it: a
//! handle duplicated or moved to another process is the same object or another
//! reference, so an end is gone only when the last of them is. Every answer
//! here is a function of the two counts and of what the ring holds, read under
//! the kernel's pipe lock.

#![forbid(unsafe_code)]

use toyos_abi::syscall::MAX_WHOLE_PIPE_WRITE;

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
    pub fn readable(&self, available: u32) -> bool {
        available > 0 || !self.held(End::Write)
    }

    /// A write waiting for `want` bytes of room answers now: it lands, or is
    /// refused for want of a reader.
    pub fn writable(&self, space: u32, want: usize) -> bool {
        space as usize >= want || !self.held(End::Read)
    }

    /// The end opposite `end` has no holder, whatever the ring holds.
    pub fn other_end_gone(&self, end: End) -> bool {
        !self.held(end.other())
    }
}

/// How a write lands in a pipe's ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Landing {
    /// All of it or none: a message.
    Whole,
    /// Whole up to [`MAX_WHOLE_PIPE_WRITE`] bytes, and as far as it fits past
    /// that: a pipe end's write.
    Pipe,
    /// As far as it fits: a connection's plain write.
    Partial,
}

impl Landing {
    /// The room a write of `len` bytes waits for: it lands as soon as there is
    /// this much, and not before.
    pub fn room(self, len: usize) -> usize {
        match self {
            Self::Whole => len,
            Self::Pipe if len <= MAX_WHOLE_PIPE_WRITE => len,
            Self::Pipe | Self::Partial => len.min(1),
        }
    }

    /// How many of `len` bytes land with `space` free; `None` until there is
    /// [`Self::room`].
    pub fn takes(self, space: u32, len: usize) -> Option<usize> {
        let space = space as usize;
        (space >= self.room(len)).then(|| len.min(space))
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

    /// What a write answers, as `kernel::pipe::write` does.
    #[derive(PartialEq, Eq, Debug)]
    enum Wrote {
        Bytes(usize),
        /// A blocking write waits for [`Landing::room`]; a non-blocking one
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

        fn space(&self) -> u32 {
            (self.cap - self.ring.len()) as u32
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
        for landing in [Landing::Whole, Landing::Pipe, Landing::Partial] {
            let mut pipe = Model::new(64);
            pipe.ring.extend([0; 64]);
            assert!(pipe.wakes(0, landing), "{landing:?}: an empty write waits on a full ring");
            assert_eq!(pipe.write(&[], landing), Wrote::Bytes(0));
            assert_eq!(pipe.holders.release(End::Read), Released::EndGone);
            assert_eq!(pipe.write(&[], landing), Wrote::Broken);
        }
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
        for landing in [Landing::Whole, Landing::Pipe, Landing::Partial] {
            for len in [0, 1, 2, 40, MAX - 1, MAX, MAX + 1, 2 * MAX] {
                for space in 0..=cap {
                    let mut pipe = Model::new(cap);
                    pipe.ring.extend(core::iter::repeat_n(0, cap - space));
                    let wakes = pipe.wakes(len, landing);
                    let lands = pipe.write(&vec![7; len], landing) != Wrote::NoRoom;
                    assert_eq!(wakes, lands, "{landing:?}, {len} bytes, {space} free: woken {wakes}, lands {lands}");
                }
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
}
