//! ToyOS's IPv4 host layer (RFC 1122), pure: per-interface addresses and the host routing table,
//! frame dispatch and the IPv4 input policy, neighbour resolution over ARP with one reachability
//! machine (RFC 4861 §7.3 with RFC 7048's UNREACHABLE), address conflict detection (RFC 5227),
//! ICMP (echo, errors under a two-level token bucket, inbound errors classified for the
//! transports), and the IGMPv3 host with its IGMPv2 compatibility mode (RFC 9776). The caller
//! hands in the time, the secret, frames, configuration and transport requests; nothing here
//! reads a clock, draws randomness or does I/O.
//!
//! **Pull egress.** [ip]'s own frames — ARP, IGMP, ICMP, and datagrams released by resolution —
//! wait in one FIFO that [`Ip::transmit`] drains with the credit the device offers, and a frame's
//! timers start when it leaves. A datagram a transport hands [`Ip::send_udp`] is written straight
//! into the device's buffer, or waits here for its next hop; a full ring never drops one.
//!
//! **Refusals are values.** Every refusal is a named [`Counter`]; one of legacy or insecure input
//! is also an [`Event::Refused`] naming the rule and the peer, which the shell logs through
//! [`RefusalLog`].
//!
//! **Draws are keyed.** Every random delay, the neighbour reachable time and the error limiter's
//! slots and jitter come from SipHash-2-4 over (secret, purpose, counter): with one secret the
//! layer is deterministic, and without it no off-link host can predict a draw.

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

mod acd;
mod addr;
mod arp;
mod config;
mod counters;
mod draw;
mod egress;
mod icmp;
mod iface;
mod igmp;
mod input;
mod limiter;
mod nud;
mod route;
mod timers;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::net::Ipv4Addr;
use core::num::NonZeroU16;
use core::time::Duration;

use toyos_net_wire::ethernet::{IndividualMac, MacAddr, MacClass};
use toyos_net_wire::icmp::{ParameterProblemCode, TimeExceededCode, UnreachableCode};
use toyos_net_wire::ipv4::{Ipv4Packet, MulticastAddr};
use toyos_net_wire::siphash::Key;
use toyos_net_wire::tcp::TcpSegment;
use toyos_net_wire::udp::UdpDatagram;
use toyos_net_wire::Port;

pub use addr::AddrState;
pub use counters::{Counter, Counters, RefusalLog};
pub use egress::{Sent, UdpOut, FRAME};
pub use igmp::IgmpMode;
pub use limiter::Limiter;
pub use nud::{Incomplete, Linked, Nud, Probing, Reachable, Unreachable};
pub use route::{NextHop, Route, Source};
pub use toyos_net_wire::Instant;

/// An interface, named by the order it was added; the lower index wins a routing tie (§3.3). An
/// index names an interface of the [`Ip`] that returned it and nothing in another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IfIndex(usize);

/// The IP MTU of every interface: an Ethernet body.
pub const MTU: usize = toyos_net_wire::ethernet::MAX_BODY;

pub mod limits {
    pub const ADDRS: usize = 8;
    pub const GATEWAYS: usize = 4;
    pub const CONTROL_QUEUE: usize = 64;
    pub const ECHO_REPLIES: usize = 16;
    pub const IGMP_GROUPS: usize = 32;
    pub const IGMP_QUERY_SOURCES: usize = 64;
    /// Refusal events held for the shell until it drains them.
    pub const EVENTS: usize = 1_024;

    /// RFC 4861 §10 and RFC 7048 §3, over ARP.
    pub mod nud {
        use core::time::Duration;

        pub const RETRANS: Duration = Duration::from_secs(1);
        pub const BROADCAST_SOLICIT: u8 = 3;
        pub const UNICAST_SOLICIT: u8 = 3;
        pub const REACHABLE_MIN: Duration = Duration::from_secs(15);
        pub const REACHABLE_MAX: Duration = Duration::from_secs(45);
        pub const REACHABLE_REDRAW: Duration = Duration::from_secs(7_200);
        pub const DELAY_FIRST_PROBE: Duration = Duration::from_secs(5);
        pub const BACKOFF_MULTIPLE: u32 = 3;
        pub const MAX_RETRANS: Duration = Duration::from_secs(60);
        pub const FAILED_HOLD: Duration = Duration::from_secs(20);
        pub const LOCKTIME: Duration = Duration::from_secs(1);
        pub const IDLE_LIFETIME: Duration = Duration::from_secs(600);
        pub const PENDING_PER_NEIGHBOUR: usize = 8;
        pub const PENDING_TOTAL: usize = 64;
        pub const TABLE_MAX: usize = 512;
    }

    /// RFC 5227 §1.1. The owner's ruling scales every probing interval by one factor, so the
    /// longest probing — PROBE_WAIT, two PROBE_MAX gaps and ANNOUNCE_WAIT, 7 s by the RFC — takes
    /// at most 200 ms; announcing, defence and the rate limit keep the RFC's values.
    pub mod acd {
        use core::time::Duration;

        const fn scaled(rfc_ms: u64) -> Duration {
            Duration::from_nanos(rfc_ms.saturating_mul(1_000_000).saturating_mul(200) / 7_000)
        }

        pub const PROBE_WAIT: Duration = scaled(1_000);
        pub const PROBE_NUM: u8 = 3;
        pub const PROBE_MIN: Duration = scaled(1_000);
        pub const PROBE_MAX: Duration = scaled(2_000);
        pub const ANNOUNCE_WAIT: Duration = scaled(2_000);
        pub const ANNOUNCE_NUM: u8 = 2;
        pub const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
        pub const MAX_CONFLICTS: u32 = 10;
        pub const RATE_LIMIT_INTERVAL: Duration = Duration::from_secs(60);
        pub const DEFEND_INTERVAL: Duration = Duration::from_secs(10);
        /// The conflict count starts over after this long without a conflict.
        pub const CONFLICT_RESET: Duration = Duration::from_secs(600);
    }

    /// The error limiter: per destination, and one global bucket whose burst is redrawn every
    /// second (RFC 4443 §2.4 (f)).
    pub mod icmp {
        pub const DEST_BURST: u64 = 10;
        pub const DEST_PER_S: u64 = 10;
        pub const DEST_SLOTS: usize = 256;
        pub const GLOBAL_BURST_MIN: u64 = 75;
        pub const GLOBAL_BURST_MAX: u64 = 100;
        pub const GLOBAL_PER_S: u64 = 100;
    }

    /// RFC 9776 §8 and RFC 2236 §8.
    pub mod igmp {
        use core::time::Duration;

        pub const ROBUSTNESS: u8 = 2;
        pub const QUERY_INTERVAL: Duration = Duration::from_secs(125);
        pub const UNSOLICITED_REPORT: Duration = Duration::from_secs(1);
        pub const V2_UNSOLICITED_REPORT: Duration = Duration::from_secs(10);
    }
}

/// How a datagram reached us (§4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cast {
    /// To an announcing or assigned address of the receiving interface.
    Unicast,
    LimitedBroadcast,
    /// To the directed broadcast of one of the interface's prefixes of /30 or shorter.
    SubnetBroadcast,
    Multicast(MulticastAddr),
    /// Admitted by the acquisition exception alone: for the DHCP client's socket and no other.
    Acquisition,
}

/// What a transport is handed with a datagram.
#[derive(Clone, Copy, Debug)]
pub struct Arrival<'a> {
    pub iface: IfIndex,
    pub packet: Ipv4Packet<'a>,
    pub cast: Cast,
    /// How the frame that carried it was addressed.
    pub link: MacClass,
}

#[derive(Clone, Copy, Debug)]
pub enum Delivery<'a> {
    Udp(Arrival<'a>, UdpDatagram<'a>),
    /// Only ever to one of our unicast addresses, from a valid unicast source.
    Tcp(Arrival<'a>, TcpSegment<'a>),
    Error(TransportError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Tcp,
    Udp,
}

/// A transport 4-tuple, as a datagram of ours carried it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Flow {
    pub source: Ipv4Addr,
    pub source_port: Port,
    pub destination: Ipv4Addr,
    pub destination_port: Port,
}

/// An ICMP error [ip] validated against one of our datagrams (§9.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub iface: IfIndex,
    pub transport: Transport,
    /// The quoted datagram's 4-tuple: ours as its source.
    pub flow: Flow,
    /// The quoted TCP sequence number.
    pub sequence: Option<u32>,
    pub kind: ErrorKind,
    /// The ICMP message's own source.
    pub reporter: Ipv4Addr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Destination unreachable, any code but fragmentation needed.
    Unreachable(UnreachableCode),
    FragmentationNeeded { next_hop_mtu: Option<NonZeroU16>, quoted_length: u16 },
    TimeExceeded(TimeExceededCode),
    ParameterProblem { code: ParameterProblemCode, pointer: u8 },
}

/// §9.5's shared classification, the vocabulary the transports act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// Codes 2 and 3: the peer refused.
    Refused,
    /// Codes 9, 10 and 13: administratively prohibited.
    Prohibited,
    PathMtu,
    Soft,
}

impl ErrorKind {
    pub const fn class(self) -> ErrorClass {
        match self {
            Self::Unreachable(UnreachableCode::Protocol | UnreachableCode::Port) => ErrorClass::Refused,
            Self::Unreachable(
                UnreachableCode::NetProhibited | UnreachableCode::HostProhibited | UnreachableCode::CommunicationProhibited,
            ) => ErrorClass::Prohibited,
            Self::FragmentationNeeded { .. } | Self::Unreachable(UnreachableCode::FragmentationNeeded) => ErrorClass::PathMtu,
            Self::Unreachable(_) | Self::TimeExceeded(_) | Self::ParameterProblem { .. } => ErrorClass::Soft,
        }
    }
}

/// Who a refusal names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    Ip(Ipv4Addr),
    Arp { ip: Ipv4Addr, mac: MacAddr },
    /// A neighbour's MAC, and the one an ARP packet named instead.
    MacChange { ip: Ipv4Addr, old: MacAddr, new: MacAddr },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub rule: Counter,
    pub iface: IfIndex,
    pub peer: Peer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Refused(Refusal),
    /// Conflict detection finished: the address is usable.
    Verified { iface: IfIndex, addr: Ipv4Addr },
    /// Another host used the address while it was probed; it was removed.
    Conflict { iface: IfIndex, addr: Ipv4Addr, mac: MacAddr },
    /// The address was lost to another host after one defence (RFC 5227 §2.4 (b)) and removed:
    /// every connection using it is to be reset.
    Lost { iface: IfIndex, addr: Ipv4Addr, mac: MacAddr },
    /// The link went down while the address was probed; it was removed.
    NotVerified { iface: IfIndex, addr: Ipv4Addr },
    /// A next hop resolved: flows waiting on it may send.
    Resolved { iface: IfIndex, next_hop: Ipv4Addr },
    /// A next hop failed resolution: flows waiting on it are told "host unreachable".
    Failed { iface: IfIndex, next_hop: Ipv4Addr },
    /// A UDP datagram [ip] took could not reach its next hop.
    Unreachable(Flow),
}

/// Upper-layer reachability advice (RFC 1122 §3.3.1.4, RFC 4861 §7.3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Advice {
    /// Forward progress: the next hop answers.
    Confirmed,
    /// No progress: re-verify the next hop.
    Reverify,
}

/// Whether a flow may build a segment for its next hop now (§6.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Resolved(MacAddr),
    Pending,
    Failed,
}

pub struct Ip {
    draws: draw::Draws,
    ifaces: Vec<iface::Interface>,
    timers: timers::Timers,
    control: egress::Control,
    limiter: Limiter,
    log: counters::Log,
    latest: Instant,
    generation: u64,
}

impl Ip {
    pub fn new(now: Instant, secret: Key) -> Self {
        let draws = draw::Draws::new(secret);
        let limiter = Limiter::new(draws.limiter_key());
        Self {
            draws,
            ifaces: Vec::new(),
            timers: timers::Timers::default(),
            control: egress::Control::default(),
            limiter,
            log: counters::Log::default(),
            latest: now,
            generation: 0,
        }
    }

    pub fn add_interface(&mut self, now: Instant, mac: IndividualMac) -> IfIndex {
        let now = self.clock(now);
        let reachable = self.draws.reachable_time();
        self.ifaces.push(iface::Interface {
            mac,
            up: false,
            addresses: Vec::new(),
            gateways: Vec::new(),
            active: None,
            neighbours: BTreeMap::new(),
            held: 0,
            reachable,
            reachable_drawn: now,
            acd: acd::Conflicts::default(),
            igmp: igmp::Igmp::default(),
        });
        IfIndex(self.ifaces.len().saturating_sub(1))
    }

    pub fn counters(&self) -> &Counters {
        &self.log.counters
    }

    /// How many refusals of the wire rule `name` (a `toyos_net_wire` reason's `name()`) [ip] met.
    pub fn wire_refusals(&self, name: &str) -> u64 {
        self.log.wire.get(name).copied().unwrap_or(0)
    }

    /// Every wire refusal [ip] met, by name, for inspect.
    pub fn wire_counters(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        self.log.wire.iter().map(|(&name, &n)| (name, n))
    }

    /// Refusals, address results and resolution news since the last call.
    pub fn drain_events(&mut self) -> alloc::vec::Drain<'_, Event> {
        self.log.drain()
    }

    /// Bumped by every change that can alter a route lookup (§3.6).
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub fn neighbour(&self, iface: IfIndex, addr: Ipv4Addr) -> Option<&Nud> {
        self.ifaces.get(iface.0)?.neighbours.get(&addr).map(|n| &n.state)
    }

    pub fn address(&self, iface: IfIndex, addr: Ipv4Addr) -> Option<AddrState> {
        self.ifaces.get(iface.0)?.addresses.iter().find(|a| a.cidr.addr == addr).map(|a| a.state())
    }

    /// The prefix length of one of the interface's addresses.
    pub fn prefix_len(&self, iface: IfIndex, addr: Ipv4Addr) -> Option<u8> {
        self.ifaces.get(iface.0)?.addresses.iter().find(|a| a.cidr.addr == addr).map(|a| a.cidr.len)
    }

    pub fn reachable_time(&self, iface: IfIndex) -> Option<Duration> {
        self.ifaces.get(iface.0).map(|i| i.reachable)
    }

    pub fn gateways(&self, iface: IfIndex) -> Option<&[Ipv4Addr]> {
        self.ifaces.get(iface.0).map(|i| i.gateways.as_slice())
    }

    /// An announcing or assigned address of any interface: one a socket may bind and send from.
    pub fn is_assigned(&self, addr: Ipv4Addr) -> bool {
        self.ifaces.iter().any(|i| i.addresses.iter().any(|a| a.cidr.addr == addr && a.usable()))
    }

    /// One of our addresses in any state.
    pub fn is_local(&self, addr: Ipv4Addr) -> bool {
        self.ifaces.iter().any(|i| i.addresses.iter().any(|a| a.cidr.addr == addr))
    }

    /// The directed broadcast of a usable prefix of /30 or shorter.
    pub fn is_directed_broadcast(&self, addr: Ipv4Addr) -> bool {
        self.ifaces.iter().any(|i| i.addresses.iter().any(|a| a.usable() && a.cidr.broadcast() == Some(addr)))
    }

    /// A `now` before the latest one seen is the latest one: no deadline fires early and no
    /// duration is negative, and the regression is counted (§1.4 (4)).
    fn clock(&mut self, now: Instant) -> Instant {
        if now < self.latest {
            self.log.count(Counter::ClockRegressed);
            self.latest
        } else {
            self.latest = now;
            now
        }
    }
}

// Each compile_fail block sits beside one that compiles, so a typo cannot pass it.
#[cfg(doctest)]
mod compile_fail {
    /// NUD-28: a MAC exists only in a state that has one, and a pending queue only in INCOMPLETE.
    ///
    /// ```
    /// use toyos_net_ip::{Linked, Nud};
    /// fn f(n: &Nud) -> usize {
    ///     match n {
    ///         Nud::Incomplete(i) => i.queued(),
    ///         Nud::Reachable(r) => usize::from(r.mac().0[0]),
    ///         Nud::Stale(s) => {
    ///             let _: &Linked = s;
    ///             2
    ///         }
    ///         Nud::Failed => 0,
    ///         _ => 1,
    ///     }
    /// }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_ip::Nud;
    /// fn f(n: &Nud) { if let Nud::Incomplete(i) = n { let _ = i.mac(); } }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_ip::Nud;
    /// fn f(n: &Nud) { if let Nud::Failed(_) = n {} }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_ip::Nud;
    /// fn f(n: &Nud) { if let Nud::Reachable(r) = n { let _ = r.queued(); } }
    /// ```
    ///
    /// ```compile_fail
    /// use toyos_net_ip::Nud;
    /// fn f(n: &Nud) { if let Nud::Stale(s) = n { let _ = s.queued(); } }
    /// ```
    #[allow(non_camel_case_types)]
    pub struct s_ip_nud_028_a_state_holds_only_its_own_fields;
}
