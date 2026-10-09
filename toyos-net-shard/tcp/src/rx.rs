//! The receive side: in-order bytes the user has not read, out-of-order ranges at their stream
//! positions, the right edge the peer was offered, and what acknowledgment is owed.
//!
//! `unread + (edge − next) ≤ capacity` always holds, so every byte the peer may send has room. The
//! edge never retreats (RFC 7323 §2.4), bytes already stored are never overwritten, and bytes once
//! reported in a SACK block are kept until delivered: this receiver never reneges.

use alloc::vec::Vec;
use core::time::Duration;

use toyos_net_wire::tcp::SackBlock;

use crate::ring::Ring;
use crate::seq::Seq;
use crate::Instant;

pub const OOO_RANGES: usize = 32;
pub const DELAYED_ACK: Duration = Duration::from_millis(40);
/// Distinct duplicate ACKs held for a starved transmit: enough for the peer's fast retransmit.
pub const DUP_ACKS_OWED: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Range {
    start: Seq,
    end: Seq,
    stamp: u32,
}

impl Range {
    fn block(self) -> SackBlock {
        SackBlock { left: self.start.into(), right: self.end.into() }
    }
}

/// What placing a segment's text did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placed {
    /// Nothing new: every byte was already held.
    Nothing,
    InOrder,
    /// In order, into a hole below out-of-order ranges.
    Filled,
    OutOfOrder,
    /// It would have made one range too many.
    RangeLimit,
}

#[derive(Debug)]
pub struct Rx {
    pub next: Seq,
    edge: Seq,
    pub shift: u8,
    buf: Ring,
    ranges: Vec<Range>,
    stamp: u32,
    fin: Option<Seq>,
    pub closed: bool,
    pub discard: bool,
    pub dup_owed: u8,
    pub ack_now: bool,
    pub delayed: Option<Instant>,
    in_order_unacked: u8,
    dsack: Option<(Seq, Seq)>,
    trigger: Option<Seq>,
    /// The window, in bytes, the last segment sent offered.
    pub last_window: u32,
    pub last_ack_sent: Seq,
}

impl Rx {
    /// `next` is IRS + 1; the SYN or SYN-ACK offered `window`, unscaled.
    pub fn new(next: Seq, capacity: usize, shift: u8, window: u32) -> Self {
        Self {
            next,
            edge: next.add(window),
            shift,
            buf: Ring::new(capacity),
            ranges: Vec::new(),
            stamp: 0,
            fin: None,
            closed: false,
            discard: false,
            dup_owed: 0,
            ack_now: false,
            delayed: None,
            in_order_unacked: 0,
            dsack: None,
            trigger: None,
            last_window: window,
            last_ack_sent: next,
        }
    }

    /// RCV.WND: the largest right edge offered, less RCV.NXT (RFC 7323 §2.4 rule 1).
    pub fn window(&self) -> u32 {
        self.edge.since(self.next)
    }

    pub const fn edge(&self) -> Seq {
        self.edge
    }

    pub fn unread(&self) -> usize {
        self.buf.len()
    }

    pub fn ranges(&self) -> usize {
        self.ranges.len()
    }

    pub fn fin_remembered(&self) -> Option<Seq> {
        self.fin
    }

    fn offset(&self, seq: Seq) -> usize {
        usize::try_from(seq.since(self.next)).unwrap_or(usize::MAX).saturating_add(self.buf.len())
    }

    /// Places text that already fits the window, never overwriting a byte already held.
    pub fn place(&mut self, seq: Seq, data: &[u8]) -> Placed {
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        let end = seq.add(len);
        if len == 0 {
            return Placed::Nothing;
        }
        let first = self.ranges.iter().position(|r| r.end.at_or_after(seq));
        let touching = self.ranges.iter().filter(|r| r.start.at_or_before(end) && r.end.at_or_after(seq)).count();
        if seq != self.next && touching == 0 && self.ranges.len() >= OOO_RANGES {
            return Placed::RangeLimit;
        }
        let lo = first.unwrap_or(self.ranges.len());
        let hi = lo.saturating_add(touching);
        let mut dsack = None;
        // `Some(true)` when every gap was stored whole: text is recorded only then.
        let mut stored = None;
        let mut cursor = seq;
        for i in lo..hi {
            let Some(r) = self.ranges.get(i).copied() else { break };
            if r.start.after(cursor) {
                stored = Some(stored.unwrap_or(true) && self.store(cursor, r.start.earlier(end), seq, data));
            }
            let dup = (cursor.later(r.start), end.earlier(r.end));
            if dsack.is_none() && dup.0.before(dup.1) {
                dsack = Some(dup);
            }
            cursor = cursor.later(r.end);
        }
        if cursor.before(end) {
            stored = Some(stored.unwrap_or(true) && self.store(cursor, end, seq, data));
        }
        if dsack.is_some() {
            self.dsack = dsack;
        }
        if stored != Some(true) {
            return Placed::Nothing;
        }
        let holes = !self.ranges.is_empty();
        let stamp = self.bump();
        let merged = self.ranges.get(lo..hi).unwrap_or_default().iter().fold(Range { start: seq, end, stamp }, |m, r| Range {
            start: m.start.earlier(r.start),
            end: m.end.later(r.end),
            stamp,
        });
        self.ranges.splice(lo..hi, [merged]);
        if merged.start != self.next {
            self.trigger = Some(seq);
            return Placed::OutOfOrder;
        }
        self.ranges.remove(lo);
        self.buf.commit(usize::try_from(merged.end.since(self.next)).unwrap_or(0));
        self.next = merged.end;
        self.trigger = None;
        if self.discard {
            let n = self.buf.len();
            self.buf.consume(n);
        }
        if holes {
            Placed::Filled
        } else {
            Placed::InOrder
        }
    }

    fn store(&mut self, from: Seq, to: Seq, seq: Seq, data: &[u8]) -> bool {
        let skip = usize::try_from(from.since(seq)).unwrap_or(usize::MAX);
        let take = usize::try_from(to.since(from)).unwrap_or(0);
        let offset = self.offset(from);
        data.get(skip..skip.saturating_add(take)).is_some_and(|bytes| self.buf.write_at(offset, bytes) == bytes.len())
    }

    fn bump(&mut self) -> u32 {
        self.stamp = self.stamp.wrapping_add(1);
        self.stamp
    }

    /// A FIN at `seq`: accepted when every byte before it is here, remembered while a hole is open.
    /// `false` is a FIN conflicting with the one already known.
    pub fn fin_at(&mut self, seq: Seq) -> bool {
        match self.fin {
            Some(known) if known != seq => return false,
            _ => self.fin = Some(seq),
        }
        self.absorb_fin();
        true
    }

    pub fn absorb_fin(&mut self) {
        if !self.closed && self.fin == Some(self.next) {
            self.next = self.next.add(1);
            self.closed = true;
            self.ack_now = true;
        }
    }

    /// Owes the acknowledgment for text that arrived.
    pub fn owe_for_text(&mut self, placed: Placed, now: Instant) {
        match placed {
            Placed::OutOfOrder | Placed::RangeLimit | Placed::Filled => self.owe_dup(),
            Placed::InOrder => {
                self.in_order_unacked = self.in_order_unacked.saturating_add(1);
                if self.in_order_unacked >= 2 {
                    self.ack_now = true;
                } else if self.delayed.is_none() {
                    self.delayed = Some(now.after(DELAYED_ACK));
                }
            }
            Placed::Nothing => self.ack_now = true,
        }
    }

    pub fn owe_dup(&mut self) {
        self.dup_owed = self.dup_owed.saturating_add(1).min(DUP_ACKS_OWED);
    }

    /// A segment wholly before RCV.NXT: its duplicated text is reported once as a D-SACK.
    pub fn duplicate(&mut self, seq: Seq, text: u32) {
        if text > 0 {
            self.dsack = Some((seq, seq.add(text)));
        }
        self.ack_now = true;
    }

    pub fn delayed_due(&mut self, now: Instant) {
        if self.delayed.is_some_and(|at| at <= now) {
            self.delayed = None;
            self.ack_now = true;
        }
    }

    /// The window field for a segment built now, moving the edge by the silly-window rule
    /// (RFC 9293 §3.8.6.2.2, Fr = 1/2) only while no hole is open.
    /// The right edge and window field a segment built now carries; [`Self::advertise`] commits them.
    pub fn offer(&self, mss: u32) -> (Seq, u16) {
        let edge = self.candidate(mss).unwrap_or(self.edge);
        let field = (edge.since(self.next) >> self.shift).min(u32::from(u16::MAX));
        (edge, u16::try_from(field).unwrap_or(u16::MAX))
    }

    pub fn advertise(&mut self, mss: u32) {
        let (edge, field) = self.offer(mss);
        self.edge = edge;
        self.last_window = u32::from(field) << self.shift;
    }

    fn candidate(&self, mss: u32) -> Option<Seq> {
        if !self.ranges.is_empty() {
            return None;
        }
        let free = u32::try_from(self.buf.capacity().saturating_sub(self.buf.len())).unwrap_or(u32::MAX);
        let mask = u32::MAX.checked_shl(u32::from(self.shift)).unwrap_or(0);
        let candidate = self.next.add(free & mask);
        let threshold = self.threshold(mss);
        (candidate.after(self.edge) && candidate.since(self.edge) >= threshold).then_some(candidate)
    }

    fn threshold(&self, mss: u32) -> u32 {
        (u32::try_from(self.buf.capacity()).unwrap_or(u32::MAX) / 2).min(mss)
    }

    /// Every segment sent carries the acknowledgment: what was owed is discharged.
    pub fn sent_ack(&mut self) {
        self.ack_now = false;
        self.delayed = None;
        self.in_order_unacked = 0;
        self.dsack = None;
        self.last_ack_sent = self.next;
    }

    /// SACK blocks for the next segment, at most `max`: a D-SACK first, then the range the last
    /// out-of-order segment landed in, then the rest by recency (RFC 2018 §4, RFC 2883 §4).
    pub fn sack_blocks(&self, max: usize) -> ([SackBlock; 4], usize) {
        let mut blocks = [SackBlock { left: self.next.into(), right: self.next.into() }; 4];
        let mut n = 0usize;
        let mut taken = 0u64;
        let bit = |i: usize| u32::try_from(i).ok().and_then(|i| 1u64.checked_shl(i)).unwrap_or(0);
        let mut push = |block: SackBlock, n: &mut usize| {
            if let Some(slot) = blocks.get_mut(*n).filter(|_| *n < max) {
                *slot = block;
                *n = n.saturating_add(1);
            }
        };
        let contains = |outer: &Range, start: Seq, end: Seq| outer.start.at_or_before(start) && end.at_or_before(outer.end);
        if let Some((start, end)) = self.dsack {
            push(SackBlock { left: start.into(), right: end.into() }, &mut n);
            if let Some((i, r)) = self.ranges.iter().enumerate().find(|(_, r)| contains(r, start, end)) {
                push(r.block(), &mut n);
                taken |= bit(i);
            }
        }
        let most_recent = |taken: u64| {
            self.ranges
                .iter()
                .enumerate()
                .filter(|&(j, r)| {
                    taken & bit(j) == 0
                        && !self.ranges.iter().enumerate().any(|(k, c)| taken & bit(k) != 0 && contains(c, r.start, r.end))
                })
                .min_by_key(|(_, r)| self.stamp.wrapping_sub(r.stamp))
                .map(|(j, _)| j)
        };
        let trigger = self.trigger.and_then(|t| self.ranges.iter().position(|r| t.within(r.start, r.end.since(r.start))));
        let mut next = trigger.filter(|&i| taken & bit(i) == 0).or_else(|| most_recent(taken));
        while let Some(i) = next.filter(|_| n < max) {
            if let Some(r) = self.ranges.get(i) {
                push(r.block(), &mut n);
            }
            taken |= bit(i);
            next = most_recent(taken);
        }
        (blocks, n)
    }

    pub fn owes_sack(&self) -> bool {
        self.dsack.is_some() || !self.ranges.is_empty()
    }

    pub fn read(&mut self, out: &mut [u8]) -> usize {
        self.buf.read(out)
    }

    /// Shows `take` the oldest bytes held, as far as they lie in one piece, and lets go of as
    /// many of them as it answers it took.
    pub fn read_with(&mut self, take: impl FnOnce(&[u8]) -> usize) -> usize {
        let (held, _) = self.buf.slices(0, self.buf.len());
        let n = take(held).min(held.len());
        self.buf.consume(n);
        n
    }

    /// `shutdown_read`: what is held is dropped, and later text is dropped as if read.
    pub fn stop_reading(&mut self) {
        self.discard = true;
        let n = self.buf.len();
        self.buf.consume(n);
    }

    /// After the user read: a window update is owed when the window offered was below the
    /// threshold and the edge would now move by at least it (RFC 9293 §3.8.6.2.2).
    pub fn after_read(&mut self, mss: u32) {
        if self.candidate(mss).is_some() && self.last_window < self.threshold(mss) {
            self.ack_now = true;
        }
    }
}
