//! The stack's end and the driver's end of a link's region.
//!
//! **Each end holds cursors, never the region**: every call is handed the
//! region's words, and an end answers the [`Span`] of a slot its adapter
//! copies a frame into or out of, once.
//!
//! **Everything the peer writes is bounded before it is believed**: a cursor
//! by the transport, a frame's length by the slot, the link word by
//! [`Link::decode`], and the groups by their count and their encoding. A
//! [`Fault`] names what the peer wrote, and ends the session. Counter values
//! are the one exception: they are displayed, never acted on.
//!
//! **A full ring wants its room back.** The producer that finds no room sets
//! its ring's want and looks again; the consumer that releases takes the want
//! and is told to ring. Both are one `AcqRel` swap of the want's word, so
//! neither misses the other: in that word's order, the consumer's swap after
//! the producer's takes the want, and the producer's after the consumer's
//! acquires the release before it, so its look finds the room.

use core::fmt;
use core::sync::atomic::Ordering;

use toyos::transport::{Consumer, Producer, Span, Violation, Word};

use crate::layout::{self, RX, RX_DEPTH, SLOT_BYTES, TX, TX_DEPTH, WORDS};
use crate::state::{Counter, Counters, Group, Link};

/// The peer wrote what no peer of the protocol writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    Ring(Violation),
    /// A frame of no bytes, or longer than its slot.
    Length(u32),
    /// A link word no driver writes.
    Link(u32),
    /// More groups than the region holds, or fewer than the driver applied.
    Groups(u32),
    /// A group's words that name no group address.
    Group,
}

impl From<Violation> for Fault {
    fn from(violation: Violation) -> Self {
        Self::Ring(violation)
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ring(violation) => write!(f, "a ring's cursor out of bounds ({violation:?})"),
            Self::Length(len) => write!(f, "a {len}-byte frame, where a frame is 1 to {SLOT_BYTES} bytes"),
            Self::Link(word) => write!(f, "link word {word:#010x}, which names no link"),
            Self::Groups(count) => write!(f, "{count} multicast groups joined"),
            Self::Group => write!(f, "a multicast group that is no group address"),
        }
    }
}

/// A frame's length as its entry says it, bounded by its slot.
fn length(word: u32) -> Result<usize, Fault> {
    match usize::try_from(word) {
        Ok(len) if (1..=SLOT_BYTES).contains(&len) => Ok(len),
        _ => Err(Fault::Length(word)),
    }
}

/// The first `len` bytes of `slot`.
fn frame(slot: Span, len: usize) -> Span {
    Span { offset: slot.offset, len }
}

/// How the want's word is swapped.
#[cfg(not(feature = "mutate-want-relaxed"))]
const WANT: Ordering = Ordering::AcqRel;
#[cfg(feature = "mutate-want-relaxed")]
const WANT: Ordering = Ordering::Relaxed;

/// Set `want`, and answer the room the ring has after it.
fn want<W: Word>(page: &[W; WORDS], want: usize, room: impl FnOnce() -> Result<u32, Violation>) -> Result<u32, Fault> {
    page[want].swap(1, WANT);
    Ok(room()?)
}

/// Whether the peer wanted the room just released, taking its want.
fn wanted<W: Word>(page: &[W; WORDS], want: usize) -> bool {
    page[want].swap(0, WANT) != 0
}

/// The stack's end: received frames in, frames to send out.
pub struct Stack {
    rx: Consumer<1, RX_DEPTH, WORDS>,
    tx: Producer<1, TX_DEPTH, WORDS>,
    received: u32,
    sent: u32,
    joined: u32,
    asked: u32,
}

impl Stack {
    /// This end of a region the driver sent, every word it owns set to 0.
    pub fn new<W: Word>(page: &[W; WORDS]) -> Self {
        for at in [layout::TX_WANT, layout::GROUPS_JOINED, layout::COUNTERS_ASKED] {
            page[at].store(0, Ordering::Relaxed);
        }
        Self { rx: Consumer::new(page, RX), tx: Producer::new(page, TX), received: 0, sent: 0, joined: 0, asked: 0 }
    }

    /// The link as the driver last said it.
    pub fn link<W: Word>(&self, page: &[W; WORDS]) -> Result<Link, Fault> {
        let word = page[layout::LINK].load(Ordering::Acquire);
        Link::decode(word).ok_or(Fault::Link(word))
    }

    /// The bytes of the next received frame, or `None` for none. They are
    /// the stack's until [`Self::release`].
    pub fn received<W: Word>(&mut self, page: &[W; WORDS]) -> Result<Option<Span>, Fault> {
        let Some([len]) = self.rx.pop(page)? else { return Ok(None) };
        let slot = layout::rx_slot(self.received).span();
        self.received = self.received.wrapping_add(1);
        Ok(Some(frame(slot, length(len)?)))
    }

    /// Give every frame taken back to the driver; `true` when it waits for
    /// the room and is to be rung.
    pub fn release<W: Word>(&mut self, page: &[W; WORDS]) -> bool {
        self.rx.release(page);
        wanted(page, layout::RX_WANT)
    }

    /// How many frames may be sent before the driver takes more.
    pub fn room<W: Word>(&mut self, page: &[W; WORDS]) -> Result<u32, Fault> {
        Ok(self.tx.space(page)?)
    }

    /// [`Self::room`], having asked to be rung when the driver frees some.
    pub fn want_room<W: Word>(&mut self, page: &[W; WORDS]) -> Result<u32, Fault> {
        want(page, layout::TX_WANT, || self.tx.space(page))
    }

    /// The slot the next frame sent is copied into.
    pub fn slot(&self) -> Span {
        layout::tx_slot(self.sent).span()
    }

    /// Hand the driver the `len` bytes just copied into [`Self::slot`].
    ///
    /// # Panics
    /// On a frame no slot holds, or a ring [`Self::room`] did not answer room
    /// in: the stack's own bug.
    pub fn send<W: Word>(&mut self, page: &[W; WORDS], len: usize) -> Result<(), Fault> {
        assert!((1..=SLOT_BYTES).contains(&len), "link: a {len}-byte frame offered to a {SLOT_BYTES}-byte slot");
        let word = u32::try_from(len).unwrap_or_else(|_| unreachable!("a slot's length is a u32"));
        assert!(self.tx.push(page, [word])?, "link: a frame offered to a ring with no room");
        self.sent = self.sent.wrapping_add(1);
        Ok(())
    }

    /// Publish every frame sent; `true` when there was one, and the driver
    /// is to be rung.
    pub fn publish<W: Word>(&mut self, page: &[W; WORDS]) -> bool {
        self.tx.publish(page)
    }

    /// Ask the driver to pass frames sent to `group`; `false` when the
    /// region holds no more groups. The driver is to be rung.
    pub fn join<W: Word>(&mut self, page: &[W; WORDS], group: Group) -> bool {
        let Ok(at) = usize::try_from(self.joined) else { return false };
        if at >= layout::MAX_GROUPS {
            return false;
        }
        let [lo, hi] = group.encode();
        page[layout::GROUPS + 2 * at].store(lo, Ordering::Relaxed);
        page[layout::GROUPS + 2 * at + 1].store(hi, Ordering::Relaxed);
        self.joined = self.joined.wrapping_add(1);
        page[layout::GROUPS_JOINED].store(self.joined, Ordering::Release);
        true
    }

    /// Ask the driver for its counters. The driver is to be rung, and
    /// [`Self::counters`] reads them once it answered.
    pub fn ask_counters<W: Word>(&mut self, page: &[W; WORDS]) {
        self.asked = self.asked.wrapping_add(1);
        page[layout::COUNTERS_ASKED].store(self.asked, Ordering::Release);
    }

    /// The counters the driver answered the last ask with, or `None` while it
    /// has not answered it.
    pub fn counters<W: Word>(&self, page: &[W; WORDS]) -> Option<Counters> {
        if page[layout::COUNTERS_ANSWERED].load(Ordering::Acquire) != self.asked {
            return None;
        }
        let present = page[layout::COUNTERS_PRESENT].load(Ordering::Relaxed);
        let mut counters = Counters::default();
        for counter in Counter::ALL.into_iter().filter(|c| present & 1 << c.index() != 0) {
            let at = layout::COUNTERS + 2 * counter.index();
            let lo = page[at].load(Ordering::Relaxed);
            let hi = page[at + 1].load(Ordering::Relaxed);
            counters.set(counter, u64::from(hi) << 32 | u64::from(lo));
        }
        Some(counters)
    }
}

/// The driver's end: received frames out, frames to send in.
pub struct Driver {
    rx: Producer<1, RX_DEPTH, WORDS>,
    tx: Consumer<1, TX_DEPTH, WORDS>,
    received: u32,
    sent: u32,
    applied: u32,
}

impl Driver {
    /// This end of a region the driver made, every word it owns set: its
    /// link down and no counters answered.
    pub fn new<W: Word>(page: &[W; WORDS]) -> Self {
        for at in [layout::LINK, layout::RX_WANT, layout::COUNTERS_ANSWERED, layout::COUNTERS_PRESENT] {
            page[at].store(0, Ordering::Relaxed);
        }
        Self { rx: Producer::new(page, RX), tx: Consumer::new(page, TX), received: 0, sent: 0, applied: 0 }
    }

    /// Say the link. The stack is to be rung.
    pub fn set_link<W: Word>(&self, page: &[W; WORDS], link: Link) {
        page[layout::LINK].store(link.encode(), Ordering::Release);
    }

    /// How many frames may be received before the stack takes more.
    pub fn room<W: Word>(&mut self, page: &[W; WORDS]) -> Result<u32, Fault> {
        Ok(self.rx.space(page)?)
    }

    /// [`Self::room`], having asked to be rung when the stack frees some.
    pub fn want_room<W: Word>(&mut self, page: &[W; WORDS]) -> Result<u32, Fault> {
        want(page, layout::RX_WANT, || self.rx.space(page))
    }

    /// The slot the next received frame is copied into.
    pub fn slot(&self) -> Span {
        layout::rx_slot(self.received).span()
    }

    /// Hand the stack the `len` bytes just copied into [`Self::slot`].
    ///
    /// # Panics
    /// As [`Stack::send`]: the driver's own bug.
    pub fn receive<W: Word>(&mut self, page: &[W; WORDS], len: usize) -> Result<(), Fault> {
        assert!((1..=SLOT_BYTES).contains(&len), "link: a {len}-byte frame offered to a {SLOT_BYTES}-byte slot");
        let word = u32::try_from(len).unwrap_or_else(|_| unreachable!("a slot's length is a u32"));
        assert!(self.rx.push(page, [word])?, "link: a frame offered to a ring with no room");
        self.received = self.received.wrapping_add(1);
        Ok(())
    }

    /// Publish every frame received; `true` when there was one, and the
    /// stack is to be rung.
    pub fn publish<W: Word>(&mut self, page: &[W; WORDS]) -> bool {
        self.rx.publish(page)
    }

    /// The bytes of the next frame to send, or `None` for none. They are the
    /// driver's until [`Self::release`].
    pub fn to_send<W: Word>(&mut self, page: &[W; WORDS]) -> Result<Option<Span>, Fault> {
        let Some([len]) = self.tx.pop(page)? else { return Ok(None) };
        let slot = layout::tx_slot(self.sent).span();
        self.sent = self.sent.wrapping_add(1);
        Ok(Some(frame(slot, length(len)?)))
    }

    /// Give every frame taken back to the stack; `true` when it waits for the
    /// room and is to be rung.
    pub fn release<W: Word>(&mut self, page: &[W; WORDS]) -> bool {
        self.tx.release(page);
        wanted(page, layout::TX_WANT)
    }

    /// The next group the stack joined since the last call, or `None`.
    pub fn joined<W: Word>(&mut self, page: &[W; WORDS]) -> Result<Option<Group>, Fault> {
        let count = page[layout::GROUPS_JOINED].load(Ordering::Acquire);
        let held = usize::try_from(count).is_ok_and(|n| n <= layout::MAX_GROUPS);
        if !held || count < self.applied {
            return Err(Fault::Groups(count));
        }
        if count == self.applied {
            return Ok(None);
        }
        let at = layout::GROUPS + 2 * usize::try_from(self.applied).unwrap_or_else(|_| unreachable!("below MAX_GROUPS"));
        let words = [page[at].load(Ordering::Relaxed), page[at + 1].load(Ordering::Relaxed)];
        self.applied = self.applied.wrapping_add(1);
        Group::decode(words).map(Some).ok_or(Fault::Group)
    }

    /// The stack's ask for counters, while it is unanswered.
    pub fn counters_asked<W: Word>(&self, page: &[W; WORDS]) -> Option<u32> {
        let asked = page[layout::COUNTERS_ASKED].load(Ordering::Acquire);
        (asked != page[layout::COUNTERS_ANSWERED].load(Ordering::Relaxed)).then_some(asked)
    }

    /// Answer the ask `asked` with `counters`. The stack is to be rung.
    pub fn answer<W: Word>(&self, page: &[W; WORDS], asked: u32, counters: &Counters) {
        for (counter, value) in counters.iter() {
            let at = layout::COUNTERS + 2 * counter.index();
            page[at].store(value as u32, Ordering::Relaxed);
            page[at + 1].store((value >> 32) as u32, Ordering::Relaxed);
        }
        page[layout::COUNTERS_PRESENT].store(counters.present(), Ordering::Relaxed);
        page[layout::COUNTERS_ANSWERED].store(asked, Ordering::Release);
    }
}
