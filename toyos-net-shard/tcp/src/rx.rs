//! The receive side: in-order bytes the user has not read, out-of-order ranges at their stream
//! positions, the right edge the peer was offered, and what acknowledgment is owed.
//!
//! `unread + (edge − next) ≤ capacity` always holds, so every byte the peer may send has room. The
//! edge never retreats (RFC 7323 §2.4), bytes already stored are never overwritten, and bytes once
//! reported in a SACK block are kept until delivered: this receiver never reneges.
//!
//! **The capacity grows only by what the user reads.** It starts at
//! [`limits::RECEIVE_BUFFER_INITIAL`](crate::limits::RECEIVE_BUFFER_INITIAL) and, once a round
//! trip has passed, rises to twice what the user read per round trip, up to the configured
//! buffer: enough that a reader keeping up never waits on the window while the sender's rate
//! doubles. Text nobody reads grows nothing, so a peer alone cannot make a connection hold more
//! than the initial capacity, and a connection whose round trip is unknown keeps it.
//!
//! **The round trip is the receiver's own** (Linux's `tcp_rcv_rtt_measure_ts` and
//! `tcp_rcv_rtt_measure`): a downloader sends no data, so the sender's SRTT keeps the handshake's
//! sample while queueing lengthens the path. With timestamps, a full-sized segment carrying a
//! TSecr not yet seen samples the age of that echo, at least one tick and at most twice the
//! estimate, averaged with gain 1/8: an echo aged by the peer's own silence moves the estimate
//! by an eighth at most. Full-sized is the largest segment the peer sends, learned from what
//! arrives (Linux's `tcp_measure_rcv_mss`), never this end's send MSS: a peer whose segments are
//! shorter would never be sampled. Without timestamps, the time the peer took to fill the window
//! offered is at least one round trip, and the least such time is kept.

use alloc::vec::Vec;
use core::time::Duration;

use toyos_net_wire::tcp::SackBlock;

use crate::ring::Ring;
use crate::seq::Seq;
use crate::Instant;

pub const OOO_RANGES: usize = 32;
pub const DELAYED_ACK: Duration = Duration::from_millis(40);
/// One TSval tick (RFC 7323 §5.4): an echo's age is never less.
const TICK: Duration = Duration::from_millis(1);
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
    /// The most the capacity grows to.
    max: usize,
    /// When the current measurement began, and what the user has read since.
    round: (Instant, usize),
    /// The receiver's round-trip estimate.
    rtt: Option<Duration>,
    /// The last TSecr sampled, or without timestamps the edge whose filling is timed, and since when.
    sampler: Sampler,
    /// The largest segment the peer sends, the length of the last shorter one, and the bounds
    /// of both: what our SYN offered less the timestamp option, and the floor MSS less it.
    rcv_mss: u32,
    short: u32,
    mss_bounds: (u32, u32),
}

#[derive(Clone, Copy, Debug)]
enum Sampler {
    Echo(Option<u32>),
    Fill(Option<(Seq, Instant)>),
}

impl Rx {
    /// `next` is IRS + 1; the SYN or SYN-ACK offered `window`, unscaled; the capacity grows to
    /// `max`, or to the most a window field at `shift` offers if that is less: an edge past it
    /// would be one the peer was never told of. `(mss, options)`: the MSS our SYN offered and the
    /// timestamp option's length on every segment, 0 without timestamps, which picks how the
    /// round trip is sampled.
    pub fn new(next: Seq, max: usize, shift: u8, window: u32, now: Instant, (mss, options): (u32, u32)) -> Self {
        let initial = usize::try_from(crate::limits::RECEIVE_BUFFER_INITIAL).unwrap_or(usize::MAX);
        let offerable = usize::from(u16::MAX).checked_shl(u32::from(shift)).unwrap_or(usize::MAX);
        let max = max.min(offerable);
        let floor = u32::from(crate::limits::MSS_FLOOR).saturating_sub(options);
        Self {
            next,
            edge: next.add(window),
            shift,
            buf: Ring::new(max.min(initial)),
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
            max,
            round: (now, 0),
            rtt: None,
            sampler: if options > 0 { Sampler::Echo(None) } else { Sampler::Fill(None) },
            rcv_mss: floor,
            short: 0,
            mss_bounds: (floor, mss.saturating_sub(options)),
        }
    }

    /// The most the peer may have unread and in flight to us: the receive buffer's present size.
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.buf.capacity()
    }

    /// After in-order text of `len` bytes arrived at `now`, with its TSecr and that echo's age
    /// where it is one this end sent.
    pub fn sample_rtt(&mut self, len: u32, echo: Option<(u32, Option<Duration>)>, now: Instant) {
        self.measure_mss(len);
        match &mut self.sampler {
            Sampler::Echo(last) => {
                let Some((echo, age)) = echo.filter(|&(echo, _)| *last != Some(echo)) else { return };
                *last = Some(echo);
                let Some(age) = age.filter(|_| len >= self.rcv_mss) else { return };
                let age = age.max(TICK);
                self.rtt = Some(self.rtt.map_or(age, |rtt| rtt.saturating_mul(7).saturating_add(age.min(rtt.saturating_mul(2))).checked_div(8).unwrap_or(rtt)));
            }
            Sampler::Fill(mark) => {
                if let Some((edge, since)) = *mark {
                    if self.next.before(edge) {
                        return;
                    }
                    let took = now.since(since).max(Duration::from_micros(1));
                    self.rtt = Some(self.rtt.map_or(took, |rtt| rtt.min(took)));
                }
                *mark = Some((self.edge, now));
            }
        }
    }

    /// Linux's `tcp_measure_rcv_mss`: a segment as long as the largest so far raises it, up to what
    /// our SYN offered; two in a row of one shorter length, not under the floor, lower it to that.
    fn measure_mss(&mut self, len: u32) {
        let (floor, offered) = self.mss_bounds;
        let short = core::mem::replace(&mut self.short, 0);
        if len >= self.rcv_mss {
            self.rcv_mss = len.min(offered);
        } else if len >= floor {
            self.short = len;
            if len == short {
                self.rcv_mss = len;
            }
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
        // A window the shift would round to zero is offered as one unit where the buffer has the
        // room, as Linux does: else a sender owing the text of a hole waits on a window not shut.
        let unit = 1u32.checked_shl(u32::from(self.shift)).unwrap_or(u32::MAX);
        let window = edge.since(self.next);
        let edge = if window > 0 && window < unit && unit <= self.free() { self.next.add(unit) } else { edge };
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
        let mask = u32::MAX.checked_shl(u32::from(self.shift)).unwrap_or(0);
        let candidate = self.next.add(self.free() & mask);
        let threshold = self.threshold(mss);
        (candidate.after(self.edge) && candidate.since(self.edge) >= threshold).then_some(candidate)
    }

    /// Room for text the user has not read.
    fn free(&self) -> u32 {
        u32::try_from(self.buf.capacity().saturating_sub(self.buf.len())).unwrap_or(u32::MAX)
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

    /// After the user read `n` bytes: the capacity grows by the module's rule, and a window update
    /// is owed when the window offered was below the threshold and the edge would now move by at
    /// least it (RFC 9293 §3.8.6.2.2).
    pub fn after_read(&mut self, n: usize, now: Instant, mss: u32) {
        let (began, read) = self.round;
        let read = read.saturating_add(n);
        self.round = (began, read);
        if let Some(rtt) = self.rtt.filter(|&rtt| now.since(began) >= rtt) {
            let read = u128::try_from(read).unwrap_or(u128::MAX);
            let per_rtt = read.saturating_mul(rtt.as_nanos()).checked_div(now.since(began).as_nanos()).unwrap_or(0);
            let want = usize::try_from(per_rtt.saturating_mul(2)).unwrap_or(usize::MAX);
            self.buf.grow(want.min(self.max));
            self.round = (now, 0);
        }
        if let Some(candidate) = self.candidate(mss) {
            if self.last_window < self.threshold(mss) || candidate.since(self.next) >= self.window().saturating_mul(2) {
                self.ack_now = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A timestamped connection's receive side at t = 0: our SYN offered an MSS of 1,460.
    fn stamped() -> Rx {
        Rx::new(Seq::new(1), 4 << 20, 7, 65_535, Instant::from_nanos(0), (1460, 12))
    }

    fn echo(rx: &mut Rx, len: u32, tsecr: u32, age: Duration) {
        rx.sample_rtt(len, Some((tsecr, Some(age))), Instant::from_nanos(u64::from(tsecr) * 1_000_000));
    }

    /// Full-sized is the peer's largest segment: 1,380-byte segments are sampled though our own
    /// send MSS is larger. After a 1,448-byte one, a single 1,380-byte segment is not; the second
    /// in a row lowers full-sized to 1,380 and is.
    #[test]
    fn full_sized_is_learned_from_what_arrives() {
        let mut rx = stamped();
        echo(&mut rx, 1380, 1, ms(15));
        assert_eq!(rx.rtt, Some(ms(15)));
        echo(&mut rx, 1448, 2, ms(15));
        echo(&mut rx, 1380, 3, ms(23));
        assert_eq!(rx.rtt, Some(ms(15)));
        echo(&mut rx, 1380, 4, ms(23));
        assert_eq!(rx.rtt, Some(ms(16)));
    }

    /// The echo of the last ACK before the peer fell silent for 10 s ages by the silence: it moves
    /// the estimate by an eighth, no more. An echo of this tick's TSval ages one tick.
    #[test]
    fn an_echo_aged_by_the_peers_silence_moves_the_estimate_an_eighth() {
        let mut rx = stamped();
        for tsecr in 1..40 {
            echo(&mut rx, 1448, tsecr, ms(16));
        }
        echo(&mut rx, 1448, 10_040, ms(10_016));
        assert_eq!(rx.rtt, Some(ms(18)));
        echo(&mut rx, 1448, 10_041, Duration::ZERO);
        assert_eq!(rx.rtt, Some(Duration::from_micros(15_875)));
    }

    /// Without timestamps the time to fill a window offered is at least a round trip, so the
    /// least is kept: the window offered at 0 ms fills at 80 ms, the next at 200 ms.
    #[test]
    fn without_timestamps_the_least_fill_time_is_kept() {
        let mut rx = Rx::new(Seq::new(1), 65_535, 0, 65_535, Instant::from_nanos(0), (1460, 0));
        let arrive = |rx: &mut Rx, len: usize, at_ms: u64| {
            let next = rx.next;
            assert_eq!(rx.place(next, &vec![0; len]), Placed::InOrder);
            rx.sample_rtt(u32::try_from(len).unwrap(), None, Instant::from_nanos(at_ms * 1_000_000));
            rx.read(&mut vec![0; len]);
            rx.advertise(1460);
        };
        arrive(&mut rx, 1460, 0);
        arrive(&mut rx, 64_075, 80);
        assert_eq!(rx.rtt, Some(ms(80)));
        arrive(&mut rx, 65_535, 200);
        assert_eq!(rx.rtt, Some(ms(80)));
    }
}
