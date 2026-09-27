//! ToyOS's TCP (RFC 9293) with RFC 5961's blind-attack checks, MSS, window scaling, timestamps
//! with PAWS (RFC 7323), SACK and D-SACK (RFC 2018, 2883), RFC 6298's timer, CUBIC (RFC 9438),
//! NewReno (RFC 6582) and SACK-based recovery (RFC 6675), keyed initial sequence numbers and ports
//! (RFC 6528, 6056), and TIME-WAIT hardened against assassination (RFC 1337). Pure: the caller hands
//! in the time, the secrets, parsed segments, classified ICMP errors, user calls and transmit
//! credit; nothing here reads a clock, draws randomness or does I/O.
//!
//! **Pull egress.** A segment exists only while [`Tcp::transmit`] hands it to the caller's sink,
//! built from the state of that moment. Sequence space counts as sent, and the retransmission timer
//! starts, only then; an expiry marks the oldest segment due and arms nothing, so a retransmission
//! timeout can never fire for a segment that has not left (`tcp.rto-unsent` stays 0).
//!
//! **Refusals are values.** Legacy or insecure input is refused, counted in [`Counters`], and
//! named by an [`Event::Refused`] the shell logs through [`RefusalLog`].

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

extern crate alloc;

mod cc;
mod conn;
mod counters;
mod open;
#[cfg(test)]
mod props;
mod ring;
mod rtt;
mod rx;
mod seq;
mod siphash;
mod stack;
mod tx;

use core::cmp::Ordering;
use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::Port;

pub use counters::{Counter, Counters, Refusal, RefusalLog, REFUSAL_LOG_INTERVAL};
pub use seq::Seq;
pub use siphash::{siphash24, Key};
pub use stack::{ConnId, Info, ListenerId, Outgoing, Tcp};

/// A point on the caller's monotonic clock, in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(u64);

impl Instant {
    pub const fn from_nanos(ns: u64) -> Self {
        Self(ns)
    }

    pub const fn from_millis(ms: u64) -> Self {
        Self(ms.saturating_mul(1_000_000))
    }

    pub const fn nanos(self) -> u64 {
        self.0
    }

    pub fn after(self, d: Duration) -> Self {
        Self(self.0.saturating_add(u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)))
    }

    /// Zero when `earlier` is not earlier.
    pub const fn since(self, earlier: Self) -> Duration {
        Duration::from_nanos(self.0.saturating_sub(earlier.0))
    }

    /// The timestamp clock's tick: one per millisecond (RFC 7323 §5.4).
    const fn millis32(self) -> u32 {
        let [a, b, c, d, ..] = (self.0 / 1_000_000).to_le_bytes();
        u32::from_le_bytes([a, b, c, d])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub addr: Ipv4Addr,
    pub port: Port,
}

impl Ord for Endpoint {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.addr, self.port.get()).cmp(&(other.addr, other.port.get()))
    }
}

impl PartialOrd for Endpoint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tuple {
    pub local: Endpoint,
    pub remote: Endpoint,
}

impl Tuple {
    /// Local address, local port, remote address, remote port, in network order: the input of
    /// the ISN and timestamp-offset functions.
    fn bytes(&self) -> [u8; 12] {
        let [a, b, c, d] = self.local.addr.octets();
        let [e, f] = self.local.port.get().to_be_bytes();
        let [g, h, i, j] = self.remote.addr.octets();
        let [k, l] = self.remote.port.get().to_be_bytes();
        [a, b, c, d, e, f, g, h, i, j, k, l]
    }
}

/// Independent 128-bit keys from the system's randomness, and the port counter table
/// (RFC 6056 §3.4, RFC 6528 §3). One leaked value predicts no other.
#[derive(Clone, Debug)]
pub struct Secrets {
    pub isn: Key,
    pub timestamp: Key,
    pub port_offset: Key,
    pub port_index: Key,
    pub port_table: [u16; 16],
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The outgoing interface's IP MTU.
    pub mtu: u16,
    pub receive_buffer: u32,
    pub send_buffer: u32,
    pub secrets: Secrets,
}

/// RFC 6528 §3: `M + F(4-tuple)`, M the 4 µs clock, F keyed SipHash-2-4.
pub fn isn(key: &Key, tuple: &Tuple, now: Instant) -> Seq {
    let [a, b, c, d, ..] = (now.nanos() / 4_000).to_le_bytes();
    Seq::new(u32::from_le_bytes([a, b, c, d])).add(siphash::low32(key, &tuple.bytes()))
}

/// RFC 7323 §5.4: the per-4-tuple offset hiding uptime, the same across incarnations.
pub fn ts_offset(key: &Key, tuple: &Tuple) -> u32 {
    siphash::low32(key, &tuple.bytes())
}

/// RFC 9293 MAY-2 (1): a connection reopened from TIME-WAIT starts beyond the old sequence space.
pub fn reuse_iss(old_snd_nxt: Seq, fresh: Seq) -> Seq {
    if fresh.after(old_snd_nxt) {
        fresh
    } else {
        old_snd_nxt.add(65_536).add(fresh.get() & 0xffff)
    }
}

/// Published bounds: inspect's `limits.tcp.*`, read by tests instead of restated.
pub mod limits {
    use core::time::Duration;

    pub const TIME_WAIT: Duration = Duration::from_secs(60);
    pub const ORPHAN_IDLE: Duration = Duration::from_secs(60);
    pub const GIVE_UP: Duration = Duration::from_secs(900);
    pub const SYN_GIVE_UP: Duration = Duration::from_secs(180);
    pub const SYNACK_GIVE_UP: Duration = Duration::from_secs(60);
    pub const RTO_INITIAL: Duration = crate::rtt::RTO_INITIAL;
    pub const RTO_MIN: Duration = crate::rtt::RTO_MIN;
    pub const RTO_MAX: Duration = crate::rtt::RTO_MAX;
    pub const DELAYED_ACK: Duration = crate::rx::DELAYED_ACK;
    pub const SWS_OVERRIDE: Duration = Duration::from_millis(200);
    pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(7_200);
    pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(75);
    pub const KEEPALIVE_PROBES: u32 = 9;
    pub const LISTEN_READY: usize = 128;
    pub const LISTEN_PENDING: usize = 256;
    pub const OOO_RANGES: usize = crate::rx::OOO_RANGES;
    pub const SACK_RANGES: usize = crate::tx::SACK_RANGES;
    pub const TIME_WAIT_MAX: usize = 16_384;
    pub const MSS_FLOOR: u16 = 536;
    pub const UNSOLICITED_ACK: Duration = Duration::from_millis(500);
    pub const REFUSAL_LOG: Duration = crate::REFUSAL_LOG_INTERVAL;

    fn ms(d: Duration) -> u64 {
        u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
    }

    fn units(n: usize) -> u64 {
        u64::try_from(n).unwrap_or(u64::MAX)
    }

    /// `limits.tcp.<name>` and its value in milliseconds or units.
    pub fn all() -> [(&'static str, u64); 21] {
        [
            ("time_wait_ms", ms(TIME_WAIT)),
            ("orphan_idle_ms", ms(ORPHAN_IDLE)),
            ("give_up_ms", ms(GIVE_UP)),
            ("syn_give_up_ms", ms(SYN_GIVE_UP)),
            ("synack_give_up_ms", ms(SYNACK_GIVE_UP)),
            ("rto_initial_ms", ms(RTO_INITIAL)),
            ("rto_min_ms", ms(RTO_MIN)),
            ("rto_max_ms", ms(RTO_MAX)),
            ("delayed_ack_ms", ms(DELAYED_ACK)),
            ("sws_override_ms", ms(SWS_OVERRIDE)),
            ("keepalive_idle_ms", ms(KEEPALIVE_IDLE)),
            ("keepalive_interval_ms", ms(KEEPALIVE_INTERVAL)),
            ("keepalive_probes", u64::from(KEEPALIVE_PROBES)),
            ("listen_ready", units(LISTEN_READY)),
            ("listen_pending", units(LISTEN_PENDING)),
            ("ooo_ranges", units(OOO_RANGES)),
            ("sack_ranges", units(SACK_RANGES)),
            ("timewait_max", units(TIME_WAIT_MAX)),
            ("mss_floor", u64::from(MSS_FLOOR)),
            ("unsolicited_ack_ms", ms(UNSOLICITED_ACK)),
            ("refusal_log_ms", ms(REFUSAL_LOG)),
        ]
    }
}

/// Per-connection options, inherited by a listener's children.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub nodelay: bool,
    pub keepalive: Option<Keepalive>,
    pub user_timeout: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Keepalive {
    pub idle: Duration,
    pub interval: Duration,
    pub probes: u32,
}

impl Default for Keepalive {
    fn default() -> Self {
        Self { idle: limits::KEEPALIVE_IDLE, interval: limits::KEEPALIVE_INTERVAL, probes: limits::KEEPALIVE_PROBES }
    }
}

/// RFC 9293 §3.3.2's states; `Closed` is a socket whose connection has ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    Closed,
}

/// Why a connection ended other than by both FINs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Refused,
    Reset,
    TimedOut,
    Unreachable(SoftError),
    /// ICMP destination unreachable, administratively prohibited, during the handshake.
    Prohibited,
}

/// An ICMP error that did not end the connection; reported as the reason if it later gives up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftError {
    Unreachable(toyos_net_wire::icmp::UnreachableCode),
    TimeExceeded,
    ParameterProblem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NoSuchSocket,
    NotConnected,
    /// The socket already listens, connects or is connected.
    Exists,
    /// The write side is shut.
    Closing,
    WouldBlock,
    AddrInUse,
    /// The remote is not a unicast address, or names the local endpoint.
    InvalidRemote,
    Failed(Failure),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Received {
    Data(usize),
    /// The peer's FIN, after every byte before it.
    End,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub state: State,
    pub readable: usize,
    pub writable: usize,
    pub failure: Option<Failure>,
    pub soft_error: Option<SoftError>,
    /// R1 reached: three expiries without progress (RFC 9293 SHLD-9).
    pub delivery_problem: bool,
}

/// Classified by [ip]: which error, about which of our segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcmpError {
    pub local: Endpoint,
    pub remote: Endpoint,
    pub sequence: Seq,
    pub kind: IcmpKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcmpKind {
    Unreachable(toyos_net_wire::icmp::UnreachableCode),
    /// Destination unreachable, fragmentation needed: the router's MTU and the refused datagram's length.
    PacketTooBig { next_hop_mtu: Option<core::num::NonZeroU16>, quoted_length: u16 },
    TimeExceeded,
    ParameterProblem,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Refused(Refusal),
    /// An ACK advanced SND.UNA: the next hop to this address answers (RFC 1122 §3.3.1.4).
    Reachable(Ipv4Addr),
    /// Three expiries without progress: re-verify the next hop to this address.
    Reverify(Ipv4Addr),
}

// Each compile_fail block sits beside one that compiles, so a typo cannot pass it.
#[cfg(doctest)]
mod compile_fail {
    /// `connect` makes a connection and names no socket: one cannot be connected again (HS-36).
    /// A listener's id is not a connection's (HS-43).
    ///
    /// ```
    /// use toyos_net_tcp::{ConnId, Instant, Tcp};
    /// fn send(tcp: &mut Tcp, now: Instant, id: ConnId) {
    ///     let _ = tcp.send(now, id, b"x");
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_tcp::{ConnId, Endpoint, Instant, Tcp};
    /// fn again(tcp: &mut Tcp, now: Instant, id: ConnId, remote: Endpoint) {
    ///     let _ = tcp.connect(now, id, remote);
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_tcp::{Instant, ListenerId, Tcp};
    /// fn send(tcp: &mut Tcp, now: Instant, id: ListenerId) {
    ///     let _ = tcp.send(now, id, b"x");
    /// }
    /// ```
    #[allow(non_camel_case_types)]
    pub struct s_hs_036_a_connection_cannot_connect_again;

    /// A segment hands [ip] no traffic class and no fragment form: DSCP 0, ECN 0 and DF are
    /// [ip]'s on every datagram (OP-30).
    ///
    /// ```
    /// use toyos_net_tcp::Outgoing;
    /// fn fields(o: &Outgoing<'_>) {
    ///     let _ = (o.source, o.destination, o.segment);
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_tcp::Outgoing;
    /// fn fields(o: &Outgoing<'_>) {
    ///     let _ = o.traffic_class;
    /// }
    /// ```
    #[allow(non_camel_case_types)]
    pub struct s_op_030_tcp_cannot_choose_dscp_ecn_or_df;
}
