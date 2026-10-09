//! Listeners and the node's places, against peers the test scripts, at 192.0.2.2 unless a test
//! puts one elsewhere, and pipe ends it fakes. No scenario ids: the specifications' listening
//! scenarios are `toyos-net-tcp`'s; these are what the node does between [tcp]'s queues, an
//! owner's wakes and its accepts.
//!
//! What is not ours: every segment the node emits is read by `etherparse` as it leaves
//! ([`Net::hears`]), its IPv4 and TCP checksums and lengths that crate's sums, and every segment
//! a peer sends is built by it. The numbers each test asserts are RFC 9293's for LISTEN and
//! SYN-RECEIVED, cited where asserted. Two tests are recorded failures of the stack this one
//! replaces, played the same way and read the other way: each says which. The peers are ours:
//! each says only what a test tells it to, and nothing answers the node by itself but ARP.

mod common;

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::time::Duration;

use common::{arp, terms, Segment, Wire, A, ELSEWHERE, MAC, MAC_R, R};
use etherparse::{ArpOperation, LinkSlice, NetSlice, PacketBuilder, SlicedPacket, TcpOptionElement, TransportSlice};
use toyos_net_node::{AcceptRefused, Accepted, ConnectRefused, FromClient, ListenRefused, ListenerId, Node, PipeEnd, Pipes, ReadRefusal, Refused, StreamEvent, StreamId, ToClient, Wake, WriteRefusal};
use toyos_net_tcp::{limits, Counter, Endpoint};
use toyos_net_wire::{Instant, Port};

const ANY: Ipv4Addr = Ipv4Addr::UNSPECIFIED;

/// The addresses peers are at.
const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
const C: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);
const D: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 4);
const E: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 5);

/// The MAC the frames of a peer at `addr` come from: 192.0.2.2's is `common::MAC_B`.
fn mac(addr: Ipv4Addr) -> [u8; 6] {
    let [.., last] = addr.octets();
    [2, 0, 0, 0, 0, 9 + last]
}

/// A peer: its address and its port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Peer {
    addr: Ipv4Addr,
    port: u16,
}

impl Peer {
    fn endpoint(self) -> Endpoint {
        Endpoint { addr: self.addr, port: Port::new(self.port).unwrap() }
    }
}

/// The port the tests listen on, and another.
const SSH: u16 = 22;
const TELNET: u16 = 23;
const P1: Peer = Peer { addr: B, port: 40_001 };
const P2: Peer = Peer { addr: B, port: 40_002 };
const P3: Peer = Peer { addr: B, port: 40_003 };
/// Where the tests connect to: nothing answers there.
const WEB: Peer = Peer { addr: B, port: 80 };
/// Every peer's initial sequence number.
const ISS: u32 = 5000;
/// [tcp]'s send buffer, as `common::Wire` configures it.
const SEND_BUFFER: usize = 65_535;

const SYN: u8 = 1;
const FIN: u8 = 2;
const RST: u8 = 4;

// ---- an owner's wake pipe ----

#[derive(Debug, Default)]
struct Notified {
    /// The wakes written and not refused.
    wakes: usize,
    /// What the pipe answers a wake instead of taking it.
    refusal: Option<WriteRefusal>,
    dropped: bool,
}

type Owner = Rc<RefCell<Notified>>;

struct WakeEnd(Owner);

impl Wake for WakeEnd {
    fn wake(&mut self) -> Result<(), WriteRefusal> {
        let mut notified = self.0.borrow_mut();
        if let Some(refusal) = notified.refusal {
            return Err(refusal);
        }
        notified.wakes += 1;
        Ok(())
    }
}

impl Drop for WakeEnd {
    fn drop(&mut self) {
        self.0.borrow_mut().dropped = true;
    }
}

fn wakes(owner: &Owner) -> usize {
    owner.borrow().wakes
}

// ---- a client's pipes ----

#[derive(Debug, Default)]
struct Ends {
    /// What the node wrote: the pipe takes everything.
    inbox: Vec<u8>,
    /// What the client wrote and the node has not read.
    outbox: VecDeque<u8>,
    /// How many of the two ends the node dropped.
    dropped: usize,
}

type Client = Rc<RefCell<Ends>>;

struct WriteEnd(Client);
struct ReadEnd(Client);

impl ToClient for WriteEnd {
    fn write(&mut self, bytes: &[u8]) -> Result<usize, WriteRefusal> {
        self.0.borrow_mut().inbox.extend_from_slice(bytes);
        Ok(bytes.len())
    }
}

impl FromClient for ReadEnd {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, ReadRefusal> {
        let mut ends = self.0.borrow_mut();
        let read = out.len().min(ends.outbox.len());
        if read == 0 {
            return Err(ReadRefusal::Empty);
        }
        for slot in &mut out[..read] {
            *slot = ends.outbox.pop_front().unwrap();
        }
        Ok(read)
    }
}

impl Drop for WriteEnd {
    fn drop(&mut self) {
        self.0.borrow_mut().dropped += 1;
    }
}

impl Drop for ReadEnd {
    fn drop(&mut self) {
        self.0.borrow_mut().dropped += 1;
    }
}

fn client() -> (Client, Pipes) {
    let client = Client::default();
    let pipes = Pipes { to_client: Box::new(WriteEnd(client.clone())), from_client: Box::new(ReadEnd(client.clone())) };
    (client, pipes)
}

// ---- the peers ----

/// A segment from the peer to the node's port `to` in its frame, built by `etherparse`: window
/// 65,535, an MSS option on a SYN, PSH with text.
fn frame(from: Peer, to: u16, seq: u32, ack: Option<u32>, flags: u8, text: &[u8]) -> Vec<u8> {
    let mut step = PacketBuilder::ethernet2(mac(from.addr), MAC).ipv4(from.addr.octets(), A.octets(), 64).tcp(from.port, to, seq, 65_535);
    if flags & SYN != 0 {
        step = step.syn().options(&[TcpOptionElement::MaximumSegmentSize(1460)]).unwrap();
    }
    if let Some(ack) = ack {
        step = step.ack(ack);
    }
    if flags & FIN != 0 {
        step = step.fin();
    }
    if flags & RST != 0 {
        step = step.rst();
    }
    if !text.is_empty() {
        step = step.psh();
    }
    let mut frame = Vec::new();
    step.write(&mut frame, text).unwrap();
    frame
}

fn v4(bytes: &[u8]) -> Ipv4Addr {
    Ipv4Addr::from(<[u8; 4]>::try_from(bytes).expect("four bytes"))
}

fn draw(draws: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        *draws = draws.wrapping_add(1);
        *draws
    }
}

// ---- the node between them ----

struct Net {
    node: Node,
    now: Instant,
    draws: u32,
    /// The addresses a peer is at: 192.0.2.2, and each one a SYN has come from.
    at: BTreeSet<Ipv4Addr>,
    /// Every TCP segment the node emitted, in order.
    heard: Vec<Segment>,
}

impl Net {
    /// The node holding its lease of 192.0.2.1/24 for an hour, with `common::PLACES` places.
    fn new() -> Self {
        let Wire { node, now, .. } = Wire::leased(&terms(3_600, Some(R)));
        Self { node, now, draws: 0x7e00_0000, at: BTreeSet::from([B]), heard: Vec::new() }
    }

    /// Offers the node all the credit it wants until it sends nothing more, its ARP requests
    /// for the peers' addresses and the router's answered.
    fn pump(&mut self) {
        for _ in 0..100_000 {
            let mut frames = Vec::new();
            self.node.transmit(self.now, usize::MAX, |frame| frames.push(frame.to_vec()), draw(&mut self.draws));
            if frames.is_empty() {
                return;
            }
            for frame in &frames {
                if let Some(answer) = self.hears(frame) {
                    self.node.receive(self.now, &answer, draw(&mut self.draws));
                }
            }
        }
        panic!("100,000 opportunities and the node still sends");
    }

    /// The outside reading of a frame the node emitted, and the ARP answer it gets if it asks
    /// for one. Ethernet II from the node's MAC; then ARP, a datagram of the DHCP client's, or
    /// a TCP segment to an address a peer is at, as `common::segment` reads it. A segment to any
    /// other address is the node's mistake.
    fn hears(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        if let Some(segment) = common::segment(frame) {
            assert!(self.at.contains(&segment.to), "a segment to {}, where no peer is", segment.to);
            assert_eq!(segment.to_mac, mac(segment.to), "to the peer's MAC");
            self.heard.push(segment);
            return None;
        }
        let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
        let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
        assert_eq!(ethernet.source(), MAC, "from the node's MAC");
        match (&packet.net, &packet.transport) {
            (Some(NetSlice::Arp(asked)), None) => {
                let asks = asked.operation() == ArpOperation::REQUEST && v4(asked.sender_protocol_addr()) == A;
                match v4(asked.target_protocol_addr()) {
                    target if asks && self.at.contains(&target) => Some(arp(MAC, false, mac(target), target, A)),
                    target if asks && target == R => Some(arp(MAC, false, MAC_R, R, A)),
                    _ => None,
                }
            }
            (Some(NetSlice::Ipv4(_)), Some(TransportSlice::Udp(_))) => None,
            other => panic!("neither ARP nor UDP: {other:?}"),
        }
    }

    fn deliver(&mut self, frame: &[u8]) {
        self.node.receive(self.now, frame, draw(&mut self.draws));
        self.pump();
    }

    /// Fires every deadline of the next `span`, each at its own instant, and ends `span` on.
    fn run(&mut self, span: Duration) {
        let end = self.now.after(span);
        for _ in 0..100_000 {
            match self.node.next_deadline() {
                Some(at) if at <= end => {
                    self.now = self.now.max(at);
                    self.node.fire(self.now, draw(&mut self.draws));
                    self.pump();
                }
                _ => {
                    self.now = end;
                    return;
                }
            }
        }
        panic!("100,000 deadlines without the clock passing {span:?}");
    }

    /// The segments the node sent to `peer`.
    fn to(&self, peer: Peer) -> Vec<Segment> {
        self.heard.iter().filter(|segment| (segment.to, segment.to_port) == (peer.addr, peer.port)).cloned().collect()
    }

    /// The last segment the node sent to `peer`.
    fn last(&self, peer: Peer) -> Segment {
        self.to(peer).pop().expect("a segment to the peer")
    }

    /// The text of each segment the node sent to `peer` that carried any, in order.
    fn texts(&self, peer: Peer) -> Vec<Vec<u8>> {
        self.to(peer).into_iter().map(|segment| segment.text).filter(|text| !text.is_empty()).collect()
    }

    /// The node's initial sequence number toward `peer`: its SYN-ACK's.
    fn iss(&self, peer: Peer) -> u32 {
        self.to(peer).iter().rev().find(|segment| segment.syn).expect("a SYN-ACK to the peer").seq
    }

    /// The peer's SYN to the node's port `to`.
    fn syn_to(&mut self, peer: Peer, to: u16) {
        self.at.insert(peer.addr);
        self.deliver(&frame(peer, to, ISS, None, SYN, &[]));
    }

    fn syn(&mut self, peer: Peer) {
        self.syn_to(peer, SSH);
    }

    /// The peer's ACK of the node's SYN-ACK (RFC 9293 §3.5, figure 6, line 4).
    fn finish(&mut self, peer: Peer) {
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), 0, &[]));
    }

    /// The peer's handshake with the node's port `to`.
    fn handshake_to(&mut self, peer: Peer, to: u16) {
        self.syn_to(peer, to);
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, to, ISS + 1, Some(ack), 0, &[]));
    }

    fn handshake(&mut self, peer: Peer) {
        self.syn(peer);
        self.finish(peer);
    }

    /// The peer's first text.
    fn text(&mut self, peer: Peer, text: &[u8]) {
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), 0, text));
    }

    /// The peer's reset at exactly the sequence number the node expects (RFC 9293 §3.10.7.4,
    /// first check), having sent no text.
    fn rst(&mut self, peer: Peer) {
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), RST, &[]));
    }

    /// The peer's FIN, acknowledging the node's (RFC 9293 §3.6, case 1), neither having sent
    /// text.
    fn fin(&mut self, peer: Peer) {
        let ack = self.iss(peer).wrapping_add(2);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), FIN, &[]));
    }

    /// The peer's acknowledgment of the node's first `bytes` bytes of text, or of as many of
    /// them as it has heard, having sent nothing since its handshake.
    fn takes(&mut self, peer: Peer, bytes: u32) {
        let first = self.iss(peer).wrapping_add(1);
        let ends = self.to(peer).into_iter().filter(|segment| !segment.text.is_empty());
        let heard = ends.map(|segment| segment.seq.wrapping_sub(first).wrapping_add(u32::try_from(segment.text.len()).unwrap())).max().expect("text to the peer");
        self.deliver(&frame(peer, SSH, ISS + 1, Some(first.wrapping_add(bytes.min(heard))), 0, &[]));
    }

    fn listen(&mut self, port: u16) -> (ListenerId, Owner) {
        self.listen_with(port, false)
    }

    /// A listen that names the listener's `TCP_NODELAY`.
    fn listen_with(&mut self, port: u16, nodelay: bool) -> (ListenerId, Owner) {
        let owner = Owner::default();
        let (id, bound) = self.node.listen(ANY, Port::new(port), nodelay, Box::new(WakeEnd(owner.clone())), draw(&mut self.draws)).expect("a free port and a place");
        assert_eq!(bound.get(), port);
        (id, owner)
    }

    /// The owner's accept, on a new client's pipes.
    fn accept(&mut self, id: ListenerId) -> Result<(Accepted, Client), AcceptRefused> {
        let (client, pipes) = client();
        let answer = self.node.accept(self.now, id, Some(pipes));
        self.pump();
        answer.map(|accepted| (accepted, client))
    }

    /// An accept that takes `peer`'s connection.
    fn accepts(&mut self, id: ListenerId, peer: Peer) -> (StreamId, Client) {
        let (accepted, client) = self.accept(id).expect("a connection and a place");
        assert_eq!((accepted.remote, accepted.local), (peer.endpoint(), Port::new(SSH).unwrap()));
        (accepted.id, client)
    }

    /// A connect to [`WEB`].
    fn connect(&mut self) -> (Result<StreamId, ConnectRefused>, Client) {
        let (client, pipes) = client();
        let answer = self.node.connect(self.now, WEB.endpoint(), None, pipes);
        self.pump();
        (answer, client)
    }

    /// The client writes `text`, and the pass netstack runs when its pipe has bytes.
    fn says(&mut self, client: &Client, text: &[u8]) {
        client.borrow_mut().outbox.extend(text);
        self.node.bridge(self.now);
        self.pump();
    }

    /// The stream's client writes 100,000 bytes and lets go of it: [tcp] has room for
    /// [`SEND_BUFFER`] of them, and the rest stays in its pipe.
    fn departs(&mut self, id: StreamId, client: &Client) {
        client.borrow_mut().outbox.extend(vec![7u8; 100_000]);
        self.node.close(self.now, id);
        self.pump();
        assert_eq!(client.borrow().outbox.len(), 100_000 - SEND_BUFFER);
    }

    fn events(&mut self) -> Vec<StreamEvent> {
        self.node.drain_stream_events().collect()
    }
}

/// A node with `places` places.
fn with_places(places: usize) -> Net {
    let mut net = Net::new();
    net.node.set_places(net.now, places);
    net
}

// ---- listen and accept ----

// RFC 9293 §3.10.7.2, third check: a SYN in LISTEN is answered <SEQ=ISS><ACK=RCV.NXT><CTL=SYN,ACK>
// with RCV.NXT the SYN's sequence number plus one, and the state is SYN-RECEIVED. §3.10.7.4, fifth
// check: the ACK of our SYN makes it ESTABLISHED, and only then is there a connection to accept.
#[test]
fn a_listener_answers_a_syn_and_wakes_its_owner_when_the_handshake_ends() {
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    net.syn(P1);
    let answered = net.to(P1);
    let [synack] = &answered[..] else { panic!("one SYN-ACK, not {:?}", net.heard) };
    assert!(synack.syn && !synack.fin && !synack.rst && synack.text.is_empty(), "{synack:?}");
    assert_eq!((synack.from_port, synack.ack), (SSH, Some(ISS + 1)));
    assert_eq!(wakes(&owner), 0, "SYN-RECEIVED is no connection yet");
    assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::Nothing);

    net.finish(P1);
    assert_eq!(wakes(&owner), 1);
    net.text(P1, b"hello");
    assert_eq!((net.node.streams(), net.node.listeners(), net.node.held()), (0, 1, 1), "a connection that waits is nobody's stream");

    let (client, pipes) = client();
    let accepted = net.node.accept(net.now, id, Some(pipes)).unwrap();
    assert_eq!((accepted.remote, accepted.local), (P1.endpoint(), Port::new(SSH).unwrap()));
    assert_eq!(client.borrow().inbox, b"hello", "what arrived before the accept moves in it");
    assert_eq!((net.node.streams(), net.node.listeners(), net.node.held()), (1, 1, 2));
    client.borrow_mut().outbox.extend(b"welcome");
    net.node.bridge(net.now);
    net.pump();
    let said = net.last(P1);
    assert_eq!((said.text.as_slice(), said.seq), (&b"welcome"[..], synack.seq.wrapping_add(1)));
    assert_eq!((wakes(&owner), client.borrow().dropped), (1, 0));
}

// One SYN and nothing more: the next peer is answered at once, and the first handshake is given up within [tcp]'s bound, after which its
// late ACK meets LISTEN: RFC 9293 §3.10.7.2, second check, <SEQ=SEG.ACK><CTL=RST>.
#[test]
fn a_handshake_nobody_finishes_leaves_the_port_open_and_is_given_up() {
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    net.syn(P1);
    net.syn(P2);
    let answered = net.to(P2);
    let [synack] = &answered[..] else { panic!("a SYN-ACK to the second peer, not {:?}", net.heard) };
    assert!(synack.syn && !synack.rst && synack.ack == Some(ISS + 1), "{synack:?}");
    net.finish(P2);
    assert_eq!(wakes(&owner), 1);
    net.accepts(id, P2);

    let first = net.iss(P1);
    net.run(2 * limits::SYNACK_GIVE_UP);
    assert_eq!(net.node.shard().tcp_counters().get(Counter::SynAckGiveUp), 1);
    let said = net.to(P1).len();
    net.run(Duration::from_secs(600));
    assert_eq!(net.to(P1).len(), said, "nothing more is sent for a handshake given up");
    assert!(net.heard.iter().all(|segment| !segment.rst), "{:?}", net.heard);

    net.deliver(&frame(P1, SSH, ISS + 1, Some(first.wrapping_add(1)), 0, &[]));
    let reset = net.last(P1);
    assert!(reset.rst && !reset.syn, "{reset:?}");
    assert_eq!((reset.seq, reset.ack), (first.wrapping_add(1), None));
    assert_eq!((wakes(&owner), net.node.streams(), net.node.listeners()), (1, 1, 1));
}

// The recorded failure of `issues/a-connect-between-two-accepts-is-reset.md`: a connection waits
// for its accept, and the stack this one replaces answered every other peer's SYN with a reset
// until the owner had taken it. Here each is answered, queued behind the one that waits, and
// accepted oldest first, with one wake a connection.
#[test]
fn a_connect_between_two_accepts_is_queued_not_reset() {
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    net.handshake(P2);
    assert_eq!(wakes(&owner), 2, "one wake a connection");
    net.accepts(id, P1);
    net.handshake(P3);
    assert_eq!(wakes(&owner), 3);
    net.accepts(id, P2);
    net.accepts(id, P3);
    assert!(net.heard.iter().all(|segment| !segment.rst), "{:?}", net.heard);
    assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::Nothing);
    assert_eq!((wakes(&owner), net.node.streams()), (3, 3));
}

// ---- wakes ----

// RFC 9293 §3.10.7.4, first check: a reset at RCV.NXT ends the connection. Nobody had taken it,
// so [tcp] returns it to no accept, and the wake written for it finds nothing.
#[test]
fn an_accept_spends_a_wake_whatever_it_answers() {
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    net.rst(P1);
    assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::Nothing);
    assert_eq!(wakes(&owner), 1);

    net.handshake(P2);
    assert_eq!(wakes(&owner), 2, "the wake that accept spent is not counted against the next connection");
    assert_eq!(net.node.accept(net.now, id, None), Err(AcceptRefused::NoPipes));
    assert_eq!(wakes(&owner), 3, "the connection an accept without pipes left is announced again");
    net.accepts(id, P2);
    assert_eq!(wakes(&owner), 3);
}

#[test]
fn a_wake_left_by_a_connection_its_peer_reset_stands_for_the_next() {
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    net.rst(P1);
    net.handshake(P2);
    assert_eq!(wakes(&owner), 1, "the owner holds one wake and one connection waits");
    net.accepts(id, P2);
    assert_eq!(wakes(&owner), 1);
}

// ---- places ----

// The close is RFC 9293 §3.6, case 1: the node's FIN, then the peer's FIN acknowledging it. Until
// then the connection is [tcp]'s to finish and holds the place its stream had.
#[test]
fn a_wake_is_owed_only_for_a_connection_there_is_a_place_for() {
    let mut net = with_places(2);
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    net.handshake(P2);
    assert_eq!(wakes(&owner), 1, "two wait, and there is a place for one");
    let (first, _client) = net.accepts(id, P1);
    assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::Full);
    assert_eq!((wakes(&owner), net.node.held()), (1, 2), "no place, no wake");

    net.node.close(net.now, first);
    net.pump();
    assert!(net.last(P1).fin);
    assert_eq!((net.node.streams(), net.node.held(), wakes(&owner)), (0, 2, 1), "a connection [tcp] is finishing holds its place");
    net.fin(P1);
    assert_eq!((net.node.held(), wakes(&owner)), (1, 2), "the place is back, and the connection that waited is announced");
    net.accepts(id, P2);
}

// A client that left with bytes its peer never takes is still a stream, by `streams`' rule for a
// departed client, and holds its place until that rule cuts it.
#[test]
fn a_departed_clients_connection_holds_its_place_until_it_is_cut() {
    let mut net = with_places(2);
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    let (stream, client) = net.accepts(id, P1);
    net.handshake(P2);
    client.borrow_mut().outbox.extend(vec![7u8; 100_000]);
    net.node.close(net.now, stream);
    net.pump();
    assert_eq!((net.node.streams(), net.node.held(), wakes(&owner)), (1, 2, 1), "its pipe still holds what [tcp] had no room for");
    let (refused, _other) = net.connect();
    assert_eq!(refused, Err(ConnectRefused::Full));
    net.run(Duration::from_secs(101));
    assert_eq!((net.node.streams(), net.node.held(), wakes(&owner)), (0, 1, 2), "cut, and its place is the connection's that waited");
}

#[test]
fn closing_a_connect_gives_its_place_to_a_connection_that_waits() {
    let mut net = with_places(2);
    let (_, owner) = net.listen(SSH);
    let (connecting, _client) = net.connect();
    net.handshake(P1);
    assert_eq!(wakes(&owner), 0);
    net.node.close(net.now, connecting.unwrap());
    assert_eq!(wakes(&owner), 1);
}

#[test]
fn a_stream_reset_for_its_pipe_gives_its_place_to_a_connection_that_waits() {
    let mut net = with_places(2);
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    let (stream, _client) = net.accepts(id, P1);
    net.handshake(P2);
    assert_eq!(wakes(&owner), 1);
    net.node.pipe_broken(net.now, stream, PipeEnd::ToClient);
    assert_eq!(wakes(&owner), 2);
}

#[test]
fn closing_a_listener_gives_its_place_to_a_connection_that_waits_at_another() {
    let mut net = with_places(2);
    let (_, owner) = net.listen(SSH);
    let (other, _) = net.listen(TELNET);
    net.handshake(P1);
    assert_eq!(wakes(&owner), 0);
    assert!(net.node.close_listener(net.now, other));
    assert_eq!(wakes(&owner), 1);
}

#[test]
fn more_places_wake_the_owner_of_a_connection_that_waits() {
    let mut net = with_places(1);
    let (_, owner) = net.listen(SSH);
    net.handshake(P1);
    assert_eq!(wakes(&owner), 0);
    net.node.set_places(net.now, 2);
    assert_eq!(wakes(&owner), 1);
}

#[test]
fn a_connect_past_the_places_is_refused_and_sends_nothing() {
    let mut net = with_places(2);
    let (first, _a) = net.connect();
    let (second, _b) = net.connect();
    assert!(first.is_ok() && second.is_ok(), "{first:?} {second:?}");
    let (third, client) = net.connect();
    assert_eq!(third, Err(ConnectRefused::Full), "a connect not yet answered holds a place");
    assert_eq!((client.borrow().dropped, net.node.held()), (2, 2));
    let sources: BTreeSet<u16> = net.to(WEB).iter().map(|segment| segment.from_port).collect();
    assert_eq!(sources.len(), 2, "no SYN left for the connect refused: {:?}", net.heard);

    net.node.close(net.now, first.unwrap());
    let (fourth, _c) = net.connect();
    assert!(fourth.is_ok(), "{fourth:?}");
}

// RFC 9293 §3.10.7.1: a SYN for a port nothing listens on is answered
// <SEQ=0><ACK=SEG.SEQ+SEG.LEN><CTL=RST,ACK>, which is how the peer reads that the refused listen
// made nothing.
#[test]
fn a_listener_holds_a_place_and_a_listen_without_one_makes_nothing() {
    let mut net = with_places(1);
    let (id, _owner) = net.listen(SSH);
    let refused = Owner::default();
    let answer = net.node.listen(ANY, Port::new(TELNET), false, Box::new(WakeEnd(refused.clone())), draw(&mut net.draws));
    assert_eq!(answer.unwrap_err(), ListenRefused::Full);
    assert!(refused.borrow().dropped);
    net.syn_to(P1, TELNET);
    let reset = net.last(P1);
    assert!(reset.rst && (reset.seq, reset.ack) == (0, Some(ISS + 1)), "{reset:?}");
    let (connect, _client) = net.connect();
    assert_eq!(connect, Err(ConnectRefused::Full), "the listener's place is a stream's");

    assert!(net.node.close_listener(net.now, id));
    assert_eq!(net.node.held(), 0);
    net.listen(TELNET);
}

// ---- a listener's end ----

// The resets are [tcp]'s for a listener closed under its connections, in RFC 9293 §3.10.5's
// form for SYN-RECEIVED and ESTABLISHED: <SEQ=SND.NXT><CTL=RST>, SND.NXT our SYN's number plus
// one.
#[test]
fn closing_a_listener_resets_what_waits_and_frees_its_port() {
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    net.syn(P1);
    net.handshake(P2);
    assert!(net.node.close_listener(net.now, id));
    net.pump();
    for peer in [P1, P2] {
        let reset = net.last(peer);
        assert!(reset.rst && reset.seq == net.iss(peer).wrapping_add(1), "{reset:?}");
    }
    assert!(owner.borrow().dropped, "the owner reads the end");
    assert_eq!((net.node.listeners(), net.node.held()), (0, 0));
    assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::NoListener);
    assert!(!net.node.close_listener(net.now, id));

    let (_, again) = net.listen(SSH);
    net.handshake(P3);
    assert_eq!((wakes(&owner), wakes(&again)), (1, 1));
}

#[test]
fn a_wake_the_owners_pipe_refuses_ends_the_listener() {
    for refusal in [WriteRefusal::Gone, WriteRefusal::Full, WriteRefusal::Broken] {
        let mut net = Net::new();
        let (id, owner) = net.listen(SSH);
        owner.borrow_mut().refusal = Some(refusal);
        net.handshake(P1);
        let ended: Vec<(ListenerId, WriteRefusal)> = net.node.drain_ended_listeners().collect();
        assert_eq!(ended, [(id, refusal)]);
        assert!(owner.borrow().dropped && net.last(P1).rst, "{refusal:?}: {:?}", net.heard);
        assert_eq!((net.node.listeners(), net.node.held()), (0, 0), "{refusal:?}");
        net.listen(SSH);
    }
}

// The drawn port is in the dynamic range, 49152 to 65535 (RFC 6335 §6), at the offset the draw's
// low sixteen bits name.
#[test]
fn a_listen_on_a_taken_port_is_refused_and_a_drawn_port_listens() {
    let mut net = Net::new();
    let (_, owner) = net.listen(SSH);
    let second = Owner::default();
    let answer = net.node.listen(ANY, Port::new(SSH), false, Box::new(WakeEnd(second.clone())), draw(&mut net.draws));
    assert_eq!(answer.unwrap_err(), ListenRefused::InUse);
    assert!(second.borrow().dropped);
    assert_eq!((net.node.listeners(), net.node.held()), (1, 1));
    net.handshake(P1);
    assert_eq!(wakes(&owner), 1, "the listener that holds the port still listens");

    let (_, port) = net.node.listen(ANY, None, false, Box::new(WakeEnd(Owner::default())), || 0x0001_0005).unwrap();
    assert_eq!(port.get(), 49_152 + 5);
    net.syn_to(P2, port.get());
    let synack = net.last(P2);
    assert!(synack.syn && synack.from_port == port.get(), "{synack:?}");
}

// A listener is at the address its listen named. One the machine does not hold is refused, as
// a datagram socket's bind is, and nothing listens: RFC 9293 §3.10.7.1, a SYN for a port
// nothing listens on is answered <SEQ=0><ACK=SEG.SEQ+SEG.LEN><CTL=RST,ACK>. One the machine
// holds listens.
#[test]
fn a_listener_is_at_the_address_it_named_and_only_one_the_machine_holds() {
    let mut net = Net::new();
    let refused = Owner::default();
    let answer = net.node.listen(ELSEWHERE, Port::new(SSH), false, Box::new(WakeEnd(refused.clone())), draw(&mut net.draws));
    assert_eq!(answer.unwrap_err(), ListenRefused::NotLocal);
    assert!(refused.borrow().dropped);
    assert_eq!((net.node.listeners(), net.node.held()), (0, 0));
    net.syn(P1);
    let reset = net.last(P1);
    assert!(reset.rst && (reset.seq, reset.ack) == (0, Some(ISS + 1)), "{reset:?}");

    let named = Owner::default();
    let (_, port) = net.node.listen(A, Port::new(SSH), false, Box::new(WakeEnd(named.clone())), draw(&mut net.draws)).expect("the machine's address, and a port no listener holds");
    assert_eq!((port.get(), net.node.listeners()), (SSH, 1));
    net.handshake(P2);
    assert_eq!(wakes(&named), 1);
}

/// A listener at `first` holds the port, and a listen at `second` on it is refused with nothing
/// made: the connection that comes next is the first listener's.
fn a_port_is_one_listeners(first: Ipv4Addr, second: Ipv4Addr) {
    let mut net = Net::new();
    let holder = Owner::default();
    let (id, _) = net.node.listen(first, Port::new(SSH), false, Box::new(WakeEnd(holder.clone())), draw(&mut net.draws)).expect("a port no listener holds");
    let refused = Owner::default();
    let answer = net.node.listen(second, Port::new(SSH), false, Box::new(WakeEnd(refused.clone())), draw(&mut net.draws));
    assert_eq!(answer.unwrap_err(), ListenRefused::InUse, "{second} beside {first}");
    assert!(refused.borrow().dropped);
    assert_eq!((net.node.listeners(), net.node.held()), (1, 1));
    net.handshake(P1);
    assert_eq!((wakes(&holder), wakes(&refused)), (1, 0));
    net.accepts(id, P1);
}

// [tcp] hands a SYN to the listener that named its address before one at every address
// (LS-09), so a second listen on a held port that named the machine's address would take every
// connection of the first from then on. A listen carries no word for its program, so the node
// cannot tell the first listener's owner from another: the port is one listener's, as a
// datagram socket's port is one socket's.
#[test]
fn a_listen_at_the_machines_address_takes_no_port_held_at_every_address() {
    a_port_is_one_listeners(ANY, A);
}

#[test]
fn a_listen_at_every_address_takes_no_port_held_at_the_machines_address() {
    a_port_is_one_listeners(A, ANY);
}

#[test]
fn a_second_listen_at_the_machines_address_takes_no_port_held_there() {
    a_port_is_one_listeners(A, A);
}

// A listener ended for a wake its owner's pipe refused gives its place back in the pass that
// ended it: the listener after it is owed a wake for that place before the node is asked
// anything more. The last handshake's ACK is delivered with no transmit opportunity after it,
// since an opportunity ends in a pass of its own.
#[test]
fn a_listener_ended_for_its_wake_gives_its_place_to_another_in_the_same_pass() {
    let mut net = with_places(3);
    let (first, refusing) = net.listen(SSH);
    let (_, owner) = net.listen(TELNET);
    net.handshake_to(P1, TELNET);
    net.handshake_to(P2, TELNET);
    assert_eq!(wakes(&owner), 1, "two wait, and there is a place for one");
    refusing.borrow_mut().refusal = Some(WriteRefusal::Gone);
    net.syn(P3);
    let ack = net.iss(P3).wrapping_add(1);
    net.node.receive(net.now, &frame(P3, SSH, ISS + 1, Some(ack), 0, &[]), draw(&mut net.draws));
    assert_eq!(wakes(&owner), 2);
    let ended: Vec<(ListenerId, WriteRefusal)> = net.node.drain_ended_listeners().collect();
    assert_eq!((ended, net.node.listeners(), net.node.held()), (vec![(first, WriteRefusal::Gone)], 1, 1));
}

// A datagram socket is something a client makes the node hold, its two queues of sixteen
// datagrams, from its bind to its close.
#[test]
fn a_datagram_socket_holds_a_place_and_a_bind_without_one_makes_nothing() {
    let undrawn = || -> u32 { panic!("a named port draws nothing") };
    let mut net = with_places(2);
    let (_, owner) = net.listen(SSH);
    let (socket, _) = net.node.udp_bind(ANY, Port::new(4_000), undrawn).expect("a free port and a place");
    assert_eq!(net.node.held(), 2);
    assert_eq!(net.node.udp_bind(ANY, Port::new(4_001), undrawn), Err(Refused::ResourceExhausted));
    let (connect, _client) = net.connect();
    assert_eq!(connect, Err(ConnectRefused::Full), "the socket's place is a stream's");
    net.handshake(P1);
    assert_eq!(wakes(&owner), 0);

    assert_eq!(net.node.udp_close(net.now, socket), Ok(()));
    assert_eq!((net.node.held(), wakes(&owner)), (1, 1), "its place is back, and the connection that waited is announced");
    assert_eq!(net.node.udp_close(net.now, socket), Err(Refused::NotConnected));
    assert_eq!(net.node.held(), 1, "a close that names no socket gives no place back");
    net.node.udp_bind(ANY, Port::new(4_001), undrawn).expect("the refused bind left its port free");
    assert_eq!(net.node.held(), 2);
}

// ---- the streams an accept makes ----

// RFC 9293 §3.7.4: with text unacknowledged a short segment waits, and with Nagle's algorithm
// off it leaves. No peer here acknowledges any text. The two answers a host gives for the option
// on a listening socket are `tests/host.rs`'s: a connection has it if its listener had it when
// the connection began, whenever it is accepted.
#[test]
fn a_stream_starts_with_the_options_its_connection_took_from_its_listener() {
    let mut net = Net::new();
    let (id, _owner) = net.listen(SSH);
    // One connection begins before the option is set and one after; both are accepted after.
    net.handshake(P1);
    assert!(net.node.set_listener_nodelay(id, true));
    net.handshake(P2);
    let (first, a) = net.accept(id).unwrap();
    let (second, b) = net.accept(id).unwrap();
    assert_eq!((first.remote, first.nodelay, second.remote, second.nodelay), (P1.endpoint(), false, P2.endpoint(), true));
    let (first, second) = (first.id, second.id);
    for client in [&a, &b] {
        net.says(client, b"a");
        net.says(client, b"b");
    }
    assert_eq!(net.texts(P1), [b"a".to_vec()], "the second write waits for the first's acknowledgment");
    assert_eq!(net.texts(P2), [b"a".to_vec(), b"b".to_vec()], "the second write waits for no acknowledgment");

    // An accepted stream's options are its own from then on, and the listener's its own.
    assert!(net.node.set_nodelay(net.now, second, false));
    net.says(&b, b"c");
    assert_eq!(net.texts(P2), [b"a".to_vec(), b"b".to_vec()], "the third write waits");
    assert!(net.node.set_nodelay(net.now, first, true));
    net.pump();
    assert_eq!(net.texts(P1), [b"a".to_vec(), b"b".to_vec()], "and the first stream's second waits no longer");

    assert!(net.node.set_listener_nodelay(id, false));
    net.handshake(P3);
    let (third, c) = net.accept(id).unwrap();
    assert_eq!((third.remote, third.nodelay), (P3.endpoint(), false));
    net.says(&c, b"a");
    net.says(&c, b"b");
    assert_eq!(net.texts(P3), [b"a".to_vec()]);

    assert!(net.node.close_listener(net.now, id));
    assert!(!net.node.set_listener_nodelay(id, true), "a closed listener's id names nothing");
}

// A listen names its listener's option, so no SYN reaches the port between the passive open and
// a set: the first connection has it with no call between, as a host's has the option set
// before `listen` (`tests/host.rs`, its first answer). The wire is RFC 9293 §3.7.4's, as above.
#[test]
fn a_listener_holds_the_option_its_listen_named_from_its_first_connection() {
    let mut net = Net::new();
    let (id, _owner) = net.listen_with(SSH, true);
    net.handshake(P1);
    let (accepted, client) = net.accept(id).unwrap();
    assert!(accepted.nodelay);
    net.says(&client, b"a");
    net.says(&client, b"b");
    assert_eq!(net.texts(P1), [b"a".to_vec(), b"b".to_vec()], "the second write waits for no acknowledgment");

    assert!(net.node.set_listener_nodelay(id, false));
    net.handshake(P2);
    let (accepted, _client) = net.accept(id).unwrap();
    assert!(!accepted.nodelay, "a listen's option is the listener's to change");
}

// A handshake its peer resets before it ends (RFC 9293 §3.10.7.4, first check, in SYN-RECEIVED)
// leaves nothing of its options behind: the listener's option changed while it was in progress,
// and the next connection begins with what the listener holds when its own SYN arrives. Both
// ways round.
#[test]
fn a_handshake_reset_before_it_ends_leaves_the_next_connection_its_listeners_option() {
    for held in [true, false] {
        let mut net = Net::new();
        let (id, owner) = net.listen_with(SSH, held);
        net.syn(P1);
        assert!(net.node.set_listener_nodelay(id, !held));
        net.rst(P1);
        assert_eq!((wakes(&owner), net.accept(id).unwrap_err()), (0, AcceptRefused::Nothing), "the reset handshake is no connection");
        net.handshake(P2);
        let (accepted, _client) = net.accept(id).unwrap();
        assert_eq!((accepted.remote, accepted.nodelay), (P2.endpoint(), !held), "the listener held {held} at the reset handshake's SYN");

        // And a set while the reset handshake was in progress, taken back before the next SYN.
        net.syn(P3);
        assert!(net.node.set_listener_nodelay(id, held));
        net.rst(P3);
        assert!(net.node.set_listener_nodelay(id, !held));
        let again = Peer { addr: C, port: 40_001 };
        net.handshake(again);
        let (accepted, _client) = net.accept(id).unwrap();
        assert_eq!((accepted.remote, accepted.nodelay), (again.endpoint(), !held));
    }
}

/// The write end of a client's pipe whose reader is gone for good: the node resets the stream.
struct BrokenEnd;

impl ToClient for BrokenEnd {
    fn write(&mut self, _: &[u8]) -> Result<usize, WriteRefusal> {
        Err(WriteRefusal::Broken)
    }
}

// The accept's own pass moves what the connection already received, and a pipe that refuses it
// for good ends the stream there (RFC 9293 §3.10.7.4's reset is the peer's notice). The answer
// still says what the stream was handed over with, though no stream is left to answer for it.
#[test]
fn an_accept_answers_the_option_of_a_stream_its_own_pass_let_go() {
    let mut net = Net::new();
    let (id, _owner) = net.listen_with(SSH, true);
    net.handshake(P1);
    net.text(P1, b"hello");
    let (client, Pipes { from_client, .. }) = client();
    let accepted = net.node.accept(net.now, id, Some(Pipes { to_client: Box::new(BrokenEnd), from_client })).unwrap();
    net.pump();
    assert!(net.last(P1).rst, "{:?}", net.heard);
    assert_eq!((accepted.remote, accepted.nodelay), (P1.endpoint(), true));
    assert_eq!((net.node.streams(), client.borrow().dropped), (0, 2));
}

// `streams` counts a stream its client can see no more by its peer's address, and lets an
// address keep sixteen alive at once. Seventeen are accepted from 192.0.2.2 and one from
// 192.0.2.3, each client leaves bytes behind, and each peer takes a segment of them fifty
// seconds on: only the seventeenth from 192.0.2.2 has no more than its 100 seconds.
#[test]
fn an_accepted_stream_is_one_of_its_peers_addresss_sixteen() {
    let mut net = Net::new();
    let (id, _owner) = net.listen(SSH);
    let peers: Vec<Peer> = (0..17).map(|nth| Peer { addr: B, port: 41_000 + nth }).chain([Peer { addr: C, port: 41_000 }]).collect();
    let mut streams = Vec::new();
    for peer in &peers {
        net.handshake(*peer);
        let (stream, client) = net.accepts(id, *peer);
        net.departs(stream, &client);
        streams.push(stream);
    }
    assert_eq!((net.node.streams(), net.events()), (18, vec![]));

    net.run(Duration::from_secs(50));
    for peer in &peers {
        net.takes(*peer, 1_460);
    }
    net.run(Duration::from_secs(51));
    assert_eq!((net.node.streams(), net.events()), (17, vec![StreamEvent::Cut { id: streams[16] }]));
    let reset: Vec<Peer> = net.heard.iter().filter(|segment| segment.rst).map(|segment| Peer { addr: segment.to, port: segment.to_port }).collect();
    assert_eq!(reset, [peers[16]]);
}

// `streams` cuts a stream whose client still holds it, once the peer's FIN ended its reading
// and it shut its writing down: what that client asks of the stream afterwards names nothing,
// and not the stream that took its place either, since no id is used twice.
#[test]
fn a_request_for_a_stream_that_was_cut_names_nothing() {
    let mut net = with_places(2);
    let (id, owner) = net.listen(SSH);
    net.handshake(P1);
    let (cut, client) = net.accepts(id, P1);
    net.handshake(P2);
    let ack = net.iss(P1).wrapping_add(1);
    net.deliver(&frame(P1, SSH, ISS + 1, Some(ack), FIN, &[]));
    client.borrow_mut().outbox.extend(vec![7u8; 100_000]);
    assert!(net.node.shutdown_write(net.now, cut));
    net.pump();
    assert_eq!((net.node.streams(), client.borrow().dropped, wakes(&owner)), (1, 1, 1), "the client reads the end and may still close");

    net.run(Duration::from_secs(101));
    assert_eq!(net.events(), [StreamEvent::Cut { id: cut }]);
    assert!(net.last(P1).rst);
    assert_eq!((net.node.streams(), net.node.held(), client.borrow().dropped, wakes(&owner)), (0, 1, 2, 2));
    let (next, other) = net.accepts(id, P2);
    assert_ne!(next, cut);

    let said = net.heard.len();
    net.node.close(net.now, cut);
    assert!(!net.node.shutdown_write(net.now, cut));
    assert!(!net.node.set_nodelay(net.now, cut, true));
    net.node.pipe_gone(net.now, cut, PipeEnd::FromClient);
    net.node.pipe_broken(net.now, cut, PipeEnd::FromClient);
    net.pump();
    assert_eq!((net.node.streams(), net.node.held(), other.borrow().dropped, wakes(&owner)), (1, 2, 0, 2), "the stream in its place stands");
    assert_eq!((net.heard.len(), net.events()), (said, vec![]));
}

// The track's exit for the bound across peer addresses. `streams` lets each address keep
// sixteen streams their clients can see no more alive and counts no addresses, so the places
// are what bounds the node. Four addresses, more than the node has places for, bring sixteen
// connections each, and every peer whose connection is accepted keeps its departed client's
// bytes coming for 250 seconds: the node holds two streams at a time, announces no connection
// it has no place for, and refuses every accept and connect past them.
#[test]
fn peers_at_more_addresses_than_there_are_places_hold_no_stream_past_them() {
    let mut net = with_places(3);
    let (id, owner) = net.listen(SSH);
    for nth in 0..16 {
        for addr in [B, C, D, E] {
            net.handshake(Peer { addr, port: 42_000 + nth });
        }
    }
    assert_eq!((wakes(&owner), net.node.streams(), net.node.held()), (2, 0, 1), "sixty-four wait, and there are places for two");

    for (round, pair) in [[B, C], [D, E]].into_iter().enumerate() {
        let peers = pair.map(|addr| Peer { addr, port: 42_000 });
        let held = peers.map(|peer| {
            let (stream, client) = net.accepts(id, peer);
            net.departs(stream, &client);
            (stream, client)
        });
        let full = |net: &mut Net| {
            assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::Full, "round {round}");
            assert_eq!(net.connect().0, Err(ConnectRefused::Full), "round {round}");
            assert_eq!((net.node.streams(), net.node.held(), wakes(&owner)), (2, 3, 2 + 2 * round));
        };
        full(&mut net);
        for step in 1..=5 {
            net.run(Duration::from_secs(50));
            for peer in peers {
                net.takes(peer, 1_460 * step);
            }
            full(&mut net);
        }
        assert!(held.iter().all(|(_, client)| client.borrow().outbox.len() < 100_000 - SEND_BUFFER), "round {round}: each pipe gave bytes up");

        // The peers take no more: both streams are cut, and two of the connections that wait
        // are announced.
        net.run(Duration::from_secs(101));
        let cut: Vec<StreamEvent> = held.iter().map(|(stream, _)| StreamEvent::Cut { id: *stream }).collect();
        assert_eq!(net.events(), cut, "round {round}");
        assert_eq!((net.node.streams(), net.node.held(), wakes(&owner)), (0, 1, 4 + 2 * round));
    }
}

// ---- the wire is not trusted ----

#[test]
fn no_cut_of_a_syn_is_a_syn() {
    let whole = frame(P1, SSH, ISS, None, SYN, &[]);
    let mut net = Net::new();
    let (id, owner) = net.listen(SSH);
    for len in 0..whole.len() {
        net.deliver(&whole[..len]);
        assert!(net.heard.is_empty(), "cut to {len} bytes: {:?}", net.heard);
    }
    net.handshake(P1);
    assert_eq!(wakes(&owner), 1);
    net.accepts(id, P1);
}

#[test]
fn no_flipped_bit_of_a_syn_wakes_an_owner_or_ends_a_listener() {
    let whole = frame(P1, SSH, ISS, None, SYN, &[]);
    for index in 0..whole.len() {
        for bit in 0..8 {
            let mut net = Net::new();
            let (id, owner) = net.listen(SSH);
            let mut flipped = whole.clone();
            flipped[index] ^= 1 << bit;
            net.deliver(&flipped);
            // A flip no checksum covers is in the link addresses: taken or refused, it is at most
            // a handshake in progress.
            assert_eq!((wakes(&owner), net.node.listeners(), net.node.streams()), (0, 1, 0), "byte {index} bit {bit}");
            net.handshake(P2);
            assert_eq!(wakes(&owner), 1, "byte {index} bit {bit}");
            net.accepts(id, P2);
        }
    }
}
