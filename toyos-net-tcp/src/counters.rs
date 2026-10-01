//! Every refusal, every drop that is not a plain duplicate, and every recovery event has a counter
//! named `tcp.<name>`. A refusal of legacy or insecure input is also an event naming the rule and
//! both endpoints; [`RefusalLog`] decides which of those become a log line.

use alloc::vec::Vec;
use core::net::Ipv4Addr;

use crate::{limits, Endpoint, Event, Tuple};

toyos_net_wire::counters! {
    ClosedRst = "tcp.closed-rst";
    ClosedRstLimited = "tcp.closed-rst-limited";
    SynRst = "tcp.syn-rst", logged;
    SynFin = "tcp.syn-fin", logged;
    ListenRst = "tcp.listen-rst";
    ListenNoSyn = "tcp.listen-no-syn";
    ListenOverflow = "tcp.listen-overflow";
    AcceptQueueFull = "tcp.accept-queue-full";
    AcceptResetDropped = "tcp.accept-reset-dropped";
    ListenerClosedReset = "tcp.listener-closed-reset";
    SynSentRstNoAck = "tcp.synsent-rst-no-ack";
    SynRcvdDupSyn = "tcp.synrcvd-dup-syn";
    SynAckGiveUp = "tcp.synack-give-up";
    SynDataDiscarded = "tcp.syn-data-discarded", logged;
    OptionMd5 = "tcp.option-md5", logged;
    OptionFastOpen = "tcp.option-fastopen", logged;
    EcnNotNegotiated = "tcp.ecn-not-negotiated";
    MssBelowFloor = "tcp.mss-below-floor", logged;
    WscaleClamped = "tcp.wscale-clamped", logged;
    SackUnnegotiated = "tcp.sack-unnegotiated";
    TsMissing = "tcp.ts-missing", logged;
    PawsReject = "tcp.paws-reject";
    TsEcrInvalid = "tcp.ts-ecr-invalid";
    RstChallenged = "tcp.rst-challenged", logged;
    SynChallenged = "tcp.syn-challenged", logged;
    AckOutOfRange = "tcp.ack-out-of-range", logged;
    UnsolicitedAckLimited = "tcp.unsolicited-ack-limited";
    UrgentIgnored = "tcp.urgent-ignored", logged;
    DataAfterFin = "tcp.data-after-fin";
    FinConflict = "tcp.fin-conflict";
    OooRangeLimit = "tcp.ooo-range-limit";
    DsackRcvd = "tcp.dsack-rcvd";
    SackBlockInvalid = "tcp.sack-block-invalid";
    CloseUnreadRst = "tcp.close-unread-rst";
    OrphanDataRst = "tcp.orphan-data-rst";
    OrphanIdleAbort = "tcp.orphan-idle-abort";
    TimeWaitRstIgnored = "tcp.timewait-rst-ignored", logged;
    TimeWaitReuse = "tcp.timewait-reuse";
    TimeWaitEvicted = "tcp.timewait-evicted";
    IcmpNoSocket = "tcp.icmp-no-socket";
    IcmpStale = "tcp.icmp-stale";
    IcmpSoft = "tcp.icmp-soft";
    IcmpHardAsSoft = "tcp.icmp-hard-as-soft", logged;
    PmtuNoMtu = "tcp.pmtu-no-mtu", logged;
    PmtuBogus = "tcp.pmtu-bogus";
    PmtuFloored = "tcp.pmtu-floored";
    PmtuLowered = "tcp.pmtu-lowered";
    SelfConnect = "tcp.self-connect";
    NoEphemeralPort = "tcp.no-ephemeral-port";
    NextHopFailed = "tcp.next-hop-failed";
    Rto = "tcp.rto";
    RtoUnsent = "tcp.rto-unsent";
    RetransmitBytes = "tcp.retransmit.bytes";
    FastRecovery = "tcp.fast-recovery";
    SackRecovery = "tcp.sack-recovery";
    LimitedTransmit = "tcp.limited-transmit";
    PersistProbe = "tcp.persist-probe";
    KeepaliveProbe = "tcp.keepalive-probe";
    EventOverflow = "tcp.event-overflow";
}

/// The counters and the events the shell has not drained: a refusal is both.
#[derive(Debug, Default)]
pub struct Log {
    pub counters: Counters,
    events: Vec<Event>,
}

impl Log {
    pub fn count(&mut self, counter: Counter) {
        self.counters.add(counter, 1);
    }

    pub fn add(&mut self, counter: Counter, n: u64) {
        self.counters.add(counter, n);
    }

    /// Counts a refusal, and names it for the log when its rule is one the log carries.
    pub fn refuse(&mut self, rule: Counter, tuple: &Tuple) {
        self.count(rule);
        if rule.logged() {
            self.event(Event::Refused(Refusal { rule, local: tuple.local, remote: tuple.remote }));
        }
    }

    /// Past [`limits::EVENTS`] undrained, an event is refused and counted: a peer never grows
    /// the list. A `Reachable` is not repeated while it is the newest advice pending for its
    /// address: one peer's ACKs never crowd out another's refusals, and [ip] still reads each
    /// address's advice in order.
    pub fn event(&mut self, event: Event) {
        match event {
            Event::Reachable(addr) if self.newest_advice(addr) == Some(&event) => {}
            _ if self.events.len() >= limits::EVENTS => self.count(Counter::EventOverflow),
            _ => self.events.push(event),
        }
    }

    fn newest_advice(&self, addr: Ipv4Addr) -> Option<&Event> {
        self.events.iter().rev().find(|e| matches!(e, Event::Reachable(a) | Event::Reverify(a) if *a == addr))
    }

    pub fn drain(&mut self) -> alloc::vec::Drain<'_, Event> {
        self.events.drain(..)
    }
}

/// A refusal of input ToyOS does not implement, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rule: Counter,
    pub local: Endpoint,
    pub remote: Endpoint,
}
