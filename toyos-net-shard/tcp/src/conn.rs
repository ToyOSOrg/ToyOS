//! Segment arrival: the checks SYN-RECEIVED, ESTABLISHED through LAST-ACK, and TIME-WAIT share
//! ([`screen`]), then ESTABLISHED through LAST-ACK in RFC 9293 §3.10.7.4's order, and the segment
//! each transmit opportunity builds.

use core::num::NonZeroU16;
use core::time::Duration;

use toyos_net_wire::tcp::{SackBlock, SynOptions, TcpFlags, TcpOptions, TcpSegment, Timestamps};

use crate::cc::Cc;
use crate::counters::{Counter, Log};
use crate::ring::Ring;
use crate::rtt::{Rtt, RTO_AFTER_HANDSHAKE_LOSS, RTO_MAX};
use crate::rx::{Placed, Rx};
use crate::seq::{Seq, Stamp};
use crate::tx::Tx;
use crate::{limits, Error, Event, Exit, Instant, NotReady, Options, Received, State, Tuple};

/// RFC 7323 §5.5: TS.Recent older than this no longer judges PAWS.
const TS_RECENT_VALID: Duration = Duration::from_secs(24 * 24 * 3600);
/// IPv4 and TCP headers without options (RFC 6691 §2).
const HEADERS: u16 = 40;
const TS_OPTION: u32 = 12;
const MIN_MTU: u16 = 576;

/// A parsed segment as TCP reads it.
#[derive(Clone, Copy, Debug)]
pub struct In<'a> {
    pub seq: Seq,
    pub ack: Option<Seq>,
    pub flags: TcpFlags,
    pub window: u16,
    pub options: TcpOptions<'a>,
    pub payload: &'a [u8],
}

impl<'a> In<'a> {
    pub fn new(segment: &TcpSegment<'a>) -> Self {
        Self {
            seq: segment.sequence().into(),
            ack: segment.acknowledgment().map(Seq::from),
            flags: segment.flags(),
            window: segment.window().0,
            options: segment.options(),
            payload: segment.payload(),
        }
    }

    pub const fn syn(&self) -> bool {
        self.flags.contains(TcpFlags::SYN)
    }

    pub const fn fin(&self) -> bool {
        self.flags.contains(TcpFlags::FIN)
    }

    pub const fn rst(&self) -> bool {
        self.flags.contains(TcpFlags::RST)
    }

    pub fn text(&self) -> u32 {
        u32::try_from(self.payload.len()).unwrap_or(u32::MAX)
    }

    /// SEG.LEN: text, plus one each for SYN and FIN.
    pub fn len(&self) -> u32 {
        self.text().saturating_add(u32::from(self.syn())).saturating_add(u32::from(self.fin()))
    }

    /// The RST RFC 7323 §5.2 answers this segment with: TSval 0, TSecr its TSval.
    pub fn answer_ts(&self) -> Option<Timestamps> {
        self.options.timestamps().map(|t| Timestamps { value: 0, echo: t.value })
    }
}

/// What a call hands a connection besides the connection itself.
pub struct Ctx<'a> {
    pub now: Instant,
    pub tuple: Tuple,
    pub options: Options,
    pub orphan: bool,
    pub log: &'a mut Log,
}

impl Ctx<'_> {
    /// Challenge ACKs and every answer to an unacceptable segment share one allowance: one in
    /// any 500 ms per connection (RFC 5961 §7). Per connection, never global: a global budget is
    /// an off-path counter of sequence-number guesses.
    pub fn unsolicited(&mut self, last: &mut Option<Instant>) -> bool {
        if last.is_some_and(|at| self.now.since(at) < limits::UNSOLICITED_ACK) {
            self.log.count(Counter::UnsolicitedAckLimited);
            return false;
        }
        *last = Some(self.now);
        true
    }
}

/// What [`screen`] made of a segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screened {
    /// Acceptable, neither an RST nor a SYN in the window: TS.Recent is updated.
    Pass,
    /// Dropped; `challenge` owes the ACK the per-connection allowance may send.
    Drop { challenge: bool },
    /// Outside the window and not an RST; `old` when every sequence number in it is before RCV.NXT.
    Unacceptable { old: bool },
    /// An RST at RCV.NXT, or a SYN in the window where `syn_ends` (a passive child's).
    Ends,
}

/// The checks every state from SYN-RECEIVED on makes of an arriving segment, each once: the drop
/// of a segment without timestamps (RFC 7323 §3.2) and PAWS (§5.3 R1), acceptability (RFC 9293
/// Table 5), the TS.Recent update (RFC 7323 §4.3), and RFC 5961's exact RST (§3.2) and SYN
/// challenge (§4.2). The receiver stands at `next` with `window` and TS.Recent `ts`, having last
/// acknowledged `last_ack_sent`.
pub fn screen(seg: &In<'_>, next: Seq, window: u32, last_ack_sent: Seq, ts: Option<&mut Ts>, syn_ends: bool, ctx: &mut Ctx<'_>) -> Screened {
    let now = ctx.now;
    if let Some(recent) = ts.as_deref().filter(|_| !seg.rst()) {
        let Some(t) = seg.options.timestamps() else {
            ctx.log.refuse(Counter::TsMissing, &ctx.tuple);
            return Screened::Drop { challenge: false };
        };
        if recent.judges(now) && Stamp(t.value).before(Stamp(recent.recent)) {
            ctx.log.count(Counter::PawsReject);
            return Screened::Drop { challenge: true };
        }
    }
    let len = seg.len();
    let offset = seg.seq.since(next);
    let acceptable = match (len, window) {
        (0, 0) => offset == 0,
        // Table 5 also takes the ACK at the right edge a peer sends once it filled the window: two
        // peers that filled each other's windows over a hole would otherwise refuse each other's
        // every ACK. An RST there stays outside (RFC 5961 §3.2).
        (0, window) => offset < window || (offset == window && !seg.rst()),
        (_, 0) => false,
        (_, window) => offset < window || seg.seq.add(len.saturating_sub(1)).since(next) < window,
    };
    if !acceptable {
        if seg.rst() {
            return Screened::Drop { challenge: false };
        }
        return Screened::Unacceptable { old: len > 0 && seg.seq.add(len).at_or_before(next) };
    }
    if let (Some(recent), Some(t)) = (ts, seg.options.timestamps()) {
        let newer = !recent.judges(now) || Stamp(t.value).at_or_after(Stamp(recent.recent));
        if !seg.rst() && newer && seg.seq.at_or_before(last_ack_sent) {
            recent.recent = t.value;
            recent.recent_at = now;
        }
    }
    if seg.rst() {
        if seg.seq == next {
            return Screened::Ends;
        }
        ctx.log.refuse(Counter::RstChallenged, &ctx.tuple);
        return Screened::Drop { challenge: true };
    }
    if seg.syn() && !seg.seq.before(next) {
        if syn_ends {
            return Screened::Ends;
        }
        ctx.log.refuse(Counter::SynChallenged, &ctx.tuple);
        return Screened::Drop { challenge: true };
    }
    Screened::Pass
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rst {
    pub seq: Seq,
    pub ack: Option<Seq>,
    pub ts: Option<Timestamps>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Syn(SynOptions),
    SynAck(Seq, SynOptions),
    Ack { ack: Seq, push: bool, fin: bool },
    Rst(Option<Seq>),
}

pub type Blocks = ([SackBlock; 4], usize);

pub const NO_BLOCKS: Blocks = ([SackBlock { left: toyos_net_wire::tcp::SeqNum::new(0), right: toyos_net_wire::tcp::SeqNum::new(0) }; 4], 0);

/// A segment's headers as a transmit opportunity builds them; its payload travels beside it.
#[derive(Clone, Copy, Debug)]
pub struct Out {
    pub seq: Seq,
    pub kind: Kind,
    pub window: u16,
    pub ts: Option<Timestamps>,
    pub sack: Blocks,
}

/// The payload of a segment that carries none.
pub const NO_PAYLOAD: (&[u8], &[u8]) = (&[], &[]);

impl Out {
    pub const fn rst(rst: &Rst) -> Self {
        Self { seq: rst.seq, kind: Kind::Rst(rst.ack), window: 0, ts: rst.ts, sack: NO_BLOCKS }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ts {
    pub recent: u32,
    pub recent_at: Instant,
    pub offset: u32,
    /// The TSval of our first segment: no echo before it is one we sent.
    pub first: u32,
}

impl Ts {
    /// RFC 7323 §5.5: TS.Recent judges PAWS until it is 24 days old.
    fn judges(&self, now: Instant) -> bool {
        now.since(self.recent_at) < TS_RECENT_VALID
    }

    /// What a segment sent now carries: TSval now, TSecr TS.Recent.
    pub fn option(&self, now: Instant) -> Timestamps {
        Timestamps { value: crate::tsval(now, self.offset), echo: self.recent }
    }

    /// RFC 7323 §4.1: the round trip an echoed TSval measures, unless it is one we never sent or
    /// one from the future.
    pub fn echo_rtt(&self, echo: u32, now: Instant) -> Option<Duration> {
        let age = Stamp(crate::tsval(now, self.offset)).since(Stamp(echo));
        (age < 1 << 31 && Stamp(echo).at_or_after(Stamp(self.first))).then(|| Duration::from_millis(u64::from(age)))
    }
}

/// What the handshake settled (RFC 9293 §3.7.1, RFC 7323 §2.2 §3.2, RFC 2018 §2).
#[derive(Clone, Copy, Debug)]
pub struct Negotiated {
    /// SendMSS, floored at 536.
    pub peer_mss: u16,
    pub snd_shift: u8,
    pub rcv_shift: u8,
    /// Both SYNs carried Window Scale.
    pub scaled: bool,
    pub sack: bool,
    pub ts: Option<Ts>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Recovery {
    None,
    /// NewReno (RFC 6582), with the inflations RFC 5681 §3.2 step 4 still allows.
    Fast { inflations: u32 },
    /// RFC 6675.
    Sack { point: Seq, high_rxt: Seq, rescue: Option<Seq> },
}

#[derive(Clone, Copy, Debug)]
struct Persist {
    at: Instant,
    interval: Duration,
    /// SND.NXT when the window shut: probe bytes past it go out again as data once it opens.
    from: Seq,
    due: bool,
    unanswered: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Keep,
    /// Both FINs acknowledged from LAST-ACK.
    Closed,
    Reset,
    TimeWait,
    /// An RST answers the segment and the connection is gone.
    Abort(Rst),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick {
    Keep,
    /// R2 without progress: reported as the recorded soft error, if any.
    GiveUp,
    /// The user timeout or keepalive.
    TimedOut,
    Orphan,
}

pub struct Params {
    pub una: Seq,
    pub rcv_next: Seq,
    pub snd_wnd: u32,
    pub wl1: Seq,
    pub negotiated: Negotiated,
    pub rtt: Rtt,
    pub handshake_timeouts: u32,
    pub buf: Ring,
    pub fin: bool,
    pub receive_buffer: usize,
    /// The unscaled window our SYN or SYN-ACK offered.
    pub offered: u32,
    pub mtu: u16,
}

pub struct Sync {
    pub phase: Phase,
    pub tx: Tx,
    pub rx: Rx,
    pub rtt: Rtt,
    pub cc: Cc,
    peer_mss: u32,
    path_mss: u32,
    pub ts: Option<Ts>,
    pub sack_ok: bool,
    recovery: Recovery,
    recover: Seq,
    episode: bool,
    dupacks: u32,
    lt_budget: u8,
    lt_bytes: u32,
    urgent: Option<Seq>,
    rtx_next: Option<Seq>,
    pub rtx_timer: Option<Instant>,
    rto_pending: bool,
    timeout_rtx: bool,
    timing: Option<(Seq, Instant)>,
    persist: Option<Persist>,
    sws: Option<Instant>,
    sws_fired: bool,
    last_data_sent: Option<Instant>,
    progress_since: Option<Instant>,
    acked_since: Option<Instant>,
    orphan_since: Option<Instant>,
    stalled: u32,
    last_heard: Instant,
    ka_probes: u32,
    ka_last: Option<Instant>,
    /// When the owed keepalive probe became due.
    ka_due: Option<Instant>,
    last_unsolicited: Option<Instant>,
    urgent_logged: bool,
    pub delivery_problem: bool,
}

fn us32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn signed(n: u32) -> i64 {
    i64::from(n)
}

impl Sync {
    pub fn new(now: Instant, p: Params) -> Self {
        let n = p.negotiated;
        let path_mss = u32::from(p.mtu.saturating_sub(HEADERS).max(limits::MSS_FLOOR));
        let mut tx = Tx::new(p.una, p.buf, p.fin);
        tx.wnd = p.snd_wnd;
        tx.max_wnd = p.snd_wnd;
        tx.wl1 = p.wl1;
        tx.wl2 = p.una;
        tx.shift = n.snd_shift;
        let ts_bytes = if n.ts.is_some() { TS_OPTION } else { 0 };
        let smss = u32::from(n.peer_mss).min(path_mss).saturating_sub(ts_bytes);
        let mut rtt = p.rtt;
        if p.handshake_timeouts > 0 {
            rtt.set_rto(RTO_AFTER_HANDSHAKE_LOSS);
        }
        let phase = if p.fin { Phase::FinWait1 } else { Phase::Established };
        let mut sync = Self {
            phase,
            tx,
            rx: Rx::new(p.rcv_next, p.receive_buffer, n.rcv_shift, p.offered),
            rtt,
            cc: Cc::new(smss, p.handshake_timeouts),
            peer_mss: u32::from(n.peer_mss),
            path_mss,
            ts: n.ts,
            sack_ok: n.sack,
            recovery: Recovery::None,
            recover: p.una.sub(1),
            episode: false,
            dupacks: 0,
            lt_budget: 0,
            lt_bytes: 0,
            urgent: None,
            rtx_next: None,
            rtx_timer: None,
            rto_pending: false,
            timeout_rtx: false,
            timing: None,
            persist: None,
            sws: None,
            sws_fired: false,
            last_data_sent: None,
            progress_since: None,
            acked_since: None,
            orphan_since: None,
            stalled: 0,
            last_heard: now,
            ka_probes: 0,
            ka_last: None,
            ka_due: None,
            last_unsolicited: None,
            urgent_logged: false,
            delivery_problem: false,
        };
        sync.refresh(now);
        sync
    }

    pub const fn state(&self) -> State {
        match self.phase {
            Phase::Established => State::Established,
            Phase::FinWait1 => State::FinWait1,
            Phase::FinWait2 => State::FinWait2,
            Phase::CloseWait => State::CloseWait,
            Phase::Closing => State::Closing,
            Phase::LastAck => State::LastAck,
        }
    }

    pub fn in_recovery(&self) -> bool {
        self.recovery != Recovery::None
    }

    /// RFC 6675's HighRxt and RescueRxt, during SACK recovery.
    pub fn sack_marks(&self) -> Option<(Seq, Option<Seq>)> {
        match self.recovery {
            Recovery::Sack { high_rxt, rescue, .. } => Some((high_rxt, rescue)),
            Recovery::None | Recovery::Fast { .. } => None,
        }
    }

    /// RFC 6675's SetPipe, HighRxt at SND.UNA outside SACK recovery.
    pub fn pipe(&self) -> u32 {
        self.tx.pipe(self.sack_marks().map_or(self.tx.una, |(high_rxt, _)| high_rxt), self.smss())
    }

    /// Eff.snd.MSS (RFC 9293 §3.7.1): SMSS everywhere below.
    pub fn smss(&self) -> u32 {
        self.cc.smss
    }

    /// The congestion window every send obeys (RFC 5681 §2): go-back, NextSeg, new data and
    /// Limited Transmit read it here and nowhere else.
    fn cwnd(&self) -> u32 {
        self.cc.cwnd
    }

    fn ts_bytes(&self) -> u32 {
        if self.ts.is_some() {
            TS_OPTION
        } else {
            0
        }
    }

    // ---- arrival ----

    pub fn receive(&mut self, seg: &In<'_>, ctx: &mut Ctx<'_>) -> Verdict {
        let now = ctx.now;
        match screen(seg, self.rx.next, self.rx.window(), self.rx.last_ack_sent, self.ts.as_mut(), false, ctx) {
            Screened::Pass => {}
            Screened::Drop { challenge } => {
                if challenge {
                    self.unsolicited(ctx);
                }
                return Verdict::Keep;
            }
            Screened::Unacceptable { old } => {
                self.unacceptable(seg, old, ctx);
                return Verdict::Keep;
            }
            Screened::Ends => return Verdict::Reset,
        }
        self.last_heard = now;
        self.ka_probes = 0;
        self.ka_due = None;
        let (seq, text, fin) = self.trim(seg);
        let Some(ack) = seg.ack else {
            return Verdict::Keep;
        };
        if !self.ack(seg, ack, ctx) {
            return Verdict::Keep;
        }
        if self.tx.fin_acked() {
            match self.phase {
                Phase::FinWait1 => self.phase = Phase::FinWait2,
                Phase::Closing => return Verdict::TimeWait,
                Phase::LastAck => return Verdict::Closed,
                Phase::Established | Phase::FinWait2 | Phase::CloseWait => {}
            }
        }
        if seg.flags.contains(TcpFlags::URG) {
            if self.urgent_logged {
                ctx.log.count(Counter::UrgentIgnored);
            } else {
                self.urgent_logged = true;
                ctx.log.refuse(Counter::UrgentIgnored, &ctx.tuple);
            }
        }
        let peer_closed = self.rx.closed;
        let mut text = text;
        if !text.is_empty() {
            if peer_closed {
                ctx.log.count(Counter::DataAfterFin);
                text = &[];
            } else if ctx.orphan {
                ctx.log.count(Counter::OrphanDataRst);
                return Verdict::Abort(Rst { seq: self.tx.reset_seq(), ack: Some(self.rx.next), ts: seg.answer_ts() });
            }
        }
        if let Some(limit) = self.rx.fin_remembered().filter(|_| !peer_closed) {
            let room = usize::try_from(limit.since(seq)).unwrap_or(usize::MAX);
            if text.len() > room || (seq.after(limit) && !text.is_empty()) {
                ctx.log.count(Counter::DataAfterFin);
                text = if seq.after(limit) { &[] } else { text.get(..room).unwrap_or_default() };
            }
        }
        if !text.is_empty() {
            let placed = self.rx.place(seq, text);
            if placed == Placed::RangeLimit {
                ctx.log.count(Counter::OooRangeLimit);
            }
            self.rx.owe_for_text(placed, now);
        }
        if fin && !peer_closed {
            if !self.rx.fin_at(seq.add(us32(text.len()))) {
                ctx.log.count(Counter::FinConflict);
            } else if !self.rx.closed && text.is_empty() {
                self.rx.owe_dup();
            }
        }
        self.rx.absorb_fin();
        if self.rx.closed && !peer_closed {
            self.orphan_progress(now);
            match self.phase {
                Phase::Established => self.phase = Phase::CloseWait,
                Phase::FinWait1 => self.phase = Phase::Closing,
                Phase::FinWait2 => return Verdict::TimeWait,
                Phase::CloseWait | Phase::Closing | Phase::LastAck => {}
            }
        }
        self.refresh(now);
        Verdict::Keep
    }

    /// An unacceptable segment that is not an RST (RFC 9293 §3.10.7.4 first check).
    fn unacceptable(&mut self, seg: &In<'_>, old: bool, ctx: &mut Ctx<'_>) {
        let next = self.rx.next;
        let keepalive = seg.seq == next.sub(1) && seg.text() <= 1;
        let probe = self.rx.window() == 0 && seg.seq == next;
        if old && seg.text() > 0 && self.sack_ok {
            self.rx.duplicate(seg.seq.add(u32::from(seg.syn())), seg.text());
        } else if old || keepalive || probe {
            self.rx.ack_now = true;
        } else {
            self.unsolicited(ctx);
        }
    }

    /// Only the new parts are processed: bytes before RCV.NXT (and a SYN there) and bytes at or
    /// past the right edge (and a FIN there) are cut.
    fn trim<'s>(&self, seg: &In<'s>) -> (Seq, &'s [u8], bool) {
        let next = self.rx.next;
        let mut seq = seg.seq;
        if seg.syn() && seq.before(next) {
            seq = seq.add(1);
        }
        let mut text = seg.payload;
        if seq.before(next) {
            let cut = usize::try_from(next.since(seq)).unwrap_or(usize::MAX);
            text = text.get(cut..).unwrap_or_default();
            seq = next;
        }
        let room = usize::try_from(self.rx.edge().since(seq)).unwrap_or(0);
        let mut fin = seg.fin();
        if text.len() >= room {
            fin = false;
            text = text.get(..room).unwrap_or(text);
        }
        (seq, text, fin)
    }

    fn unsolicited(&mut self, ctx: &mut Ctx<'_>) {
        if ctx.unsolicited(&mut self.last_unsolicited) {
            self.rx.ack_now = true;
        }
    }

    /// The ACK field (RFC 9293 §3.10.7.4 fifth, RFC 5961 §5.2). `false` drops the segment.
    fn ack(&mut self, seg: &In<'_>, ack: Seq, ctx: &mut Ctx<'_>) -> bool {
        let now = ctx.now;
        let lower = self.tx.una.sub(self.tx.max_wnd);
        if ack.since(lower) > self.tx.max_wnd.saturating_add(self.tx.flight()) {
            ctx.log.refuse(Counter::AckOutOfRange, &ctx.tuple);
            self.unsolicited(ctx);
            return false;
        }
        let newly = if self.sack_ok {
            self.tx.read_sack(ack, &seg.options, ctx.log)
        } else {
            if seg.options.sack_blocks().len() > 0 {
                ctx.log.count(Counter::SackUnnegotiated);
            }
            false
        };
        let flight = self.tx.flight();
        let acked = ack.since(self.tx.una);
        let window = u32::from(seg.window).checked_shl(u32::from(self.tx.shift)).unwrap_or(u32::MAX);
        if acked > 0 && acked <= flight {
            self.new_ack(seg, ack, acked, flight, ctx);
            if newly {
                self.duplicate(ack, ctx);
            }
        } else if self.sack_ok {
            if newly {
                self.duplicate(ack, ctx);
            }
        } else if flight > 0 && seg.payload.is_empty() && !seg.syn() && !seg.fin() && ack == self.tx.una && window == self.tx.wnd {
            self.duplicate(ack, ctx);
        }
        if ack.at_or_after(self.tx.una) && (self.tx.wl1.before(seg.seq) || (self.tx.wl1 == seg.seq && self.tx.wl2.at_or_before(ack))) {
            self.tx.wnd = window;
            self.tx.wl1 = seg.seq;
            self.tx.wl2 = ack;
            self.tx.max_wnd = self.tx.max_wnd.max(window);
        }
        if let Some(persist) = self.persist.as_mut() {
            persist.unanswered = false;
            self.stalled = 0;
            if self.progress_since.is_some() {
                self.progress_since = Some(now);
            }
        }
        self.update_persist(now);
        true
    }

    fn new_ack(&mut self, seg: &In<'_>, ack: Seq, acked: u32, flight: u32, ctx: &mut Ctx<'_>) {
        let now = ctx.now;
        let fin_now = self.tx.fin.is_some_and(|f| ack.after(f));
        let bytes = acked.saturating_sub(u32::from(fin_now));
        self.tx.buf.consume(usize::try_from(bytes).unwrap_or(usize::MAX));
        self.tx.una = ack;
        self.tx.trim_scoreboard();
        self.rtx_next = self.rtx_next.map(|p| p.later(ack)).filter(|p| p.before(self.tx.nxt));
        self.urgent = self.urgent.filter(|u| u.at_or_after(ack));
        self.sample(seg, ack, flight, ctx);
        self.progress_since = Some(now);
        self.acked_since = Some(now);
        self.stalled = 0;
        self.orphan_progress(now);
        ctx.log.event(Event::Reachable(ctx.tuple.remote.addr));
        let smss = self.smss();
        match self.recovery {
            Recovery::Fast { .. } if ack.sub(1).at_or_after(self.recover) => {
                let after = signed(self.tx.flight().max(smss)).saturating_add(signed(smss));
                self.cc.cwnd = u32::try_from(after).unwrap_or(u32::MAX).min(self.cc.ssthresh);
                self.recovery = Recovery::None;
                self.cc.end_recovery();
            }
            Recovery::Fast { .. } => {
                self.urgent = Some(ack);
                self.cc.cwnd = self.cc.cwnd.saturating_sub(bytes);
                if bytes >= smss {
                    self.cc.cwnd = self.cc.cwnd.saturating_add(smss);
                }
            }
            Recovery::Sack { point, .. } if ack.at_or_after(point) => {
                self.recovery = Recovery::None;
                self.cc.end_recovery();
            }
            Recovery::Sack { .. } => {}
            Recovery::None => self.cc.on_ack(now, bytes, self.rtt.srtt()),
        }
        self.dupacks = 0;
        self.lt_budget = 0;
        self.lt_bytes = 0;
        if self.tx.flight() == 0 {
            self.rto_pending = false;
        }
        // RFC 6298 (5.2), (5.3); while the retransmission an expiry marked has not left, that
        // hand-off starts the timer and a later expiry is the same segment's again.
        if !self.rto_pending {
            self.timeout_rtx = false;
            self.rtx_timer = None;
            self.arm(now);
        }
    }

    /// RFC 6298 (5.1): the timer runs while sequence space that left is outstanding, and never
    /// while the retransmission an expiry marked has yet to leave.
    fn arm(&mut self, now: Instant) {
        if self.rtx_timer.is_none() && self.persist.is_none() && !self.rto_pending && self.tx.flight() > 0 {
            self.rtx_timer = Some(now.after(self.rtt.rto()));
        }
    }

    fn sample(&mut self, seg: &In<'_>, ack: Seq, flight: u32, ctx: &mut Ctx<'_>) {
        let now = ctx.now;
        match self.ts {
            Some(ts) => {
                let Some(t) = seg.options.timestamps() else { return };
                match ts.echo_rtt(t.echo, now) {
                    Some(rtt) => {
                        let expected = flight.div_ceil(self.smss().saturating_mul(2).max(1)).max(1);
                        self.rtt.sample(rtt, expected);
                    }
                    None => ctx.log.count(Counter::TsEcrInvalid),
                }
            }
            None => {
                if let Some((_, at)) = self.timing.filter(|&(end, _)| ack.at_or_after(end)) {
                    self.timing = None;
                    self.rtt.sample(now.since(at), 1);
                }
            }
        }
    }

    /// A duplicate acknowledgment: RFC 5681 §2's without SACK, RFC 6675 §2's with it.
    fn duplicate(&mut self, ack: Seq, ctx: &mut Ctx<'_>) {
        let smss = self.smss();
        match &mut self.recovery {
            Recovery::Fast { inflations } => {
                if *inflations > 0 {
                    *inflations = inflations.saturating_sub(1);
                    self.cc.cwnd = self.cc.cwnd.saturating_add(smss);
                }
                return;
            }
            Recovery::Sack { .. } => return,
            Recovery::None => {}
        }
        self.dupacks = self.dupacks.saturating_add(1);
        let (enter, guarded) = if self.sack_ok {
            (self.dupacks >= 3 || self.tx.is_lost(self.tx.una, smss), !self.episode || self.tx.una.after(self.recover))
        } else {
            (self.dupacks == 3, !self.episode || ack.sub(1).after(self.recover))
        };
        if !enter || !guarded {
            if !self.sack_ok && self.dupacks < 3 {
                self.lt_budget = self.lt_budget.saturating_add(1);
            }
            return;
        }
        let flight = self.tx.flight();
        self.cc.on_loss(flight.saturating_sub(self.lt_bytes));
        self.recover = self.tx.nxt.sub(1);
        self.episode = true;
        self.urgent = Some(self.tx.una);
        if self.sack_ok {
            self.cc.cwnd = self.cc.ssthresh;
            self.recovery = Recovery::Sack { point: self.tx.nxt, high_rxt: self.tx.una, rescue: None };
            ctx.log.count(Counter::SackRecovery);
        } else {
            self.cc.cwnd = self.cc.ssthresh.saturating_add(smss.saturating_mul(3));
            self.recovery = Recovery::Fast { inflations: flight.div_ceil(smss.max(1)) };
            ctx.log.count(Counter::FastRecovery);
        }
    }

    fn orphan_progress(&mut self, now: Instant) {
        if self.orphan_since.is_some() {
            self.orphan_since = Some(now);
        }
    }

    /// Persist runs while the peer offers a zero window and something is owed (RFC 9293 §3.8.6.1);
    /// data behind a shut window is not retransmitted by the timer.
    fn update_persist(&mut self, now: Instant) {
        if self.tx.window() == 0 && self.tx.owes() {
            if self.persist.is_none() {
                let rto = self.rtt.rto();
                self.persist = Some(Persist { at: now.after(rto), interval: rto, from: self.tx.nxt, due: false, unanswered: false });
                self.rtx_timer = None;
            }
        } else if let Some(persist) = self.persist.take() {
            if self.tx.nxt.after(persist.from) {
                self.rtx_next = Some(self.tx.una);
            }
            self.arm(now);
        }
    }

    /// Starts the give-up and user-timeout clocks when something becomes owed, and stops them
    /// when nothing is.
    fn refresh(&mut self, now: Instant) {
        if !self.tx.owes() {
            self.progress_since = None;
        } else if self.progress_since.is_none() {
            self.progress_since = Some(now);
        }
        let waiting = self.tx.flight() > 0 || (self.tx.window() == 0 && self.tx.owes());
        if !waiting {
            self.acked_since = None;
        } else if self.acked_since.is_none() {
            self.acked_since = Some(now);
        }
    }

    pub fn packet_too_big(&mut self, mtu: Option<NonZeroU16>, quoted_length: u16, seq: Seq, ctx: &mut Ctx<'_>) {
        let Some(mtu) = mtu.map(NonZeroU16::get) else {
            ctx.log.refuse(Counter::PmtuNoMtu, &ctx.tuple);
            return;
        };
        let current = self.path_mss.saturating_add(u32::from(HEADERS));
        if u32::from(mtu) >= current || mtu >= quoted_length {
            ctx.log.count(Counter::PmtuBogus);
            return;
        }
        let mtu = if mtu < MIN_MTU {
            ctx.log.count(Counter::PmtuFloored);
            MIN_MTU
        } else {
            mtu
        };
        self.path_mss = u32::from(mtu.saturating_sub(HEADERS));
        let smss = self.peer_mss.min(self.path_mss).saturating_sub(self.ts_bytes());
        self.cc.set_smss(smss);
        if seq.within(self.tx.una, self.tx.flight()) {
            self.urgent = Some(seq);
        }
        ctx.log.count(Counter::PmtuLowered);
    }

    // ---- user calls ----

    pub fn send(&mut self, data: &[u8], now: Instant) -> Result<usize, Error> {
        if self.tx.fin.is_some() {
            return Err(Error::Closing);
        }
        let n = self.tx.buf.push(data);
        if n == 0 && !data.is_empty() {
            return Err(Error::WouldBlock);
        }
        self.refresh(now);
        self.update_persist(now);
        Ok(n)
    }

    pub fn recv(&mut self, out: &mut [u8]) -> Result<Received, Error> {
        let n = self.rx.read(out);
        if n > 0 {
            self.rx.after_read(self.smss());
            Ok(Received::Data(n))
        } else if self.rx.closed {
            Ok(Received::End)
        } else if out.is_empty() {
            Ok(Received::Data(0))
        } else {
            Err(Error::WouldBlock)
        }
    }

    pub fn shutdown_write(&mut self, now: Instant) {
        if self.tx.fin.is_some() {
            return;
        }
        self.tx.fin = Some(self.tx.data_end());
        self.phase = match self.phase {
            Phase::Established => Phase::FinWait1,
            Phase::CloseWait => Phase::LastAck,
            phase => phase,
        };
        self.refresh(now);
        self.update_persist(now);
    }

    pub fn orphan(&mut self, now: Instant) {
        self.orphan_since = Some(now);
    }

    /// The RST an abort sends (RFC 9293 §3.10.5): none from CLOSING or LAST-ACK.
    pub fn reset(&self, now: Instant) -> Option<Rst> {
        (!matches!(self.phase, Phase::Closing | Phase::LastAck)).then(|| self.reset_always(now))
    }

    /// The RST a timer's abort sends, whatever the state.
    pub fn reset_always(&self, now: Instant) -> Rst {
        let ts = self.ts.map(|ts| ts.option(now));
        Rst { seq: self.tx.reset_seq(), ack: Some(self.rx.next), ts }
    }

    // ---- timers ----

    /// The next probe's time; with one owed, the give-up's, which runs on wall time whether or not
    /// the probes leave (§11.3): the owed one and the rest at their interval, unanswered.
    fn keepalive_at(&self, ctx: &Ctx<'_>) -> Option<Instant> {
        let ka = ctx.options.keepalive?;
        let open = matches!(self.phase, Phase::Established | Phase::CloseWait | Phase::FinWait2);
        if ctx.orphan || !open || self.tx.owes() {
            return None;
        }
        if let Some(due) = self.ka_due {
            return Some(due.after(ka.interval.saturating_mul(ka.probes.saturating_sub(self.ka_probes))));
        }
        Some(match self.ka_last.filter(|_| self.ka_probes > 0) {
            Some(last) => last.after(ka.interval),
            None => self.last_heard.after(ka.idle),
        })
    }

    fn give_up_at(&self, ctx: &Ctx<'_>) -> Option<Instant> {
        match ctx.options.user_timeout {
            Some(d) => self.acked_since.map(|at| at.after(d)),
            None => self.progress_since.map(|at| at.after(limits::GIVE_UP)),
        }
    }

    pub fn deadline(&self, ctx: &Ctx<'_>) -> Option<Instant> {
        [
            self.rtx_timer,
            self.persist.filter(|p| !p.due).map(|p| p.at),
            self.keepalive_at(ctx),
            self.sws.filter(|_| !self.sws_fired),
            self.rx.delayed,
            self.give_up_at(ctx),
            self.orphan_since.map(|at| at.after(limits::ORPHAN_IDLE)),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Every expired timer, deletions first.
    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> Tick {
        let now = ctx.now;
        if self.give_up_at(ctx).is_some_and(|at| at <= now) {
            return if ctx.options.user_timeout.is_some() { Tick::TimedOut } else { Tick::GiveUp };
        }
        if self.orphan_since.is_some_and(|at| at.after(limits::ORPHAN_IDLE) <= now) {
            ctx.log.count(Counter::OrphanIdleAbort);
            return Tick::Orphan;
        }
        if self.rtx_timer.is_some_and(|at| at <= now) {
            self.expire(ctx);
        }
        if let Some(persist) = self.persist.as_mut().filter(|p| !p.due && p.at <= now) {
            persist.due = true;
            if persist.unanswered {
                self.stall(ctx);
            }
        }
        if self.keepalive_at(ctx).is_some_and(|at| at <= now) {
            if self.ka_due.is_some() || ctx.options.keepalive.is_some_and(|ka| self.ka_probes >= ka.probes) {
                return Tick::TimedOut;
            }
            self.ka_due = Some(now);
        }
        if self.sws.is_some_and(|at| at <= now) {
            self.sws = None;
            self.sws_fired = true;
        }
        self.rx.delayed_due(now);
        Tick::Keep
    }

    /// RFC 6298 (5.4)–(5.6): the oldest segment is due, the RTO backs off, and nothing is armed
    /// until that retransmission leaves.
    fn expire(&mut self, ctx: &mut Ctx<'_>) {
        self.rtx_timer = None;
        ctx.log.count(Counter::Rto);
        if self.rto_pending {
            ctx.log.count(Counter::RtoUnsent);
        }
        self.rto_pending = true;
        self.rtt.back_off();
        self.timing = None;
        let repeat = core::mem::replace(&mut self.timeout_rtx, true);
        self.cc.on_timeout(self.tx.flight(), repeat);
        self.recovery = Recovery::None;
        self.recover = self.tx.nxt.sub(1);
        self.episode = true;
        self.dupacks = 0;
        self.lt_budget = 0;
        self.lt_bytes = 0;
        self.tx.clear_scoreboard();
        self.urgent = None;
        self.rtx_next = Some(self.tx.una);
        self.stall(ctx);
    }

    /// R1 (RFC 9293 §3.8.3): the third expiry without progress re-verifies the next hop and
    /// tells the user delivery is in trouble.
    fn stall(&mut self, ctx: &mut Ctx<'_>) {
        self.stalled = self.stalled.saturating_add(1);
        if self.stalled == 3 {
            self.delivery_problem = true;
            ctx.log.event(Event::Reverify(ctx.tuple.remote.addr));
        }
    }

    // ---- transmit ----

    /// Hands off the next segment this opportunity owes, after SYNs and resets: `true` if one left.
    pub fn next_segment<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>) -> Result<bool, NotReady> {
        if self.rx.dup_owed > 0 {
            self.emit_ack(ctx, exit, self.tx.nxt)?;
            self.rx.dup_owed = self.rx.dup_owed.saturating_sub(1);
            return Ok(true);
        }
        if self.retransmission(ctx, exit)? || self.new_data(ctx, exit)? || self.probe(ctx, exit)? {
            return Ok(true);
        }
        if self.rx.ack_now {
            self.emit_ack(ctx, exit, self.tx.nxt)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn blocks(&self) -> Blocks {
        if self.sack_ok && self.rx.owes_sack() {
            self.rx.sack_blocks(if self.ts.is_some() { 3 } else { 4 })
        } else {
            NO_BLOCKS
        }
    }

    /// Payload room: SMSS less the SACK option this segment carries (RFC 9293 §3.7.1).
    fn room(&self, blocks: &Blocks) -> u32 {
        let sack = if blocks.1 > 0 { us32(blocks.1).saturating_mul(8).saturating_add(4) } else { 0 };
        self.smss().saturating_sub(sack).max(1)
    }

    /// Asks for the next hop, then hands `exit` the segment `[seq, seq + len)` (and the FIN after
    /// it) as the state of now builds it, with its payload; nothing changes here.
    fn frame<T>(&self, now: Instant, exit: &mut dyn Exit<T>, seq: Seq, len: u32, fin: bool, sack: Blocks) -> Result<(), NotReady> {
        let via = exit.ask()?;
        let push = len > 0 && seq.add(len) == self.tx.data_end();
        let window = self.rx.offer(self.smss()).1;
        let ts = self.ts.map(|ts| ts.option(now));
        let out = Out { seq, kind: Kind::Ack { ack: self.rx.next, push, fin }, window, ts, sack };
        let offset = self.tx.offset(seq).min(self.tx.buf.len());
        exit.send(via, &out, self.tx.buf.slices(offset, usize::try_from(len).unwrap_or(0)))
    }

    /// A segment without sequence space at `seq`: it discharges every owed acknowledgment.
    fn emit_ack<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>, seq: Seq) -> Result<(), NotReady> {
        self.frame(ctx.now, exit, seq, 0, false, self.blocks())?;
        self.acknowledged();
        Ok(())
    }

    /// `[start, start + len)` and the FIN after it, framed, and only then handed off.
    fn emit<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>, start: Seq, len: u32, fin: bool, sack: Blocks) -> Result<(), NotReady> {
        self.frame(ctx.now, exit, start, len, fin, sack)?;
        self.hand_off(ctx, start, len, fin);
        Ok(())
    }

    /// Every segment sent carries the window and the acknowledgment it was built with.
    fn acknowledged(&mut self) {
        self.rx.advertise(self.smss());
        self.rx.sent_ack();
    }

    /// `[start, start + len)` (and the FIN after it) has left: the sequence space counts as sent
    /// and the timer runs from now (RFC 6298 (5.1)), never earlier.
    fn hand_off(&mut self, ctx: &mut Ctx<'_>, start: Seq, len: u32, fin: bool) {
        let now = ctx.now;
        let end = start.add(len).add(u32::from(fin));
        let old = self.tx.nxt;
        let resent = start.before(old);
        if end.after(old) {
            self.tx.nxt = end;
        }
        if len > 0 {
            self.last_data_sent = Some(now);
            self.sws = None;
            self.sws_fired = false;
        }
        if resent {
            ctx.log.add(Counter::RetransmitBytes, u64::from(old.earlier(end).since(start)));
            self.timing = None;
            if start == self.tx.una {
                self.rto_pending = false;
            }
        } else if end.after(old) && self.timing.is_none() && self.ts.is_none() {
            self.timing = Some((end, now));
        }
        self.arm(now);
        self.refresh(now);
        self.acknowledged();
    }

    /// Whether the FIN rides on a segment ending at `end`.
    fn fin_fits(&self, end: Seq) -> bool {
        self.tx.fin.is_some_and(|f| f == end && (self.tx.nxt.after(f) || end.before(self.tx.right_edge())))
    }

    fn retransmission<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>) -> Result<bool, NotReady> {
        if self.persist.is_some() {
            return Ok(false);
        }
        if let Some(start) = self.urgent.map(|u| u.later(self.tx.una)).filter(|u| u.before(self.tx.nxt)) {
            let blocks = self.blocks();
            let stop = start.add(self.room(&blocks)).earlier(self.tx.data_end()).earlier(self.tx.nxt);
            let len = if start.before(stop) { stop.since(start) } else { 0 };
            let fin = self.tx.fin.is_some_and(|f| f == start.add(len) && self.tx.nxt.after(f));
            if len > 0 || fin {
                self.emit(ctx, exit, start, len, fin, blocks)?;
                self.urgent = None;
                if let Recovery::Sack { high_rxt, rescue, .. } = &mut self.recovery {
                    let end = start.add(len);
                    *high_rxt = high_rxt.later(end);
                    rescue.get_or_insert(end);
                }
                return Ok(true);
            }
        }
        self.urgent = None;
        if matches!(self.recovery, Recovery::Sack { .. }) {
            return self.next_seg(ctx, exit);
        }
        let Some(next) = self.rtx_next else { return Ok(false) };
        let mut pos = next.later(self.tx.una);
        while let Some((_, end)) = self.tx.sacked_at(pos) {
            pos = end;
        }
        let end_of_data = self.tx.data_end();
        let fin_resend = self.tx.fin.is_some_and(|f| pos == f && self.tx.nxt.after(f));
        if !pos.before(self.tx.nxt) || (!pos.before(end_of_data) && !fin_resend) {
            self.rtx_next = None;
            return Ok(false);
        }
        let limit = self.tx.una.add(self.cwnd().min(self.tx.window()));
        let blocks = self.blocks();
        let mut stop = pos.add(self.room(&blocks)).earlier(end_of_data).earlier(limit);
        if let Some(sacked) = self.tx.next_sacked(pos) {
            stop = stop.earlier(sacked);
        }
        let len = if pos.before(stop) { stop.since(pos) } else { 0 };
        let fin = self.fin_fits(pos.add(len)) && (len > 0 || fin_resend);
        if len == 0 && !fin {
            self.cc.limited = true;
            return Ok(false);
        }
        self.emit(ctx, exit, pos, len, fin, blocks)?;
        self.rtx_next = Some(pos.add(len).add(u32::from(fin)));
        Ok(true)
    }

    /// RFC 6675 §5 step C: while cwnd − pipe ≥ SMSS, what NextSeg() names.
    fn next_seg<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>) -> Result<bool, NotReady> {
        let Recovery::Sack { high_rxt, rescue, point } = self.recovery else { return Ok(false) };
        let smss = self.smss();
        if self.cwnd().saturating_sub(self.pipe()) < smss {
            return Ok(false);
        }
        let blocks = self.blocks();
        let room = self.room(&blocks);
        let highest = self.tx.highest_sacked();
        let hole = |lost: bool| {
            self.tx
                .holes(smss)
                .filter(|h| highest.is_some_and(|top| h.end.at_or_before(top)) && (h.lost || !lost))
                .map(|h| (h.start.later(high_rxt), h.end))
                .find(|(start, end)| start.before(*end))
        };
        let retransmit = |this: &mut Self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>, (start, end): (Seq, Seq)| {
            let stop = start.add(room).earlier(end);
            this.emit(ctx, exit, start, stop.since(start), false, blocks)?;
            if let Recovery::Sack { high_rxt, .. } = &mut this.recovery {
                *high_rxt = high_rxt.later(stop);
            }
            Ok(true)
        };
        if let Some(range) = hole(true) {
            return retransmit(self, ctx, exit, range);
        }
        let unsent = self.tx.unsent();
        let usable = u32::try_from(self.tx.usable().max(0)).unwrap_or(u32::MAX);
        if (unsent > 0 || self.tx.fin_unsent()) && usable > 0 {
            let len = unsent.min(room).min(usable);
            let start = self.tx.nxt;
            let fin = self.fin_fits(start.add(len));
            if len > 0 || fin {
                self.emit(ctx, exit, start, len, fin, blocks)?;
                return Ok(true);
            }
        }
        if let Some(range) = hole(false) {
            return retransmit(self, ctx, exit, range);
        }
        if rescue.is_none_or(|r| self.tx.una.after(r)) {
            let Some(top) = self.tx.holes(smss).last() else { return Ok(false) };
            let data_top = top.end.earlier(self.tx.data_end());
            let start = top.start.later(data_top.sub(room));
            if start.before(data_top) {
                self.emit(ctx, exit, start, data_top.since(start), false, blocks)?;
                self.recovery = Recovery::Sack { high_rxt, rescue: Some(point), point };
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// New data and the FIN (RFC 9293 §3.7.4 Nagle, §3.8.6.2.1 sender silly-window avoidance),
    /// within SND.UNA + min(cwnd, SND.WND) (RFC 5681 §2).
    fn new_data<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>) -> Result<bool, NotReady> {
        if self.persist.is_some() || matches!(self.recovery, Recovery::Sack { .. }) || self.tx.window() == 0 {
            return Ok(false);
        }
        let now = ctx.now;
        let unsent = self.tx.unsent();
        if unsent == 0 && !self.tx.fin_unsent() {
            return Ok(false);
        }
        if unsent > 0 && self.last_data_sent.is_some_and(|at| now.since(at) > self.rtt.rto()) && self.tx.flight() == 0 {
            self.cc.restart_after_idle();
        }
        let smss = self.smss();
        let blocks = self.blocks();
        let mss = signed(self.room(&blocks));
        let w_rcv = self.tx.usable();
        let in_flight = if self.sack_ok && self.dupacks > 0 { self.pipe() } else { self.tx.flight() };
        let cwnd = signed(self.cwnd());
        let mut w_cc = cwnd.saturating_sub(signed(in_flight));
        let limited_transmit = self.recovery == Recovery::None && if self.sack_ok { self.dupacks > 0 } else { self.lt_budget > 0 };
        if limited_transmit && !self.sack_ok {
            w_cc = cwnd.saturating_add(signed(smss.saturating_mul(2))).saturating_sub(signed(in_flight));
        }
        let d = signed(unsent);
        let start = self.tx.nxt;
        if d == 0 {
            if w_rcv < 1 {
                return Ok(false);
            }
            self.emit(ctx, exit, start, 0, true, blocks)?;
            return Ok(true);
        }
        let nagle = ctx.options.nodelay || self.tx.flight() == 0;
        let half = signed(self.tx.max_wnd / 2);
        let usable = d.min(w_rcv).min(w_cc).min(mss);
        let rules = |w_rcv: i64| {
            d.min(w_rcv).min(w_cc) >= mss || (nagle && d <= w_rcv.min(w_cc)) || (nagle && d.min(w_rcv) >= half && w_cc > 0)
        };
        let go = usable > 0 && (rules(w_rcv) || self.sws_fired || self.tx.fin.is_some());
        if w_cc < d.min(w_rcv).min(mss) {
            self.cc.limited = true;
        }
        if !go {
            if w_rcv > 0 && rules(i64::MAX) && self.sws.is_none() && !self.sws_fired {
                self.sws = Some(now.after(limits::SWS_OVERRIDE));
            }
            return Ok(false);
        }
        let len = u32::try_from(usable).unwrap_or(0);
        let beyond_cwnd = limited_transmit && signed(len) > cwnd.saturating_sub(signed(self.tx.flight()));
        let fin = self.fin_fits(start.add(len)) && signed(len) < w_rcv;
        self.emit(ctx, exit, start, len, fin, blocks)?;
        if beyond_cwnd {
            if !self.sack_ok {
                self.lt_budget = self.lt_budget.saturating_sub(1);
            }
            self.lt_bytes = self.lt_bytes.saturating_add(len);
            ctx.log.count(Counter::LimitedTransmit);
        }
        Ok(true)
    }

    /// A due persist probe (RFC 9293 §3.8.6.1) or keepalive (§3.8.4).
    fn probe<T>(&mut self, ctx: &mut Ctx<'_>, exit: &mut dyn Exit<T>) -> Result<bool, NotReady> {
        let now = ctx.now;
        if let Some(persist) = self.persist.filter(|p| p.due) {
            let blocks = self.blocks();
            let (start, len, fin) = if self.tx.flight() > 0 {
                let una = self.tx.una;
                if self.tx.fin == Some(una) {
                    (una, 0, true)
                } else {
                    (una, 1, false)
                }
            } else if self.tx.unsent() > 0 {
                (self.tx.nxt, 1, false)
            } else if self.tx.fin_unsent() {
                (self.tx.nxt, 0, true)
            } else {
                return Ok(false);
            };
            self.emit(ctx, exit, start, len, fin, blocks)?;
            let interval = persist.interval.saturating_mul(2).min(RTO_MAX);
            self.persist = Some(Persist { at: now.after(interval), interval, due: false, unanswered: true, ..persist });
            ctx.log.count(Counter::PersistProbe);
            return Ok(true);
        }
        if self.ka_due.is_some() {
            self.emit_ack(ctx, exit, self.tx.nxt.sub(1))?;
            self.ka_due = None;
            self.ka_probes = self.ka_probes.saturating_add(1);
            self.ka_last = Some(now);
            ctx.log.count(Counter::KeepaliveProbe);
            return Ok(true);
        }
        Ok(false)
    }
}
