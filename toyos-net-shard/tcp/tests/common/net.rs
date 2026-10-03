//! Two of our stacks on one moved clock, joined by a link with seeded impairment: the consistency
//! control (ours against ours; not an independent oracle). Applications write a
//! seeded byte stream and hash what they read, so a transfer is checked end to end without
//! keeping it.

use std::net::Ipv4Addr;

use toyos_net_tcp::{ConnId, Counter, Error, Failure, Hop, Instant, ListenerId, Options, Received, Tcp, Tuple};
use toyos_net_wire::ipv4::Ipv4Packet;
use toyos_net_wire::tcp::TcpSegment;

use super::*;

pub const NODE_A: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 1);
pub const NODE_B: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 2);

/// FNV-1a: a stream's fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digest(pub u64, pub u64, pub Option<Vec<u8>>);

impl Digest {
    /// `keep`, or `NET_KEEP` in the environment, keeps the bytes as well.
    pub fn new(keep: bool) -> Self {
        Self(0xcbf2_9ce4_8422_2325, 0, (keep || std::env::var("NET_KEEP").is_ok()).then(Vec::new))
    }
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.1 += bytes.len() as u64;
        if let Some(all) = self.2.as_mut() {
            all.extend_from_slice(bytes);
        }
    }
}

/// A seeded stream of bytes: what one direction of one connection carries.
pub struct Source {
    state: u64,
    pub left: usize,
    pending: Vec<u8>,
}

impl Source {
    pub fn new(seed: u64, len: usize) -> Self {
        Self { state: seed | 1, left: len, pending: Vec::new() }
    }
    fn fill(&mut self, n: usize) {
        while self.pending.len() < n.min(self.left) {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.pending.push(self.state as u8);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Fin,
    Failed(Failure),
}

/// One side of one connection, driven like an application.
pub struct App {
    pub id: ConnId,
    pub node: usize,
    pub tuple: Tuple,
    pub source: Source,
    pub sent: Digest,
    pub received: Digest,
    pub end: Option<End>,
    pub shut: bool,
    /// Read at most this many bytes per step; `None` reads everything.
    pub read_limit: Option<usize>,
    pub reading: bool,
    /// Write at most this many bytes per pass; `None` writes all the stack takes.
    pub write_limit: Option<usize>,
}

/// What the link does with one datagram.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fate {
    Pass,
    Drop,
    Duplicate,
    /// Hold it until the next datagram on this link has gone.
    Hold,
    /// Deliver it this many milliseconds after the link's delay.
    Late(u64),
}

pub struct Node {
    pub tcp: Tcp,
    pub addr: Ipv4Addr,
    /// Added to the network's clock: each node's ISN and TSval clocks are its own.
    pub offset: u64,
    /// Transmit credit per millisecond; `None` is unlimited.
    pub credit_per_ms: Option<usize>,
    credit: usize,
    credit_ms: u64,
}

struct Flight {
    at: u64,
    order: u64,
    to: usize,
    bytes: Vec<u8>,
}

pub type Impair = Box<dyn FnMut(usize, &O) -> Fate>;
pub type Rewrite = Box<dyn FnMut(usize, &O) -> Option<Vec<u8>>>;
/// Called with a node's stack after each arrival and each firing (`None`), and with each segment it
/// hands off (`Some`).
pub type Check = Box<dyn FnMut(usize, &mut Tcp, Instant, Option<&O>)>;
/// A node's answer to one hop question (`ip.md` §6.7).
pub type Hops = Box<dyn FnMut(usize) -> Hop<()>>;
/// Whether a node's device frames the segment it is handed.
pub type Frames = Box<dyn FnMut(usize) -> bool>;

pub struct Net {
    pub now: u64,
    pub nodes: Vec<Node>,
    pub apps: Vec<App>,
    pub delay: u64,
    in_flight: Vec<Flight>,
    held: [Option<Vec<u8>>; 2],
    order: u64,
    pub impair: Impair,
    /// Every datagram each node handed off, parsed: `(from, segment)`.
    pub wire: Vec<(usize, O)>,
    pub keep_wire: bool,
    /// Replaces a datagram on the wire, as a middlebox would.
    pub rewrite: Option<Rewrite>,
    /// Node 0's local port for its connections; ephemeral when `None`.
    pub local_port: Option<u16>,
    /// Applications shut their write side once their stream is written.
    pub auto_shut: bool,
    pending_accepts: Vec<(ListenerId, usize)>,
    /// When timers last fired: a deadline still due then is a timer that does not fire.
    fired_at: Option<u64>,
    /// Applications keep every byte they send and receive, not only its digest.
    pub keep_streams: bool,
    pub check: Option<Check>,
    /// Every next hop is known when `None`.
    pub hop: Option<Hops>,
    /// Every frame is built when `None`.
    pub framed: Option<Frames>,
}

pub fn ns(ms: u64) -> u64 {
    ms * 1_000_000
}

impl Net {
    /// Two nodes, a one-way delay of `rtt / 2`, clocks starting an hour in.
    pub fn new(rtt_ms: u64) -> Self {
        let node = |addr, seed: u8| Node {
            tcp: Tcp::new(toyos_net_tcp::Config {
                mtu: 1500,
                receive_buffer: 65_535,
                send_buffer: 65_535,
                secrets: toyos_net_tcp::Secrets {
                    isn: key(seed),
                    timestamp: key(seed.wrapping_add(0x10)),
                    port_offset: key(seed.wrapping_add(0x20)),
                    port_index: key(seed.wrapping_add(0x30)),
                    port_table: [0; 16],
                },
            })
            .unwrap(),
            addr,
            offset: 0,
            credit_per_ms: None,
            credit: 0,
            credit_ms: u64::MAX,
        };
        Self {
            now: ns(3_600_000),
            nodes: vec![node(NODE_A, 0), node(NODE_B, 0x80)],
            apps: Vec::new(),
            delay: ns(rtt_ms) / 2,
            in_flight: Vec::new(),
            held: [None, None],
            order: 0,
            impair: Box::new(|_, _| Fate::Pass),
            wire: Vec::new(),
            keep_wire: false,
            rewrite: None,
            local_port: None,
            auto_shut: true,
            pending_accepts: Vec::new(),
            fired_at: None,
            keep_streams: false,
            check: None,
            hop: None,
            framed: None,
        }
    }

    pub fn instant(&self, node: usize) -> Instant {
        Instant::from_nanos(self.now + self.nodes[node].offset)
    }

    /// Node 1 listens on `port`; node 0 opens `n` connections to it, each side sending `len`
    /// bytes and then shutting its write side. Returns the listener.
    pub fn connections(&mut self, n: usize, port_: u16, len: [usize; 2]) -> ListenerId {
        let listener = self.nodes[1].tcp.listen(NODE_B, Some(port(port_)), || 0).unwrap();
        for i in 0..n {
            let now = self.instant(0);
            let local = self.local_port.map(|p| port(p + i as u16));
            let id = self.nodes[0].tcp.connect(now, NODE_A, local, ep(NODE_B, port_)).unwrap();
            self.add_app(0, id, Source::new(0x1000 + i as u64, len[0]));
        }
        self.pending_accepts.push((listener, len[1]));
        listener
    }

    pub fn set_options(&mut self, node: usize, options: Options) {
        let now = self.instant(node);
        for app in self.apps.iter().filter(|a| a.node == node) {
            self.nodes[node].tcp.set_options(now, app.id, options).unwrap();
        }
    }

    fn add_app(&mut self, node: usize, id: ConnId, source: Source) {
        let tuple = self.nodes[node].tcp.tuple(id).unwrap();
        let keep = self.keep_streams;
        self.apps.push(App {
            id,
            node,
            tuple,
            source,
            sent: Digest::new(keep),
            received: Digest::new(keep),
            end: None,
            shut: false,
            read_limit: None,
            reading: true,
            write_limit: None,
        });
    }

    fn accept_all(&mut self) {
        for k in 0..self.pending_accepts.len() {
            let (listener, len) = self.pending_accepts[k];
            while let Some(id) = self.nodes[1].tcp.accept(listener).unwrap() {
                let seed = 0x2000 + self.apps.len() as u64;
                self.add_app(1, id, Source::new(seed, len));
            }
        }
    }

    fn drive_apps(&mut self) {
        self.accept_all();
        let auto_shut = self.auto_shut;
        for app in &mut self.apps {
            let node = &mut self.nodes[app.node];
            let now = Instant::from_nanos(self.now + node.offset);
            if matches!(app.end, Some(End::Failed(_))) {
                continue;
            }
            let mut room = app.write_limit.unwrap_or(usize::MAX);
            while room > 0 {
                app.source.fill(room.min(65_536));
                let chunk = app.source.pending.len().min(room);
                if chunk == 0 {
                    break;
                }
                match node.tcp.send(now, app.id, &app.source.pending[..chunk]) {
                    Ok(n) => {
                        app.sent.feed(&app.source.pending[..n]);
                        app.source.pending.drain(..n);
                        app.source.left -= n;
                        room -= n;
                        if n == 0 {
                            break;
                        }
                    }
                    Err(Error::WouldBlock) => break,
                    Err(Error::Failed(f)) => {
                        app.end = Some(End::Failed(f));
                        break;
                    }
                    Err(e) => panic!("send: {e:?}"),
                }
            }
            if auto_shut && app.source.left == 0 && app.source.pending.is_empty() && !app.shut && node.tcp.shutdown_write(now, app.id).is_ok() {
                app.shut = true;
            }
            let mut budget = app.read_limit.unwrap_or(usize::MAX);
            while app.reading && app.end.is_none() && budget > 0 {
                let mut buf = vec![0u8; budget.min(65_536)];
                match node.tcp.recv(now, app.id, &mut buf) {
                    Ok(Received::Data(n)) => {
                        app.received.feed(&buf[..n]);
                        budget -= n;
                    }
                    Ok(Received::End) => {
                        app.end = Some(End::Fin);
                        break;
                    }
                    Err(Error::WouldBlock) => break,
                    Err(Error::Failed(f)) => {
                        app.end = Some(End::Failed(f));
                        break;
                    }
                    Err(e) => panic!("recv: {e:?}"),
                }
            }
            if app.read_limit.is_some() {
                app.reading = false;
            }
        }
    }

    fn transmit(&mut self, node: usize) {
        let now = self.instant(node);
        let ms = self.now / 1_000_000;
        let Self { nodes, hop, framed, .. } = self;
        let n = &mut nodes[node];
        let credit = match n.credit_per_ms {
            None => usize::MAX,
            Some(per) => {
                if n.credit_ms != ms {
                    n.credit_ms = ms;
                    n.credit = per;
                }
                n.credit
            }
        };
        let mut out = Vec::new();
        let ask = |_: &Tuple| hop.as_mut().map_or(Hop::Ready(()), |hop| hop(node));
        let sent = n.tcp.transmit(now, credit, ask, |o, ()| {
            let built = framed.as_mut().is_none_or(|framed| framed(node));
            if built {
                out.push(datagram(o));
            }
            built
        });
        if n.credit_per_ms.is_some() {
            n.credit -= sent;
        }
        for bytes in out {
            self.launch(node, bytes);
        }
    }

    fn launch(&mut self, from: usize, bytes: Vec<u8>) {
        let mut parsed = parse_out(&bytes, (self.now / 1_000_000) as i64);
        self.checked(from, Some(&parsed));
        let bytes = match self.rewrite.as_mut().and_then(|r| r(from, &parsed)) {
            Some(new) => {
                parsed = parse_out(&new, parsed.t);
                new
            }
            None => bytes,
        };
        if self.keep_wire {
            self.wire.push((from, parsed.clone()));
        }
        let fate = (self.impair)(from, &parsed);
        let to = 1 - from;
        let push = |net: &mut Net, bytes: Vec<u8>, late: u64| {
            net.order += 1;
            net.in_flight.push(Flight { at: net.now + net.delay + ns(late), order: net.order, to, bytes });
        };
        match fate {
            Fate::Drop => {}
            Fate::Pass => {
                push(self, bytes, 0);
                if let Some(held) = self.held[from].take() {
                    push(self, held, 0);
                }
            }
            Fate::Duplicate => {
                push(self, bytes.clone(), 0);
                push(self, bytes, 0);
            }
            Fate::Hold => {
                if let Some(held) = self.held[from].replace(bytes) {
                    push(self, held, 0);
                }
            }
            Fate::Late(ms) => push(self, bytes, ms),
        }
    }

    fn deliver(&mut self, flight: Flight) {
        let now = self.instant(flight.to);
        let ip = Ipv4Packet::parse(&flight.bytes).unwrap();
        let tcp = TcpSegment::parse(&ip).unwrap();
        self.nodes[flight.to].tcp.receive(now, ip.source(), ip.destination(), &tcp, |_| true);
        self.checked(flight.to, None);
    }

    fn checked(&mut self, node: usize, seg: Option<&O>) {
        let now = self.instant(node);
        if let Some(check) = self.check.as_mut() {
            check(node, &mut self.nodes[node].tcp, now, seg);
        }
    }

    fn next_event(&self) -> Option<u64> {
        for (i, node) in self.nodes.iter().enumerate() {
            let due = node.tcp.next_deadline().map(|d| d.nanos() - node.offset);
            assert!(due.is_none_or(|d| d > self.now || self.fired_at != Some(self.now)), "node {i}'s timer due at {due:?} did not fire");
        }
        let flight = self.in_flight.iter().map(|f| f.at).min();
        let timers = (0..2).filter_map(|i| self.nodes[i].tcp.next_deadline().map(|d| d.nanos() - self.nodes[i].offset)).min();
        let credit = self.nodes.iter().any(|n| n.credit_per_ms.is_some()).then(|| (self.now / 1_000_000 + 1) * 1_000_000);
        [flight, timers, credit].into_iter().flatten().min()
    }

    /// One pass at the current instant: arrivals one at a time, timers, applications, egress.
    pub fn pass(&mut self) {
        loop {
            let due = self.in_flight.iter().enumerate().filter(|(_, f)| f.at <= self.now).min_by_key(|(_, f)| (f.at, f.order)).map(|(i, _)| i);
            let Some(i) = due else { break };
            let flight = self.in_flight.swap_remove(i);
            self.deliver(flight);
            for node in 0..2 {
                self.transmit(node);
            }
        }
        for node in 0..2 {
            let now = self.instant(node);
            self.nodes[node].tcp.fire(now);
            self.checked(node, None);
        }
        self.fired_at = Some(self.now);
        self.drive_apps();
        for node in 0..2 {
            self.transmit(node);
        }
    }

    /// Runs until `done` or the clock passes `limit_ms` more milliseconds; `true` when done.
    pub fn run(&mut self, limit_ms: u64, mut done: impl FnMut(&Net) -> bool) -> bool {
        let end = self.now + ns(limit_ms);
        self.pass();
        while !done(self) {
            let Some(next) = self.next_event() else { return false };
            if next > end {
                return false;
            }
            self.now = next.max(self.now);
            self.pass();
        }
        true
    }

    /// Every application finished: both FINs in, or a failure.
    pub fn finished(&self) -> bool {
        self.apps.iter().all(|a| a.end.is_some() && (a.shut || matches!(a.end, Some(End::Failed(_))))) && !self.apps.is_empty()
    }

    pub fn count(&self, node: usize, counter: Counter) -> u64 {
        self.nodes[node].tcp.counters().get(counter)
    }

    /// Each connection's two directions arrived whole, and both ends saw the FIN.
    pub fn assert_exact(&self) {
        for app in &self.apps {
            let peer = self.peer_of(app);
            if let (Some(a), Some(b)) = (&app.sent.2, &peer.received.2) {
                if let Some(i) = a.iter().zip(b.iter()).position(|(x, y)| x != y) {
                    panic!("first difference at offset {i} of {:?}: sent {:?} got {:?}", app.id, &a[i..i + 8], &b[i..i + 8]);
                }
            }
            assert_eq!(app.sent, peer.received, "a direction of {:?} arrived other than it left", app.id);
            assert_eq!(app.end, Some(End::Fin), "{:?} ended {:?}", app.id, app.end);
        }
    }

    pub fn peer_of(&self, app: &App) -> &App {
        self.apps
            .iter()
            .find(|other| other.node != app.node && other.tuple.local == app.tuple.remote && other.tuple.remote == app.tuple.local)
            .expect("both ends")
    }
}

impl Net {
    /// Moves each node's clock so the connection on node 0's `local_port` to node 1's `remote_port`
    /// starts with ISS `iss` on both sides: node 0 draws it at connect, node 1 when the SYN lands.
    pub fn pin_isn(&mut self, iss: u32, local_port: u16, remote_port: u16) {
        let period = (1u64 << 32) * 4_000;
        let tuples = [
            Tuple { local: ep(NODE_A, local_port), remote: ep(NODE_B, remote_port) },
            Tuple { local: ep(NODE_B, remote_port), remote: ep(NODE_A, local_port) },
        ];
        for (node, tuple) in tuples.iter().enumerate() {
            let key = if node == 0 { key(0) } else { key(0x80) };
            let f = toyos_net_tcp::isn(&key, tuple, Instant::from_nanos(0)).get();
            let m = u64::from(iss.wrapping_sub(f));
            let at = self.now + if node == 0 { 0 } else { self.delay };
            self.nodes[node].offset = (m * 4_000 + period - at % period) % period;
        }
        self.local_port = Some(local_port);
    }

    /// Moves each node's clock so its TSvals on that connection start at `tsval`.
    pub fn pin_tsval(&mut self, tsval: u32, local_port: u16, remote_port: u16) {
        let tuples = [
            Tuple { local: ep(NODE_A, local_port), remote: ep(NODE_B, remote_port) },
            Tuple { local: ep(NODE_B, remote_port), remote: ep(NODE_A, local_port) },
        ];
        for (node, tuple) in tuples.iter().enumerate() {
            let key = if node == 0 { key(0x10) } else { key(0x90) };
            let offset = toyos_net_tcp::ts_offset(&key, tuple);
            let now_ms = (self.now / 1_000_000) as u32;
            let shift = u64::from(tsval.wrapping_sub(offset).wrapping_sub(now_ms));
            self.nodes[node].offset = shift * 1_000_000;
        }
        self.local_port = Some(local_port);
    }
}

impl Net {
    /// Every connection's state, for a failure message.
    pub fn dump(&mut self) -> String {
        let mut text = format!("t = {} ms\n", (self.now - ns(3_600_000)) / 1_000_000);
        for app in &self.apps {
            let tcp = &mut self.nodes[app.node].tcp;
            text += &format!(
                "node {} {:?}: end {:?} sent {} received {} source left {}\n  {:?}\n  {:?}\n",
                app.node,
                app.tuple.local.port.get(),
                app.end,
                app.sent.1,
                app.received.1,
                app.source.left,
                tcp.status(app.id),
                tcp.info(app.id)
            );
        }
        text
    }
}

impl Net {
    /// Moves the clock exactly `ms` forward, through every event on the way.
    pub fn advance(&mut self, ms: u64) {
        let target = self.now + ns(ms);
        while let Some(next) = self.next_event().filter(|&e| e <= target) {
            self.now = next.max(self.now);
            self.pass();
        }
        self.now = target;
        self.pass();
    }
}

impl Net {
    /// Delivers `bytes` to node `to` in the next pass, as if the link had carried them.
    pub fn inject(&mut self, to: usize, bytes: Vec<u8>) {
        self.order += 1;
        self.in_flight.push(Flight { at: self.now, order: self.order, to, bytes });
    }
}
