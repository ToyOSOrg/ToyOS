//! Where each batch of handles a connection carries sits in its byte stream.
//!
//! A message is bytes and the handles they announce, sent together: the bytes
//! go into the stream whole, and the batch is pinned at the byte just past
//! them. The reader's reads never cross a pinned batch it has not taken, and
//! it takes a batch only where it is pinned. So a batch is never received
//! without its bytes, nor its bytes read past without the batch noticed.
//!
//! Positions count every byte the stream ever carried, never wrapping: a
//! `u64` of bytes outlives any connection.

#![forbid(unsafe_code)]

use alloc::collections::VecDeque;

/// What a read at the reader's position may take now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Window {
    /// Up to this many bytes; zero is nothing to read yet.
    Bytes(usize),
    /// A batch is pinned exactly here, and the read must take it first.
    AtBatch,
}

/// Why a message is refused. Nothing of it is written or moved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Handles with no bytes announcing them.
    Empty,
    /// The reader holds as many batches as it may.
    QueueFull,
    /// The stream has no room for the whole message now.
    NoRoom,
    /// The reader's end is gone.
    Gone,
}

/// The batches a reader has not taken, each at the stream position it
/// follows, in the order they were sent.
pub struct Batches<B> {
    pinned: VecDeque<(u64, B)>,
}

impl<B> Default for Batches<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B> Batches<B> {
    pub const fn new() -> Self {
        Self { pinned: VecDeque::new() }
    }

    pub fn len(&self) -> usize {
        self.pinned.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty()
    }

    /// Whether a message of `len` bytes, carrying a batch or not, may be sent
    /// now: `cap` batches at most, `space` bytes of room, and a reader there.
    pub fn admit(&self, len: usize, carries: bool, cap: usize, space: usize, reader: bool) -> Result<(), Refused> {
        if carries && len == 0 {
            return Err(Refused::Empty);
        }
        if !reader {
            return Err(Refused::Gone);
        }
        if carries && self.pinned.len() >= cap {
            return Err(Refused::QueueFull);
        }
        if len > space {
            return Err(Refused::NoRoom);
        }
        Ok(())
    }

    /// Pin `batch` at `at`, the position just past the bytes that announce it.
    ///
    /// # Panics
    /// When `at` is before the last batch's position: positions only grow, so
    /// that is a caller that wrote out of order.
    pub fn pin(&mut self, at: u64, batch: B) {
        if let Some((last, _)) = self.pinned.back() {
            assert!(*last <= at, "message: a batch pinned at {at} after one at {last}");
        }
        self.pinned.push_back((at, batch));
    }

    /// What a read at `read` may take of the `available` bytes the stream holds.
    pub fn window(&self, read: u64, available: usize) -> Window {
        match self.pinned.front() {
            Some((at, _)) if *at == read => Window::AtBatch,
            Some((at, _)) => {
                let before = at.checked_sub(read).expect("message: the reader read past a pinned batch");
                Window::Bytes(available.min(usize::try_from(before).unwrap_or(usize::MAX)))
            }
            None => Window::Bytes(available),
        }
    }

    /// The batch pinned at `read`, if any, without taking it.
    pub fn at(&self, read: u64) -> Option<&B> {
        self.pinned.front().filter(|(at, _)| *at == read).map(|(_, b)| b)
    }

    /// Take the batch pinned at `read`; none elsewhere.
    pub fn take(&mut self, read: u64) -> Option<B> {
        match self.pinned.front() {
            Some((at, _)) if *at == read => self.pinned.pop_front().map(|(_, b)| b),
            _ => None,
        }
    }

    /// Every batch still pinned, for its end's release.
    pub fn drain(&mut self) -> impl Iterator<Item = B> + '_ {
        self.pinned.drain(..).map(|(_, b)| b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A stream and its batches as the kernel holds them: the bytes are
    /// numbered by their position, so a read says exactly which it took.
    struct Stream {
        batches: Batches<u32>,
        bytes: VecDeque<u64>,
        written: u64,
        read: u64,
        ring: usize,
    }

    impl Stream {
        fn new(ring: usize) -> Self {
            Self { batches: Batches::new(), bytes: VecDeque::new(), written: 0, read: 0, ring }
        }

        /// One send, whole or not at all, as the kernel makes it under the
        /// queue lock and then the pipe lock.
        fn send(&mut self, len: usize, batch: Option<u32>) -> Result<(), Refused> {
            let space = self.ring - self.bytes.len();
            self.batches.admit(len, batch.is_some(), 4, space, true)?;
            for _ in 0..len {
                self.bytes.push_back(self.written);
                self.written += 1;
            }
            if let Some(b) = batch {
                self.batches.pin(self.written, b);
            }
            Ok(())
        }

        fn read(&mut self, want: usize) -> Result<Vec<u64>, Window> {
            match self.batches.window(self.read, self.bytes.len()) {
                Window::AtBatch => Err(Window::AtBatch),
                Window::Bytes(n) => {
                    let got: Vec<u64> = self.bytes.drain(..n.min(want)).collect();
                    self.read += got.len() as u64;
                    Ok(got)
                }
            }
        }
    }

    #[test]
    fn a_batch_is_taken_only_where_its_bytes_end_and_no_read_crosses_it() {
        let mut s = Stream::new(64);
        s.send(5, Some(7)).unwrap();
        s.send(3, None).unwrap();
        assert_eq!(s.batches.take(s.read), None, "a batch taken before its bytes were read");
        assert_eq!(s.read(100).unwrap(), [0, 1, 2, 3, 4], "a read crossed the batch");
        assert_eq!(s.read(100), Err(Window::AtBatch), "a read went on past an untaken batch");
        assert_eq!(s.batches.take(s.read), Some(7));
        assert_eq!(s.read(100).unwrap(), [5, 6, 7]);
    }

    /// A refusal writes no byte and pins no batch.
    #[test]
    fn a_refused_send_leaves_nothing_behind() {
        let mut s = Stream::new(8);
        assert_eq!(s.send(0, Some(1)), Err(Refused::Empty));
        assert_eq!(s.send(9, Some(1)), Err(Refused::NoRoom));
        assert_eq!((s.bytes.len(), s.batches.len()), (0, 0));
        for b in 0..4 {
            s.send(1, Some(b)).unwrap();
        }
        assert_eq!(s.send(1, Some(9)), Err(Refused::QueueFull));
        assert_eq!((s.bytes.len(), s.batches.len()), (4, 4));
        assert_eq!(s.send(1, None), Ok(()), "a message with no handles waits on no queue");
        let gone = Batches::<u32>::new().admit(1, true, 4, 8, false);
        assert_eq!(gone, Err(Refused::Gone));
    }

    /// xorshift64*: one seed is one run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Random sends and reads of random sizes against the order they were
    /// sent: every batch reaches the reader exactly after the last byte of its
    /// own message and before any byte of the next, and every byte arrives
    /// once, in order.
    #[test]
    fn every_batch_arrives_between_its_own_bytes_and_the_next_messages() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut s = Stream::new(256);
        // What the reader must meet: per message, its bytes then its batch.
        let mut sent: VecDeque<(u64, Option<u32>)> = VecDeque::new();
        let mut next_byte = 0u64;
        let mut taken = 0;
        for op in 0..20_000u32 {
            if rng.below(2) == 0 {
                let len = 1 + rng.below(40) as usize;
                let batch = (rng.below(3) == 0).then_some(op);
                if s.send(len, batch).is_ok() {
                    sent.push_back((s.written, batch));
                }
            } else {
                match s.read(1 + rng.below(50) as usize) {
                    Ok(got) => {
                        for byte in got {
                            assert_eq!(byte, next_byte, "a byte out of order");
                            next_byte += 1;
                            // A message ending before this byte with a batch must have been taken.
                            while sent.front().is_some_and(|(end, batch)| *end <= byte && batch.is_none()) {
                                sent.pop_front();
                            }
                            assert!(
                                !sent.front().is_some_and(|(end, b)| *end <= byte && b.is_some()),
                                "byte {byte} was read past a batch pinned at {}",
                                sent.front().unwrap().0
                            );
                        }
                    }
                    Err(Window::AtBatch) => {
                        let (end, batch) = sent.pop_front().expect("a batch the model never sent");
                        assert_eq!(end, s.read, "a batch met away from the end of its bytes");
                        assert_eq!(s.batches.take(s.read), batch, "the batch taken is not its message's");
                        taken += 1;
                    }
                    Err(Window::Bytes(_)) => unreachable!(),
                }
            }
        }
        assert!(taken > 1000, "the run took only {taken} batches");
    }

    #[test]
    #[should_panic(expected = "pinned at 3 after one at 5")]
    fn a_batch_pinned_before_the_last_is_a_caller_bug() {
        let mut b = Batches::new();
        b.pin(5, 1);
        b.pin(3, 2);
    }
}
