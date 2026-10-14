//! The receive reordering buffer of one block-ack agreement (§10.25.6.6): MPDUs held by sequence
//! number and passed up in order.
//!
//! A frame in the window is held, and the run from the window's start passes up. A frame past
//! the window's end, within half the sequence space, moves the window to end at it, passing up
//! everything held before the new start. One further back than that is old, and one already held
//! is a duplicate: both are dropped, the ordinary fate of a retransmission. A
//! BlockAckReq's Starting Sequence Number moves the start forward as a frame would, and so does
//! a device's release or a timeout, through [`Window::release_to`].

use alloc::vec::Vec;
use core::num::{NonZeroU16, NonZeroUsize};

use crate::{Refused, Seq};

/// The largest buffer an HE agreement negotiates (§10.25.2); HT and VHT stop at 64.
pub const MAX_SIZE: u16 = 256;
const HALF: u16 = Seq::MODULUS / 2;

/// What became of a received frame.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    /// Held, or passed up with the run it completed.
    Taken,
    /// Dropped: before the window.
    Old,
    /// Dropped: its sequence number is already held.
    Duplicate,
}

pub struct Window<T> {
    start: Seq,
    /// Slot `(head + n) % size` holds sequence number `start + n`.
    slots: Vec<Option<T>>,
    size: NonZeroU16,
    head: usize,
}

impl<T> Window<T> {
    /// A window of `size` MPDUs starting at the agreement's Starting Sequence Number.
    pub fn new(ssn: Seq, size: u16) -> Result<Self, Refused> {
        let Some(size) = NonZeroU16::new(size).filter(|s| s.get() <= MAX_SIZE) else {
            return Err(Refused::WindowSize);
        };
        let mut slots = Vec::new();
        slots.resize_with(usize::from(size.get()), || None);
        Ok(Self { start: ssn, slots, size, head: 0 })
    }

    pub const fn start(&self) -> Seq {
        self.start
    }

    /// Take `item`, received with sequence number `seq`, and pass up into `out` every item it
    /// puts in order.
    pub fn receive(&mut self, seq: Seq, item: T, out: &mut Vec<T>) -> Received {
        let ahead = seq.since(self.start);
        if ahead >= HALF {
            return Received::Old;
        }
        // A frame past the window's end becomes its end: the start moves to `size - 1` before it.
        if let Some(past) = ahead.checked_sub(self.size.get().saturating_sub(1)).filter(|&p| p > 0) {
            self.advance(past, out);
        }
        let offset = usize::from(seq.since(self.start));
        let slot = self.slot(offset);
        if slot.is_some() {
            return Received::Duplicate;
        }
        *slot = Some(item);
        self.pass_run(out);
        Received::Taken
    }

    /// Move the window's start forward to `ssn`, passing up every held item before it, then the
    /// run from it. A `ssn` at or behind the start changes nothing.
    pub fn release_to(&mut self, ssn: Seq, out: &mut Vec<T>) {
        let ahead = ssn.since(self.start);
        if ahead == 0 || ahead >= HALF {
            return;
        }
        self.advance(ahead, out);
        self.pass_run(out);
    }

    /// The sequence number of the first item held, which a timeout releases to.
    pub fn first_held(&self) -> Option<Seq> {
        (0..self.slots.len())
            .find(|&n| self.peek(n))
            .and_then(|n| u16::try_from(n).ok())
            .map(|n| self.start.add(n))
    }

    fn index(&self, offset: usize) -> usize {
        self.head.wrapping_add(offset) % NonZeroUsize::from(self.size)
    }

    fn peek(&self, offset: usize) -> bool {
        self.slots.get(self.index(offset)).is_some_and(Option::is_some)
    }

    fn slot(&mut self, offset: usize) -> &mut Option<T> {
        let index = self.index(offset);
        match self.slots.get_mut(index) {
            Some(slot) => slot,
            None => unreachable!("an index modulo the slot count is a slot"),
        }
    }

    /// Move the start `n` forward, passing up what the slots it leaves hold.
    fn advance(&mut self, n: u16, out: &mut Vec<T>) {
        for _ in 0..usize::from(n).min(self.slots.len()) {
            out.extend(self.slot(0).take());
            self.head = self.index(1);
        }
        self.start = self.start.add(n);
    }

    /// Pass up the run of held items from the start.
    fn pass_run(&mut self, out: &mut Vec<T>) {
        while let Some(item) = self.slot(0).take() {
            out.push(item);
            self.head = self.index(1);
            self.start = self.start.add(1);
        }
    }
}
