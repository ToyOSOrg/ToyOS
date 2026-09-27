//! SYN-SENT and SYN-RECEIVED (RFC 9293 §3.10.7.3, §3.10.7.4), and what the handshake negotiates.
//! Payload on a SYN is discarded, never queued: no state is held for a peer not yet verified.

use core::time::Duration;

use toyos_net_wire::tcp::{SynOptions, Timestamps, WindowShift};

use crate::conn::{Ctx, In, Kind, Negotiated, Out, Params, Rst, Sync, Ts, NO_BLOCKS};
use crate::counters::Counter;
use crate::ring::Ring;
use crate::rtt::Rtt;
use crate::seq::{Seq, Stamp};
use crate::stack::TimeWait;
use crate::{limits, Instant, Tuple};

/// What a SYN or SYN-ACK of ours offers, fixed for the connection's life.
#[derive(Clone, Copy, Debug)]
pub struct Local {
    pub mss: u16,
    pub shift: u8,
    /// The unscaled window of a SYN: `min(65,535, receive buffer)`.
    pub window: u16,
    pub receive_buffer: usize,
    pub send_buffer: usize,
    pub mtu: u16,
    pub ts_offset: u32,
}

/// The peer's SYN or SYN-ACK decides every option: we offer all of them first, or answer only
/// what it offered.
pub fn negotiate(seg: &In<'_>, local: &Local, first_tsval: u32, ctx: &mut Ctx<'_>) -> Negotiated {
    let options = seg.options;
    let peer_mss = match options.mss() {
        None => limits::MSS_FLOOR,
        Some(mss) if mss < limits::MSS_FLOOR => {
            ctx.refuse(Counter::MssBelowFloor);
            limits::MSS_FLOOR
        }
        Some(mss) => mss,
    };
    let scaled = options.window_scale().is_some();
    let (snd_shift, rcv_shift) = match options.window_scale() {
        Some(scale) => {
            if scale.raw() > scale.effective() {
                ctx.refuse(Counter::WscaleClamped);
            }
            (scale.effective(), local.shift)
        }
        None => (0, 0),
    };
    let ts = options.timestamps().map(|t| Ts { recent: t.value, recent_at: ctx.now, offset: local.ts_offset, first: first_tsval });
    Negotiated { peer_mss, snd_shift, rcv_shift, scaled, sack: options.sack_permitted(), ts }
}

fn ts_now(local: &Local, now: Instant) -> u32 {
    Ts { recent: 0, recent_at: now, offset: local.ts_offset, first: 0 }.clock(now)
}

/// An RTT sample from the handshake: from the echoed timestamp, or from a SYN sent once (Karn).
fn handshake_sample(rtt: &mut Rtt, seg: &In<'_>, n: &Negotiated, once: Option<Instant>, now: Instant) {
    match (n.ts, seg.options.timestamps()) {
        (Some(ts), Some(t)) => {
            let age = Stamp(ts.clock(now)).since(Stamp(t.echo));
            if age < 1 << 31 && Stamp(t.echo).at_or_after(Stamp(ts.first)) {
                rtt.sample(Duration::from_millis(u64::from(age)), 1);
            }
        }
        _ => {
            if let Some(sent) = once {
                rtt.sample(now.since(sent), 1);
            }
        }
    }
}

/// The SYN (or SYN-ACK) timer: sent at hand-off, doubling from 1 s.
#[derive(Clone, Copy, Debug)]
pub struct Retransmit {
    pub rtt: Rtt,
    pub timer: Option<Instant>,
    pub first: Option<Instant>,
    pub first_tsval: u32,
    pub sent: u32,
    pub timeouts: u32,
    pub owed: bool,
    pub stalled: u32,
}

impl Retransmit {
    fn new() -> Self {
        Self { rtt: Rtt::new(), timer: None, first: None, first_tsval: 0, sent: 0, timeouts: 0, owed: true, stalled: 0 }
    }

    /// When the first transmission was also the only one.
    fn once(&self) -> Option<Instant> {
        self.first.filter(|_| self.sent == 1)
    }

    fn handed_off(&mut self, now: Instant, tsval: u32, timed: bool) {
        if self.first.is_none() {
            self.first = Some(now);
            self.first_tsval = tsval;
        }
        self.sent = self.sent.saturating_add(1);
        if timed {
            self.owed = false;
            if self.timer.is_none() {
                self.timer = Some(now.after(self.rtt.rto()));
            }
        }
    }

    /// `true` when the handshake gives up: at the first expiry at or after `bound` since the first
    /// transmission (RFC 9293 MUST-23 for an active open).
    fn expire(&mut self, now: Instant, bound: Duration, ctx: &mut Ctx<'_>) -> bool {
        if !self.timer.is_some_and(|at| at <= now) {
            return false;
        }
        self.timer = None;
        if self.first.is_some_and(|first| now.since(first) >= bound) {
            return true;
        }
        self.rtt.back_off();
        self.timeouts = self.timeouts.saturating_add(1);
        self.owed = true;
        self.stalled = self.stalled.saturating_add(1);
        if self.stalled == 3 {
            ctx.events.push(crate::Event::Reverify(ctx.tuple.remote.addr));
        }
        false
    }
}

fn syn_options(local: &Local, peer: Option<&Negotiated>, tsval: u32) -> SynOptions {
    let timestamps = match peer {
        None => Some(Timestamps { value: tsval, echo: 0 }),
        Some(n) => n.ts.map(|ts| Timestamps { value: tsval, echo: ts.recent }),
    };
    SynOptions {
        mss: Some(local.mss),
        sack_permitted: peer.is_none_or(|n| n.sack),
        timestamps,
        window_scale: if peer.is_none_or(|n| n.scaled) { WindowShift::new(local.shift).ok() } else { None },
    }
}

#[derive(Debug)]
pub struct SynSent {
    pub iss: Seq,
    pub timer: Retransmit,
    pub answer: Option<Rst>,
    pub buf: Ring,
}

pub enum Sent {
    Keep,
    Refused,
    Established(alloc::boxed::Box<Sync>),
    Simultaneous(alloc::boxed::Box<SynRcvd>),
}

impl SynSent {
    pub fn new(iss: Seq, local: &Local) -> Self {
        Self { iss, timer: Retransmit::new(), answer: None, buf: Ring::new(local.send_buffer) }
    }

    pub fn receive(mut self, seg: &In<'_>, local: &Local, ctx: &mut Ctx<'_>) -> (Option<Self>, Sent) {
        let now = ctx.now;
        let first = self.iss.add(1);
        if let Some(ack) = seg.ack.filter(|&ack| ack != first) {
            if !seg.rst() {
                self.answer = Some(Rst { seq: ack, ack: None, ts: seg.answer_ts() });
            }
            return (Some(self), Sent::Keep);
        }
        if seg.rst() {
            if seg.ack.is_some() {
                return (None, Sent::Refused);
            }
            ctx.count(Counter::SynSentRstNoAck);
            return (Some(self), Sent::Keep);
        }
        if !seg.syn() {
            return (Some(self), Sent::Keep);
        }
        if !seg.payload.is_empty() {
            ctx.refuse(Counter::SynDataDiscarded);
        }
        let negotiated = negotiate(seg, local, self.timer.first_tsval, ctx);
        if seg.ack.is_none() {
            let rcvd = SynRcvd {
                iss: self.iss,
                irs: seg.seq,
                negotiated,
                timer: Retransmit { owed: true, timer: None, ..self.timer },
                dup_answer: false,
                answer: None,
                ack_owed: false,
                last_unsolicited: None,
                origin: Origin::Active { buf: self.buf, fin: false },
            };
            return (None, Sent::Simultaneous(alloc::boxed::Box::new(rcvd)));
        }
        let mut rtt = self.timer.rtt;
        handshake_sample(&mut rtt, seg, &negotiated, self.timer.once().filter(|_| self.timer.timeouts == 0), now);
        let mut sync = Sync::new(
            now,
            Params {
                una: first,
                rcv_next: seg.seq.add(1),
                snd_wnd: u32::from(seg.window),
                wl1: seg.seq,
                negotiated,
                rtt,
                handshake_timeouts: self.timer.timeouts,
                buf: self.buf,
                fin: false,
                receive_buffer: local.receive_buffer,
                offered: u32::from(local.window),
                mtu: local.mtu,
            },
        );
        sync.rx.ack_now = true;
        (None, Sent::Established(alloc::boxed::Box::new(sync)))
    }

    pub fn next_segment(&mut self, local: &Local, now: Instant) -> Option<Out> {
        if let Some(rst) = self.answer.take() {
            return Some(Out::rst(&rst));
        }
        if !self.timer.owed {
            return None;
        }
        let tsval = ts_now(local, now);
        self.timer.handed_off(now, tsval, true);
        Some(Out { seq: self.iss, kind: Kind::Syn(syn_options(local, None, tsval)), window: local.window, ts: None, sack: NO_BLOCKS, data: (0, 0) })
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.timer.timer
    }

    /// `true` when the active open gives up.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> bool {
        self.timer.expire(ctx.now, limits::SYN_GIVE_UP, ctx)
    }
}

#[derive(Debug)]
pub enum Origin {
    /// A listener's child, and the TIME-WAIT it reopened, which it returns to if it never
    /// reaches ESTABLISHED (RFC 9293 MAY-2 (2)).
    Passive { listener: u32, time_wait: Option<(Tuple, TimeWait)> },
    /// A simultaneous open (MUST-10): the user's queued data and FIN wait for ESTABLISHED.
    Active { buf: Ring, fin: bool },
}

#[derive(Debug)]
pub struct SynRcvd {
    pub iss: Seq,
    pub irs: Seq,
    pub negotiated: Negotiated,
    pub timer: Retransmit,
    /// A SYN-ACK owed to a duplicate SYN, sent without touching the timer.
    dup_answer: bool,
    pub answer: Option<Rst>,
    ack_owed: bool,
    last_unsolicited: Option<Instant>,
    pub origin: Origin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rcvd {
    Keep,
    /// Deleted without a word: a passive child answered by an RST, a new SYN, or an ICMP error.
    Gone,
    Refused,
    /// The handshake completed on this segment; the rest of it is processed in ESTABLISHED.
    Established,
    /// A crossing SYN-ACK completed it: nothing else in the segment is new.
    Crossed,
}

impl SynRcvd {
    pub fn passive(iss: Seq, seg: &In<'_>, negotiated: Negotiated, listener: u32, time_wait: Option<(Tuple, TimeWait)>) -> Self {
        Self {
            iss,
            irs: seg.seq,
            negotiated,
            timer: Retransmit::new(),
            dup_answer: false,
            answer: None,
            ack_owed: false,
            last_unsolicited: None,
            origin: Origin::Passive { listener, time_wait },
        }
    }

    pub const fn is_passive(&self) -> bool {
        matches!(self.origin, Origin::Passive { .. })
    }

    fn rcv_next(&self) -> Seq {
        self.irs.add(1)
    }

    fn unsolicited(&mut self, ctx: &mut Ctx<'_>) {
        if ctx.unsolicited(&mut self.last_unsolicited) {
            self.ack_owed = true;
        }
    }

    pub fn receive(&mut self, seg: &In<'_>, local: &Local, ctx: &mut Ctx<'_>) -> Rcvd {
        let first = self.iss.add(1);
        if seg.syn() && seg.seq == self.irs {
            match seg.ack {
                None => {
                    ctx.count(Counter::SynRcvdDupSyn);
                    self.dup_answer = true;
                }
                Some(ack) if ack == first => return Rcvd::Crossed,
                Some(ack) => self.answer = Some(Rst { seq: ack, ack: None, ts: seg.answer_ts() }),
            }
            return Rcvd::Keep;
        }
        if let Some(ts) = self.negotiated.ts {
            if !seg.rst() {
                let Some(t) = seg.options.timestamps() else {
                    ctx.refuse(Counter::TsMissing);
                    return Rcvd::Keep;
                };
                if Stamp(t.value).before(Stamp(ts.recent)) {
                    ctx.count(Counter::PawsReject);
                    self.unsolicited(ctx);
                    return Rcvd::Keep;
                }
            }
        }
        let next = self.rcv_next();
        let window = u32::from(local.window);
        let len = seg.len();
        let acceptable = match len {
            0 => seg.seq.since(next) < window,
            _ => seg.seq.since(next) < window || seg.seq.add(len.saturating_sub(1)).since(next) < window,
        };
        if !acceptable {
            if !seg.rst() {
                self.unsolicited(ctx);
            }
            return Rcvd::Keep;
        }
        if let (Some(ts), Some(t)) = (self.negotiated.ts.as_mut(), seg.options.timestamps()) {
            if !seg.rst() && Stamp(t.value).at_or_after(Stamp(ts.recent)) && seg.seq.at_or_before(next) {
                ts.recent = t.value;
                ts.recent_at = ctx.now;
            }
        }
        if seg.rst() {
            if seg.seq == next {
                return if self.is_passive() { Rcvd::Gone } else { Rcvd::Refused };
            }
            ctx.refuse(Counter::RstChallenged);
            self.unsolicited(ctx);
            return Rcvd::Keep;
        }
        if seg.syn() {
            if self.is_passive() {
                return Rcvd::Gone;
            }
            ctx.refuse(Counter::SynChallenged);
            self.unsolicited(ctx);
            return Rcvd::Keep;
        }
        match seg.ack {
            None => Rcvd::Keep,
            Some(ack) if ack == first => Rcvd::Established,
            Some(ack) => {
                self.answer = Some(Rst { seq: ack, ack: None, ts: seg.answer_ts() });
                Rcvd::Keep
            }
        }
    }

    /// ESTABLISHED from the segment that completed the handshake. A crossing SYN-ACK's window is a
    /// SYN's, never scaled (RFC 7323 §2.2).
    pub fn establish(self, seg: &In<'_>, crossed: bool, local: &Local, now: Instant) -> Sync {
        let shift = if crossed { 0 } else { self.negotiated.snd_shift };
        let snd_wnd = u32::from(seg.window).checked_shl(u32::from(shift)).unwrap_or(u32::MAX);
        let mut negotiated = self.negotiated;
        if let Some(ts) = negotiated.ts.as_mut() {
            ts.first = self.timer.first_tsval;
        }
        let mut rtt = self.timer.rtt;
        let once = self.timer.once().filter(|_| self.timer.timeouts == 0 && !self.dup_answer);
        handshake_sample(&mut rtt, seg, &negotiated, once, now);
        let (buf, fin) = match self.origin {
            Origin::Active { buf, fin, .. } => (buf, fin),
            Origin::Passive { .. } => (Ring::new(local.send_buffer), false),
        };
        let mut sync = Sync::new(
            now,
            Params {
                una: self.iss.add(1),
                rcv_next: self.irs.add(1),
                snd_wnd,
                wl1: seg.seq,
                negotiated,
                rtt,
                handshake_timeouts: self.timer.timeouts,
                buf,
                fin,
                receive_buffer: local.receive_buffer,
                offered: u32::from(local.window),
                mtu: local.mtu,
            },
        );
        if crossed {
            sync.rx.ack_now = true;
        }
        sync
    }

    pub fn next_segment(&mut self, local: &Local, now: Instant) -> Option<Out> {
        if let Some(rst) = self.answer.take() {
            return Some(Out::rst(&rst));
        }
        let tsval = ts_now(local, now);
        let ts = self.negotiated.ts.map(|ts| Timestamps { value: tsval, echo: ts.recent });
        let dup = core::mem::replace(&mut self.dup_answer, false);
        if self.timer.owed || dup {
            self.timer.handed_off(now, tsval, self.timer.owed);
            let options = syn_options(local, Some(&self.negotiated), tsval);
            return Some(Out {
                seq: self.iss,
                kind: Kind::SynAck(self.rcv_next(), options),
                window: local.window,
                ts: None,
                sack: NO_BLOCKS,
                data: (0, 0),
            });
        }
        if core::mem::replace(&mut self.ack_owed, false) {
            let ack = Kind::Ack { ack: self.rcv_next(), push: false, fin: false };
            return Some(Out { seq: self.iss.add(1), kind: ack, window: local.window, ts, sack: NO_BLOCKS, data: (0, 0) });
        }
        None
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.timer.timer
    }

    /// `true` when the handshake gives up: 60 s for a passive child, 180 s for an active open.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> bool {
        let bound = if self.is_passive() { limits::SYNACK_GIVE_UP } else { limits::SYN_GIVE_UP };
        self.timer.expire(ctx.now, bound, ctx)
    }

    pub fn reset(&self, now: Instant, local: &Local) -> Rst {
        let ts = self.negotiated.ts.map(|ts| Timestamps { value: ts_now(local, now), echo: ts.recent });
        Rst { seq: self.iss.add(1), ack: Some(self.rcv_next()), ts }
    }

    pub fn shutdown_write(&mut self) -> bool {
        match &mut self.origin {
            Origin::Active { fin, .. } => {
                *fin = true;
                true
            }
            Origin::Passive { .. } => false,
        }
    }

    pub fn send(&mut self, data: &[u8]) -> Option<usize> {
        match &mut self.origin {
            Origin::Active { buf, fin: false, .. } => Some(buf.push(data)),
            Origin::Active { .. } | Origin::Passive { .. } => None,
        }
    }
}
