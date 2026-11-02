//! Streams on the node, against peers the test scripts, at 192.0.2.2:80 unless a test puts one
//! elsewhere, and clients that are `toyos::transport::session`'s own client end over a region on
//! the heap (`common::client`). No scenario ids: the specifications' TCP scenarios are
//! `toyos-net-tcp`'s; these are what the node does between [tcp] and a client's region.
//!
//! What is not ours: every segment the node emits is read by `etherparse` as it leaves
//! ([`Far::hears`]), its IPv4 and TCP checksums and lengths that crate's sums, and every segment
//! the peer sends is built by it. The order and numbers each test asserts are RFC 9293's, cited
//! where asserted. The scripted peer itself is ours: it acknowledges what arrives in order and
//! says only what a test tells it to.

mod common;

use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use common::client::Client;
use common::{arp, batch, terms, Segment, Wire, A, BUDGET, MAC, MAC_B, MAC_R, R};
use etherparse::{ArpOperation, LinkSlice, NetSlice, PacketBuilder, SlicedPacket, TcpOptionElement, TransportSlice};
use toyos::net::stream::{ADDRESS_GONE, RESET};
use toyos_net_node::{ConnectRefused, ExportRefused, Node, StreamEvent, StreamId};
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{Counter, Endpoint, Failure};
use toyos_net_wire::{Instant, Port};
use toyos::transport::session::{Holder, Refused};
use toyos::transport::Violation;

const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
/// A second peer's address, and its MAC.
const C: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 3);
const MAC_C: [u8; 6] = [2, 0, 0, 0, 0, 0x0c];
const PORT: u16 = 80;
/// The peer's initial sequence number.
const ISS: u32 = 5000;
/// The region's header words a hostile client writes: the receive ring's head and the send
/// ring's tail (`toyos::net::stream::LAYOUT`).
const RX_HEAD: usize = 0;
const TX_TAIL: usize = 5 * 16;
/// The send ring's gate, which a holder that dies may hold.
const TX_GATE: usize = 12 * 16;

fn peer() -> Endpoint {
    Endpoint { addr: B, port: Port::new(PORT).unwrap() }
}

/// Text that repeats at no power of two.
fn text(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8 ^ (i / 251) as u8).collect()
}

/// What the client reads now: every byte ready, and `Ok` where the end came after them.
fn reads(client: &Client) -> (Vec<u8>, Result<(), Refused>) {
    client.read_all()
}

/// The bytes the client can read now, which must end short of the end.
fn inbox(client: &Client) -> Vec<u8> {
    let (bytes, stopped) = client.read_all();
    assert_eq!(stopped, Err(Refused::WouldBlock), "the stream did not end");
    bytes
}

// ---- the far end ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Manner {
    /// Completes the handshake and acknowledges what arrives in order.
    Answers,
    /// Says nothing to any segment.
    Deaf,
    /// Resets a SYN (RFC 9293 §3.10.7.1).
    Refuses,
}

struct Far {
    addr: Ipv4Addr,
    mac: [u8; 6],
    manner: Manner,
    /// Nobody answers an ARP request for the peer's address.
    absent: bool,
    /// The test turned to another peer ([`Net::turn_to`]): of the node's segments this one hears
    /// its own connection's and no other, a SYN least of all.
    parked: bool,
    /// The node's port, from its last SYN to this address: segments from another are heard and
    /// not answered.
    port: u16,
    /// The peer's next sequence number.
    snd_nxt: u32,
    /// The node's next sequence number the peer has everything before.
    rcv_nxt: u32,
    /// The node's last acknowledgment, and the window it came with.
    acked: u32,
    window: u16,
    received: Vec<u8>,
    /// The node's FIN arrived, after everything before it.
    fin: bool,
    resets: Vec<Segment>,
    /// Every segment the node emitted, in order.
    segments: Vec<Segment>,
}

fn v4(bytes: &[u8]) -> Ipv4Addr {
    Ipv4Addr::from(<[u8; 4]>::try_from(bytes).expect("four bytes"))
}

impl Far {
    fn new() -> Self {
        Self::at(B, MAC_B)
    }

    /// A peer at `addr`, whose frames come from `mac`.
    fn at(addr: Ipv4Addr, mac: [u8; 6]) -> Self {
        Self {
            addr,
            mac,
            manner: Manner::Answers,
            absent: false,
            parked: false,
            port: 0,
            snd_nxt: ISS,
            rcv_nxt: 0,
            acked: 0,
            window: 0,
            received: Vec::new(),
            fin: false,
            resets: Vec::new(),
            segments: Vec::new(),
        }
    }

    /// A segment from the peer in its frame, built by `etherparse`: PSH with text.
    fn frame(&self, seq: u32, ack: Option<u32>, syn: bool, fin: bool, rst: bool, text: &[u8]) -> Vec<u8> {
        let mut step = PacketBuilder::ethernet2(self.mac, MAC).ipv4(self.addr.octets(), A.octets(), 64).tcp(PORT, self.port, seq, 65_535);
        if syn {
            step = step.syn().options(&[TcpOptionElement::MaximumSegmentSize(1460)]).unwrap();
        }
        if let Some(ack) = ack {
            step = step.ack(ack);
        }
        if fin {
            step = step.fin();
        }
        if rst {
            step = step.rst();
        }
        if !text.is_empty() {
            step = step.psh();
        }
        let mut frame = Vec::new();
        step.write(&mut frame, text).unwrap();
        frame
    }

    /// The peer's next text.
    fn text(&mut self, text: &[u8]) -> Vec<u8> {
        let frame = self.frame(self.snd_nxt, Some(self.rcv_nxt), false, false, false, text);
        self.snd_nxt = self.snd_nxt.wrapping_add(u32::try_from(text.len()).unwrap());
        frame
    }

    /// The peer's FIN.
    fn fin(&mut self) -> Vec<u8> {
        let frame = self.frame(self.snd_nxt, Some(self.rcv_nxt), false, true, false, &[]);
        self.snd_nxt = self.snd_nxt.wrapping_add(1);
        frame
    }

    /// A reset at exactly the sequence number the node expects (RFC 9293 §3.10.7.4, first check).
    fn rst(&self) -> Vec<u8> {
        self.frame(self.snd_nxt, Some(self.rcv_nxt), false, false, true, &[])
    }

    /// How many bytes the node's window still lets the peer send.
    fn open(&self) -> usize {
        self.acked.wrapping_add(u32::from(self.window)).wrapping_sub(self.snd_nxt) as usize
    }

    /// The outside reading of a frame the node emitted, and the frames it is answered with.
    /// Ethernet II from the node's MAC; then ARP, whose requests for this peer and the router
    /// are answered; a datagram of the DHCP client's, which `lease.rs` reads; or a TCP segment
    /// to port 80, as `common::segment` reads it, heard if it is to this peer's address.
    fn hears(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        if let Some(segment) = common::segment(frame) {
            assert_eq!(segment.to_port, PORT);
            if segment.to != self.addr || (self.parked && segment.from_port != self.port) {
                return Vec::new();
            }
            assert_eq!(segment.to_mac, self.mac, "to the peer's MAC");
            self.segments.push(segment.clone());
            if segment.syn {
                self.port = segment.from_port;
                self.received.clear();
                self.fin = false;
            } else if segment.from_port != self.port {
                return Vec::new();
            }
            return self.answer(&segment);
        }
        let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
        let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
        assert_eq!(ethernet.source(), MAC, "from the node's MAC");
        match (&packet.net, &packet.transport) {
            (Some(NetSlice::Arp(asked)), None) => {
                let asks = asked.operation() == ArpOperation::REQUEST && v4(asked.sender_protocol_addr()) == A;
                match v4(asked.target_protocol_addr()) {
                    target if asks && target == self.addr && !self.absent => vec![arp(MAC, false, self.mac, self.addr, A)],
                    target if asks && target == R => vec![arp(MAC, false, MAC_R, R, A)],
                    _ => Vec::new(),
                }
            }
            (Some(NetSlice::Ipv4(_)), Some(TransportSlice::Udp(_))) => Vec::new(),
            other => panic!("neither ARP nor UDP: {other:?}"),
        }
    }

    fn answer(&mut self, segment: &Segment) -> Vec<Vec<u8>> {
        if segment.rst {
            self.resets.push(segment.clone());
            return Vec::new();
        }
        if let Some(ack) = segment.ack {
            (self.acked, self.window) = (ack, segment.window);
        }
        match self.manner {
            Manner::Deaf => return Vec::new(),
            Manner::Refuses => return vec![self.frame(0, Some(segment.seq.wrapping_add(1)), false, false, true, &[])],
            Manner::Answers => {}
        }
        if segment.syn {
            self.rcv_nxt = segment.seq.wrapping_add(1);
            self.snd_nxt = ISS.wrapping_add(1);
            return vec![self.frame(ISS, Some(self.rcv_nxt), true, false, false, &[])];
        }
        if segment.seq == self.rcv_nxt {
            self.received.extend_from_slice(&segment.text);
            self.rcv_nxt = self.rcv_nxt.wrapping_add(u32::try_from(segment.text.len()).unwrap());
            if segment.fin {
                self.fin = true;
                self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            }
        }
        if segment.text.is_empty() && !segment.fin {
            return Vec::new();
        }
        vec![self.frame(self.snd_nxt, Some(self.rcv_nxt), false, false, false, &[])]
    }

    /// The lengths of the node's segments that carried text, in order.
    fn texts(&self) -> Vec<usize> {
        self.segments.iter().filter(|segment| !segment.text.is_empty()).map(|segment| segment.text.len()).collect()
    }
}

// ---- the node between them ----

struct Net {
    node: Node,
    link: toyos_net_node::IfIndex,
    now: Instant,
    draws: u32,
    far: Far,
    /// The peers the test turned from, each with its connection.
    parked: Vec<Far>,
    /// Each stream's client, whose rings [`Self::bridge`] reads.
    clients: Vec<(StreamId, Rc<Client>)>,
}

fn draw(draws: &mut u32) -> impl FnMut() -> u32 + '_ {
    move || {
        *draws = draws.wrapping_add(1);
        *draws
    }
}

impl Net {
    /// The node holding its lease of 192.0.2.1/24 for an hour.
    fn new() -> Self {
        let Wire { node, link, now, .. } = Wire::leased(&terms(3_600, Some(R)));
        Self { node, link, now, draws: 0x7c00_0000, far: Far::new(), parked: Vec::new(), clients: Vec::new() }
    }

    /// Leaves the present peer with the connection it has and makes `next` the peer the test
    /// talks to and connects to.
    fn turn_to(&mut self, next: Far) {
        let mut far = std::mem::replace(&mut self.far, next);
        far.parked = true;
        self.parked.push(far);
    }

    /// What the peers answer a frame the node emitted with. Two at one address give an ARP
    /// request the same answer, which is one frame. A segment to an address no peer is at is
    /// the node's mistake, and no peer would hear it.
    fn hears(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        if let Some(Segment { to, .. }) = common::segment(frame) {
            assert!(self.far.addr == to || self.parked.iter().any(|far| far.addr == to), "a segment to {to}, where no peer is");
        }
        let mut answers = self.far.hears(frame);
        for far in &mut self.parked {
            for answer in far.hears(frame) {
                if !answers.contains(&answer) {
                    answers.push(answer);
                }
            }
        }
        answers
    }

    /// Offers the node all the credit it wants and delivers what each frame is answered with,
    /// until it sends nothing more.
    fn pump(&mut self) {
        for _ in 0..100_000 {
            if self.opportunity() == 0 {
                return;
            }
        }
        panic!("100,000 opportunities and the node still sends");
    }

    /// One transmit opportunity with all the credit the node wants, each frame answered once
    /// the opportunity is over. Returns how many frames left.
    fn opportunity(&mut self) -> usize {
        let mut frames = Vec::new();
        self.node.transmit(self.now, usize::MAX, |_, frame| frames.push(frame.to_vec()), draw(&mut self.draws));
        for frame in &frames {
            for answer in self.hears(frame) {
                self.node.receive(self.now, self.link, BUDGET, batch(&[&answer]), draw(&mut self.draws));
            }
        }
        frames.len()
    }

    fn deliver(&mut self, frame: &[u8]) {
        self.node.receive(self.now, self.link, BUDGET, batch(&[frame]), draw(&mut self.draws));
        self.pump();
    }

    /// A pass, as netstack runs one when the connections it watches answer, each stream whose
    /// client rang among them, and what it sends.
    fn bridge(&mut self) {
        let rung: Vec<StreamId> = self.clients.iter().filter(|(_, client)| client.rang()).map(|(id, _)| *id).collect();
        self.node.bridge(self.now, rung);
        self.pump();
    }

    /// Moves the clock to `at` and fires what is due.
    fn fire(&mut self, at: Instant) {
        self.now = self.now.max(at);
        self.node.fire(self.now, draw(&mut self.draws));
        self.pump();
    }

    /// Fires deadline after deadline until `done`, giving up at the first deadline more than
    /// `limit` away.
    fn run_until(&mut self, limit: Duration, done: impl Fn(&Net) -> bool) -> bool {
        let end = self.now.after(limit);
        for _ in 0..100_000 {
            if done(self) {
                return true;
            }
            match self.node.next_deadline() {
                Some(at) if at <= end => self.fire(at),
                _ => return false,
            }
        }
        panic!("100,000 deadlines without the clock passing {limit:?}");
    }

    fn connect(&mut self, timeout: Option<Duration>) -> (StreamId, Rc<Client>) {
        self.connect_as(Client::new(), timeout)
    }

    /// A connect on a client's region, set up as `made` set it.
    fn connect_as(&mut self, made: (Rc<Client>, Box<dyn toyos_net_node::Session>), timeout: Option<Duration>) -> (StreamId, Rc<Client>) {
        let (client, session) = made;
        let remote = Endpoint { addr: self.far.addr, ..peer() };
        let id = self.node.connect(self.now, remote, timeout, session).expect("a route to the peer");
        self.clients.push((id, client.clone()));
        self.pump();
        (id, client)
    }

    fn events(&mut self) -> Vec<StreamEvent> {
        self.node.drain_stream_events().collect()
    }

    /// The sequence number of the node's first byte of text: its SYN's, plus one.
    fn first(&self) -> u32 {
        let syn = self.far.segments.iter().find(|segment| segment.syn).expect("a SYN");
        syn.seq.wrapping_add(1)
    }

    /// The client leaves: its connection's hang-up, and what the node sends.
    fn leaves(&mut self, id: StreamId) {
        self.node.left(self.now, id);
        self.pump();
    }

    /// Everything the client writes of `text`, a pass after each ring, until it is all in the
    /// send ring.
    fn writes(&mut self, client: &Client, text: &[u8]) {
        let mut written = 0;
        while written < text.len() {
            written += client.write(&text[written..]).expect("room in the send ring");
            self.bridge();
        }
    }
}

/// A stream whose connect was answered `Connected`.
fn established() -> (Net, StreamId, Rc<Client>) {
    established_within(None)
}

/// A stream whose connect, with `timeout`, was answered `Connected`.
fn established_within(timeout: Option<Duration>) -> (Net, StreamId, Rc<Client>) {
    let mut net = Net::new();
    let (id, client) = net.connect(timeout);
    let local = Endpoint { addr: A, port: Port::new(net.far.port).expect("the SYN's source port") };
    assert_eq!(net.events(), [StreamEvent::Connected { id, local }]);
    (net, id, client)
}

// ---- connect ----

// RFC 9293 §3.5, figure 6: a SYN that acknowledges nothing, and after the peer's SYN-ACK an ACK
// of its sequence number plus one from ours plus one.
#[test]
fn a_connect_is_a_handshake_and_its_answer_names_the_address_and_port() {
    let (net, _, client) = established();
    let [syn, ack] = &net.far.segments[..] else { panic!("a SYN and an ACK, not {:?}", net.far.segments) };
    assert!(syn.syn && syn.ack.is_none() && syn.text.is_empty() && !syn.fin && !syn.rst, "{syn:?}");
    assert_eq!((ack.syn, ack.seq, ack.ack, ack.text.len()), (false, syn.seq.wrapping_add(1), Some(ISS + 1), 0));
    assert_eq!((net.node.streams(), client.dropped()), (1, false));
}

#[test]
fn a_connect_before_the_lease_is_refused_and_sends_nothing() {
    let mut wire = Wire::new();
    wire.link(true);
    let (client, session) = Client::new();
    let refused = wire.node.connect(wire.now, peer(), None, session);
    assert!(matches!(refused, Err(ConnectRefused::Stack(ConnectError::Route(_)))), "{refused:?}");
    assert_eq!((wire.node.streams(), client.dropped()), (0, true));
    // `common::outside` takes no TCP segment.
    wire.pump();
}

// RFC 9293 §3.10.7.3: a reset that acknowledges the SYN, in SYN-SENT, is "connection refused".
#[test]
fn a_refused_connect_is_answered_with_the_refusal() {
    let mut net = Net::new();
    net.far.manner = Manner::Refuses;
    let (id, client) = net.connect(None);
    assert_eq!(net.events(), [StreamEvent::Failed { id, failure: Failure::Refused }]);
    assert_eq!((net.node.streams(), client.dropped()), (0, true));
}

// The deadline is the node's own and falls between two of the SYN's retransmissions; RFC 9293
// §3.10.5: an abort in SYN-SENT sends nothing.
#[test]
fn a_connect_past_its_deadline_is_timed_out_at_the_deadline() {
    let mut net = Net::new();
    net.far.manner = Manner::Deaf;
    let start = net.now;
    let (id, client) = net.connect(Some(Duration::from_millis(2_500)));
    assert_eq!(net.node.streams(), 1);
    assert!(net.run_until(Duration::from_secs(10), |net| net.node.streams() == 0), "the connect ends");
    assert_eq!(net.now, start.after(Duration::from_millis(2_500)));
    assert_eq!(net.events(), [StreamEvent::TimedOut { id }]);
    assert!(client.dropped());
    assert!(!net.far.segments.is_empty() && net.far.segments.iter().all(|segment| segment.syn), "{:?}", net.far.segments);
    // The connection is nobody's from here: its SYN is not sent again.
    let sent = net.far.segments.len();
    assert!(!net.run_until(Duration::from_secs(60), |_| false));
    assert_eq!((net.far.segments.len(), net.far.resets.len()), (sent, 0));
}

// The peer's address answers no ARP request, so the connect's next hop fails inside a transmit
// opportunity: the answer is there when that opportunity is over, with no frame and no deadline
// of the stream's own to bring it.
#[test]
fn a_connect_nobody_answers_arp_for_fails_when_tcp_knows() {
    let mut net = Net::new();
    net.far.absent = true;
    let renewal = net.now.after(Duration::from_secs(1_700));
    let (id, client) = net.connect(None);
    for _ in 0..10_000 {
        let failed = net.node.shard().tcp_counters().get(Counter::NextHopFailed) > 0;
        assert_eq!(net.node.streams() == 0, failed, "the stream goes in the opportunity that failed its connect");
        if failed {
            break;
        }
        let at = net.node.next_deadline().expect("address resolution is running");
        assert!(at < renewal, "before the lease's renewal");
        net.fire(at);
    }
    let events = net.events();
    assert!(matches!(events[..], [StreamEvent::Failed { id: failed, failure: Failure::Unreachable(_) }] if failed == id), "{events:?}");
    assert_eq!((client.dropped(), net.far.segments.len()), (true, 0));
}

// A connect's deadline ends with its answer: the stream has none while its client holds it.
#[test]
fn an_established_streams_connect_deadline_is_gone() {
    let (mut net, _, _client) = established_within(Some(Duration::from_secs(30)));
    net.fire(net.now.after(Duration::from_secs(31)));
    assert!(net.node.next_deadline().is_none_or(|at| at > net.now), "{:?} at {:?}", net.node.next_deadline(), net.now);
    assert_eq!(net.node.streams(), 1);
}

#[test]
fn a_connect_whose_client_left_before_its_answer_is_answered_closed() {
    let mut net = Net::new();
    net.far.manner = Manner::Deaf;
    let (id, client) = net.connect(None);
    net.leaves(id);
    assert_eq!(net.events(), [StreamEvent::Closed { id }]);
    assert_eq!((net.node.streams(), client.dropped()), (0, true));
    // The connection is nobody's from here: its SYN is not sent again.
    let sent = net.far.segments.len();
    assert!(!net.run_until(Duration::from_secs(60), |_| false));
    assert_eq!((net.far.segments.len(), net.far.resets.len()), (sent, 0));
}

// A batch of frames and a deadline each end in a pass of their own: an opportunity's pass is over
// the connects only, so neither test offers one. The batch's pass is one, after its last frame:
// what the batch brought is one publish, one ring of the client that waits.
#[test]
fn a_batch_of_frames_moves_an_established_stream_once_with_no_opportunity_after_it() {
    let (mut net, _, client) = established();
    assert_eq!(client.read(64), Err(Refused::WouldBlock), "the read arms its want");
    let frames = [net.far.text(b"all "), net.far.text(b"at "), net.far.text(b"once")];
    let rung = client.rung();
    net.node.receive(net.now, net.link, BUDGET, batch(&[&frames[0], &frames[1], &frames[2]]), draw(&mut net.draws));
    assert_eq!(client.rung() - rung, 1, "one ring for the batch");
    assert_eq!(inbox(&client), b"all at once");
}

// A device that refills its ring as fast as it is read never says it has no frame: a batch
// stops at the budget it was given and answers that another pass is owed, and one that found the
// ring empty owes none, a ring one short of the budget included.
#[test]
fn a_batch_stops_at_its_budget_and_says_a_pass_is_owed() {
    let (mut net, _, client) = established();
    let frame = net.far.text(b"again");
    let mut handed = 0usize;
    let endless = |sink: &mut dyn FnMut(&[u8])| {
        handed += 1;
        sink(&frame);
        true
    };
    let owed = net.node.receive(net.now, net.link, BUDGET, endless, draw(&mut net.draws));
    assert_eq!((owed, handed), (true, BUDGET), "the batch is the budget, and a pass is owed");
    assert_eq!(inbox(&client), b"again", "the rest of the batch was the same segment again");

    let next = net.far.text(b"once");
    let short: Vec<&[u8]> = vec![&next; BUDGET - 1];
    assert!(!net.node.receive(net.now, net.link, BUDGET, batch(&short), draw(&mut net.draws)), "the ring was emptied");
    assert_eq!(inbox(&client), b"once");
}

// netstack's want for the client's bytes is armed whenever it took them all and [tcp] has room:
// a write rings it, and the pass the ring brings takes the bytes.
#[test]
fn a_write_rings_netstack_and_the_pass_it_brings_takes_the_bytes() {
    let (mut net, _, client) = established();
    assert_eq!(client.write(b"request"), Ok(7));
    assert!(client.rang(), "netstack wanted the client's bytes");
    net.pump();
    assert!(net.far.received.is_empty(), "nothing is taken before the pass");
    net.node.bridge(net.now, []);
    net.pump();
    assert_eq!(net.far.received, b"request");
    assert_eq!(client.write(b"again"), Ok(5));
    assert!(client.rang(), "armed again by the pass that took the first");
}

// Every pass moves every established stream: a deadline's takes what its client published
// with no ring delivered.
#[test]
fn a_deadline_moves_an_established_stream_with_no_opportunity_after_it() {
    let (mut net, _, client) = established();
    assert_eq!(client.write(b"unrung"), Ok(6));
    let at = net.now.after(Duration::from_secs(1));
    net.node.fire(at, draw(&mut net.draws));
    net.pump();
    assert_eq!(net.far.received, b"unrung");
}

// ---- the stream's bytes ----

// [tcp]'s send buffer is the client's whole send ring: a write larger than the peer's window is
// queued whole, with no copy, and the peer's stream is the client's, byte for byte.
#[test]
fn a_write_larger_than_the_window_is_queued_whole_and_arrives_whole() {
    let (mut net, id, client) = established();
    let sent = text(100_000);
    assert_eq!(client.write(&sent), Ok(sent.len()), "the send ring holds all of it");
    net.node.bridge(net.now, [id]);
    net.pump();
    assert!(net.run_until(Duration::from_secs(5), |net| net.far.received.len() >= sent.len()), "{} bytes arrived", net.far.received.len());
    assert!(net.far.received == sent, "the peer's stream is the client's, byte for byte");
}

// RFC 9293 §3.8.6: the peer sends what the node's window allows, and the client reads it as it
// comes: the client's stream is the peer's, byte for byte, the ring wrapped many times over.
#[test]
fn the_clients_stream_is_the_peers_byte_for_byte() {
    let (mut net, _, client) = established();
    let sent = text(200_000);
    let (mut read, mut offset) = (Vec::new(), 0);
    for _ in 0..10_000 {
        if read.len() == sent.len() {
            break;
        }
        let len = net.far.open().min(1_460).min(sent.len() - offset);
        if len > 0 {
            let frame = net.far.text(&sent[offset..offset + len]);
            offset += len;
            net.deliver(&frame);
            continue;
        }
        let taken = inbox(&client);
        if taken.is_empty() {
            let at = net.node.next_deadline().expect("the lease's renewal, if nothing else");
            net.fire(at);
        } else {
            read.extend(taken);
            net.bridge();
        }
    }
    assert_eq!(offset, sent.len(), "the window let all of it through");
    assert!(read == sent, "the client's stream is the peer's, byte for byte: {} of {} bytes", read.len(), sent.len());
}

// RFC 9293 §3.8.6.2.2: text the client has not read shuts the window, and its read, with the
// window last offered below the threshold, rings netstack, which opens it at the next
// opportunity.
#[test]
fn a_read_opens_a_shut_window_at_the_next_opportunity() {
    let (mut net, _, client) = established();
    let sent = text(65_535);
    let mut offset = 0;
    for _ in 0..1_000 {
        let len = net.far.open().min(1_460).min(sent.len() - offset);
        if offset == sent.len() {
            break;
        } else if len > 0 {
            let frame = net.far.text(&sent[offset..offset + len]);
            offset += len;
            net.deliver(&frame);
        } else {
            let at = net.node.next_deadline().expect("a delayed acknowledgment");
            net.fire(at);
        }
    }
    assert_eq!(offset, sent.len(), "a window of 65,535 bytes let all of it through");
    let shut = |net: &Net| (net.far.acked, net.far.window) == (net.far.snd_nxt, 0);
    assert!(net.run_until(Duration::from_secs(1), shut), "all of it acknowledged, and the window shut: {} {}", net.far.acked, net.far.window);

    assert!(inbox(&client) == sent);
    assert!(client.rang(), "netstack wanted the room a read gives");
    let rung: Vec<StreamId> = net.clients.iter().map(|(id, _)| *id).collect();
    net.node.bridge(net.now, rung);
    assert_eq!(net.opportunity(), 1, "the window update");
    let update = net.far.segments.last().expect("a segment");
    assert_eq!((update.ack, update.window, update.text.len()), (Some(net.far.snd_nxt), 65_535, 0));
}

// RFC 9293 §3.8.6 and §3.6: an echo of more than both windows moves whole both ways at once,
// each end sending only what the other's window allows, and the peer's FIN and then the client's
// end close it with nothing reset.
#[test]
fn an_echo_past_both_windows_arrives_whole_both_ways_and_ends_in_both_fins() {
    let (mut net, _, client) = established();
    let sent = text(200_000);
    let mut offset = 0;
    let mut echo = Vec::new();
    for _ in 0..100_000 {
        if net.far.received.len() == sent.len() {
            break;
        }
        let len = net.far.open().min(1_460).min(sent.len() - offset);
        if len > 0 {
            let frame = net.far.text(&sent[offset..offset + len]);
            offset += len;
            net.deliver(&frame);
            continue;
        }
        // The client echoes what it reads; with nothing to echo the clock moves.
        echo.extend(inbox(&client));
        let wrote = client.write(&echo).unwrap_or(0);
        echo.drain(..wrote);
        let before = net.far.received.len();
        net.bridge();
        if net.far.received.len() == before && wrote == 0 {
            let at = net.node.next_deadline().expect("the lease's renewal, if nothing else");
            net.fire(at);
        }
    }
    assert_eq!(offset, sent.len(), "the node's window let all of it through");
    assert!(net.far.received == sent, "the echo is the peer's stream byte for byte: {} of {} bytes", net.far.received.len(), sent.len());

    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!(reads(&client), (Vec::new(), Ok(())), "the peer's FIN is the end");
    client.shutdown();
    net.bridge();
    assert!(net.far.fin, "the client's end is a FIN");
    assert_eq!(net.far.acked, net.far.snd_nxt, "the peer's FIN is acknowledged");
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert!(net.far.resets.is_empty(), "{:?}", net.far.resets);
}

// A writer over a full send ring sleeps on its bell, and the room the peer's acknowledgments give
// back rings it there.
#[test]
fn room_released_rings_a_writer_over_a_full_send_ring_on_its_bell() {
    let (mut net, _, client) = established();
    net.far.manner = Manner::Deaf;
    let mut written = 0;
    loop {
        match client.write(&text(1 << 20)) {
            Ok(n) => written += n,
            Err(Refused::WouldBlock) => break,
            Err(other) => panic!("{other:?}"),
        }
        net.bridge();
    }
    assert_eq!(written, toyos::net::stream::SEND_BYTES as usize, "the ring is full");
    let bell = client.bell();
    // The peer acknowledges every byte it heard of the node's.
    let heard = u32::try_from(net.far.segments.iter().map(|segment| segment.text.len()).sum::<usize>()).unwrap();
    let frame = net.far.frame(net.far.snd_nxt, Some(net.first().wrapping_add(heard)), false, false, false, &[]);
    net.deliver(&frame);
    assert!(client.bell() > bell, "the bell is rung for the room");
    assert!(client.write(b"more").is_ok());
}

// ---- the stream's ends ----

// RFC 9293 §3.6, case 1: our FIN follows our last byte, and in FIN-WAIT-2 the peer's text is
// still received; its FIN is acknowledged and ends the stream, which the client reads after the
// text.
#[test]
fn the_end_of_the_clients_writing_is_a_fin_and_the_peer_still_sends() {
    let (mut net, _, client) = established();
    net.writes(&client, b"request");
    client.shutdown();
    net.bridge();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"request"[..], true));
    let fin = net.far.segments.iter().find(|segment| segment.fin).expect("a FIN");
    assert_eq!(fin.seq.wrapping_add(u32::try_from(fin.text.len()).unwrap()), net.first().wrapping_add(7));
    assert_eq!((client.dropped(), net.node.streams()), (false, 1));
    assert_eq!(client.write(b"late"), Err(Refused::Shut));

    let frame = net.far.text(b"response");
    net.deliver(&frame);
    assert_eq!(inbox(&client), b"response");
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert_eq!(reads(&client), (Vec::new(), Ok(())), "the end");
    assert_eq!(net.far.acked, net.far.snd_nxt, "the peer's FIN is acknowledged");
    assert!(net.far.resets.is_empty() && net.events().is_empty());
}

// RFC 9293 §3.6, case 2: the peer's FIN ends the client's reading after its last byte, and the
// client still writes until its own end.
#[test]
fn the_peers_fin_ends_the_clients_reading_and_not_its_writing() {
    let (mut net, _, client) = established();
    let frame = net.far.text(b"all there is");
    net.deliver(&frame);
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!(reads(&client), (b"all there is".to_vec(), Ok(())));
    assert_eq!((client.dropped(), net.node.streams()), (false, 1));

    // CLOSE-WAIT lasts as long as the client likes.
    net.fire(net.now.after(Duration::from_secs(150)));
    assert_eq!((net.node.streams(), net.events()), (1, vec![]));
    net.writes(&client, b"noted");
    client.shutdown();
    net.bridge();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"noted"[..], true));
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert!(net.far.resets.is_empty());
}

// RFC 9293 §3.6: both FINs end the connection, not the client's reading: text it had not read when
// the stream ended is still its to read, and the end comes after it.
#[test]
fn text_unread_when_both_fins_ended_the_connection_is_still_the_clients() {
    let (mut net, _, client) = established();
    client.shutdown();
    net.bridge();
    assert!(net.far.fin);
    let frame = net.far.text(b"held back");
    net.deliver(&frame);
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!(net.far.acked, net.far.snd_nxt, "the peer's FIN is acknowledged");
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert_eq!(reads(&client), (b"held back".to_vec(), Ok(())));
}

#[test]
fn a_shutdown_sends_what_was_written_and_then_the_fin() {
    let (mut net, _, client) = established();
    assert_eq!(client.write(b"written before the shutdown"), Ok(27));
    client.shutdown();
    net.bridge();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"written before the shutdown"[..], true));
    assert_eq!((client.dropped(), net.node.streams()), (false, 1));
}

// RFC 9293 §3.10.7.4 after the client's FIN: a reset is the end the client reads, not a FIN.
#[test]
fn a_reset_after_the_clients_shutdown_is_the_end_the_client_reads() {
    let (mut net, _, client) = established();
    client.shutdown();
    net.bridge();
    assert!(net.far.fin);
    net.far.manner = Manner::Deaf;
    let frame = net.far.rst();
    net.deliver(&frame);
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert_eq!(reads(&client), (Vec::new(), Err(Refused::Failed(RESET))));
}

// A connection cannot move to another address, so one whose address the machine loses ends
// at once, named for it, rather than retransmitting until it gives up: here another host takes
// the address after one defence (RFC 5227 §2.4 (b)). Nothing is sent from the lost address.
#[test]
fn a_stream_whose_address_is_lost_ends_as_address_gone() {
    let (mut net, _, client) = established();
    let conflict = arp([0xff; 6], false, MAC_B, A, A);
    net.fire(net.now.after(Duration::from_secs(100)));
    net.deliver(&conflict);
    net.fire(net.now.after(Duration::from_secs(5)));
    let heard = net.far.segments.len();
    net.deliver(&conflict);
    assert_eq!(net.node.lease(net.link), None, "the premise: the address is lost");
    assert_eq!(reads(&client), (Vec::new(), Err(Refused::Failed(ADDRESS_GONE))));
    assert_eq!(net.far.segments.len(), heard, "no reset from an address the machine gave up");
}

// RFC 9293 §3.10.7.4 and §3.10.2: a reset after the peer's FIN flushes what was queued, and a
// write after it, with room in the send ring, answers the reset and copies nothing.
#[test]
fn a_reset_after_the_peers_fin_fails_the_clients_writes() {
    let (mut net, _, client) = established();
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!(reads(&client), (Vec::new(), Ok(())));
    net.far.manner = Manner::Deaf;
    net.writes(&client, b"late");
    let frame = net.far.rst();
    net.deliver(&frame);
    assert_eq!(client.write(b"later"), Err(Refused::Failed(RESET)));
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
}

// RFC 9293 §3.10.4: the client's leaving is a CLOSE, and what it wrote leaves before the FIN.
#[test]
fn a_client_that_leaves_has_what_it_wrote_sent_and_then_the_fin() {
    let (mut net, id, client) = established();
    assert_eq!(client.write(b"written and left"), Ok(16));
    net.leaves(id);
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"written and left"[..], true));
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert!(net.far.resets.is_empty());
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!(net.far.acked, net.far.snd_nxt, "the peer's FIN is acknowledged by a connection nobody holds");
}

// RFC 9293 §3.6.1 (SHLD-3): a client that leaves with text unread resets, which shows the peer
// it was lost. The rule is [tcp]'s; the node's is to hand it the close.
#[test]
fn a_client_that_leaves_with_text_unread_resets() {
    let (mut net, id, _client) = established();
    let frame = net.far.text(b"never read");
    net.deliver(&frame);
    net.leaves(id);
    assert_eq!((net.far.resets.len(), net.node.streams()), (1, 0));
}

// RFC 9293 §3.10.7.4: a reset in sequence ends the connection, and the client reads it as the
// failure it was.
#[test]
fn a_reset_is_the_end_the_client_reads_and_lets_the_stream_go() {
    let (mut net, _, client) = established();
    net.far.manner = Manner::Deaf;
    net.writes(&client, b"never sent");
    let frame = net.far.rst();
    net.deliver(&frame);
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
    assert_eq!(reads(&client), (Vec::new(), Err(Refused::Failed(RESET))));
    assert!(net.events().is_empty() && net.far.resets.is_empty());
}

// Text and the reset after it in one batch: the client reads the text before the failure, as it
// would had they come in passes of their own.
#[test]
fn a_reset_in_the_batch_of_the_text_before_it_is_read_after_the_text() {
    let (mut net, _, client) = established();
    let text = net.far.text(b"last");
    let rst = net.far.rst();
    net.node.receive(net.now, net.link, BUDGET, batch(&[&text, &rst]), draw(&mut net.draws));
    assert_eq!(reads(&client), (b"last".to_vec(), Err(Refused::Failed(RESET))));
}

// A writer asleep over a full send ring wakes for a reset (ruling 2): the failure makes the send
// direction ready and rings its bell, and the write answers it.
#[test]
fn a_reset_rings_a_writer_over_a_full_send_ring_on_its_bell() {
    let (mut net, _, client) = established();
    net.far.manner = Manner::Deaf;
    while client.write(&text(1 << 20)).is_ok() {
        net.bridge();
    }
    assert_eq!(client.write(b"x"), Err(Refused::WouldBlock), "full, and the writer's want armed");
    let bell = client.bell();
    let frame = net.far.rst();
    net.deliver(&frame);
    assert!(client.bell() > bell, "the reset rings the bell");
    assert_eq!(client.write(b"x"), Err(Refused::Failed(RESET)));
}

// ---- a ring back ----

// A source registered while its send ring has room rings netstack, which rings it back on the
// bell (amendment D): its first `WRITABLE` event comes.
#[test]
fn a_source_registered_with_room_is_rung_back_on_its_bell() {
    let (mut net, _, client) = established();
    let bell = client.bell();
    client.register();
    assert!(client.rang());
    let rung: Vec<StreamId> = net.clients.iter().map(|(id, _)| *id).collect();
    net.node.bridge(net.now, rung);
    assert_eq!(client.bell(), bell + 1);
}

// ---- a hostile client ----

// A head the client stored past what was published is a violation of its own stream alone: it
// is reset, with RFC 9293 §3.10.5's reset, and another client's is untouched.
#[test]
fn a_head_past_what_was_published_resets_that_stream_alone() {
    let (mut net, id, client) = established();
    net.turn_to(Far::at(C, MAC_C));
    let (other, bystander) = net.connect(None);
    net.events();
    client.word(RX_HEAD).store(7, Ordering::Release);
    net.bridge();
    assert_eq!(net.events(), [StreamEvent::Violated { id, violation: Violation::HeadPastTail }]);
    assert_eq!((client.dropped(), bystander.dropped(), net.node.streams()), (true, false, 1));
    let reset = net.parked[0].resets.first().expect("the reset");
    assert_eq!(reset.seq, net.parked[0].segments.iter().find(|segment| segment.syn).unwrap().seq.wrapping_add(1));
    net.writes(&bystander, b"still here");
    assert_eq!(net.far.received, b"still here");
    let _ = other;
}

// A tail the client stored past its ring's room is a violation of its own stream alone.
#[test]
fn a_tail_past_the_send_rings_room_resets_that_stream() {
    let (mut net, id, client) = established();
    client.word(TX_TAIL).store(u32::MAX - 3, Ordering::Release);
    client.region_ring();
    net.bridge();
    assert_eq!(net.events(), [StreamEvent::Violated { id, violation: Violation::TailPastDepth }]);
    assert_eq!((client.dropped(), net.node.streams(), net.far.resets.len()), (true, 0, 1));
}

// Text published after the client's own shutdown is past the FIN [tcp] queued: a violation.
#[test]
fn text_published_after_the_shutdown_resets_that_stream() {
    let (mut net, id, client) = established();
    client.shutdown();
    net.bridge();
    assert!(net.far.fin);
    client.word(TX_TAIL).store(5, Ordering::Release);
    client.region_ring();
    net.bridge();
    assert_eq!(net.events(), [StreamEvent::Violated { id, violation: Violation::TailPastDepth }]);
    assert_eq!((client.dropped(), net.node.streams()), (true, 0));
}

// ---- options ----

// RFC 9293 §3.7.4: with text unacknowledged a short segment waits (TX-02), and with Nagle's
// algorithm off it leaves (TX-03).
#[test]
fn nodelay_reaches_the_stack() {
    let (mut net, _, client) = established();
    net.far.manner = Manner::Deaf;
    net.writes(&client, &text(100));
    assert_eq!(net.far.texts(), [100]);
    net.writes(&client, &text(50));
    assert_eq!(net.far.texts(), [100], "the second write waits for the first's acknowledgment");
    client.set_nodelay(true);
    net.bridge();
    assert_eq!(net.far.texts(), [100, 50]);
}

// A client's `nodelay` written before its connect holds from the first segment.
#[test]
fn nodelay_set_before_the_connect_holds_from_the_first_segment() {
    let mut net = Net::new();
    let (_, client) = net.connect_as(Client::nodelay(), None);
    net.events();
    net.far.manner = Manner::Deaf;
    net.writes(&client, &text(100));
    net.writes(&client, &text(50));
    assert_eq!(net.far.texts(), [100, 50]);
}

// ---- the wire is not trusted ----

/// An established stream, and a frame of the peer's carrying `text`.
fn expecting(text: &[u8]) -> (Net, Rc<Client>, Vec<u8>) {
    let (mut net, _, client) = established();
    let frame = net.far.text(text);
    (net, client, frame)
}

#[test]
fn no_cut_of_a_segment_is_a_segment() {
    let (_, _, frame) = expecting(b"whole or nothing");
    for len in 0..frame.len() {
        let (mut net, client, frame) = expecting(b"whole or nothing");
        net.deliver(&frame[..len]);
        assert!(inbox(&client).is_empty(), "cut to {len} bytes");
        net.deliver(&frame);
        assert_eq!(inbox(&client), b"whole or nothing", "whole, after the cut to {len} bytes");
    }
}

#[test]
fn no_flipped_bit_of_a_segment_delivers_other_bytes() {
    let (_, _, frame) = expecting(b"whole or nothing");
    for index in 0..frame.len() {
        for bit in 0..8 {
            let (mut net, client, mut frame) = expecting(b"whole or nothing");
            frame[index] ^= 1 << bit;
            net.deliver(&frame);
            // A flip no checksum covers is in the link addresses: taken or refused, the client
            // reads the peer's text or nothing.
            let inbox = inbox(&client);
            assert!(inbox.is_empty() || inbox == b"whole or nothing", "byte {index} bit {bit}: {inbox:?}");
            assert_eq!(net.node.streams(), 1, "byte {index} bit {bit}");
        }
    }
}

// ---- holders ----

/// The second holder of a stream.
fn second() -> Holder {
    Holder::new(2).unwrap()
}

// A holder past a stream's first takes a place of its own, so netstack's bound on what clients
// make it hold holds for holders too; past the places an export makes nothing.
#[test]
fn a_holder_past_the_first_takes_a_place_and_none_is_made_past_the_places() {
    let (mut net, id, _client) = established();
    net.node.set_places(2);
    assert_eq!(net.node.held(), 1);
    let (remote, local) = net.node.export(id).expect("a place for the second holder");
    assert_eq!(remote, peer(), "the holder is told the stream's peer");
    assert_eq!(local.addr, A, "and the address the stream left from");
    assert_eq!(net.node.held(), 2);
    assert_eq!(net.node.export(id), Err(ExportRefused::Full));
    assert_eq!(net.node.held(), 2, "a refused export took nothing");
    net.node.departed(net.now, id, second());
    assert_eq!((net.node.held(), net.node.streams()), (1, 1), "the departed holder's place is back, and the stream stays");
}

// A holder that dies holding the send ring's gate, while another holder waits for it, has the
// gate let go and the waiter rung: the stream goes on for the holder that stays.
#[test]
fn a_holder_that_left_holding_the_gate_lets_the_one_that_stays_write() {
    let (mut net, id, first) = established();
    net.node.export(id).expect("a place for the second holder");
    let _second = first.holder(2);
    first.word(TX_GATE).store(2, Ordering::Release);
    assert_eq!(first.write(b"after"), Err(Refused::WouldBlock), "the gate is the second holder's");
    let bell = first.bell();
    net.node.departed(net.now, id, second());
    assert!(first.bell() > bell, "the departure rang the holder waiting for the gate");
    net.writes(&first, b"after");
    assert_eq!(net.far.received, b"after");
    assert!(!net.far.fin, "a holder stays");
}

// Bytes that arrive ring every holder that waits for them, and the one that reads first takes
// them.
#[test]
fn bytes_that_arrive_ring_every_holder_waiting_for_them() {
    let (mut net, id, first) = established();
    net.node.export(id).expect("a place for the second holder");
    let other = first.holder(2);
    assert_eq!(first.read(8), Err(Refused::WouldBlock));
    assert_eq!(other.read(8), Err(Refused::WouldBlock));
    let frame = net.far.text(b"hi");
    net.deliver(&frame);
    assert!(first.receive_bell() > 0 && other.receive_bell() > 0, "a holder waiting for the bytes was not rung");
    assert_eq!(other.read(8), Ok(b"hi".to_vec()));
    assert_eq!(first.read(8), Err(Refused::WouldBlock), "bytes another holder took");
}

// The last holder's leaving is the stream's: what was written is sent, then the FIN.
#[test]
fn the_last_holder_leaving_is_the_streams_close() {
    let (mut net, id, first) = established();
    net.node.export(id).expect("a place for the second holder");
    let other = first.holder(2);
    assert_eq!(other.write(b"from the second"), Ok(15));
    net.node.departed(net.now, id, second());
    net.pump();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"from the second"[..], false), "the departure's pass sent what the holder published");
    net.node.departed(net.now, id, Holder::FIRST);
    net.pump();
    assert!(net.far.fin, "the last holder left");
    assert_eq!((first.dropped(), net.node.streams()), (true, 0));
}
