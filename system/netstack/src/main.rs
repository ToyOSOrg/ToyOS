//! The network server: ToyOS's own stack (`toyos-net-node`) over the link a
//! NIC driver brings (`toyos-link`), served to programs over `toyos::net`'s
//! protocol.
//!
//! **This program holds no protocol and drives no device.** Every byte off the
//! wire goes to the node as the link handed it over, and every decision about
//! an address, a socket or a segment is the node's. What is here is what the
//! node cannot do and stay pure: the link (`link`), the clock, the kernel's
//! random source, the regions clients lend (`lent`), the bells a socket's
//! holders are rung on (`bells`), the clients' connections (`client`) and
//! their requests (`serve`).
//!
//! **One link, until netstack runs several**
//! (`issues/netstack-drives-one-card.md`): the node is made with the first
//! driver's address, so nothing is served before that driver's hello, and a
//! machine none says hello on within [`LINK_BOUND`] has no network: netstack
//! leaves, as a client then hears. A link that ends leaves every connection
//! to the node, and only a hello with that address is opened again.
//!
//! **One pass**: the link's state and its received frames, as one batch, go to the node, then
//! every deadline that is due, then the node is offered the link's transmit
//! room until it has no frame left or the link no room; what the node has to
//! say is written to the log and to the clients that waited for it; and the
//! loop sleeps on the kernel until the link, a client, a watched handle or the
//! node's next deadline wakes it. A request is carried out in the pass that
//! read it and its frames leave in the next, which follows at once.
//!
//! **A frame leaves only into room the link said it has**: the node builds
//! none without it, and a full ring rings this program when it has room.
//!
//! **Every draw is the kernel's**, the stack's secrets and each port and
//! transaction id after them, and a kernel that refuses one ends netstack by
//! name: a value anyone can predict is a forged answer's way in.

use std::time::{Duration, Instant as Wall};

use toyos::endow;
use toyos::ipc::{self, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::say;
use toyos::AsHandle;
use toyos::port::Acceptor;
use toyos_abi::syscall::PIPE_CAPACITY;
use toyos_dhcp::{HostName, Lease};
use toyos_inspect::Snapshot;
use toyos_link::state::{Counters, Group};
use toyos_link::wire::{Hello, Refusal};
use toyos_net_ip::Nud;
use toyos_net_node::Node;
use toyos_net_shard::{Config, Secrets};
use toyos_net_wire::ethernet::IndividualMac;
use toyos_net_wire::Instant;

mod bells;
mod client;
mod lent;
mod link;
mod serve;
mod watches;

use client::{Client, ClientRx, HANDSHAKE_TIMEOUT, MAX_PENDING_CONNS, PendingConn, Request};
use link::{Ended, Heard, Pending, Session};

/// How long netstack waits for its first link before it leaves as a machine
/// with no network. Policy: a driver says hello once its device is up, which
/// is seconds of this; a machine with no NIC driver waits it out, and its
/// clients with it.
const LINK_BOUND: Duration = Duration::from_secs(30);

/// How long an `inspect` waits for the driver's counters. The driver answers
/// in the pass the ask's doorbell starts; one that does not is answered for
/// without them.
const INSPECT_BOUND: Duration = Duration::from_secs(1);

/// Drivers' connections that have not said hello, watched at once.
const MAX_PENDING_LINKS: u32 = 2;

/// The name this machine asks its network to record for it, and answers to on
/// it as `<name>.local` once no other host does. One name, because there is
/// one machine.
pub const HOSTNAME: &str = "toyos-t14";

/// How long this machine waits for its first lease before saying it has none.
/// It bounds the report, never the client: the node asks for the life of the
/// boot and a lease that lands later is applied like any other.
const LEASE_BOUND: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

/// The most a connection's receive buffer grows to, and so the most a window
/// offers: gigabit for a round trip up to 33 ms, at window scale 7. It is the
/// region's receive ring, and the receive side grows to it only as its reader
/// keeps up (`toyos-net-tcp`'s `rx`); a send buffer is the region's send ring.
const TCP_BUFFER: u32 = toyos::net::stream::RECEIVE_BYTES;

/// The most a kernel pipe holds unread; the frames it takes for that add a
/// frame and their list's few KiB (`kernel::pipe::MOST_FRAMES`). A client's
/// pipes are counted to it, and netstack holding their far ends keeps what is
/// in them alive.
const PIPE_BYTES: u64 = PIPE_CAPACITY as u64;

/// What one holder of a stream can make this machine hold: the two pipes of
/// its connection, each full. Its bells are events, which hold no bytes.
const HOLDER_BYTES: u64 = 2 * PIPE_BYTES;

/// What one holder of a listener or a datagram socket can make it hold: the
/// two pipes of its connection, its bell being an event.
const SOCKET_HOLDER_BYTES: u64 = 2 * PIPE_BYTES;

/// What one place can make this machine hold, at the largest of the things a
/// place is (`toyos-net-node`'s `places`): a stream, its region and its first
/// holder; a holder past a stream's first; a listener, whose peers fill its
/// queue of `LISTEN_READY` finished connections with an initial receive buffer
/// of text each, nobody reading them to grow one, and its first holder; a
/// datagram socket, its two queues and its first holder; a holder past a
/// socket's first.
const PLACE_BYTES: u64 = {
    let stream = toyos::net::stream::REGION_BYTES as u64 + HOLDER_BYTES;
    let listener = toyos_net_tcp::limits::LISTEN_READY as u64 * toyos_net_tcp::limits::RECEIVE_BUFFER_INITIAL as u64 + SOCKET_HOLDER_BYTES;
    let datagram = SOCKET_HOLDER_BYTES + (toyos_net_udp::limits::RX_BYTES + toyos_net_udp::limits::TX_BYTES) as u64;
    assert!(stream >= listener && stream >= datagram && stream >= HOLDER_BYTES && stream >= SOCKET_HOLDER_BYTES);
    stream
};

/// Share of physical memory netstack lets its clients' places tie up.
///
/// Policy, not derivation, and the same eighth the compositor takes for the
/// same reason: nothing in the kernel says what a process may use
/// (`issues/no-physical-memory-fairness.md`), so without a bound a client
/// that opens sockets in a loop walks the machine into exhaustion. Delete
/// this in favour of a kernel memory limit, not in favour of a bigger number.
const PLACE_BUDGET_SHARE: u64 = 8;

/// Watches that are no place's: the service's two acceptors, the link, and
/// the drivers' connections that have not said hello.
const FIXED_WATCHES: u32 = 3 + MAX_PENDING_LINKS;

/// The most places one poller can watch: each place is one watched handle, a
/// holder's connection of a stream, a listener or a datagram socket, asked
/// about in the same batch as the fixed watches, the connections that have not
/// said what they want and the lookups' clients.
const MAX_PLACES: u32 = Poller::MAX_HANDLES - FIXED_WATCHES - MAX_PENDING_CONNS - serve::LOOKUP_WATCHES;

/// How many places the node is given, of `total_mem` bytes of physical
/// memory: its share by what a place costs, at least one, at most what the
/// poller watches.
fn places_for(total_mem: u64) -> usize {
    (total_mem / PLACE_BUDGET_SHARE / PLACE_BYTES).clamp(1, u64::from(MAX_PLACES)) as usize
}

/// Total physical memory, as the kernel reports it.
fn total_memory() -> u64 {
    let mut buf = [0u8; toyos::system::SYSINFO_HEADER_SIZE];
    let n = toyos::system::sysinfo(&mut buf);
    assert!(n >= toyos::system::SYSINFO_HEADER_SIZE, "sysinfo returned {n} bytes");
    toyos_abi::syscall::SysinfoHeader::decode(&buf).memory_total
}

/// One draw of the kernel's random source.
fn draw() -> u32 {
    let mut bytes = [0u8; 4];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("netstack: the kernel's random source refused a draw: {e:?}"));
    u32::from_le_bytes(bytes)
}

/// One of the stack's keys: no draw is part of two.
fn key() -> [u8; 16] {
    let mut key = [0u8; 16];
    for word in key.chunks_exact_mut(4) {
        word.copy_from_slice(&draw().to_le_bytes());
    }
    key
}

fn secrets() -> Secrets {
    Secrets {
        ip: key(),
        resets: key(),
        tcp: toyos_net_tcp::Secrets {
            isn: key(),
            timestamp: key(),
            port_offset: key(),
            port_index: key(),
            port_table: std::array::from_fn(|_| draw() as u16),
        },
    }
}

/// What of a lease the log and `inspect` say: everything the server decided.
#[derive(Clone, PartialEq, Eq)]
struct Said {
    address: std::net::Ipv4Addr,
    prefix_len: u8,
    router: Option<std::net::Ipv4Addr>,
    server: std::net::Ipv4Addr,
    dns: Vec<std::net::Ipv4Addr>,
}

impl Said {
    fn of(lease: &Lease) -> Self {
        Self { address: lease.address, prefix_len: lease.prefix_len, router: lease.router, server: lease.server, dns: lease.dns.clone() }
    }

    fn dns(&self) -> String {
        self.dns.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
    }
}

/// What the boot's log has said of the lease, and still owes.
struct Leases {
    began: Wall,
    said: Option<Said>,
    /// Whether this boot has settled the question once: a lease landed, or
    /// the bound passed with none.
    settled: bool,
}

impl Leases {
    /// Says what changed of the lease the node holds, and answers whether
    /// this machine's address question has just been settled.
    fn pass(&mut self, held: Option<&Lease>) -> bool {
        let held = held.map(Said::of);
        if held != self.said {
            match &held {
                // One record carrying every field the lease decided: a boot
                // read off a stick or a stream has this line and nothing else
                // to say what this machine's network was.
                Some(lease) => say!(
                    "netstack: DHCP: lease {}/{} from {}, gateway {}, dns [{}], {} ms after \
                     netstack came up",
                    lease.address,
                    lease.prefix_len,
                    lease.server,
                    match lease.router {
                        Some(router) => router.to_string(),
                        None => "none".to_string(),
                    },
                    lease.dns(),
                    self.began.elapsed().as_millis(),
                ),
                None => say!("netstack: DHCP: the lease is gone; this machine has no address"),
            }
            self.said = held;
        }
        if self.settled {
            return false;
        }
        if self.said.is_none() {
            if self.began.elapsed() < LEASE_BOUND {
                return false;
            }
            say!(
                "netstack: DHCP: no lease as {} in {} s; this machine has no address and every \
                 connect through it is refused",
                HOSTNAME,
                self.began.elapsed().as_secs(),
            );
        }
        self.settled = true;
        true
    }

    /// How long until the bound's report is due, while it is owed.
    fn due_in(&self) -> Option<Duration> {
        (!self.settled).then(|| LEASE_BOUND.saturating_sub(self.began.elapsed()))
    }

    /// The lease as `inspect` reads it, and the router's entry in the
    /// stack's neighbour table where the lease names one.
    fn inspect(&self, node: &Node, snap: &mut Snapshot) {
        let Some(held) = &self.said else {
            snap.put("lease.held", false);
            return;
        };
        snap.put("lease.held", true);
        snap.put("lease.address", format!("{}/{}", held.address, held.prefix_len));
        snap.put("lease.server", held.server.to_string());
        snap.put("lease.dns", held.dns());
        let Some(router) = held.router else { return };
        snap.put("lease.router", router.to_string());
        let shard = node.shard();
        snap.put(
            "neighbour.router",
            match shard.ip().neighbour(shard.iface(), router) {
                None => "none",
                Some(Nud::Incomplete(_)) => "incomplete",
                Some(Nud::Reachable(_)) => "reachable",
                Some(Nud::Stale(_)) => "stale",
                Some(Nud::Delay(_)) => "delay",
                Some(Nud::Probe(_)) => "probe",
                Some(Nud::Unreachable(_)) => "unreachable",
                Some(Nud::Failed) => "failed",
            },
        );
    }
}

/// Answer `inspect` with what this pass knows, in one non-blocking write, and
/// let the connection close as every other answer does.
fn answer_inspect(request: &Request, link: Option<(&Session, Option<&Counters>)>, leases: &Leases, sockets: &serve::Sockets, node: &Node) {
    let mut snap = Snapshot::new(toyos_inspect::NET);
    match link {
        Some((link, counters)) => link.inspect(counters, &mut snap),
        None => snap.put("link.state", "none"),
    }
    leases.inspect(node, &mut snap);
    sockets.inspect(node, &mut snap);
    let encoded = snap.encode().unwrap_or_else(|why| panic!("netstack: its snapshot: {why}"));
    request.client.snapshot(&encoded);
}

/// Open the link `asked` asks for, and say so.
fn open(asked: Asked, groups: &[Group]) -> Option<Session> {
    let label = asked.hello.label.clone();
    let opened = Session::open(asked.hello, asked.mac, asked.conn, asked.region, groups);
    match &opened {
        Some(_) => say!("netstack: {label}'s link is open"),
        None => say!("netstack: {label} went before its link was opened"),
    }
    opened
}

/// Refuse `asked` by `why`, and say so.
fn refuse(asked: &Asked, why: Refusal, because: &str) {
    say!("netstack: refused {}'s link: {because}", asked.hello.label);
    link::refuse(&asked.conn, why);
}

/// Every hello the pending drivers said, in the order heard; a connection
/// that went, said what no driver says first, or has said nothing within
/// [`HANDSHAKE_TIMEOUT`] is let go.
fn hear_links(pending: &mut Vec<Pending>) -> Vec<Asked> {
    pending.retain(|p| p.since.elapsed() < HANDSHAKE_TIMEOUT);
    let mut asked = Vec::new();
    let mut i = 0;
    while i < pending.len() {
        match pending[i].hear() {
            Heard::Nothing => i += 1,
            Heard::Gone => {
                pending.remove(i);
            }
            Heard::Hello(hello, mac, region) => {
                let conn = pending.remove(i).into_conn();
                asked.push(Asked { hello, mac, conn, region });
            }
        }
    }
    asked
}

/// End `link` for `why`; the node hears a change, not a state.
fn end_link(link: &mut Option<Session>, why: Ended, node: &mut Node, now: Instant, faulted: &mut bool) {
    let Some(session) = link.take() else { return };
    match why {
        Ended::HungUp => say!("netstack: {}'s link ended: it hung up", session.hello().label),
        Ended::Fault(fault) => {
            say!("netstack: {}'s link ended: it wrote {fault}", session.hello().label);
            *faulted = true;
        }
    }
    if session.is_up() {
        node.link(now, false, draw);
    }
}

/// A driver's connection, kept to hear its hello while there is room.
fn accept_link(links: &Acceptor, pending: &mut Vec<Pending>) {
    match links.accept() {
        Ok(conn) if pending.len() < MAX_PENDING_LINKS as usize => pending.push(Pending::new(conn)),
        Ok(_) => say!("netstack: refused a NIC driver's connection: {MAX_PENDING_LINKS} have not said hello yet"),
        Err(why) => say!("netstack: the kernel refused a NIC driver's connection: {why:?}"),
    }
}

/// The first link a driver brings, within [`LINK_BOUND`].
fn first_link(links: &Acceptor, groups: &[Group]) -> Option<Session> {
    let began = Wall::now();
    let mut pending: Vec<Pending> = Vec::new();
    let mut poller = watches::Watches::new(1 + MAX_PENDING_LINKS);
    loop {
        let mut opened = None;
        for asked in hear_links(&mut pending) {
            match opened {
                Some(_) => refuse(&asked, Refusal::OneLink, "netstack drives one link (issues/netstack-drives-one-card.md)"),
                None => opened = open(asked, groups),
            }
        }
        if opened.is_some() {
            return opened;
        }
        let left = LINK_BOUND.checked_sub(began.elapsed())?;
        poller.watch(links, READABLE, TOKEN_LINKS);
        for (i, p) in pending.iter().enumerate() {
            p.watch(&mut poller, TOKEN_LINK_PENDING_BASE + i as u64);
        }
        let mut accept = false;
        poller.wait_answers(1, u64::try_from(left.as_nanos()).unwrap_or(u64::MAX), |token, _| accept |= token == TOKEN_LINKS);
        if accept {
            accept_link(links, &mut pending);
        }
    }
}

const TOKEN_ACCEPTOR: u64 = 0;
const TOKEN_LINKS: u64 = 1;
const TOKEN_LINK: u64 = 2;
const TOKEN_LINK_PENDING_BASE: u64 = 3;

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

/// A driver's hello, with what it came with.
struct Asked {
    hello: Hello,
    mac: IndividualMac,
    conn: toyos::Connection,
    region: toyos_link::region::Region,
}

/// An `inspect` waiting for the driver's counters.
struct Inspecting {
    request: Request,
    since: Wall,
}

fn main() {
    // The `netstack` port exists before this process does: a client's
    // connection is queued on it whether or not this program ever reaches
    // `accept`, and if netstack exits the queued client sees `Gone` rather
    // than silence.
    let acceptor = endow::acceptor("netstack")
        .expect("the manifest declares this program serves `netstack`");
    let links = endow::acceptor(toyos_link::PORT)
        .expect("the manifest declares this program serves `link`");
    let groups = [Group::new(toyos_mdns::GROUP_MAC).unwrap_or_else(|| panic!("netstack: mDNS's address is no group's"))];
    let Some(first) = first_link(&links, &groups) else {
        say!("netstack: no NIC driver brought a link in {} s; this machine has no network, exiting", LINK_BOUND.as_secs());
        return;
    };
    let mac = first.mac();
    let mut link = Some(first);

    let began = Wall::now();
    let clock = || Instant::from_nanos(u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX));
    let config = Config {
        mac,
        receive_buffer: TCP_BUFFER,
        send_buffer: toyos::net::stream::SEND_BYTES,
        secrets: secrets(),
    };
    let host = HostName::new(HOSTNAME).unwrap_or_else(|| panic!("netstack: {HOSTNAME:?} is no host name"));
    let mut node = Node::new(clock(), config, Some(host), draw)
        .unwrap_or_else(|why| panic!("netstack: the stack refused its configuration: {why:?}"));

    let name = toyos_mdns::Host::new(HOSTNAME).unwrap_or_else(|_| panic!("netstack: {HOSTNAME:?} is no host name"));
    node.answer_as(clock(), name, draw)
        .unwrap_or_else(|why| panic!("netstack: a new node refused the multicast DNS port: {why:?}"));

    let total_mem = total_memory();
    let places = places_for(total_mem);
    node.set_places(places);
    let mut sockets = serve::Sockets::new(places);
    let mut leases = Leases { began, said: None, settled: false };

    let mut poller = watches::Watches::new(
        FIXED_WATCHES + MAX_PLACES + MAX_PENDING_CONNS + serve::LOOKUP_WATCHES,
    );
    // Clear of the ones above and of a connection's own handle by more than
    // `MAX_HANDLES` (4096, `kernel/src/object/handle.rs`); `serve`'s tokens
    // are all above the low word.
    const TOKEN_PENDING_BASE: u64 = 0x1_0000;

    let mut pending: Vec<PendingConn> = Vec::new();
    let mut pending_links: Vec<Pending> = Vec::new();
    let mut inspecting: Vec<Inspecting> = Vec::new();
    // The held link ended for what its driver wrote, and its next hello is
    // refused for it.
    let mut faulted = false;
    // Accepts the kernel refused since the last it did not.
    let mut accept_refused: u64 = 0;

    loop {
        // The node begins with its link down, and hears each change of it here.
        let now = clock();
        let mut owed = false;
        let mut ended = None;
        if let Some(session) = &mut link {
            match session.receive(&mut node, now, draw) {
                Ok(more) => owed = more,
                Err(why) => ended = Some(why),
            }
        }
        let now = clock();
        if node.next_deadline().is_some_and(|at| at <= now) {
            node.fire(now, draw);
        }
        if ended.is_none()
            && let Some(session) = &mut link
            && let Err(why) = session.transmit(&mut node, now, draw)
        {
            ended = Some(why);
        }

        // The driver's counters, once it answered them or its time to is up.
        if !inspecting.is_empty() {
            // A link opened since the ask is asked in its turn.
            if let Some(session) = &mut link {
                session.ask_counters();
            }
            let counters = link.as_mut().and_then(Session::counters);
            let late = inspecting.first().is_some_and(|i| i.since.elapsed() >= INSPECT_BOUND);
            if counters.is_some() || late || link.is_none() {
                for waiting in inspecting.drain(..) {
                    answer_inspect(&waiting.request, link.as_ref().map(|l| (l, counters.as_ref())), &leases, &sockets, &node);
                }
            }
        }
        if ended.is_none()
            && let Some(session) = &mut link
            && let Err(why) = session.ring()
        {
            ended = Some(why);
        }
        if let Some(why) = ended {
            end_link(&mut link, why, &mut node, clock(), &mut faulted);
        }

        sockets.settle(&mut node);
        // Before anything is served: the lease is what gives this machine an
        // address, a route and its resolvers.
        if leases.pass(node.lease()) {
            say!(
                "netstack: ready, at most {places} places ({} MiB each of {} MiB total)",
                PLACE_BYTES / (1024 * 1024),
                total_mem / (1024 * 1024),
            );
        }

        poller.watch(&acceptor, READABLE, TOKEN_ACCEPTOR);
        poller.watch(&links, READABLE, TOKEN_LINKS);
        if let Some(session) = &link {
            session.watch(&mut poller, TOKEN_LINK);
        }
        for (i, p) in pending_links.iter().enumerate() {
            p.watch(&mut poller, TOKEN_LINK_PENDING_BASE + i as u64);
        }
        sockets.watch(&mut poller);
        for p in pending.iter() {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().raw().0 as u64);
        }

        // The node's next deadline: a retransmission, a lease's timer, a
        // lookup's wait, a connect's. Zero when one is due, and when the
        // receive stopped at its bound.
        let nanos = |left: Duration| u64::try_from(left.as_nanos()).unwrap_or(u64::MAX);
        let mut timeout = node.next_deadline().map_or(u64::MAX, |at| nanos(at.since(clock())));
        if owed {
            timeout = 0;
        }
        if let Some(left) = leases.due_in() {
            timeout = timeout.min(nanos(left));
        }
        if let Some(first) = inspecting.first() {
            timeout = timeout.min(nanos(INSPECT_BOUND.saturating_sub(first.since.elapsed())));
        }
        // A client that connects and then says nothing wakes nothing, so the
        // deadline that removes it is a wake in its own right.
        if !pending.is_empty() || !pending_links.is_empty() {
            timeout = timeout.min(nanos(HANDSHAKE_TIMEOUT));
        }

        let mut ready: Vec<u64> = Vec::new();
        poller.wait_answers(1, timeout, |token, answer| {
            if !sockets.answered(&mut node, clock(), token, answer) {
                ready.push(token);
            }
        });
        sockets.bridge(&mut node, clock());

        // A driver's connection: a hello with the address the node was made
        // with is opened, once a link it replaces has ended. The kernel closes
        // a dead driver's handles before its end is published, so a
        // restarted driver's hello finds the old link already hung up.
        if ready.contains(&TOKEN_LINKS) {
            accept_link(&links, &mut pending_links);
        }
        for asked in hear_links(&mut pending_links) {
            if asked.mac != mac {
                refuse(&asked, Refusal::OneLink, "its address is not the one netstack drives (issues/netstack-drives-one-card.md)");
                continue;
            }
            if std::mem::take(&mut faulted) {
                refuse(&asked, Refusal::Faulted, "its last link ended for what it wrote");
                continue;
            }
            if let Some(session) = &link {
                match session.alive() {
                    Ok(()) => {
                        refuse(&asked, Refusal::OneLink, "a link of its address is open");
                        continue;
                    }
                    Err(why) => end_link(&mut link, why, &mut node, clock(), &mut faulted),
                }
            }
            link = open(asked, &groups);
        }
        // On a pass that found nothing ready too: otherwise a silent client
        // is only ever timed out by some other client's traffic.
        let now_wall = Wall::now();
        for p in pending.iter().filter(|p| now_wall.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
            say!(
                "netstack: dropping client {} — it never finished its request",
                p.conn.as_handle().raw().0
            );
        }
        pending.retain(|p| now_wall.duration_since(p.since) < HANDSHAKE_TIMEOUT);

        // Accept and the request are two events. Nothing is read here: a client
        // that connects and then says nothing costs a slot and a deadline, not
        // the network stack.
        // A connection the kernel would not hand over is gone from its
        // queue, and its client told: the first of a run is named, and the
        // next accept says how many followed it.
        let accepted = match ready.contains(&TOKEN_ACCEPTOR).then(|| acceptor.accept()) {
            Some(Err(why)) => {
                if accept_refused == 0 {
                    say!("netstack: the kernel refused a client's connection: {why:?}");
                }
                accept_refused = accept_refused.saturating_add(1);
                None
            }
            Some(Ok(conn)) => {
                if accept_refused > 1 {
                    say!("netstack: accepting again, after {accept_refused} connections the kernel refused");
                }
                accept_refused = 0;
                Some(conn)
            }
            None => None,
        };
        if let Some(conn) = accepted {
            if pending.len() >= MAX_PENDING_CONNS as usize {
                say!(
                    "netstack: refusing client {} — {MAX_PENDING_CONNS} connections are already \
                     waiting to say what they want",
                    conn.as_handle().raw().0
                );
            } else {
                pending.push(PendingConn { conn, rx: ClientRx::new(), since: Wall::now() });
            }
        }

        // `remove` rather than `swap_remove`: the entries after `i` shift down,
        // so leaving `i` alone visits each connection exactly once. At
        // `MAX_PENDING_CONNS` entries the shift is not worth a subtler loop.
        let mut requests: Vec<Request> = Vec::new();
        let mut i = 0;
        while i < pending.len() {
            let handle = pending[i].conn.as_handle();
            if !ready.contains(&(TOKEN_PENDING_BASE + handle.raw().0 as u64)) {
                i += 1;
                continue;
            }
            let step = {
                let p = &mut pending[i];
                p.rx.pump(&p.conn)
            };
            match step {
                RxStep::Idle => i += 1,
                // Unlogged, and the only removal here that is: a client may
                // connect to find out whether netstack exists and hang up, which is
                // its business. The two below are the client getting something
                // wrong, and those netstack names.
                RxStep::Eof => {
                    pending.remove(i);
                }
                RxStep::Malformed => {
                    say!(
                        "netstack: dropping client {} — it sent a frame this protocol cannot \
                         describe",
                        pending[i].conn.as_handle().raw().0
                    );
                    pending.remove(i);
                }
                RxStep::Frame { msg_type, payload_len, .. } => {
                    let mut payload = [0u8; toyos::net::MAX_REQUEST];
                    payload[..payload_len].copy_from_slice(pending[i].rx.payload(payload_len));
                    let handles = pending[i].rx.take_batch();
                    let p = pending.remove(i);
                    requests.push(Request {
                        client: Client { conn: p.conn },
                        msg_type,
                        payload,
                        payload_len,
                        handles,
                    });
                }
            }
        }

        for request in requests {
            if request.msg_type == toyos_inspect::MSG_INSPECT {
                // The request is a bare header, and anything riding on one is
                // not this protocol.
                if request.payload_len != 0 {
                    request.client.error(toyos::net::ERR_INVALID_INPUT);
                } else if let Some(session) = link.as_mut().filter(|_| inspecting.len() < MAX_PENDING_CONNS as usize) {
                    session.ask_counters();
                    inspecting.push(Inspecting { request, since: Wall::now() });
                } else {
                    answer_inspect(&request, link.as_ref().map(|l| (l, None)), &leases, &sockets, &node);
                }
                continue;
            }
            sockets.request(&mut node, clock(), request, &acceptor, draw);
        }
    }
}
