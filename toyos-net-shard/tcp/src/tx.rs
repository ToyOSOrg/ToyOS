//! The send side: bytes from SND.UNA on, the peer's window, and the SACK scoreboard of what the
//! peer holds above SND.UNA (RFC 6675 §4). SND.NXT is one past the highest sequence number ever
//! handed off: retransmissions resend older ranges and never move it.

use alloc::vec::Vec;

use toyos_net_wire::tcp::TcpOptions;

use crate::counters::{Counter, Log};
use crate::ring::Ring;
use crate::seq::Seq;

pub const SACK_RANGES: usize = 64;
/// RFC 6675's DupThresh.
const DUP_THRESH: usize = 3;

#[derive(Debug)]
pub struct Tx {
    pub una: Seq,
    pub nxt: Seq,
    pub wnd: u32,
    pub wl1: Seq,
    pub wl2: Seq,
    pub max_wnd: u32,
    pub shift: u8,
    pub buf: Ring,
    /// The FIN's sequence number, once the user shut the write side.
    pub fin: Option<Seq>,
    /// SACKed ranges above SND.UNA, ascending, disjoint.
    sacked: Vec<(Seq, Seq)>,
}

/// An un-SACKed range between SND.UNA and SND.NXT, and whether RFC 6675's IsLost holds for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hole {
    pub start: Seq,
    pub end: Seq,
    pub lost: bool,
}

fn len32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Tx {
    pub fn new(una: Seq, buf: Ring, fin: bool) -> Self {
        let fin = fin.then(|| una.add(len32(buf.len())));
        Self { una, nxt: una, wnd: 0, wl1: una, wl2: una, max_wnd: 0, shift: 0, buf, fin, sacked: Vec::new() }
    }

    /// One past the last byte the user queued.
    pub fn data_end(&self) -> Seq {
        self.una.add(len32(self.buf.len()))
    }

    pub fn flight(&self) -> u32 {
        self.nxt.since(self.una)
    }

    pub fn unsent(&self) -> u32 {
        let end = self.data_end();
        if self.nxt.at_or_before(end) {
            end.since(self.nxt)
        } else {
            0
        }
    }

    pub fn fin_unsent(&self) -> bool {
        self.fin.is_some_and(|f| self.nxt.at_or_before(f))
    }

    pub fn fin_acked(&self) -> bool {
        self.fin.is_some_and(|f| self.una.after(f))
    }

    /// Anything outstanding, queued, or a FIN not yet sent.
    pub fn owes(&self) -> bool {
        self.flight() > 0 || self.unsent() > 0 || self.fin_unsent()
    }

    /// The window from SND.UNA to the edge the peer offered: SND.WND counts from the ACK it came
    /// with (SND.WL2), which an ACK that fails the SND.WL1 test leaves behind SND.UNA, where
    /// SND.UNA + SND.WND would lie past anything the peer offered. Zero once that edge is at or
    /// before SND.UNA.
    pub fn window(&self) -> u32 {
        self.wnd.saturating_sub(self.una.since(self.wl2))
    }

    /// The peer's right edge, never below SND.UNA.
    pub fn right_edge(&self) -> Seq {
        self.una.add(self.window())
    }

    /// W_rcv of RFC 9293 §3.8.6.2.1: negative once the peer shrank its window below SND.NXT.
    pub fn usable(&self) -> i64 {
        i64::from(self.wnd).saturating_sub(i64::from(self.nxt.since(self.wl2)))
    }

    pub fn offset(&self, seq: Seq) -> usize {
        usize::try_from(seq.since(self.una)).unwrap_or(usize::MAX)
    }

    /// The reset's sequence number: SND.NXT inside the peer's window, since one outside it is
    /// dropped; else a shut window's edge, or an open one's edge less one, which an RFC 5961 peer
    /// answers with the challenge ACK whose reset is exact.
    pub fn reset_seq(&self) -> Seq {
        let edge = self.right_edge();
        match self.window() {
            _ if self.nxt.before(edge) => self.nxt,
            0 => edge,
            _ => edge.sub(1),
        }
    }

    pub fn sacked(&self) -> &[(Seq, Seq)] {
        &self.sacked
    }

    pub fn clear_scoreboard(&mut self) {
        self.sacked.clear();
    }

    /// SND.UNA moved: ranges at or below it leave the scoreboard.
    pub fn trim_scoreboard(&mut self) {
        let una = self.una;
        self.sacked.retain_mut(|(start, end)| {
            *start = start.later(una);
            end.after(una)
        });
    }

    /// Reads an ACK's SACK blocks (RFC 2018 §3, RFC 2883 §4): whether a block covered bytes the
    /// scoreboard did not hold, RFC 6675's duplicate acknowledgment, and a D-SACK's right edge.
    pub fn read_sack(&mut self, ack: Seq, options: &TcpOptions<'_>, log: &mut Log) -> (bool, Option<Seq>) {
        let second = options.sack_blocks().nth(1);
        let mut newly = false;
        let mut dsack = None;
        for (i, block) in options.sack_blocks().enumerate() {
            let (left, right) = (Seq::from(block.left), Seq::from(block.right));
            let inside_second = second.is_some_and(|s| Seq::from(s.left).at_or_before(left) && right.at_or_before(Seq::from(s.right)));
            if i == 0 && (right.at_or_before(ack) || inside_second) {
                log.count(Counter::DsackRcvd);
                dsack = Some(right);
                continue;
            }
            if !(ack.before(left) && left.before(right) && right.at_or_before(self.nxt)) {
                log.count(Counter::SackBlockInvalid);
                continue;
            }
            // An old ACK's blocks can reach below SND.UNA: that part is acknowledged already.
            let left = left.later(self.una);
            if left.before(right) {
                newly |= self.insert(left, right);
            }
        }
        (newly, dsack)
    }

    fn insert(&mut self, left: Seq, right: Seq) -> bool {
        let lo = self.sacked.iter().position(|&(_, end)| end.at_or_after(left)).unwrap_or(self.sacked.len());
        let touching = self.sacked.iter().skip(lo).take_while(|&&(start, _)| start.at_or_before(right)).count();
        let hi = lo.saturating_add(touching);
        let merged = self.sacked.get(lo..hi).unwrap_or_default();
        let covered = merged.iter().fold(0u32, |sum, &(start, end)| {
            let (a, b) = (start.later(left), end.earlier(right));
            sum.saturating_add(if a.before(b) { b.since(a) } else { 0 })
        });
        let newly = covered < right.since(left);
        let range = merged.iter().fold((left, right), |(a, b), &(start, end)| (a.earlier(start), b.later(end)));
        self.sacked.splice(lo..hi, [range]);
        self.sacked.truncate(SACK_RANGES);
        newly
    }

    /// RFC 6675 IsLost for a byte in a hole: DupThresh SACKed ranges above it, or more than
    /// (DupThresh − 1) · SMSS SACKed bytes above it.
    pub fn is_lost(&self, seq: Seq, smss: u32) -> bool {
        let above = self.sacked.iter().filter(|&&(start, _)| start.after(seq));
        let bytes = above.clone().fold(0u32, |sum, &(start, end)| sum.saturating_add(end.since(start)));
        above.count() >= DUP_THRESH || bytes > smss.saturating_mul(2)
    }

    /// The un-SACKed ranges between SND.UNA and SND.NXT, ascending.
    pub fn holes(&self, smss: u32) -> impl Iterator<Item = Hole> + '_ {
        let tops = self.sacked.iter().map(|&(start, end)| (start, Some(end))).chain([(self.nxt, None)]);
        tops.scan(self.una, move |from, (start, end)| {
            let hole = Hole { start: *from, end: start, lost: self.is_lost(*from, smss) };
            if let Some(end) = end {
                *from = end;
            }
            Some(hole)
        })
        .filter(|hole| hole.start.before(hole.end))
    }

    /// RFC 6675 SetPipe: un-SACKed bytes not lost, plus those retransmitted below `high_rxt`.
    pub fn pipe(&self, high_rxt: Seq, smss: u32) -> u32 {
        self.holes(smss).fold(0u32, |pipe, hole| {
            let kept = if hole.lost { 0 } else { hole.end.since(hole.start) };
            let resent = if hole.start.before(high_rxt) { high_rxt.earlier(hole.end).since(hole.start) } else { 0 };
            pipe.saturating_add(kept).saturating_add(resent)
        })
    }

    /// The highest SACKed sequence number, exclusive.
    pub fn highest_sacked(&self) -> Option<Seq> {
        self.sacked.last().map(|&(_, end)| end)
    }

    /// The SACKed range starting at or covering `seq`, if any.
    pub fn sacked_at(&self, seq: Seq) -> Option<(Seq, Seq)> {
        self.sacked.iter().copied().find(|&(start, end)| seq.within(start, end.since(start)))
    }

    /// The first SACKed range above `seq`.
    pub fn next_sacked(&self, seq: Seq) -> Option<Seq> {
        self.sacked.iter().map(|&(start, _)| start).find(|start| start.after(seq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 200 segments outstanding, every other one SACKed: 100 ranges offered, the lowest 64 kept,
    /// and the lowest hole still leads.
    #[test]
    fn s_lr_015_the_scoreboard_keeps_the_lowest_64() {
        let smss = 1448u32;
        let una = Seq::new(1001);
        let mut tx = Tx::new(una, Ring::new(0), false);
        tx.nxt = una.add(200 * smss);
        for k in (1..200u32).step_by(2) {
            tx.insert(una.add(k * smss), una.add((k + 1) * smss));
        }
        assert_eq!(tx.sacked().len(), 64);
        assert_eq!(tx.sacked().first().map(|r| r.0), Some(una.add(smss)));
        let first = tx.holes(smss).next().unwrap();
        assert_eq!((first.start, first.end, first.lost), (una, una.add(smss), true));
    }
}
