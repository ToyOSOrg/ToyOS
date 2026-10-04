//! ToyOS's TCP (RFC 9293) with RFC 5961's blind-attack checks, MSS, window scaling, timestamps
//! with PAWS (RFC 7323), SACK and D-SACK (RFC 2018, 2883), RFC 6298's timer, CUBIC (RFC 9438),
//! NewReno (RFC 6582) and SACK-based recovery (RFC 6675), keyed initial sequence numbers and ports
//! (RFC 6528, 6056), and TIME-WAIT hardened against assassination (RFC 1337). Pure: the caller hands
//! in the time, the secrets, parsed segments, classified ICMP errors, user calls and transmit
//! credit; nothing here reads a clock, draws randomness or does I/O.
//!
//! **Pull egress.** A segment exists only while [`Tcp::transmit_owed`] or [`Tcp::serve`] hands it
//! to the caller's sink, built from the state of that moment, once the caller has answered that its
//! next hop is known; it counts as sent only once the sink took its frame. Which connection is
//! served is the caller's round: [`Tcp::drain_eligible`] offers each once it has something to send,
//! and [`Tcp::drain_gone`] names each freed while in it.
//!
//! **Refusals are values.** Legacy or insecure input is refused, counted in [`Counters`], and
//! named by an [`Event::Refused`] the shell logs through [`RefusalLog`].

#![cfg_attr(not(test), no_std)]
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
/// `tests/common`, which the property tests share, names the crate from outside.
#[cfg(test)]
extern crate self as toyos_net_tcp;

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
mod stack;
mod tx;

use core::net::Ipv4Addr;
use core::time::Duration;

use toyos_net_wire::siphash;
use toyos_net_wire::Port;

pub use counters::{Counter, Counters, Refusal, RefusalLog};
pub use seq::Seq;
pub use stack::{ConnId, Info, ListenerId, Outgoing, Served, Tcp};
pub use toyos_net_wire::siphash::Key;
pub use toyos_net_wire::Instant;

/// The TSval a 4-tuple with `offset` sends at `now`: one tick per millisecond (RFC 7323 §5.4).
const fn tsval(now: Instant, offset: u32) -> u32 {
    let [a, b, c, d, ..] = (now.nanos() / 1_000_000).to_le_bytes();
    u32::from_le_bytes([a, b, c, d]).wrapping_add(offset)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Endpoint {
    pub addr: Ipv4Addr,
    pub port: Port,
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

pub mod limits {
    use core::time::Duration;

    pub const TIME_WAIT: Duration = Duration::from_secs(60);
    pub const ORPHAN_IDLE: Duration = Duration::from_secs(60);
    pub const GIVE_UP: Duration = Duration::from_secs(900);
    pub const SYN_GIVE_UP: Duration = Duration::from_secs(180);
    pub const SYNACK_GIVE_UP: Duration = Duration::from_secs(60);
    pub const SWS_OVERRIDE: Duration = Duration::from_millis(200);
    pub const KEEPALIVE_IDLE: Duration = Duration::from_secs(7_200);
    pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(75);
    pub const KEEPALIVE_PROBES: u32 = 9;
    pub const LISTEN_READY: usize = 128;
    pub const LISTEN_PENDING: usize = 256;
    pub const TIME_WAIT_MAX: usize = 16_384;
    pub const MSS_FLOOR: u16 = 536;
    pub const UNSOLICITED_ACK: Duration = Duration::from_millis(500);
    /// Events held for the shell until it drains them.
    pub const EVENTS: usize = 1_024;
    /// The largest window scaling can offer (RFC 7323 §2.3): no receive buffer is larger.
    pub const RECEIVE_BUFFER_MAX: u32 = 65_535 << 14;
}

/// A [`Config`] that [`Tcp::new`] refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    /// Above [`limits::RECEIVE_BUFFER_MAX`]: the window could never offer all of it.
    ReceiveBufferTooLarge,
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

/// Whether a segment for a 4-tuple can be built now (`ip.md` §6.7): its next hop's link address
/// is known, and `T` is what the caller needs to use it; resolution is under way; or it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hop<T> {
    Ready(T),
    Pending,
    Unreachable,
}

/// One flow's way out of a transmit opportunity: the hop question, asked once a segment is due and
/// before it is built (`ip.md` §6.7 (1)), then the segment's frame, built before anything about the
/// segment is committed (§11.3). Either refusing leaves everything owed as it was.
pub(crate) trait Exit<T> {
    fn ask(&mut self) -> Result<T, NotReady>;
    fn send(&mut self, via: T, segment: &conn::Out, payload: (&[u8], &[u8])) -> Result<(), NotReady>;
}

/// Why a due segment did not leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotReady {
    Pending,
    Unreachable,
    /// The caller could not frame it.
    Unframed,
}

impl<T> Hop<T> {
    pub(crate) fn ready(self) -> Result<T, NotReady> {
        match self {
            Self::Ready(via) => Ok(via),
            Self::Pending => Err(NotReady::Pending),
            Self::Unreachable => Err(NotReady::Unreachable),
        }
    }
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
