//! Listeners and the node's places, against peers the test scripts at 192.0.2.2 and pipe ends it
//! fakes. No scenario ids: the specifications' listening scenarios are `toyos-net-tcp`'s; these
//! are what the node does between [tcp]'s queues, an owner's wakes and its accepts.
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

use common::{arp, terms, Wire, A, MAC, MAC_B, MAC_R, R};
use etherparse::{ArpOperation, LinkSlice, NetSlice, PacketBuilder, SlicedPacket, TcpOptionElement, TransportSlice};
use toyos_net_node::{AcceptRefused, Accepted, ConnectRefused, FromClient, ListenRefused, ListenerId, Node, PipeEnd, PipeRefusal, Pipes, StreamId, ToClient, Wake};
use toyos_net_tcp::{limits, Counter, Endpoint};
use toyos_net_wire::{Instant, Port};

const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
/// The port the tests listen on, and another.
const SSH: u16 = 22;
const TELNET: u16 = 23;
/// The peers' ports.
const P1: u16 = 40_001;
const P2: u16 = 40_002;
const P3: u16 = 40_003;
/// Every peer's initial sequence number.
const ISS: u32 = 5000;

const SYN: u8 = 1;
const FIN: u8 = 2;
const RST: u8 = 4;

// ---- an owner's wake pipe ----

#[derive(Debug, Default)]
struct Notified {
    /// The wakes written and not refused.
    wakes: usize,
    /// What the pipe answers a wake instead of taking it.
    refusal: Option<PipeRefusal>,
    dropped: bool,
}

type Owner = Rc<RefCell<Notified>>;

struct WakeEnd(Owner);

impl Wake for WakeEnd {
    fn wake(&mut self) -> Result<(), PipeRefusal> {
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
    fn write(&mut self, bytes: &[u8]) -> Result<usize, PipeRefusal> {
        self.0.borrow_mut().inbox.extend_from_slice(bytes);
        Ok(bytes.len())
    }
}

impl FromClient for ReadEnd {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, PipeRefusal> {
        let mut ends = self.0.borrow_mut();
        let read = out.len().min(ends.outbox.len());
        if read == 0 {
            return Err(PipeRefusal::WouldBlock);
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

/// A segment the node emitted, as `etherparse` read it.
#[derive(Clone, Debug)]
struct Segment {
    /// The node's port, and the peer's.
    from: u16,
    to: u16,
    seq: u32,
    ack: Option<u32>,
    syn: bool,
    fin: bool,
    rst: bool,
    text: Vec<u8>,
}

/// A segment from the peer's port `from` to the node's port `to` in its frame, built by
/// `etherparse`: window 65,535, an MSS option on a SYN, PSH with text.
fn frame(from: u16, to: u16, seq: u32, ack: Option<u32>, flags: u8, text: &[u8]) -> Vec<u8> {
    let mut step = PacketBuilder::ethernet2(MAC_B, MAC).ipv4(B.octets(), A.octets(), 64).tcp(from, to, seq, 65_535);
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
    /// Every TCP segment the node emitted, in order.
    heard: Vec<Segment>,
}

impl Net {
    /// The node holding its lease of 192.0.2.1/24 for an hour, with `common::PLACES` places.
    fn new() -> Self {
        let Wire { node, now, .. } = Wire::leased(&terms(3_600, Some(R)));
        Self { node, now, draws: 0x7e00_0000, heard: Vec::new() }
    }

    /// Offers the node all the credit it wants until it sends nothing more, its ARP requests
    /// for the peers' address and the router's answered.
    fn pump(&mut self) {
        for _ in 0..100_000 {
            let mut frames = Vec::new();
            self.node.transmit(self.now, usize::MAX, |frame| frames.push(frame.to_vec()));
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
    /// a TCP segment to a peer, with the lengths and checksums `etherparse` computes.
    fn hears(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
        let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
        assert_eq!(ethernet.source(), MAC, "from the node's MAC");
        match (&packet.net, &packet.transport) {
            (Some(NetSlice::Arp(asked)), None) => {
                let asks = asked.operation() == ArpOperation::REQUEST && v4(asked.sender_protocol_addr()) == A;
                match v4(asked.target_protocol_addr()) {
                    target if asks && target == B => Some(arp(MAC, false, MAC_B, B, A)),
                    target if asks && target == R => Some(arp(MAC, false, MAC_R, R, A)),
                    _ => None,
                }
            }
            (Some(NetSlice::Ipv4(_)), Some(TransportSlice::Udp(_))) => None,
            (Some(NetSlice::Ipv4(ip)), Some(TransportSlice::Tcp(tcp))) => {
                let header = ip.header();
                assert_eq!(ethernet.destination(), MAC_B, "to the peers' MAC");
                assert_eq!(header.header_checksum(), header.to_header().calc_header_checksum(), "the IPv4 header checksum");
                assert_eq!(usize::from(header.total_len()), 20 + tcp.slice().len(), "IPv4's length is the segment's");
                assert_eq!((header.source_addr(), header.destination_addr()), (A, B));
                assert_eq!(tcp.checksum(), tcp.calc_checksum_ipv4(header.source(), header.destination()).unwrap(), "the TCP checksum");
                self.heard.push(Segment {
                    from: tcp.source_port(),
                    to: tcp.destination_port(),
                    seq: tcp.sequence_number(),
                    ack: tcp.ack().then(|| tcp.acknowledgment_number()),
                    syn: tcp.syn(),
                    fin: tcp.fin(),
                    rst: tcp.rst(),
                    text: tcp.payload().to_vec(),
                });
                None
            }
            other => panic!("neither ARP, UDP nor TCP: {other:?}"),
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

    /// The segments the node sent to the peer at `port`.
    fn to(&self, port: u16) -> Vec<Segment> {
        self.heard.iter().filter(|segment| segment.to == port).cloned().collect()
    }

    /// The last segment the node sent to the peer at `port`.
    fn last(&self, port: u16) -> Segment {
        self.to(port).pop().expect("a segment to the peer")
    }

    /// The node's initial sequence number toward the peer at `port`: its SYN-ACK's.
    fn iss(&self, port: u16) -> u32 {
        self.to(port).iter().rev().find(|segment| segment.syn).expect("a SYN-ACK to the peer").seq
    }

    /// The peer's SYN to the node's port `to`.
    fn syn_to(&mut self, peer: u16, to: u16) {
        self.deliver(&frame(peer, to, ISS, None, SYN, &[]));
    }

    fn syn(&mut self, peer: u16) {
        self.syn_to(peer, SSH);
    }

    /// The peer's ACK of the node's SYN-ACK (RFC 9293 §3.5, figure 6, line 4).
    fn finish(&mut self, peer: u16) {
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), 0, &[]));
    }

    fn handshake(&mut self, peer: u16) {
        self.syn(peer);
        self.finish(peer);
    }

    /// The peer's first text.
    fn text(&mut self, peer: u16, text: &[u8]) {
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), 0, text));
    }

    /// The peer's reset at exactly the sequence number the node expects (RFC 9293 §3.10.7.4,
    /// first check), having sent no text.
    fn rst(&mut self, peer: u16) {
        let ack = self.iss(peer).wrapping_add(1);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), RST, &[]));
    }

    /// The peer's FIN, acknowledging the node's (RFC 9293 §3.6, case 1), neither having sent
    /// text.
    fn fin(&mut self, peer: u16) {
        let ack = self.iss(peer).wrapping_add(2);
        self.deliver(&frame(peer, SSH, ISS + 1, Some(ack), FIN, &[]));
    }

    fn listen(&mut self, port: u16) -> (ListenerId, Owner) {
        let owner = Owner::default();
        let (id, bound) = self.node.listen(Port::new(port), Box::new(WakeEnd(owner.clone())), draw(&mut self.draws)).expect("a free port and a place");
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

    /// An accept that takes the connection of the peer at `port`.
    fn accepts(&mut self, id: ListenerId, port: u16) -> (StreamId, Client) {
        let (accepted, client) = self.accept(id).expect("a connection and a place");
        assert_eq!((accepted.remote, accepted.local), (Endpoint { addr: B, port: Port::new(port).unwrap() }, Port::new(SSH).unwrap()));
        (accepted.id, client)
    }

    /// A connect to the peers' port 80, which nothing answers.
    fn connect(&mut self) -> (Result<StreamId, ConnectRefused>, Client) {
        let (client, pipes) = client();
        let answer = self.node.connect(self.now, Endpoint { addr: B, port: Port::new(80).unwrap() }, None, pipes);
        self.pump();
        (answer, client)
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
    assert_eq!((synack.from, synack.ack), (SSH, Some(ISS + 1)));
    assert_eq!(wakes(&owner), 0, "SYN-RECEIVED is no connection yet");
    assert_eq!(net.accept(id).unwrap_err(), AcceptRefused::Nothing);

    net.finish(P1);
    assert_eq!(wakes(&owner), 1);
    net.text(P1, b"hello");
    assert_eq!((net.node.streams(), net.node.listeners(), net.node.held()), (0, 1, 1), "a connection that waits is nobody's stream");

    let (client, pipes) = client();
    let accepted = net.node.accept(net.now, id, Some(pipes)).unwrap();
    assert_eq!((accepted.remote, accepted.local), (Endpoint { addr: B, port: Port::new(P1).unwrap() }, Port::new(SSH).unwrap()));
    assert_eq!(client.borrow().inbox, b"hello", "what arrived before the accept moves in it");
    assert_eq!((net.node.streams(), net.node.listeners(), net.node.held()), (1, 1, 2));
    client.borrow_mut().outbox.extend(b"welcome");
    net.node.bridge(net.now);
    net.pump();
    let said = net.last(P1);
    assert_eq!((said.text.as_slice(), said.seq), (&b"welcome"[..], synack.seq.wrapping_add(1)));
    assert_eq!((wakes(&owner), client.borrow().dropped), (1, 0));
}

// The recorded failure of `issues/a-handshake-nobody-finishes-holds-a-listeners-port-shut.md`:
// one SYN and nothing more, and the stack this one replaces answered the next peer's SYN with a
// reset for as long as the first handshake hung, which was for good. Here the next peer is
// answered at once, and the first handshake is given up within [tcp]'s bound, after which its
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
    let sources: BTreeSet<u16> = net.to(80).iter().map(|segment| segment.from).collect();
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
    let answer = net.node.listen(Port::new(TELNET), Box::new(WakeEnd(refused.clone())), draw(&mut net.draws));
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
    for refusal in [PipeRefusal::Gone, PipeRefusal::WouldBlock, PipeRefusal::Broken] {
        let mut net = Net::new();
        let (id, owner) = net.listen(SSH);
        owner.borrow_mut().refusal = Some(refusal);
        net.handshake(P1);
        let ended: Vec<(ListenerId, PipeRefusal)> = net.node.drain_refused_listeners().collect();
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
    let answer = net.node.listen(Port::new(SSH), Box::new(WakeEnd(second.clone())), draw(&mut net.draws));
    assert_eq!(answer.unwrap_err(), ListenRefused::InUse);
    assert!(second.borrow().dropped);
    assert_eq!((net.node.listeners(), net.node.held()), (1, 1));
    net.handshake(P1);
    assert_eq!(wakes(&owner), 1, "the listener that holds the port still listens");

    let (_, port) = net.node.listen(None, Box::new(WakeEnd(Owner::default())), || 0x0001_0005).unwrap();
    assert_eq!(port.get(), 49_152 + 5);
    net.syn_to(P2, port.get());
    let synack = net.last(P2);
    assert!(synack.syn && synack.from == port.get(), "{synack:?}");
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
