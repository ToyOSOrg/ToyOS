//! Every refusal, every drop that is not a plain duplicate, and every recovery event has a counter
//! named `tcp.<name>`. A refusal of legacy or insecure input is also an event naming the rule and
//! both endpoints; [`RefusalLog`] decides which of those become a log line.

use core::time::Duration;

use crate::{Endpoint, Instant};

macro_rules! counters {
    ($($variant:ident = $name:literal $(, $logged:ident)?;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Counter {
            $($variant,)*
        }

        impl Counter {
            pub const ALL: &'static [Counter] = &[$(Counter::$variant,)*];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }

            /// A refusal the log names: legacy or insecure input ToyOS does not implement.
            pub const fn logged(self) -> bool {
                match self {
                    $(Self::$variant => counters!(@logged $($logged)?),)*
                }
            }
        }

        /// One `T` per counter.
        #[allow(non_snake_case)]
        #[derive(Clone, Debug, Default)]
        struct PerCounter<T> {
            $($variant: T,)*
        }

        impl<T> PerCounter<T> {
            fn get(&self, counter: Counter) -> &T {
                match counter {
                    $(Counter::$variant => &self.$variant,)*
                }
            }

            fn get_mut(&mut self, counter: Counter) -> &mut T {
                match counter {
                    $(Counter::$variant => &mut self.$variant,)*
                }
            }
        }
    };
    (@logged logged) => { true };
    (@logged) => { false };
}

counters! {
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
    Rto = "tcp.rto";
    RtoUnsent = "tcp.rto-unsent";
    RetransmitBytes = "tcp.retransmit.bytes";
    FastRecovery = "tcp.fast-recovery";
    SackRecovery = "tcp.sack-recovery";
    LimitedTransmit = "tcp.limited-transmit";
    PersistProbe = "tcp.persist-probe";
    KeepaliveProbe = "tcp.keepalive-probe";
}

#[derive(Clone, Debug, Default)]
pub struct Counters(PerCounter<u64>);

impl Counters {
    pub fn get(&self, counter: Counter) -> u64 {
        *self.0.get(counter)
    }

    pub(crate) fn add(&mut self, counter: Counter, n: u64) {
        let value = self.0.get_mut(counter);
        *value = value.saturating_add(n);
    }

    pub fn iter(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        Counter::ALL.iter().map(|&c| (c.name(), self.get(c)))
    }
}

/// A refusal of input ToyOS does not implement, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rule: Counter,
    pub local: Endpoint,
    pub remote: Endpoint,
}

/// At most one line per rule in any 10 s, each carrying how many of that rule it stood for.
pub const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default)]
pub struct RefusalLog(PerCounter<(Option<Instant>, u64)>);

impl RefusalLog {
    /// `Some(suppressed)` when this refusal is to be logged, with how many of its rule were not.
    pub fn admit(&mut self, now: Instant, refusal: &Refusal) -> Option<u64> {
        let (last, suppressed) = self.0.get_mut(refusal.rule);
        if last.is_some_and(|at| now.since(at) < REFUSAL_LOG_INTERVAL) {
            *suppressed = suppressed.saturating_add(1);
            return None;
        }
        *last = Some(now);
        Some(core::mem::replace(suppressed, 0))
    }
}
