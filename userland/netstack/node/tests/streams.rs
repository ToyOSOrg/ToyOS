//! Streams on the node, against a peer the test scripts at 192.0.2.2:80 and pipe ends it fakes.
//! No scenario ids: the specifications' TCP scenarios are `toyos-net-tcp`'s; these are what the
//! node does between [tcp] and a client's two pipes.
//!
//! What is not ours: every segment the node emits is read by `etherparse` as it leaves
//! ([`Far::hears`]), its IPv4 and TCP checksums and lengths that crate's sums, and every segment
//! the peer sends is built by it. The order and numbers each test asserts are RFC 9293's, cited
//! where asserted. The scripted peer itself is ours: it acknowledges what arrives in order and
//! says only what a test tells it to.

mod common;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::time::Duration;

use common::{arp, terms, Wire, A, MAC, MAC_B, MAC_R, R};
use etherparse::{ArpOperation, LinkSlice, NetSlice, PacketBuilder, SlicedPacket, TcpOptionElement, TransportSlice};
use toyos_net_node::{ConnectRefused, FromClient, Node, PipeEnd, PipeRefusal, Pipes, StreamEvent, StreamId, ToClient, Watch};
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{Endpoint, Failure};
use toyos_net_wire::{Instant, Port};

const B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);
const PORT: u16 = 80;
/// The peer's initial sequence number.
const ISS: u32 = 5000;
/// [tcp]'s send buffer, as `common::Wire` configures it.
const SEND_BUFFER: usize = 65_535;

fn peer() -> Endpoint {
    Endpoint { addr: B, port: Port::new(PORT).unwrap() }
}

/// Text that repeats at no power of two.
fn text(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8 ^ (i / 251) as u8).collect()
}

// ---- the client's pipes ----

#[derive(Default)]
struct Ends {
    /// What the client wrote and the node has not read.
    outbox: VecDeque<u8>,
    /// The client holds no write end: the outbox, once empty, reads as the end.
    writer_gone: bool,
    /// What the node wrote and the client has not read: at most `room` bytes.
    inbox: Vec<u8>,
    room: usize,
    /// The client holds no read end.
    reader_gone: bool,
    /// The handle the client gave for an end is no pipe end.
    write_broken: bool,
    read_broken: bool,
    to_dropped: bool,
    from_dropped: bool,
}

type Client = Rc<RefCell<Ends>>;

struct WriteEnd(Client);
struct ReadEnd(Client);

impl ToClient for WriteEnd {
    fn write(&mut self, bytes: &[u8]) -> Result<usize, PipeRefusal> {
        let mut ends = self.0.borrow_mut();
        if ends.write_broken {
            return Err(PipeRefusal::Broken);
        }
        if ends.reader_gone {
            return Err(PipeRefusal::Gone);
        }
        let taken = bytes.len().min(ends.room - ends.inbox.len());
        if taken == 0 {
            return Err(PipeRefusal::WouldBlock);
        }
        ends.inbox.extend_from_slice(&bytes[..taken]);
        Ok(taken)
    }
}

impl Drop for WriteEnd {
    fn drop(&mut self) {
        self.0.borrow_mut().to_dropped = true;
    }
}

impl FromClient for ReadEnd {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, PipeRefusal> {
        let mut ends = self.0.borrow_mut();
        if ends.read_broken {
            return Err(PipeRefusal::Broken);
        }
        let read = out.len().min(ends.outbox.len());
        if read == 0 {
            return if ends.writer_gone { Ok(0) } else { Err(PipeRefusal::WouldBlock) };
        }
        for slot in &mut out[..read] {
            *slot = ends.outbox.pop_front().unwrap();
        }
        Ok(read)
    }
}

impl Drop for ReadEnd {
    fn drop(&mut self) {
        self.0.borrow_mut().from_dropped = true;
    }
}

/// A client whose receive pipe holds 65,536 bytes, and the two ends it hands over.
fn client() -> (Client, Pipes) {
    let client = Rc::new(RefCell::new(Ends { room: 65_536, ..Ends::default() }));
    let pipes = Pipes { to_client: Box::new(WriteEnd(client.clone())), from_client: Box::new(ReadEnd(client.clone())) };
    (client, pipes)
}

fn dropped(client: &Client) -> (bool, bool) {
    let ends = client.borrow();
    (ends.to_dropped, ends.from_dropped)
}

// ---- the far end ----

/// A segment the node emitted, as `etherparse` read it.
#[derive(Clone, Debug)]
struct Segment {
    seq: u32,
    ack: Option<u32>,
    syn: bool,
    fin: bool,
    rst: bool,
    window: u16,
    text: Vec<u8>,
}

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
    manner: Manner,
    /// The node's port, from its last segment.
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
        Self { manner: Manner::Answers, port: 0, snd_nxt: ISS, rcv_nxt: 0, acked: 0, window: 0, received: Vec::new(), fin: false, resets: Vec::new(), segments: Vec::new() }
    }

    /// A segment from the peer in its frame, built by `etherparse`: window 65,535, PSH with text.
    fn frame(&self, seq: u32, ack: Option<u32>, syn: bool, fin: bool, rst: bool, text: &[u8]) -> Vec<u8> {
        let mut step = PacketBuilder::ethernet2(MAC_B, MAC).ipv4(B.octets(), A.octets(), 64).tcp(PORT, self.port, seq, 65_535);
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
    /// Ethernet II from the node's MAC; then ARP, whose requests for the peer and the router are
    /// answered; a datagram of the DHCP client's, which `lease.rs` reads; or a TCP segment to the
    /// peer, with the lengths and checksums `etherparse` computes.
    fn hears(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        let packet = SlicedPacket::from_ethernet(frame).expect("Ethernet II");
        let Some(LinkSlice::Ethernet2(ethernet)) = &packet.link else { panic!("{:?}", packet.link) };
        assert_eq!(ethernet.source(), MAC, "from the node's MAC");
        match (&packet.net, &packet.transport) {
            (Some(NetSlice::Arp(asked)), None) => {
                let asks = asked.operation() == ArpOperation::REQUEST && v4(asked.sender_protocol_addr()) == A;
                match v4(asked.target_protocol_addr()) {
                    target if asks && target == B => vec![arp(MAC, false, MAC_B, B, A)],
                    target if asks && target == R => vec![arp(MAC, false, MAC_R, R, A)],
                    _ => Vec::new(),
                }
            }
            (Some(NetSlice::Ipv4(_)), Some(TransportSlice::Udp(_))) => Vec::new(),
            (Some(NetSlice::Ipv4(ip)), Some(TransportSlice::Tcp(tcp))) => {
                let header = ip.header();
                assert_eq!(ethernet.destination(), MAC_B, "to the peer's MAC");
                assert_eq!(header.header_checksum(), header.to_header().calc_header_checksum(), "the IPv4 header checksum");
                assert_eq!(usize::from(header.total_len()), 20 + tcp.slice().len(), "IPv4's length is the segment's");
                assert_eq!((header.source_addr(), header.destination_addr()), (A, B));
                assert_eq!(tcp.checksum(), tcp.calc_checksum_ipv4(header.source(), header.destination()).unwrap(), "the TCP checksum");
                assert_eq!(tcp.destination_port(), PORT);
                self.port = tcp.source_port();
                let segment = Segment {
                    seq: tcp.sequence_number(),
                    ack: tcp.ack().then(|| tcp.acknowledgment_number()),
                    syn: tcp.syn(),
                    fin: tcp.fin(),
                    rst: tcp.rst(),
                    window: tcp.window_size(),
                    text: tcp.payload().to_vec(),
                };
                self.segments.push(segment.clone());
                self.answer(&segment)
            }
            other => panic!("neither ARP, UDP nor TCP: {other:?}"),
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
    now: Instant,
    draws: u32,
    far: Far,
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
        let Wire { node, now, .. } = Wire::leased(&terms(3_600, Some(R)));
        Self { node, now, draws: 0x7c00_0000, far: Far::new() }
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
        self.node.transmit(self.now, usize::MAX, |frame| frames.push(frame.to_vec()));
        for frame in &frames {
            for answer in self.far.hears(frame) {
                self.node.receive(self.now, &answer, draw(&mut self.draws));
            }
        }
        frames.len()
    }

    fn deliver(&mut self, frame: &[u8]) {
        self.node.receive(self.now, frame, draw(&mut self.draws));
        self.pump();
    }

    /// A pass, as netstack runs one when a pipe it watches is ready, and what it sends.
    fn bridge(&mut self) {
        self.node.bridge(self.now);
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

    fn connect(&mut self, timeout: Option<Duration>) -> (StreamId, Client) {
        let (client, pipes) = client();
        let id = self.node.connect(self.now, peer(), timeout, pipes).expect("a route to the peer");
        self.pump();
        (id, client)
    }

    fn events(&mut self) -> Vec<StreamEvent> {
        self.node.drain_stream_events().collect()
    }

    fn watch(&self, id: StreamId) -> Option<Watch> {
        self.node.watches().find(|(watched, _)| *watched == id).map(|(_, watch)| watch)
    }

    /// The sequence number of the node's first byte of text: its SYN's, plus one.
    fn first(&self) -> u32 {
        let syn = self.far.segments.iter().find(|segment| segment.syn).expect("a SYN");
        syn.seq.wrapping_add(1)
    }
}

/// A stream whose connect was answered `Connected`.
fn established() -> (Net, StreamId, Client) {
    let mut net = Net::new();
    let (id, client) = net.connect(None);
    let local = Port::new(net.far.port).expect("the SYN's source port");
    assert_eq!(net.events(), [StreamEvent::Connected { id, local }]);
    (net, id, client)
}

const IDLE: Watch = Watch { readable: true, writer: true, writable: false, reader: true };

// ---- connect ----

// RFC 9293 §3.5, figure 6: a SYN that acknowledges nothing, and after the peer's SYN-ACK an ACK
// of its sequence number plus one from ours plus one.
#[test]
fn a_connect_is_a_handshake_and_its_answer_names_the_port() {
    let (net, id, client) = established();
    let [syn, ack] = &net.far.segments[..] else { panic!("a SYN and an ACK, not {:?}", net.far.segments) };
    assert!(syn.syn && syn.ack.is_none() && syn.text.is_empty() && !syn.fin && !syn.rst, "{syn:?}");
    assert_eq!((ack.syn, ack.seq, ack.ack, ack.text.len()), (false, syn.seq.wrapping_add(1), Some(ISS + 1), 0));
    assert_eq!((net.node.streams(), net.watch(id), net.node.nodelay(id)), (1, Some(IDLE), Some(false)));
    assert_eq!(dropped(&client), (false, false));
}

#[test]
fn a_connect_before_the_lease_is_refused_and_sends_nothing() {
    let mut wire = Wire::new();
    wire.link(true);
    let (client, pipes) = client();
    let refused = wire.node.connect(wire.now, peer(), None, pipes);
    assert!(matches!(refused, Err(ConnectRefused::Stack(ConnectError::Route(_)))), "{refused:?}");
    assert_eq!((wire.node.streams(), dropped(&client)), (0, (true, true)));
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
    assert_eq!((net.node.streams(), dropped(&client)), (0, (true, true)));
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
    assert_eq!(dropped(&client), (true, true));
    assert!(!net.far.segments.is_empty() && net.far.segments.iter().all(|segment| segment.syn), "{:?}", net.far.segments);
    assert!(net.far.resets.is_empty());
}

#[test]
fn a_connect_closed_before_its_answer_is_answered_closed() {
    let mut net = Net::new();
    net.far.manner = Manner::Deaf;
    let (id, client) = net.connect(None);
    assert!(!net.node.shutdown_write(net.now, id), "not established");
    assert_eq!(net.watch(id), None);
    net.node.close(net.now, id);
    assert_eq!(net.events(), [StreamEvent::Closed { id }]);
    assert_eq!((net.node.streams(), dropped(&client)), (0, (true, true)));
}

// ---- the bridge ----

#[test]
fn nothing_leaves_the_pipe_that_the_stack_will_not_take() {
    let (mut net, id, client) = established();
    let sent = text(100_000);
    client.borrow_mut().outbox.extend(&sent);
    // A pass with no transmit opportunity after it: the send buffer fills and nothing leaves it.
    net.node.bridge(net.now);
    assert_eq!(client.borrow().outbox.len(), sent.len() - SEND_BUFFER, "what [tcp] had no room for is still the pipe's");
    assert_eq!(net.watch(id), Some(Watch { readable: false, ..IDLE }));

    net.pump();
    assert!(net.run_until(Duration::from_secs(5), |net| net.far.received.len() >= sent.len()), "{} bytes arrived", net.far.received.len());
    assert!(net.far.received == sent, "the peer's stream is the client's, byte for byte");
    assert!(client.borrow().outbox.is_empty());
    assert_eq!(net.watch(id), Some(IDLE));
}

#[test]
fn a_full_pipe_keeps_the_rest_in_the_stack_and_is_watched_for_room() {
    let (mut net, id, client) = established();
    client.borrow_mut().room = 100;
    let sent = text(300);
    let frame = net.far.text(&sent);
    net.deliver(&frame);
    for taken in [0, 100, 200] {
        assert!(client.borrow().inbox == sent[taken..taken + 100], "after {taken} bytes");
        // Watched for room only while the stack holds bytes it refused.
        assert_eq!(net.watch(id), Some(Watch { writable: taken < 200, ..IDLE }), "after {taken} bytes");
        client.borrow_mut().inbox.clear();
        net.bridge();
    }
    assert!(client.borrow().inbox.is_empty());
    assert_eq!(net.watch(id), Some(IDLE));
}

#[test]
fn nothing_leaves_the_stack_that_the_pipe_did_not_take() {
    let (mut net, _, client) = established();
    client.borrow_mut().room = 1_000;
    let sent = text(200_000);
    let (mut read, mut offset) = (Vec::new(), 0);
    for _ in 0..10_000 {
        if read.len() == sent.len() {
            break;
        }
        // The peer sends what the node's window allows (RFC 9293 §3.8.6), and no more.
        let len = net.far.open().min(1_460).min(sent.len() - offset);
        if len > 0 {
            let frame = net.far.text(&sent[offset..offset + len]);
            offset += len;
            net.deliver(&frame);
            continue;
        }
        // The window is shut or the text all sent: the client reads what its pipe holds.
        let taken = std::mem::take(&mut client.borrow_mut().inbox);
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

// RFC 9293 §3.8.6.2.2: the window a full pipe shut is opened again by an acknowledgment once the
// pipe has taken what the stack held, at the first opportunity after the pass that moved it.
#[test]
fn room_in_the_pipe_opens_the_window_at_the_next_opportunity() {
    let (mut net, _, client) = established();
    client.borrow_mut().room = 0;
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

    client.borrow_mut().room = 65_536;
    net.node.bridge(net.now);
    assert!(client.borrow().inbox == sent);
    assert_eq!(net.opportunity(), 1, "the window update");
    let update = net.far.segments.last().expect("a segment");
    assert_eq!((update.ack, update.window, update.text.len()), (Some(net.far.snd_nxt), 65_535, 0));
}

// ---- the stream's ends ----

// RFC 9293 §3.6, case 1: our FIN follows our last byte, and in FIN-WAIT-2 the peer's text is
// still received; its FIN is acknowledged and the connection is the stack's to wait out.
#[test]
fn the_end_of_the_clients_writing_is_a_fin_and_the_peer_still_sends() {
    let (mut net, id, client) = established();
    client.borrow_mut().outbox.extend(b"request");
    client.borrow_mut().writer_gone = true;
    net.bridge();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"request"[..], true));
    let fin = net.far.segments.iter().find(|segment| segment.fin).expect("a FIN");
    assert_eq!(fin.seq.wrapping_add(u32::try_from(fin.text.len()).unwrap()), net.first().wrapping_add(7));
    assert_eq!((dropped(&client), net.node.streams()), ((false, true), 1));
    assert_eq!(net.watch(id), Some(Watch { readable: false, writer: false, writable: false, reader: true }));

    let frame = net.far.text(b"response");
    net.deliver(&frame);
    assert_eq!(client.borrow().inbox, b"response");
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!((dropped(&client), net.node.streams()), ((true, true), 0));
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
    assert_eq!(client.borrow().inbox, b"all there is");
    assert_eq!((dropped(&client), net.node.streams()), ((true, false), 1));

    client.borrow_mut().outbox.extend(b"noted");
    client.borrow_mut().writer_gone = true;
    net.bridge();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"noted"[..], true));
    assert_eq!((dropped(&client), net.node.streams()), ((true, true), 0));
    assert!(net.far.resets.is_empty());
}

#[test]
fn a_shutdown_sends_what_the_pipe_held_and_then_the_fin() {
    let (mut net, id, client) = established();
    client.borrow_mut().outbox.extend(b"written before the shutdown");
    assert!(net.node.shutdown_write(net.now, id));
    net.pump();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"written before the shutdown"[..], true));
    assert_eq!((dropped(&client), net.node.streams()), ((false, true), 1));
}

#[test]
fn a_close_sends_what_the_pipe_held_and_then_the_fin() {
    let (mut net, id, client) = established();
    client.borrow_mut().outbox.extend(b"written and dropped");
    net.node.close(net.now, id);
    net.pump();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"written and dropped"[..], true));
    assert_eq!((dropped(&client), net.node.streams()), ((true, true), 0));
    assert!(net.far.resets.is_empty());
    // The id names nothing.
    net.node.close(net.now, id);
    assert_eq!((net.node.nodelay(id), net.node.set_nodelay(net.now, id, true), net.node.shutdown_write(net.now, id)), (None, false, false));
}

// RFC 9293 §3.6.1 (SHLD-3): a close with text unread is a reset, which shows the peer it was
// lost. The rule is [tcp]'s; the node's is to hand it the close.
#[test]
fn a_close_with_text_unread_resets() {
    let (mut net, id, client) = established();
    client.borrow_mut().room = 0;
    let frame = net.far.text(b"never read");
    net.deliver(&frame);
    net.node.close(net.now, id);
    net.pump();
    assert_eq!((net.far.resets.len(), net.node.streams()), (1, 0));
}

// RFC 9293 §3.10.7.4: a reset in sequence ends the connection, and the client learns it as the
// end of both pipes.
#[test]
fn a_reset_ends_both_pipes_and_the_stream() {
    let (mut net, _, client) = established();
    client.borrow_mut().outbox.extend(b"never sent");
    net.far.manner = Manner::Deaf;
    let frame = net.far.rst();
    net.deliver(&frame);
    assert_eq!((dropped(&client), net.node.streams()), ((true, true), 0));
    assert!(net.events().is_empty() && net.far.resets.is_empty());
}

// ---- a client that leaves ----

#[test]
fn a_client_that_left_is_finished_by_the_stack() {
    let (mut net, id, client) = established();
    client.borrow_mut().outbox.extend(b"last words");
    client.borrow_mut().writer_gone = true;
    net.node.pipe_gone(net.now, id, PipeEnd::ToClient);
    assert_eq!((dropped(&client), net.node.streams()), ((true, true), 0), "nothing of the client's is left to serve");
    net.node.pipe_gone(net.now, id, PipeEnd::FromClient);
    net.pump();
    assert_eq!((net.far.received.as_slice(), net.far.fin), (&b"last words"[..], true));
    let frame = net.far.fin();
    net.deliver(&frame);
    assert_eq!(net.far.acked, net.far.snd_nxt, "the peer's FIN is acknowledged by a connection nobody holds");
    assert!(net.far.resets.is_empty() && net.events().is_empty());
}

#[test]
fn a_reader_that_left_is_not_written_to_again() {
    let (mut net, id, client) = established();
    client.borrow_mut().reader_gone = true;
    let frame = net.far.text(b"to nobody");
    net.deliver(&frame);
    assert_eq!((dropped(&client), net.node.streams()), ((true, false), 1), "its writer may still write");
    assert_eq!(net.watch(id), Some(Watch { reader: false, ..IDLE }));
}

// R2 of RFC 9293 §3.8.3, at the 100 seconds it asks for at least, from the client's leaving; the
// reset is §3.10.5's.
#[test]
fn a_departed_clients_unsent_bytes_have_100_seconds() {
    let (mut net, id, client) = established();
    net.far.manner = Manner::Deaf;
    client.borrow_mut().outbox.extend(text(100_000));
    client.borrow_mut().writer_gone = true;
    net.bridge();
    assert_eq!(client.borrow().outbox.len(), 100_000 - SEND_BUFFER);

    net.fire(net.now.after(Duration::from_secs(5)));
    net.node.pipe_gone(net.now, id, PipeEnd::FromClient);
    net.fire(net.now.after(Duration::from_secs(5)));
    let left = net.now;
    net.node.pipe_gone(left, id, PipeEnd::ToClient);
    net.pump();
    assert_eq!((dropped(&client), net.node.streams()), ((true, false), 1));
    assert_eq!(net.watch(id), Some(Watch { readable: false, writer: false, writable: false, reader: false }));

    assert!(net.run_until(Duration::from_secs(200), |net| net.node.streams() == 0), "the stream is let go");
    assert_eq!(net.now, left.after(Duration::from_secs(100)), "from the moment nobody was left");
    assert_eq!(net.events(), [StreamEvent::Cut { id }]);
    assert_eq!((dropped(&client), net.far.resets.len()), ((true, true), 1));
    assert_eq!(client.borrow().outbox.len(), 100_000 - SEND_BUFFER, "and no byte more was taken");
}

// ---- pipes that refuse the node ----

// RFC 9293 §3.10.5: the reset an abort sends is at SND.NXT.
#[test]
fn a_pipe_that_is_no_pipe_resets_its_connection() {
    for write in [true, false] {
        let (mut net, _, client) = established();
        if write {
            client.borrow_mut().write_broken = true;
            let frame = net.far.text(b"to a handle that is no pipe");
            net.deliver(&frame);
        } else {
            client.borrow_mut().read_broken = true;
            net.bridge();
        }
        assert_eq!((dropped(&client), net.node.streams()), ((true, true), 0), "write: {write}");
        let [reset] = &net.far.resets[..] else { panic!("one reset, not {:?}", net.far.resets) };
        assert_eq!(reset.seq, net.first(), "write: {write}");
    }
}

#[test]
fn a_refused_watch_resets_only_a_stream_that_holds_the_end() {
    let (mut net, id, client) = established();
    client.borrow_mut().writer_gone = true;
    net.bridge();
    assert_eq!(dropped(&client), (false, true));
    net.node.pipe_broken(net.now, id, PipeEnd::FromClient);
    net.pump();
    assert_eq!((net.node.streams(), net.far.resets.len()), (1, 0), "the end was let go before its watch answered");
    net.node.pipe_broken(net.now, id, PipeEnd::ToClient);
    net.pump();
    assert_eq!((dropped(&client), net.node.streams(), net.far.resets.len()), ((true, true), 0, 1));
}

// ---- options ----

// RFC 9293 §3.7.4: with text unacknowledged a short segment waits (TX-02), and with Nagle's
// algorithm off it leaves (TX-03).
#[test]
fn nodelay_reaches_the_stack() {
    let (mut net, id, client) = established();
    net.far.manner = Manner::Deaf;
    client.borrow_mut().outbox.extend(text(100));
    net.bridge();
    assert_eq!(net.far.texts(), [100]);
    client.borrow_mut().outbox.extend(text(50));
    net.bridge();
    assert_eq!(net.far.texts(), [100], "the second write waits for the first's acknowledgment");
    assert!(net.node.set_nodelay(net.now, id, true));
    net.pump();
    assert_eq!((net.far.texts(), net.node.nodelay(id)), (vec![100, 50], Some(true)));
}

// ---- the wire is not trusted ----

/// An established stream, and a frame of the peer's carrying `text`.
fn expecting(text: &[u8]) -> (Net, Client, Vec<u8>) {
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
        assert!(client.borrow().inbox.is_empty(), "cut to {len} bytes");
        net.deliver(&frame);
        assert_eq!(client.borrow().inbox, b"whole or nothing", "whole, after the cut to {len} bytes");
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
            let inbox = client.borrow().inbox.clone();
            assert!(inbox.is_empty() || inbox == b"whole or nothing", "byte {index} bit {bit}: {inbox:?}");
            assert_eq!(net.node.streams(), 1, "byte {index} bit {bit}");
        }
    }
}
