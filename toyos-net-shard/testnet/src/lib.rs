//! A fake Ethernet for `toyos-net-shard`'s host tests: N shards on one segment under one moved
//! clock. A frame a node's device pulls goes on the wire record as it leaves, and then each
//! receiver's link carries it: one frame at a time at the link's rate, after its delay, unless a
//! dark window or the link's rule drops, duplicates or holds it — all deterministic, so a run
//! repeats exactly. Serialising spaces a burst out, so a receiver takes its frames one by one as
//! a real link hands them over.
//!
//! One pass at an instant is netstack's loop: every frame due is received, then each node's timers
//! fire, then the applications run, then each node's device offers its credit; passes repeat at
//! that instant until nothing is due. An application writes a stream every byte of which its peer
//! can check where it lands. Ours against ours is a consistency check, never an independent
//! oracle.

#![forbid(unsafe_code)]

mod pcap;

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::Duration;

use toyos_net_shard::{Config, Event, Secrets, Shard};
use toyos_net_tcp::{ConnId, Endpoint, Error, Failure, ListenerId, Received, Tuple};
use toyos_net_wire::ethernet::{Frame, IndividualMac, MacAddr, MacClass};
use toyos_net_wire::{Instant, Port};

/// Passes at one instant before the network is taken to be spinning.
const PASSES_PER_INSTANT: usize = 1_000;
/// What an application hands its stack per call.
const CHUNK: u64 = 65_536;
/// Every link's rate, in bits per second.
const RATE: u64 = 1_000_000_000;

/// What a node's device offers each pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credit {
    Unlimited,
    /// No transmit descriptor is free.
    None,
    /// This many frames in each millisecond of the clock.
    PerMs(usize),
}

/// What a link does with one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    Pass,
    Drop,
    Duplicate,
    /// Delivered after the next frame on this link: the two swap.
    Hold,
}

/// Decides each frame's fate on one link; deterministic in what it is shown.
pub type Rule = Box<dyn FnMut(&[u8]) -> Fate>;

/// One direction between two nodes.
pub struct Link {
    pub delay: Duration,
    /// Everything sent in `[start, end)` is lost.
    pub dark: Option<(Instant, Instant)>,
    pub rule: Option<Rule>,
    held: Option<Vec<u8>>,
    /// When the frame now on the link has gone.
    free: Instant,
}

impl Default for Link {
    fn default() -> Self {
        Self { delay: Duration::from_micros(50), dark: None, rule: None, held: None, free: Instant::from_nanos(0) }
    }
}

impl Link {
    /// Puts `frame` on the link behind the frame before it; when its last bit arrives.
    fn serialise(&mut self, now: Instant, frame: &[u8]) -> Instant {
        let bits = u64::try_from(frame.len()).expect("a length") * 8;
        self.free = self.free.max(now).after(Duration::from_nanos(bits * 1_000_000_000 / RATE));
        self.free.after(self.delay)
    }
}

pub struct Node {
    pub shard: Shard,
    pub mac: MacAddr,
    pub credit: Credit,
    /// What the shard reported, oldest first.
    pub events: Vec<Event>,
    budget: (u64, usize),
}

/// A frame as a node's device put it on the wire.
#[derive(Clone, Debug)]
pub struct Carried {
    pub at: Instant,
    pub from: usize,
    pub frame: Vec<u8>,
}

struct Flight {
    at: Instant,
    order: u64,
    to: usize,
    frame: Vec<u8>,
}

/// How an application's connection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The peer's FIN, after every byte before it.
    Fin,
    Failed(Failure),
}

/// One end of a connection: it writes `len` bytes of its stream, shuts its write side, and checks
/// every byte it reads against its peer's stream.
#[derive(Clone, Debug)]
pub struct App {
    pub node: usize,
    pub id: ConnId,
    pub tuple: Tuple,
    pub len: u64,
    pub written: u64,
    pub read: u64,
    /// The first offset whose byte was not the peer's.
    pub corrupt: Option<u64>,
    pub end: Option<End>,
    pub shut: bool,
}

struct Server {
    node: usize,
    listener: ListenerId,
    len: u64,
}

pub struct Net {
    now: Instant,
    pub nodes: Vec<Node>,
    pub apps: Vec<App>,
    servers: Vec<Server>,
    links: BTreeMap<(usize, usize), Link>,
    flights: Vec<Flight>,
    order: u64,
    wire: Vec<Carried>,
}

/// A distinct, fixed key per node and purpose.
fn key(node: usize, purpose: u8) -> [u8; 16] {
    let n = u8::try_from(node).expect("fewer than 256 nodes");
    core::array::from_fn(|i| n.wrapping_mul(16).wrapping_add(purpose).wrapping_add(u8::try_from(i).expect("16 bytes")))
}

/// The stream one end of `tuple` writes, keyed by the end that writes it.
fn seed(from: Endpoint, to: Endpoint) -> u64 {
    let [a, b, c, d] = from.addr.octets();
    let [e, f, g, h] = to.addr.octets();
    u64::from_be_bytes([a, b, c, d, e, f, g, h]) ^ u64::from(from.port.get()) << 16 ^ u64::from(to.port.get())
}

/// The byte a stream holds at `offset` (SplitMix64's finaliser).
fn stream_byte(seed: u64, offset: u64) -> u8 {
    let mut z = seed.wrapping_add(offset.wrapping_add(1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    (z ^ (z >> 31)).to_le_bytes()[0]
}

impl App {
    fn new(node: usize, id: ConnId, tuple: Tuple, len: u64) -> Self {
        Self { node, id, tuple, len, written: 0, read: 0, corrupt: None, end: None, shut: false }
    }

    fn run(&mut self, shard: &mut Shard, now: Instant) {
        if matches!(self.end, Some(End::Failed(_))) {
            return;
        }
        let ours = seed(self.tuple.local, self.tuple.remote);
        while self.written < self.len {
            let chunk: Vec<u8> = (self.written..self.len.min(self.written + CHUNK)).map(|i| stream_byte(ours, i)).collect();
            match shard.send(now, self.id, &chunk) {
                Ok(n) => self.written += u64::try_from(n).expect("a length"),
                Err(Error::WouldBlock) => break,
                Err(Error::Failed(failure)) => return self.end = Some(End::Failed(failure)),
                Err(e) => panic!("send on {:?}: {e:?}", self.tuple),
            }
        }
        if self.written == self.len && !self.shut {
            shard.shutdown_write(now, self.id).expect("the write side shuts");
            self.shut = true;
        }
        let theirs = seed(self.tuple.remote, self.tuple.local);
        let mut buf = vec![0u8; 65_536];
        while self.end.is_none() {
            match shard.recv(now, self.id, &mut buf) {
                Ok(Received::Data(n)) => {
                    for (i, &byte) in buf[..n].iter().enumerate() {
                        let offset = self.read + u64::try_from(i).expect("an offset");
                        if self.corrupt.is_none() && byte != stream_byte(theirs, offset) {
                            self.corrupt = Some(offset);
                        }
                    }
                    self.read += u64::try_from(n).expect("a length");
                }
                Ok(Received::End) => self.end = Some(End::Fin),
                Err(Error::WouldBlock) => break,
                Err(Error::Failed(failure)) => self.end = Some(End::Failed(failure)),
                Err(e) => panic!("recv on {:?}: {e:?}", self.tuple),
            }
        }
    }
}

impl Net {
    /// An empty segment whose clock starts at `start`.
    pub fn new(start: Instant) -> Self {
        Self {
            now: start,
            nodes: Vec::new(),
            apps: Vec::new(),
            servers: Vec::new(),
            links: BTreeMap::new(),
            flights: Vec::new(),
            order: 0,
            wire: Vec::new(),
        }
    }

    pub fn now(&self) -> Instant {
        self.now
    }

    /// A node with link up and `addr/prefix_len` added: conflict detection runs before the
    /// address is usable. Its secrets are fixed per node, so a run repeats.
    pub fn add_node(&mut self, mac: MacAddr, addr: Ipv4Addr, prefix_len: u8) -> usize {
        let index = self.nodes.len();
        let secrets = Secrets {
            ip: key(index, 0x00),
            resets: key(index, 0x10),
            tcp: toyos_net_tcp::Secrets {
                isn: key(index, 0x20),
                timestamp: key(index, 0x30),
                port_offset: key(index, 0x40),
                port_index: key(index, 0x50),
                port_table: [0; 16],
            },
        };
        let config = Config { mac: IndividualMac::new(mac).expect("an individual MAC"), receive_buffer: 65_535, send_buffer: 65_535, secrets };
        let mut shard = Shard::new(self.now, config).expect("a valid configuration");
        shard.link_up(self.now).expect("its own interface");
        shard.add_address(self.now, addr, prefix_len).expect("a valid address");
        for other in 0..index {
            self.links.insert((index, other), Link::default());
            self.links.insert((other, index), Link::default());
        }
        self.nodes.push(Node { shard, mac, credit: Credit::Unlimited, events: Vec::new(), budget: (u64::MAX, 0) });
        index
    }

    pub fn link(&mut self, from: usize, to: usize) -> &mut Link {
        self.links.get_mut(&(from, to)).expect("both nodes are on the segment")
    }

    /// Every frame any device carried, oldest first.
    pub fn wire(&self) -> &[Carried] {
        &self.wire
    }

    /// The wire as a capture file.
    pub fn pcap(&self) -> Vec<u8> {
        pcap::pcap(self.wire.iter().map(|c| (c.at, c.frame.as_slice())))
    }

    /// `node` accepts every connection to `port` on any of its addresses, and each child writes
    /// `len` bytes.
    pub fn serve(&mut self, node: usize, port: u16, len: u64) {
        let port = Port::new(port).expect("a port");
        let listener = self.nodes[node].shard.listen(Ipv4Addr::UNSPECIFIED, Some(port), || 0).expect("the port is free");
        self.servers.push(Server { node, listener, len });
    }

    /// `node` connects to `remote` from an ephemeral port and writes `len` bytes.
    pub fn open(&mut self, node: usize, remote: Endpoint, len: u64) -> ConnId {
        let id = self.nodes[node].shard.connect(self.now, None, remote).expect("a route");
        let tuple = self.nodes[node].shard.tuple(id).expect("a connection");
        self.apps.push(App::new(node, id, tuple, len));
        id
    }

    /// Every application ended, with its write side shut unless it failed.
    pub fn finished(&self) -> bool {
        self.apps.iter().all(|a| matches!(a.end, Some(End::Failed(_))) || (a.end == Some(End::Fin) && a.shut))
    }

    /// Each connection's two streams arrived whole and in order, and both ends saw the FIN.
    pub fn assert_exact(&self) {
        for app in &self.apps {
            let peer = self
                .apps
                .iter()
                .find(|p| p.node != app.node && p.tuple.local == app.tuple.remote && p.tuple.remote == app.tuple.local)
                .unwrap_or_else(|| panic!("{:?} has a peer", app.tuple));
            assert_eq!(app.corrupt, None, "{:?} read a byte its peer did not write", app.tuple);
            assert_eq!((app.written, app.read), (app.len, peer.len), "{:?} wrote and read whole streams", app.tuple);
            assert_eq!(app.end, Some(End::Fin), "{:?} ended", app.tuple);
        }
    }

    /// The frames due by `now`, in the order they arrive.
    fn due(&mut self) -> Vec<Flight> {
        let (mut due, later): (Vec<Flight>, Vec<Flight>) = self.flights.drain(..).partition(|f| f.at <= self.now);
        self.flights = later;
        due.sort_by_key(|f| (f.at, f.order));
        due
    }

    /// One pass of every node's loop at the current instant; `true` if anything happened.
    fn pass(&mut self) -> bool {
        let due = self.due();
        let mut busy = !due.is_empty();
        for flight in due {
            self.nodes[flight.to].shard.receive(self.now, &flight.frame);
        }
        for node in &mut self.nodes {
            node.shard.fire(self.now);
        }
        self.run_apps();
        for from in 0..self.nodes.len() {
            busy |= self.transmit(from);
        }
        for node in &mut self.nodes {
            node.events.extend(node.shard.drain_events());
        }
        busy
    }

    fn run_apps(&mut self) {
        let Self { now, servers, nodes, apps, .. } = self;
        for server in servers.iter() {
            let shard = &mut nodes[server.node].shard;
            while let Some(id) = shard.accept(server.listener).expect("the listener") {
                let tuple = shard.tuple(id).expect("a connection");
                apps.push(App::new(server.node, id, tuple, server.len));
            }
        }
        for app in apps {
            app.run(&mut nodes[app.node].shard, *now);
        }
    }

    fn transmit(&mut self, from: usize) -> bool {
        let now = self.now;
        let node = &mut self.nodes[from];
        let ms = now.nanos() / 1_000_000;
        let credit = match node.credit {
            Credit::Unlimited => usize::MAX,
            Credit::None => 0,
            Credit::PerMs(n) => {
                if node.budget.0 != ms {
                    node.budget = (ms, n);
                }
                node.budget.1
            }
        };
        let mut frames = Vec::new();
        let spent = node.shard.transmit(now, credit, |frame| frames.push(frame.to_vec()));
        if let Credit::PerMs(_) = node.credit {
            node.budget.1 -= spent;
        }
        let busy = !frames.is_empty();
        for frame in frames {
            self.launch(from, &frame);
            self.wire.push(Carried { at: now, from, frame });
        }
        busy
    }

    /// The segment carries `frame` from `from` to each node its destination names.
    fn launch(&mut self, from: usize, frame: &[u8]) {
        let destination = Frame::parse(frame).expect("a node builds well-formed frames").destination();
        let receivers: Vec<usize> = (0..self.nodes.len())
            .filter(|&to| to != from && (destination == self.nodes[to].mac || destination.class() != MacClass::Individual))
            .collect();
        for to in receivers {
            self.carry(from, to, frame);
        }
    }

    fn carry(&mut self, from: usize, to: usize, frame: &[u8]) {
        let now = self.now;
        let link = self.links.get_mut(&(from, to)).expect("both nodes are on the segment");
        if link.dark.is_some_and(|(start, end)| start <= now && now < end) {
            return;
        }
        let fate = link.rule.as_mut().map_or(Fate::Pass, |rule| rule(frame));
        let mut deliveries = Vec::new();
        match fate {
            Fate::Drop => {}
            Fate::Pass => {
                deliveries.push(frame.to_vec());
                deliveries.extend(link.held.take());
            }
            Fate::Duplicate => deliveries.extend([frame.to_vec(), frame.to_vec()]),
            Fate::Hold => deliveries.extend(link.held.replace(frame.to_vec())),
        }
        for frame in deliveries {
            let at = link.serialise(now, &frame);
            self.order += 1;
            self.flights.push(Flight { at, order: self.order, to, frame });
        }
    }

    /// Passes at the current instant until nothing more happens in it.
    pub fn settle(&mut self) {
        for _ in 0..PASSES_PER_INSTANT {
            if !self.pass() && !self.flights.iter().any(|f| f.at <= self.now) {
                return;
            }
        }
        panic!("the network is still busy at {:?} after {PASSES_PER_INSTANT} passes", self.now);
    }

    /// The next instant something is due: a frame's arrival, a node's deadline, or a node's next
    /// millisecond of credit.
    fn next_event(&self) -> Option<Instant> {
        let flights = self.flights.iter().map(|f| f.at).min();
        let deadlines = self.nodes.iter().filter_map(|n| n.shard.next_deadline()).min();
        let credit = self
            .nodes
            .iter()
            .any(|n| matches!(n.credit, Credit::PerMs(_)))
            .then(|| Instant::from_millis(self.now.nanos() / 1_000_000 + 1));
        [flights, deadlines, credit].into_iter().flatten().min()
    }

    /// Moves the clock `by`, settling at every instant something is due on the way and at the end.
    pub fn advance(&mut self, by: Duration) {
        let end = self.now.after(by);
        self.settle();
        while let Some(next) = self.next_event().filter(|&at| at <= end) {
            self.now = next.max(self.now);
            self.settle();
        }
        self.now = end;
        self.settle();
    }

    /// Moves the clock until `done` holds, through every event on the way; `false` when it still
    /// does not hold at `limit` from now.
    pub fn run_until(&mut self, limit: Duration, mut done: impl FnMut(&mut Net) -> bool) -> bool {
        let end = self.now.after(limit);
        self.settle();
        while !done(self) {
            match self.next_event().filter(|&at| at <= end) {
                Some(next) => {
                    self.now = next.max(self.now);
                    self.settle();
                }
                None => return false,
            }
        }
        true
    }
}
