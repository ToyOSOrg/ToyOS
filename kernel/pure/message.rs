//! One direction of a connection: whole messages in the order they were sent,
//! each its length and the handles it carries.
//!
//! A message is queued whole or refused, and taken whole or left queued, so a
//! reader never sees part of one and its handles never come apart from its
//! bytes. The bytes live beside this queue in the same order; it counts them
//! only to say where a message ends.

#![forbid(unsafe_code)]

use alloc::collections::VecDeque;

use toyos_abi::syscall::{MAX_MESSAGE_LEN, MAX_QUEUED_HANDLES, MAX_QUEUED_MESSAGES, MAX_TRANSFER_HANDLES};

/// The message at the front of a queue.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Head {
    pub len: usize,
    pub handles: usize,
}

/// Why the front message stays queued.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Untaken {
    /// The reader's byte or handle buffer cannot hold it whole.
    TooSmall,
    /// The reader has no room in its table for the handles it carries.
    NoRoom,
}

/// The messages a reader has not taken, and their handles, at most
/// [`MAX_QUEUED_MESSAGES`] and [`MAX_QUEUED_HANDLES`].
pub struct Queue<H> {
    messages: VecDeque<Head>,
    handles: VecDeque<H>,
}

impl<H> Default for Queue<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> Queue<H> {
    pub const fn new() -> Self {
        Self { messages: VecDeque::new(), handles: VecDeque::new() }
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn head(&self) -> Option<Head> {
        self.messages.front().copied()
    }

    /// Whether a message of `len` bytes carrying `handles` fits now, beside
    /// `space` bytes of room in the bytes it is written to.
    pub fn fits(&self, space: usize, len: usize, handles: usize) -> bool {
        len <= space
            && self.messages.len() < MAX_QUEUED_MESSAGES
            && self.handles.len() + handles <= MAX_QUEUED_HANDLES
    }

    /// Whether the largest message one send may make fits now: what a
    /// writable connection promises.
    pub fn fits_any(&self, space: usize) -> bool {
        self.fits(space, MAX_MESSAGE_LEN, MAX_TRANSFER_HANDLES)
    }

    /// Queue a message whose bytes are written.
    ///
    /// # Panics
    /// When it does not fit by count: [`Self::fits`] is asked under the same
    /// hold, before its bytes are written.
    pub fn push(&mut self, len: usize, handles: impl ExactSizeIterator<Item = H>) {
        let count = handles.len();
        assert!(
            self.messages.len() < MAX_QUEUED_MESSAGES && self.handles.len() + count <= MAX_QUEUED_HANDLES,
            "message: a message queued past the queue's bounds"
        );
        self.handles.extend(handles);
        self.messages.push_back(Head { len, handles: count });
    }

    /// Take the front message into a reader holding `buf` bytes and `cap`
    /// handles, whose table `room` says can take its handles, which come
    /// back in order. Its bytes are the caller's to copy out before this
    /// hold ends.
    pub fn pop(
        &mut self,
        buf: usize,
        cap: usize,
        room: impl FnOnce(usize) -> bool,
    ) -> Option<Result<(Head, impl Iterator<Item = H> + '_), Untaken>> {
        let head = self.head()?;
        if head.len > buf || head.handles > cap {
            return Some(Err(Untaken::TooSmall));
        }
        if head.handles > 0 && !room(head.handles) {
            return Some(Err(Untaken::NoRoom));
        }
        self.messages.pop_front();
        Some(Ok((head, self.handles.drain(..head.handles))))
    }

    /// Every handle still queued, for the reader's end, with the messages.
    pub fn clear(&mut self) -> VecDeque<H> {
        self.messages.clear();
        core::mem::take(&mut self.handles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// A message as taken: its bytes, numbered by the order they were sent,
    /// and its handles.
    type Message = (Vec<u64>, Vec<u32>);

    /// A direction as the kernel holds it: the bytes numbered by the order
    /// they were sent, in a store of `room` bytes.
    struct Direction {
        queue: Queue<u32>,
        bytes: VecDeque<u64>,
        next_byte: u64,
        room: usize,
    }

    impl Direction {
        fn new(room: usize) -> Self {
            Self { queue: Queue::new(), bytes: VecDeque::new(), next_byte: 0, room }
        }

        fn send(&mut self, len: usize, handles: &[u32]) -> bool {
            if !self.queue.fits(self.room - self.bytes.len(), len, handles.len()) {
                return false;
            }
            for _ in 0..len {
                self.bytes.push_back(self.next_byte);
                self.next_byte += 1;
            }
            self.queue.push(len, handles.iter().copied());
            true
        }

        fn recv(&mut self, buf: usize, cap: usize, room: bool) -> Option<Result<Message, Untaken>> {
            let (head, got) = match self.queue.pop(buf, cap, |_| room)? {
                Ok((head, handles)) => (head, handles.collect()),
                Err(untaken) => return Some(Err(untaken)),
            };
            Some(Ok((self.bytes.drain(..head.len).collect(), got)))
        }
    }

    #[test]
    fn a_message_is_taken_whole_with_its_own_handles() {
        let mut d = Direction::new(64);
        assert!(d.send(3, &[7, 8]));
        assert!(d.send(2, &[]));
        assert!(d.send(1, &[9]));
        assert_eq!(d.recv(64, 8, true), Some(Ok((alloc::vec![0, 1, 2], alloc::vec![7, 8]))));
        assert_eq!(d.recv(64, 8, true), Some(Ok((alloc::vec![3, 4], alloc::vec![]))));
        assert_eq!(d.recv(64, 8, true), Some(Ok((alloc::vec![5], alloc::vec![9]))));
        assert_eq!(d.recv(64, 8, true), None);
    }

    /// A reader that cannot hold the front message whole takes nothing of it,
    /// and takes it whole once it can.
    #[test]
    fn a_message_too_large_for_the_reader_stays_queued_whole() {
        let mut d = Direction::new(64);
        assert!(d.send(4, &[1, 2]));
        assert_eq!(d.recv(3, 8, true), Some(Err(Untaken::TooSmall)), "a buffer a byte short");
        assert_eq!(d.recv(4, 1, true), Some(Err(Untaken::TooSmall)), "a handle buffer one short");
        assert_eq!(d.recv(4, 2, false), Some(Err(Untaken::NoRoom)), "a table with no room");
        assert_eq!(d.recv(4, 2, true), Some(Ok((alloc::vec![0, 1, 2, 3], alloc::vec![1, 2]))));
    }

    /// The table's room is asked only for a message that carries handles.
    #[test]
    fn a_message_without_handles_asks_no_room() {
        let mut d = Direction::new(8);
        assert!(d.send(1, &[]));
        assert_eq!(d.recv(8, 0, false), Some(Ok((alloc::vec![0], alloc::vec![]))));
    }

    /// Each bound refuses the message that would cross it and nothing before.
    #[test]
    fn each_bound_refuses_the_message_past_it() {
        let mut d = Direction::new(MAX_QUEUED_MESSAGES * 2);
        for _ in 0..MAX_QUEUED_MESSAGES {
            assert!(d.send(1, &[]));
        }
        assert!(!d.send(1, &[]), "one message past the count bound was queued");

        let mut d = Direction::new(1 << 20);
        let full = [0u32; MAX_TRANSFER_HANDLES];
        for _ in 0..MAX_QUEUED_HANDLES / MAX_TRANSFER_HANDLES {
            assert!(d.send(1, &full));
        }
        assert!(!d.send(1, &[1]), "one handle past the handle bound was queued");
        assert!(d.send(1, &[]), "a message with no handles waits on no handle room");

        let mut d = Direction::new(10);
        assert!(d.send(10, &[]));
        assert!(!d.send(1, &[]), "a byte past the store was queued");
    }

    /// Writable promises room for the largest message one send may make.
    #[test]
    fn fits_any_is_room_for_the_largest_send() {
        let q: Queue<u32> = Queue::new();
        assert!(q.fits_any(MAX_MESSAGE_LEN));
        assert!(!q.fits_any(MAX_MESSAGE_LEN - 1));
        let mut q: Queue<u32> = Queue::new();
        q.push(1, core::iter::repeat_n(0, MAX_QUEUED_HANDLES - MAX_TRANSFER_HANDLES + 1));
        assert!(!q.fits_any(usize::MAX), "room for fewer than a send's handles is not writable");
    }

    #[test]
    #[should_panic(expected = "past the queue's bounds")]
    fn a_push_past_the_bounds_is_a_caller_bug() {
        let mut q = Queue::new();
        q.push(1, core::iter::repeat_n(0u32, MAX_QUEUED_HANDLES + 1));
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

    /// Random sends and receives of random sizes against the order they were
    /// sent: every message arrives once, whole, in order, with exactly its own
    /// handles, and a refused receive takes nothing.
    #[test]
    fn every_message_arrives_once_whole_in_order_with_its_handles() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut d = Direction::new(512);
        let mut sent: VecDeque<(Vec<u64>, Vec<u32>)> = VecDeque::new();
        let mut next_handle = 0u32;
        let mut taken = 0;
        for _ in 0..50_000u32 {
            if rng.below(2) == 0 {
                let len = rng.below(48) as usize;
                let count = if rng.below(3) == 0 { 1 + rng.below(MAX_TRANSFER_HANDLES as u64) as usize } else { 0 };
                let handles: Vec<u32> = (next_handle..next_handle + count as u32).collect();
                let first = d.next_byte;
                if d.send(len, &handles) {
                    next_handle += count as u32;
                    sent.push_back(((first..first + len as u64).collect(), handles));
                }
            } else {
                let buf = rng.below(56) as usize;
                let cap = rng.below(MAX_TRANSFER_HANDLES as u64 + 1) as usize;
                let room = rng.below(4) != 0;
                let before = (d.queue.len(), d.bytes.len());
                match d.recv(buf, cap, room) {
                    None => assert!(sent.is_empty(), "a queued message was not offered"),
                    Some(Ok(got)) => {
                        assert!(got.0.len() <= buf && got.1.len() <= cap, "a message taken into a reader too small for it");
                        assert_eq!(Some(got), sent.pop_front(), "a message out of order, cut or with another's handles");
                        taken += 1;
                    }
                    Some(Err(_)) => assert_eq!((d.queue.len(), d.bytes.len()), before, "a refused receive took something"),
                }
            }
        }
        assert!(taken > 5_000, "the run took only {taken} messages");
    }
}
