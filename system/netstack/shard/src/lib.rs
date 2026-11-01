//! One shard of ToyOS's stack: `toyos-net-ip`, `toyos-net-tcp` and `toyos-net-udp` over every
//! interface the caller adds ([`Shard::add_interface`]), one per link. Frames come in through
//! [`Shard::receive`] and leave only when the links pull them with [`Shard::transmit`]; the
//! caller hands in the time and the secrets, and nothing here reads a clock, draws randomness or
//! does I/O.
//!
//! **Egress.** One opportunity serves every link with room, each frame handed out with the
//! interface it leaves by and spending that link's credit. A frame of credit goes to [ip]'s own
//! frames first — ARP, IGMP, ICMP and the datagrams resolution released — then to what [tcp]
//! owes outside a connection, and otherwise to the round: a deficit round-robin in bytes over every TCP connection and UDP
//! sender with something to send, in the order each became eligible (RFC 8290 §3, without its new-flows list). A flow
//! waiting for its next hop leaves the round, and rejoins at its tail when woken. A TCP segment is
//! built only once its next hop's link address is known, and committed only once its frame is
//! handed off; the send registers with the neighbour entry then. A flow whose
//! next hop is unresolved or failed builds nothing, spends nothing, and is not asked again until
//! [ip] reports a change for that next hop, for the routes, or, to a flow a full neighbour table
//! refused, that the table has room: a waiting flow costs one question per such change. A UDP
//! datagram [ip] does not take, its next hop unresolved, stays in its sender's queue in [udp],
//! spending nothing and charged nothing, while the sender's datagrams to other next hops leave:
//! it is handed to [ip] again when [ip] reports the hop on its interface resolved or the routes
//! changed, and dropped by [udp] when [ip] reports the hop on its interface failed. One [ip] refuses, for want of a route or
//! for a next hop it holds failed or has no room for, is [ip]'s to count and report. A flow
//! whose route leaves by a link with no room waits on that link as on a next hop, and asks again
//! at the next opportunity that link has room in.
//!
//! **Refusals.** Each crate's refusals of legacy or insecure input pass through that crate's
//! `RefusalLog` here: at most one [`Event::Refused`] per rule in any 10 s, carrying how many
//! were suppressed since the last.
//!
//! **Resets for no socket** draw from their own instance of [ip]'s error limiter,
//! keyed by its own secret.

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

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_ip::{Advice, Delivery, ErrorKind, Ip, Limiter, NextHop, Nud, Resolution, Sent, Source, Transport, TransportError, FRAME};
use toyos_net_tcp::{ConnId, Endpoint, Hop, IcmpError, IcmpKind, Lent, ListenerId, Outgoing, Received, Seq, Served, Status, Tcp, Tuple};
use toyos_net_udp::{Offer, Sender, SocketId, Udp, Verdict};
use toyos_net_udp::Served as UdpServed;
use toyos_net_wire::ethernet::{FrameBuilder, IndividualMac, MacAddr};
use toyos_net_wire::ipv4::{Ipv4Builder, Ipv4Source, MulticastAddr, TrafficClass, Ttl};
use toyos_net_wire::siphash::Key;
use toyos_net_wire::{Instant, Port};

pub use toyos_net_ip::{Credit, IfIndex, Rank};
pub use toyos_net_udp::Origin;

/// A connected datagram socket's peer, and the address it sends there from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpPeer {
    pub addr: Ipv4Addr,
    pub port: Port,
    pub local: Ipv4Addr,
}

/// Independent 128-bit keys from the system's randomness: one leaked value predicts no other.
#[derive(Clone, Debug)]
pub struct Secrets {
    pub ip: Key,
    /// The limiter on resets for segments that matched no socket.
    pub resets: Key,
    pub tcp: toyos_net_tcp::Secrets,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub receive_buffer: u32,
    pub send_buffer: u32,
    pub secrets: Secrets,
}

/// A refusal of legacy or insecure input, as the crate that refused it named it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    Ip(toyos_net_ip::Refusal),
    Tcp(toyos_net_tcp::Refusal),
    Udp(toyos_net_udp::Refusal),
}

/// What the shell acts on: a log line, or an address's fate for the DHCP client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A line for the log, and how many refusals of its rule it stands for beyond itself.
    Refused { refusal: Refusal, suppressed: u64 },
    Verified { iface: IfIndex, addr: Ipv4Addr },
    Conflict { iface: IfIndex, addr: Ipv4Addr, mac: MacAddr },
    /// Lost to another host after one defence and removed; every TCP connection from it ended.
    Lost { iface: IfIndex, addr: Ipv4Addr, mac: MacAddr },
    NotVerified { iface: IfIndex, addr: Ipv4Addr },
}

/// Why an active open was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectError {
    /// [ip] has no route to the peer, or no address to send from.
    Route(toyos_net_ip::Counter),
    /// The peer is a broadcast or group address of a link.
    NotUnicast,
    Tcp(toyos_net_tcp::Error),
}

/// Why a passive open was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenError {
    /// The address named is none the interface may be bound to: the rule [udp]'s bind refuses
    /// by (`udp.bind-address-not-local`).
    NotLocal,
    Tcp(toyos_net_tcp::Error),
}

/// [ip]'s MTU, as TCP's configuration takes it.
const TCP_MTU: u16 = {
    let [low, high, rest @ ..] = toyos_net_ip::MTU.to_le_bytes();
    assert!(matches!(rest, [0, 0, 0, 0, 0, 0]), "[ip]'s MTU fits a u16");
    u16::from_le_bytes([low, high])
};

/// What a waiting flow waits on: its next hop's entry, room in a full neighbour table, or room
/// on the link its route leaves by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Wait {
    Hop(IfIndex, Ipv4Addr),
    Room,
    Link(IfIndex),
}

/// A TCP segment's way out, as the hop question answered it.
struct Via {
    iface: IfIndex,
    /// `iface`'s own link address.
    source: IndividualMac,
    next_hop: Ipv4Addr,
    mac: MacAddr,
}

/// A flow of the round: one TCP connection, or one UDP sender.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Flow {
    Tcp(ConnId),
    Udp(Sender),
}

#[derive(Clone, Copy, Debug)]
struct Member {
    flow: Flow,
    /// Bytes the flow may still send in its turn; below 0, what it overdrew, repaid from its
    /// next quanta.
    deficit: i32,
}

/// What serving a flow did with a frame of credit.
struct Outcome {
    /// The length of the frame that left; none when the flow had nothing to offer, or when [ip]
    /// refused its datagram.
    frame: Option<i32>,
    /// The flow has nothing left: it leaves the round, its deficit with it.
    leaves: bool,
}

/// The quantum: the largest frame the interface sends (RFC 8290 §5.2.4).
const QUANTUM: i32 = {
    let [a, b, c, d, rest @ ..] = FRAME.to_le_bytes();
    assert!(matches!(rest, [0, 0, 0, 0]) && d < 0x80, "a frame's length fits an i32");
    i32::from_le_bytes([a, b, c, d])
};

#[derive(Default)]
struct Log {
    ip: toyos_net_ip::RefusalLog,
    tcp: toyos_net_tcp::RefusalLog,
    udp: toyos_net_udp::RefusalLog,
}

pub struct Shard {
    ip: Ip,
    /// Every interface added, in the order it was.
    ifaces: Vec<IfIndex>,
    tcp: Tcp,
    udp: Udp,
    resets: Limiter,
    log: Log,
    events: Vec<Event>,
    /// Every flow with something to send, in the order each became eligible.
    round: VecDeque<Member>,
    /// The round's head is in its turn: the quantum is added once per turn.
    turn: bool,
    frame: Box<[u8; FRAME]>,
    /// The remote addresses of the TCP flows each wait holds, each until [ip] reports a change
    /// for it, and under a link's wait the next hops of the UDP datagrams too; a flow with no
    /// route waits on the routes.
    waiting: BTreeMap<Wait, BTreeSet<Ipv4Addr>>,
    /// [ip]'s routing generation when the waiting flows last asked.
    routes: u64,
}

impl Shard {
    /// No interface: the shell adds one per link.
    pub fn new(now: Instant, config: Config) -> Result<Self, toyos_net_tcp::ConfigError> {
        let ip = Ip::new(now, config.secrets.ip);
        let tcp = Tcp::new(toyos_net_tcp::Config {
            mtu: TCP_MTU,
            receive_buffer: config.receive_buffer,
            send_buffer: config.send_buffer,
            secrets: config.secrets.tcp,
        })?;
        Ok(Self {
            routes: ip.generation(),
            ip,
            ifaces: Vec::new(),
            tcp,
            udp: Udp::new(),
            resets: Limiter::new(config.secrets.resets),
            log: Log::default(),
            events: Vec::new(),
            round: VecDeque::new(),
            turn: false,
            frame: Box::new([0; FRAME]),
            waiting: BTreeMap::new(),
        })
    }

    pub fn ip(&self) -> &Ip {
        &self.ip
    }

    /// A link's interface, down and with no address: the shell configures it.
    pub fn add_interface(&mut self, now: Instant, mac: IndividualMac, rank: Rank) -> IfIndex {
        let iface = self.ip.add_interface(now, mac, rank);
        self.ifaces.push(iface);
        iface
    }

    pub fn tcp_counters(&self) -> &toyos_net_tcp::Counters {
        self.tcp.counters()
    }

    pub fn udp_counters(&self) -> &toyos_net_udp::Counters {
        self.udp.counters()
    }

    /// Log lines and address results since the last call.
    pub fn drain_events(&mut self) -> alloc::vec::Drain<'_, Event> {
        self.events.drain(..)
    }

    // ---- the device ----

    /// A frame `iface`'s link received.
    pub fn receive(&mut self, now: Instant, iface: IfIndex, frame: &[u8]) {
        match self.ip.receive(now, iface, frame) {
            Some(Delivery::Tcp(arrival, segment)) => {
                let resets = &mut self.resets;
                let (from, to) = (arrival.packet.source(), arrival.packet.destination());
                self.tcp.receive(now, from, to, &segment, |peer| resets.allow(now, peer));
            }
            Some(Delivery::Udp(arrival, datagram)) => {
                if self.udp.receive(now, &arrival, &datagram) == Verdict::NoSocket {
                    self.ip.port_unreachable(now, &arrival);
                }
            }
            Some(Delivery::Error(error)) => match error.transport {
                Transport::Tcp { sequence } => self.tcp.icmp(now, tcp_error(&error, sequence)),
                Transport::Udp => self.udp.icmp_error(&error),
            },
            None => {}
        }
        self.settle(now);
    }

    /// A transmit opportunity with `credit`'s room on each link, each frame handed to `sink` as
    /// it is built, with the interface it leaves by. Returns how many left.
    pub fn transmit(&mut self, now: Instant, mut credit: impl Credit, mut sink: impl FnMut(IfIndex, &[u8])) -> usize {
        let credit = &mut credit;
        let room: Vec<IfIndex> = self.ifaces.iter().copied().filter(|&iface| credit.room(iface)).collect();
        for iface in room {
            for hop in self.waiting.remove(&Wait::Link(iface)).into_iter().flatten() {
                self.tcp.wake(hop);
                self.udp.wake(iface, hop);
            }
        }
        self.settle(now);
        let mut spent = 0usize;
        while self.ifaces.iter().any(|&iface| credit.room(iface)) {
            let own = self.ip.transmit(now, &mut *credit, &mut sink);
            spent = spent.saturating_add(own);
            let sent = own > 0
                || self.tcp_frame(now, credit, &mut sink, |tcp, hop, out| tcp.transmit_owed(now, 1, hop, out)).1.is_some()
                || self.round_frame(now, credit, &mut sink);
            if sent && own == 0 {
                spent = spent.saturating_add(1);
            }
            if sent {
                continue;
            }
            // A flow that waits has queued the request it waits on.
            let requests = self.ip.transmit(now, &mut *credit, &mut sink);
            spent = spent.saturating_add(requests);
            if requests == 0 {
                break;
            }
        }
        self.settle(now);
        spent
    }

    /// One frame of the round's. The head's turn adds the quantum to its
    /// deficit once, however many opportunities the turn spans; the flow is served while the
    /// deficit is above 0, each frame's length charged as it leaves, and then goes to the tail
    /// keeping its deficit. A flow with nothing left leaves the round, its deficit with it: a UDP
    /// sender as its last datagram is handed out, a TCP connection once served with nothing due.
    fn round_frame(&mut self, now: Instant, credit: &mut impl Credit, sink: &mut impl FnMut(IfIndex, &[u8])) -> bool {
        loop {
            let Some(head) = self.round.front_mut() else { return false };
            if !core::mem::replace(&mut self.turn, true) {
                head.deficit = head.deficit.saturating_add(QUANTUM);
            }
            let flow = head.flow;
            let Outcome { frame, leaves } = match flow {
                Flow::Tcp(id) => match self.tcp_frame(now, credit, sink, |tcp, hop, out| tcp.serve(now, id, hop, out)) {
                    (Served::Sent, Some(len)) => Outcome { frame: Some(len), leaves: false },
                    (Served::Done, None) => Outcome { frame: None, leaves: true },
                    (Served::Sent, None) | (Served::Done, Some(_)) => unreachable!("[tcp] answers Sent exactly when the sink framed a segment"),
                },
                Flow::Udp(sender) => self.udp_frame(now, sender, credit, sink),
            };
            if let (Some(len), Some(head)) = (frame, self.round.front_mut()) {
                head.deficit = head.deficit.saturating_sub(len);
            }
            if leaves {
                self.round.pop_front();
                self.turn = false;
            } else if frame.is_some() && self.round.front().is_some_and(|head| head.deficit <= 0) {
                self.round.rotate_left(1);
                self.turn = false;
            }
            if frame.is_some() {
                return true;
            }
        }
    }

    /// One TCP segment, if `send` gives one whose next hop is known: `send` hands [tcp] the hop
    /// question and the sink that frames the segment. Returns what `send` did and the length of
    /// the frame that left.
    fn tcp_frame<R>(
        &mut self,
        now: Instant,
        credit: &mut impl Credit,
        sink: &mut impl FnMut(IfIndex, &[u8]),
        send: impl FnOnce(&mut Tcp, &mut dyn FnMut(&Tuple) -> Hop<Via>, &mut dyn FnMut(&Outgoing<'_>, Via)) -> R,
    ) -> (R, Option<i32>) {
        let Self { ip, tcp, frame, waiting, .. } = self;
        let mut built = None;
        let room = &*credit;
        let done = send(tcp, &mut |tuple| hop(ip, now, room, tuple, waiting), &mut |out, via| {
            let Some(bytes) = tcp_datagram(out, via.source, via.mac, frame) else { unreachable!("TCP's MTU, [tcp]'s bounds and [ip]'s addresses fit every segment in a frame") };
            sink(via.iface, bytes);
            built = Some((via.iface, via.next_hop, out.source, charge(bytes)));
        });
        let Some((iface, next_hop, source, len)) = built else { return (done, None) };
        credit.spend(iface);
        // The send the neighbour machine counts: STALE moves to DELAY (RFC 4861 §7.3.3).
        ip.resolve(now, iface, next_hop, source);
        (done, Some(len))
    }

    /// One datagram of `sender`'s, framed by [ip]: the oldest whose next hop's link address is
    /// known, or that needs none. One [ip] refuses as it leaves makes no frame.
    fn udp_frame(&mut self, now: Instant, sender: Sender, credit: &mut impl Credit, sink: &mut impl FnMut(IfIndex, &[u8])) -> Outcome {
        let Self { ip, udp, frame, waiting, .. } = self;
        let mut sent = None;
        let room = &*credit;
        let served = udp.serve(sender, |out| match ip.send_udp(now, room, out, frame) {
            Ok(Sent::Frame { len, iface }) => {
                sent = frame.get(..len).map(|bytes| {
                    sink(iface, bytes);
                    (iface, charge(bytes))
                });
                Offer::Taken
            }
            Ok(Sent::Pending { iface, next_hop }) => Offer::Waits { iface, hop: next_hop },
            Ok(Sent::NoRoom { iface, hop }) => {
                waiting.entry(Wait::Link(iface)).or_default().insert(hop);
                Offer::Waits { iface, hop }
            }
            // Counted by [ip], which tells the datagram's flow.
            Err(_) => Offer::Taken,
        });
        if let Some((iface, _)) = sent {
            credit.spend(iface);
        }
        Outcome { frame: sent.map(|(_, len)| len), leaves: served != UdpServed::More }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.ip.next_deadline().into_iter().chain(self.tcp.next_deadline()).min()
    }

    /// Every deadline at or before `now`; the work they make due waits for [`Self::transmit`].
    pub fn fire(&mut self, now: Instant) {
        self.ip.fire(now);
        self.tcp.fire(now);
        self.settle(now);
    }

    /// Routes what each crate reported to the one that acts on it, takes the flows freed or closed
    /// out of the round, and puts the flows that became eligible at the round's tail. Every call that can make a flow eligible ends here, so the
    /// round holds flows in the order they became eligible.
    fn settle(&mut self, now: Instant) {
        let Self { ip, tcp, udp, log, events, waiting, .. } = self;
        for event in tcp.drain_events() {
            match event {
                toyos_net_tcp::Event::Refused(r) => {
                    if let Some(suppressed) = log.tcp.admit(now, r.rule) {
                        events.push(Event::Refused { refusal: Refusal::Tcp(r), suppressed });
                    }
                }
                toyos_net_tcp::Event::Reachable { local, remote } => ip.advise(now, remote, Source::Bound(local), Advice::Confirmed),
                toyos_net_tcp::Event::Reverify { local, remote } => ip.advise(now, remote, Source::Bound(local), Advice::Reverify),
            }
        }
        for event in ip.drain_events() {
            match event {
                toyos_net_ip::Event::Refused(r) => {
                    if let Some(suppressed) = log.ip.admit(now, r.rule) {
                        events.push(Event::Refused { refusal: Refusal::Ip(r), suppressed });
                    }
                }
                toyos_net_ip::Event::Unreachable(flow) => udp.unreachable(&flow),
                toyos_net_ip::Event::Resolved { iface, next_hop } => {
                    wake(tcp, waiting, Wait::Hop(iface, next_hop));
                    udp.wake(iface, next_hop);
                }
                toyos_net_ip::Event::Failed { iface, next_hop } => {
                    wake(tcp, waiting, Wait::Hop(iface, next_hop));
                    udp.fail(iface, next_hop);
                }
                // No datagram waits on a failed next hop: the failure dropped them.
                toyos_net_ip::Event::Cleared { iface, next_hop } => wake(tcp, waiting, Wait::Hop(iface, next_hop)),
                toyos_net_ip::Event::Room { .. } => wake(tcp, waiting, Wait::Room),
                toyos_net_ip::Event::Verified { iface, addr } => events.push(Event::Verified { iface, addr }),
                toyos_net_ip::Event::Conflict { iface, addr, mac } => events.push(Event::Conflict { iface, addr, mac }),
                toyos_net_ip::Event::Lost { iface, addr, mac } => {
                    tcp.lose_address(addr);
                    events.push(Event::Lost { iface, addr, mac });
                }
                toyos_net_ip::Event::NotVerified { iface, addr } => events.push(Event::NotVerified { iface, addr }),
            }
        }
        for r in udp.drain_refusals() {
            if let Some(suppressed) = log.udp.admit(now, r.rule) {
                events.push(Event::Refused { refusal: Refusal::Udp(r), suppressed });
            }
        }
        if ip.generation() != self.routes {
            self.routes = ip.generation();
            waiting.clear();
            tcp.wake_all();
            udp.wake_all();
        }
        let head = self.round.front().map(|m| m.flow);
        let gone: BTreeSet<Flow> = tcp.drain_gone().map(Flow::Tcp).chain(udp.drain_gone().map(Flow::Udp)).collect();
        if !gone.is_empty() {
            self.round.retain(|m| !gone.contains(&m.flow));
        }
        if self.round.front().map(|m| m.flow) != head {
            self.turn = false;
        }
        let tcp = tcp.drain_eligible().map(Flow::Tcp);
        let udp = udp.drain_eligible().map(Flow::Udp);
        self.round.extend(tcp.chain(udp).map(|flow| Member { flow, deficit: 0 }));
    }

    // ---- configuration, the shell's ----

    pub fn link_up(&mut self, now: Instant, iface: IfIndex) -> Result<(), toyos_net_ip::Counter> {
        let up = self.ip.link_up(now, iface);
        self.settle(now);
        up
    }

    pub fn link_down(&mut self, now: Instant, iface: IfIndex) -> Result<(), toyos_net_ip::Counter> {
        let down = self.ip.link_down(now, iface);
        self.settle(now);
        down
    }

    /// Added tentative: conflict detection runs before it is used ([`Event::Verified`]).
    pub fn add_address(&mut self, now: Instant, iface: IfIndex, addr: Ipv4Addr, prefix_len: u8) -> Result<(), toyos_net_ip::Counter> {
        let added = self.ip.add_address(now, iface, addr, prefix_len);
        self.settle(now);
        added
    }

    pub fn set_gateways(&mut self, now: Instant, iface: IfIndex, gateways: &[Ipv4Addr]) -> Result<(), toyos_net_ip::Counter> {
        let set = self.ip.set_gateways(now, iface, gateways);
        self.settle(now);
        set
    }

    /// Takes one of the interface's addresses away; [ip] withdraws the gateways no usable prefix
    /// holds any more, and every TCP connection from it ends (`Failure::AddressGone`).
    pub fn remove_address(&mut self, now: Instant, iface: IfIndex, addr: Ipv4Addr) -> Result<(), toyos_net_ip::Counter> {
        let removed = self.ip.remove_address(now, iface, addr);
        if removed.is_ok() {
            self.tcp.lose_address(addr);
        }
        self.settle(now);
        removed
    }

    /// Joins `group` on `iface`: frames to its link address are taken from here on, and [ip]
    /// reports the membership whenever the link can carry the report. Nothing to settle: a join
    /// makes no flow eligible and no event.
    pub fn join(&mut self, now: Instant, iface: IfIndex, group: MulticastAddr) -> Result<(), toyos_net_ip::Counter> {
        self.ip.join(now, iface, group)
    }

    // ---- TCP ----

    /// An active open from the source [ip]'s route lookup picks (RFC 9293 MUST-44), to a peer the
    /// route reaches through one neighbour.
    pub fn connect(&mut self, now: Instant, port: Option<Port>, remote: Endpoint) -> Result<ConnId, ConnectError> {
        let route = self.ip.route(remote.addr, Source::Any, None).map_err(ConnectError::Route)?;
        let NextHop::Neighbour(_) = route.next_hop else { return Err(ConnectError::NotUnicast) };
        let connected = self.tcp.connect(now, route.source, port, remote).map_err(ConnectError::Tcp);
        self.settle(now);
        connected
    }

    /// `addr` is the local address to listen on, UNSPECIFIED for any, and otherwise one [ip]
    /// holds assigned or announcing, as a datagram socket's is; port 0 takes `random`'s draws.
    pub fn listen(&mut self, addr: Ipv4Addr, port: Option<Port>, random: impl FnMut() -> u16) -> Result<ListenerId, ListenError> {
        if !addr.is_unspecified() && !self.ip.is_assigned(addr) {
            return Err(ListenError::NotLocal);
        }
        self.tcp.listen(addr, port, random).map_err(ListenError::Tcp)
    }

    pub fn accept(&mut self, id: ListenerId) -> Result<Option<ConnId>, toyos_net_tcp::Error> {
        self.tcp.accept(id)
    }

    pub fn listener_port(&mut self, id: ListenerId) -> Result<Port, toyos_net_tcp::Error> {
        self.tcp.listener_port(id)
    }

    /// [`Tcp::set_listener_options`]: what a connection whose SYN arrives from here on starts
    /// with.
    pub fn set_listener_options(&mut self, id: ListenerId, options: toyos_net_tcp::Options) -> Result<(), toyos_net_tcp::Error> {
        self.tcp.set_listener_options(id, options)
    }

    /// [`Tcp::ready`].
    pub fn ready(&mut self, id: ListenerId) -> Result<usize, toyos_net_tcp::Error> {
        self.tcp.ready(id)
    }

    /// [`Tcp::close_listener`]. The resets it owes leave outside the round.
    pub fn close_listener(&mut self, now: Instant, id: ListenerId) -> Result<(), toyos_net_tcp::Error> {
        self.tcp.close_listener(now, id)
    }

    /// [`Tcp::orphans`].
    pub fn orphans(&self) -> usize {
        self.tcp.orphans()
    }

    pub fn send(&mut self, now: Instant, id: ConnId, data: &[u8]) -> Result<usize, toyos_net_tcp::Error> {
        let done = self.tcp.send(now, id, data);
        self.settle(now);
        done
    }

    pub fn recv(&mut self, now: Instant, id: ConnId, out: &mut [u8]) -> Result<Received, toyos_net_tcp::Error> {
        let done = self.tcp.recv(now, id, out);
        self.settle(now);
        done
    }

    /// [`Tcp::lend`].
    pub fn lend(&mut self, id: ConnId, rx: Box<dyn Lent>, tx: Box<dyn Lent>) -> Result<(), toyos_net_tcp::Error> {
        self.tcp.lend(id, rx, tx)
    }

    /// [`Tcp::consume`].
    pub fn consume(&mut self, now: Instant, id: ConnId, n: usize) -> Result<usize, toyos_net_tcp::Error> {
        let done = self.tcp.consume(now, id, n);
        self.settle(now);
        done
    }

    /// [`Tcp::queue`].
    pub fn queue(&mut self, now: Instant, id: ConnId, n: usize) -> Result<usize, toyos_net_tcp::Error> {
        let done = self.tcp.queue(now, id, n);
        self.settle(now);
        done
    }

    pub fn set_options(&mut self, now: Instant, id: ConnId, options: toyos_net_tcp::Options) -> Result<(), toyos_net_tcp::Error> {
        let done = self.tcp.set_options(now, id, options);
        self.settle(now);
        done
    }

    /// [`Tcp::options`].
    pub fn options(&mut self, id: ConnId) -> Result<toyos_net_tcp::Options, toyos_net_tcp::Error> {
        self.tcp.options(id)
    }

    pub fn shutdown_write(&mut self, now: Instant, id: ConnId) -> Result<(), toyos_net_tcp::Error> {
        let done = self.tcp.shutdown_write(now, id);
        self.settle(now);
        done
    }

    pub fn close(&mut self, now: Instant, id: ConnId) -> Result<(), toyos_net_tcp::Error> {
        let done = self.tcp.close(now, id);
        self.settle(now);
        done
    }

    pub fn abort(&mut self, now: Instant, id: ConnId) -> Result<(), toyos_net_tcp::Error> {
        let done = self.tcp.abort(now, id);
        self.settle(now);
        done
    }

    pub fn status(&mut self, id: ConnId) -> Result<Status, toyos_net_tcp::Error> {
        self.tcp.status(id)
    }

    pub fn tuple(&mut self, id: ConnId) -> Result<Tuple, toyos_net_tcp::Error> {
        self.tcp.tuple(id)
    }

    // ---- UDP ----

    /// Binds to `addr`, 0.0.0.0 meaning any, and `port`, or an ephemeral one drawn by `draw`.
    pub fn bind(&mut self, addr: Ipv4Addr, port: Option<Port>, draw: impl FnOnce() -> u32) -> Result<SocketId, toyos_net_udp::Error> {
        self.udp.bind(&self.ip, addr, port, draw)
    }

    /// Connects to one unicast peer: the local address is fixed now.
    pub fn udp_connect(&mut self, now: Instant, id: SocketId, peer: Ipv4Addr, port: u16) -> Result<(), toyos_net_udp::Error> {
        let connected = self.udp.connect(&mut self.ip, id, peer, port);
        self.settle(now);
        connected
    }

    /// Queues a datagram; accepted means queued, not sent.
    pub fn send_to(&mut self, now: Instant, id: SocketId, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), toyos_net_udp::Error> {
        let queued = self.udp.send_to(&mut self.ip, id, destination, port, payload);
        self.settle(now);
        queued
    }

    pub fn recv_from(&mut self, id: SocketId, out: &mut [u8]) -> Result<Option<toyos_net_udp::Received>, toyos_net_udp::Error> {
        self.udp.recv(id, out)
    }

    /// [`Udp::readable`](toyos_net_udp::Udp::readable).
    pub fn udp_readable(&mut self, id: SocketId) -> Result<bool, toyos_net_udp::Error> {
        self.udp.readable(id)
    }

    /// The peer a connected socket's datagrams go to and come from, and the address it sends
    /// from, which its connect fixed.
    pub fn udp_peer(&mut self, id: SocketId) -> Result<Option<UdpPeer>, toyos_net_udp::Error> {
        self.udp.binding(id).map(|(binding, _)| match binding {
            toyos_net_udp::Binding::Connected { local, peer, peer_port } => Some(UdpPeer { addr: peer, port: peer_port, local }),
            toyos_net_udp::Binding::Any | toyos_net_udp::Binding::Specific(_) => None,
        })
    }

    /// Queues a datagram for a connected socket's peer; accepted means queued, not sent.
    pub fn udp_send(&mut self, now: Instant, id: SocketId, payload: &[u8]) -> Result<(), toyos_net_udp::Error> {
        let queued = self.udp.send(&mut self.ip, id, payload);
        self.settle(now);
        queued
    }

    /// [`Udp::broadcast`](toyos_net_udp::Udp::broadcast).
    pub fn udp_broadcast(&mut self, id: SocketId) -> Result<bool, toyos_net_udp::Error> {
        self.udp.broadcast(id)
    }

    /// The port a socket holds.
    pub fn udp_port(&mut self, id: SocketId) -> Result<Port, toyos_net_udp::Error> {
        self.udp.binding(id).map(|(_, port)| port)
    }

    /// The TTL of a socket's datagrams to one host, and of those to a group.
    pub fn udp_set_ttl(&mut self, id: SocketId, unicast: Ttl, multicast: Ttl) -> Result<(), toyos_net_udp::Error> {
        self.udp.set_ttl(id, unicast, multicast)
    }

    /// Whether a socket's datagrams may go to a broadcast address.
    pub fn udp_set_broadcast(&mut self, id: SocketId, permitted: bool) -> Result<(), toyos_net_udp::Error> {
        self.udp.set_broadcast(id, permitted)
    }

    /// Closes a socket: its port is free at once, and what it had accepted leaves as [udp]'s
    /// closed sender, in the round from this call.
    pub fn udp_close(&mut self, now: Instant, id: SocketId) -> Result<(), toyos_net_udp::Error> {
        let closed = self.udp.close(id);
        self.settle(now);
        closed
    }

    /// The DHCP client's socket, on port 68: the one socket that may send from 0.0.0.0 and name
    /// its source, and the one the acquisition exception delivers to (RFC 2131 §4.1).
    pub fn acquisition(&mut self) -> Result<SocketId, toyos_net_udp::Error> {
        let id = self.udp.bind(&self.ip, Ipv4Addr::UNSPECIFIED, Port::new(68), || 0)?;
        self.udp.set_acquisition(id)?;
        self.udp.set_broadcast(id, true)?;
        Ok(id)
    }

    /// [`Udp::send_via`](toyos_net_udp::Udp::send_via): a datagram that leaves by `iface` alone.
    pub fn send_via(&mut self, now: Instant, id: SocketId, iface: IfIndex, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), toyos_net_udp::Error> {
        let queued = self.udp.send_via(&mut self.ip, id, iface, destination, port, payload);
        self.settle(now);
        queued
    }

    /// The acquisition socket's send, naming its source, 0.0.0.0 to the limited broadcast or an
    /// assigned address, and the interface whose address it acquires.
    pub fn send_from(&mut self, now: Instant, id: SocketId, origin: Origin, destination: Ipv4Addr, port: u16, payload: &[u8]) -> Result<(), toyos_net_udp::Error> {
        let queued = self.udp.send_from(&mut self.ip, id, origin, destination, port, payload);
        self.settle(now);
        queued
    }
}

/// Whether `tuple`'s next segment can be built now, peeking its next hop's entry, which does not
/// move: only a next hop with no entry is resolved, which queues its request.
/// No route is a local destination unreachable (RFC 1122 §3.3.1.1). A flow told to wait is
/// recorded under what it waits on; one with no route waits on the routes.
fn hop(ip: &mut Ip, now: Instant, credit: &impl Credit, tuple: &Tuple, waiting: &mut BTreeMap<Wait, BTreeSet<Ipv4Addr>>) -> Hop<Via> {
    let remote = tuple.remote.addr;
    let Ok(route) = ip.route(remote, Source::Bound(tuple.local.addr), None) else { return Hop::Unreachable };
    let NextHop::Neighbour(next_hop) = route.next_hop else { return Hop::Unreachable };
    let iface = route.iface;
    let Some(source) = ip.mac(iface) else { unreachable!("a route names an interface [ip] holds") };
    if !credit.room(iface) {
        waiting.entry(Wait::Link(iface)).or_default().insert(remote);
        return Hop::Pending;
    }
    let (answer, wait) = match ip.neighbour(iface, next_hop) {
        Some(entry) => match entry.mac() {
            Some(mac) => (Hop::Ready(Via { iface, source, next_hop, mac }), None),
            None if matches!(entry, Nud::Failed) => (Hop::Unreachable, Some(Wait::Hop(iface, next_hop))),
            None => (Hop::Pending, Some(Wait::Hop(iface, next_hop))),
        },
        None => match ip.resolve(now, iface, next_hop, route.source) {
            Resolution::Resolved(mac) => (Hop::Ready(Via { iface, source, next_hop, mac }), None),
            Resolution::Pending => (Hop::Pending, Some(Wait::Hop(iface, next_hop))),
            Resolution::Failed => (Hop::Unreachable, Some(Wait::Room)),
        },
    };
    if let Some(wait) = wait {
        waiting.entry(wait).or_default().insert(remote);
    }
    answer
}

/// What a frame costs its flow's deficit: its length, which `FRAME` bounds and `QUANTUM` fits.
fn charge(frame: &[u8]) -> i32 {
    let Ok(len) = i32::try_from(frame.len()) else { unreachable!("a frame is at most FRAME bytes") };
    len
}

/// The TCP flows waiting on `wait` ask again at the next opportunity.
fn wake(tcp: &mut Tcp, waiting: &mut BTreeMap<Wait, BTreeSet<Ipv4Addr>>, wait: Wait) {
    for remote in waiting.remove(&wait).into_iter().flatten() {
        tcp.wake(remote);
    }
}

/// A segment in its IPv4 datagram and Ethernet frame to `to`: DF, TTL 64, DSCP and ECN 0.
/// `None` is a segment longer than a frame carries, which TCP's MTU rules out, or
/// from a source no datagram may carry, which [ip]'s addresses rule out.
fn tcp_datagram<'f>(out: &Outgoing<'_>, mac: IndividualMac, to: MacAddr, frame: &'f mut [u8; FRAME]) -> Option<&'f [u8]> {
    let datagram = Ipv4Builder {
        source: Ipv4Source::new(out.source).ok()?,
        destination: out.destination,
        ttl: Ttl::DEFAULT,
        traffic_class: TrafficClass::ZERO,
        options: &[],
        payload: out.segment,
    };
    FrameBuilder { destination: to, source: mac }.emit(&datagram, frame).ok()
}

/// An error [ip] validated against a TCP segment of ours, in TCP's terms.
fn tcp_error(error: &TransportError, sequence: u32) -> IcmpError {
    let kind = match error.kind {
        ErrorKind::Unreachable(code) => IcmpKind::Unreachable(code),
        ErrorKind::FragmentationNeeded { next_hop_mtu, quoted_length } => IcmpKind::PacketTooBig { next_hop_mtu, quoted_length },
        ErrorKind::TimeExceeded(_) => IcmpKind::TimeExceeded,
        ErrorKind::ParameterProblem { .. } => IcmpKind::ParameterProblem,
    };
    let flow = error.flow;
    IcmpError {
        local: Endpoint { addr: flow.source, port: flow.source_port },
        remote: Endpoint { addr: flow.destination, port: flow.destination_port },
        sequence: Seq::new(sequence),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
    const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);

    /// A shard whose address is verified, its device then offering nothing.
    fn verified() -> (Shard, Instant) {
        let secrets = Secrets {
            ip: [1; 16],
            resets: [2; 16],
            tcp: toyos_net_tcp::Secrets { isn: [3; 16], timestamp: [4; 16], port_offset: [5; 16], port_index: [6; 16], port_table: [0; 16] },
        };
        let mac = IndividualMac::new(MacAddr([2, 0, 0, 0, 0, 0x0a])).unwrap();
        let mut now = Instant::from_millis(3_600_000);
        let mut shard = Shard::new(now, Config { receive_buffer: 65_535, send_buffer: 65_535, secrets }).unwrap();
        let iface = shard.add_interface(now, mac, Rank(0));
        shard.link_up(now, iface).unwrap();
        shard.add_address(now, iface, A, 24).unwrap();
        while !shard.drain_events().any(|e| matches!(e, Event::Verified { addr: A, .. })) {
            shard.transmit(now, usize::MAX, |_, _| {});
            now = shard.next_deadline().expect("conflict detection is still running");
            shard.fire(now);
        }
        (shard, now)
    }

    // No id: a connection freed while in the round leaves it at once, while the device offers
    // nothing: the round holds nothing without a bound.
    #[test]
    fn a_connection_freed_in_the_round_leaves_it() {
        let (mut shard, now) = verified();
        let remote = Endpoint { addr: B, port: Port::new(80).unwrap() };
        for _ in 0..1_000 {
            let id = shard.connect(now, None, remote).unwrap();
            assert_eq!(shard.round.len(), 1, "its SYN put it in the round");
            shard.abort(now, id).unwrap();
            assert!(shard.round.is_empty());
        }
    }

    // No id: a sender all of whose datagrams wait for a next hop is out of the round, and a next
    // hop that resolves puts back the senders that waited for it and no other: a waiting datagram
    // costs one question per change of its own next hop.
    #[test]
    fn a_resolved_next_hop_wakes_the_senders_that_waited_for_it_and_no_other() {
        const C: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);
        const D: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);
        let (mut shard, now) = verified();
        shard.transmit(now, usize::MAX, |_, _| {});
        let port = |n| Port::new(n).unwrap();
        let (to_c, to_d) = (shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5000)), || 0).unwrap(), shard.bind(Ipv4Addr::UNSPECIFIED, Some(port(5001)), || 0).unwrap());
        shard.send_to(now, to_c, C, 9, b"c").unwrap();
        shard.send_to(now, to_d, D, 9, b"d").unwrap();
        assert_eq!(shard.round.len(), 2);
        assert_eq!(shard.transmit(now, usize::MAX, |_, _| {}), 2, "a request for each");
        assert!(shard.round.is_empty(), "what waits is not served");

        let mut reply = [[2, 0, 0, 0, 0, 0x0a], [2, 0, 0, 0, 0, 0x0c]].concat();
        reply.extend_from_slice(&[0x08, 0x06, 0, 1, 8, 0, 6, 4, 0, 2, 2, 0, 0, 0, 0, 0x0c]);
        reply.extend_from_slice(&C.octets());
        reply.extend_from_slice(&[2, 0, 0, 0, 0, 0x0a]);
        reply.extend_from_slice(&A.octets());
        shard.receive(now, shard.ifaces[0], &reply);
        assert_eq!(shard.round.iter().map(|m| m.flow).collect::<Vec<_>>(), [Flow::Udp(Sender::Socket(to_c))]);
    }

    // No id: the head freed in its turn takes the turn with it, so the next flow's turn adds its
    // quantum; a flow behind it leaves the turn as it was.
    #[test]
    fn the_head_freed_in_its_turn_ends_the_turn() {
        let (mut shard, now) = verified();
        let remote = Endpoint { addr: B, port: Port::new(80).unwrap() };
        let (head, next) = (shard.connect(now, None, remote).unwrap(), shard.connect(now, None, remote).unwrap());
        shard.turn = true;
        shard.abort(now, head).unwrap();
        assert_eq!(shard.round.iter().map(|m| m.flow).collect::<Vec<_>>(), [Flow::Tcp(next)]);
        assert!(!shard.turn);
        let tail = shard.connect(now, None, remote).unwrap();
        shard.turn = true;
        shard.abort(now, tail).unwrap();
        assert!(shard.turn, "a flow behind the head takes no turn with it");
    }
}
