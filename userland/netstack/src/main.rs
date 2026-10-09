//! The network server: one Ethernet card claimed from the kernel, ToyOS's own
//! stack over it (`toyos-net-node`), served to programs over the pipe ABI
//! (`toyos::net`).
//!
//! **This program holds no protocol and reads no frame.** Every byte off the
//! wire goes to the node as the card handed it over, and every decision about
//! an address, a socket or a segment is the node's. What is here is what the
//! node cannot do and stay pure: the card (`card`), the clock, the kernel's
//! random source, the kernel's pipes (`pipes`), the clients' connections
//! (`client`) and their requests (`serve`).
//!
//! **One pass**: the card's link and its received frames go to the node, then
//! every deadline that is due, then the node is offered the card's transmit
//! room until it has no frame left or the card no room; what the node has to
//! say is written to the log and to the clients that waited for it; and the
//! loop sleeps on the kernel until the card, a client, a watched pipe or the
//! node's next deadline wakes it. A request is carried out in the pass that
//! read it and its frames leave in the next, which follows at once.
//!
//! **A frame leaves only into room the card said it has** (`Card::tx_room`):
//! the node builds none without it, and a card with none wakes the pass that
//! has (`Card::wake_on_room`).
//!
//! **Every draw is the kernel's**, the stack's secrets and each id, port and
//! transaction id after them, and a kernel that refuses one ends netstack by
//! name: a value anyone can predict is a forged answer's way in.

use std::time::{Duration, Instant as Wall};

use toyos::endow;
use toyos::ipc::{self, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::say;
use toyos::AsHandle;
use toyos_abi::syscall::PciId;
use toyos_dhcp::{HostName, Lease};
use toyos_i219::Part;
use toyos_inspect::Snapshot;
use toyos_net_ip::Nud;
use toyos_net_node::Node;
use toyos_net_shard::{Config, Secrets};
use toyos_net_wire::ethernet::{IndividualMac, MacAddr};
use toyos_net_wire::Instant;

mod card;
mod client;
mod device;
mod i219;
mod pipes;
mod serve;
mod virtio_net;

use card::Card;
use client::{Client, ClientRx, HANDSHAKE_TIMEOUT, MAX_KEPT_REQUEST, MAX_PENDING_CONNS, PendingConn, Request};

/// The cards this program can drive, named by what identifies one rather than
/// by the slot firmware put it in, and each with the driver that opens it. The
/// manifest row spells the same pair and the claim arrives under a label
/// composed from it, so which of these exists is `/system/bin/supervisor`'s answer
/// and not this program's — at most one is ever endowed, and a machine with
/// none is a machine netstack leaves.
///
/// `1af4:1041` is virtio's transitional device id `1000 + 1` for a network
/// device (virtio 1.2 §5.1.1). `8086:15fc` is the ThinkPad T14's onboard I219
/// at `00:1f.6`; `8086:10d3` is the 82574L, which QEMU's `e1000e` models. One
/// driver takes both, and each row names which part it is because below the
/// register file they are not one.
const CARDS: [(PciId, fn(toyos::PciDev) -> Card); 3] = [
    (PciId { vendor: 0x8086, device: 0x15fc }, |c| Card::intel(c, Part::I219)),
    (PciId { vendor: 0x8086, device: 0x10d3 }, |c| Card::intel(c, Part::E82574)),
    (PciId { vendor: 0x1af4, device: 0x1041 }, Card::virtio),
];

/// The name this machine asks its network to record for it, and answers to on
/// it as `<name>.local` once no other host does. One name, because there is
/// one machine.
pub const HOSTNAME: &str = "toyos-t14";

/// How long this machine waits for its first lease before saying it has none.
/// It bounds the report, never the client: the node asks for the life of the
/// boot and a lease that lands later is applied like any other.
const LEASE_BOUND: Duration = Duration::from_millis(toyos_tco::LEASE_BOUND_MS);

/// The most payload each direction of a connection buffers in the stack, and
/// so the most a window offers: gigabit for a round trip up to 33 ms, at
/// window scale 7. Storage is taken as text is held, and the receive side
/// grows to this only as its reader keeps up (`toyos-net-tcp`'s `rx`).
const TCP_BUFFER: u32 = 4 << 20;

/// A kernel pipe is one 2 MiB page (`kernel/src/pipe.rs`). The client
/// allocates it, and netstack holding its far end is what keeps it alive.
const PIPE_BYTES: u64 = 2 * 1024 * 1024;

/// What one place can make this machine hold, at the largest of the three
/// things a place is (`toyos-net-node`'s `places`): a stream, its two pipes
/// and two buffers; a listener, whose peers fill its queue of `LISTEN_READY`
/// finished connections with an initial receive buffer of text each, nobody
/// reading them to grow one, and its wake pipe; a datagram socket, its two
/// pipes and two queues.
const PLACE_BYTES: u64 = {
    let stream = 2 * PIPE_BYTES + 2 * TCP_BUFFER as u64;
    let listener = toyos_net_tcp::limits::LISTEN_READY as u64 * toyos_net_tcp::limits::RECEIVE_BUFFER_INITIAL as u64 + PIPE_BYTES;
    let datagram = 2 * PIPE_BYTES + (toyos_net_udp::limits::RX_BYTES + toyos_net_udp::limits::TX_BYTES) as u64;
    assert!(stream >= listener && stream >= datagram);
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

/// Watches that are no place's: the service's acceptor and the card's claim.
const FIXED_WATCHES: u32 = 2;

/// The most places one poller can watch: every place's pipes are asked about
/// in the same batch as the fixed watches, the connections that have not said
/// what they want and the lookups' clients.
const MAX_PLACES: u32 =
    (Poller::MAX_HANDLES - FIXED_WATCHES - MAX_PENDING_CONNS - serve::LOOKUP_WATCHES) / serve::WATCHES_PER_PLACE;

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
                    "{}{}/{} from {}, gateway {}, dns [{}], {} ms after netstack came up",
                    toyos_tco::LEASE_SAID,
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
                "{}{} in {} s; this machine has no address and every connect through it is refused",
                toyos_tco::NO_LEASE_SAID,
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
fn answer_inspect(request: &Request, card: &Card, leases: &Leases, sockets: &serve::Sockets, node: &Node) {
    // The request is a bare header, and anything riding on one is not this
    // protocol.
    if request.payload_len != 0 {
        request.client.error(toyos::net::ERR_INVALID_INPUT);
        return;
    }
    let mut snap = Snapshot::new(toyos_inspect::NET);
    card.inspect(&mut snap);
    leases.inspect(node, &mut snap);
    sockets.inspect(node, &mut snap);
    let encoded = snap.encode().unwrap_or_else(|why| panic!("netstack: its snapshot: {why}"));
    request.client.snapshot(&encoded);
}

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

fn main() {
    // The `netstack` port exists before this process does: a client's
    // connection is queued on it whether or not this program ever reaches
    // `accept`, and if netstack exits the queued client sees `Gone` rather
    // than silence.
    let Some((open, claim)) = CARDS
        .iter()
        .find_map(|(id, open)| endow::pci_function::<toyos::PciDev>(*id).map(|c| (*open, c)))
    else {
        say!("netstack: no NIC on this machine, exiting");
        return;
    };
    let acceptor = endow::acceptor("netstack")
        .expect("the manifest declares this program serves `netstack`");
    let card = open(claim);
    let mac = card.mac();
    say!(
        "netstack: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );

    let began = Wall::now();
    let clock = || Instant::from_nanos(u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX));
    let config = Config {
        mac: IndividualMac::new(MacAddr(mac)).unwrap_or_else(|| panic!("netstack: the card's address is a group's")),
        receive_buffer: TCP_BUFFER,
        send_buffer: TCP_BUFFER,
        secrets: secrets(),
    };
    let host = HostName::new(HOSTNAME).unwrap_or_else(|| panic!("netstack: {HOSTNAME:?} is no host name"));
    let mut node = Node::new(clock(), config, Some(host), draw)
        .unwrap_or_else(|why| panic!("netstack: the stack refused its configuration: {why:?}"));

    if let Card::Intel(nic) = &card {
        nic.accept_multicast(toyos_mdns::GROUP_MAC);
    }
    let name = toyos_mdns::Host::new(HOSTNAME).unwrap_or_else(|_| panic!("netstack: {HOSTNAME:?} is no host name"));
    node.answer_as(clock(), name, draw)
        .unwrap_or_else(|why| panic!("netstack: a new node refused the multicast DNS port: {why:?}"));

    let total_mem = total_memory();
    let places = places_for(total_mem);
    node.set_places(clock(), places);
    let mut sockets = serve::Sockets::new(draw(), places);
    let mut leases = Leases { began, said: None, settled: false };

    // The link as the card came up with it, which the first change a pass
    // reports is measured against. Virtio reports no link changes at all. The
    // node begins with its link down.
    let mut link_up = match &card {
        Card::Intel(intel) => intel.link().is_up(),
        Card::Virtio(_) => true,
    };
    if link_up {
        node.link(clock(), true, draw);
    }

    let poller = Poller::new(
        FIXED_WATCHES + serve::WATCHES_PER_PLACE * MAX_PLACES + MAX_PENDING_CONNS + serve::LOOKUP_WATCHES,
    );
    const TOKEN_ACCEPTOR: u64 = 0;
    const TOKEN_NIC: u64 = 1;
    // Clear of the two above and of a connection's own handle by more than
    // `MAX_HANDLES` (4096, `kernel/src/object/handle.rs`); `serve`'s tokens
    // are all above the low word.
    const TOKEN_PENDING_BASE: u64 = 0x1_0000;

    let mut pending: Vec<PendingConn> = Vec::new();
    // Accepts the kernel refused since the last it did not.
    let mut accept_refused: u64 = 0;

    loop {
        // First, because it is what makes the interrupt taken and what gives
        // a driver with a per-pass receive budget that budget back.
        if let Some(link) = card.begin_pass() {
            // A change of state only: a speed change is no new network.
            if link.is_up() != link_up {
                link_up = link.is_up();
                node.link(clock(), link_up, draw);
            }
        }
        while card.rx(|frame| node.receive(clock(), frame, draw)) {}
        let now = clock();
        if node.next_deadline().is_some_and(|at| at <= now) {
            node.fire(now, draw);
        }
        loop {
            // A card with no room is asked to say when it has some, and a
            // frame the node still holds leaves in the pass that wakes.
            let room = match card.tx_room() {
                0 => card.wake_on_room(),
                room => room,
            };
            if room == 0 || node.transmit(now, room, |frame| card.tx(frame.len(), |slot| slot.copy_from_slice(frame)), draw) < room {
                break;
            }
        }
        card.report();

        sockets.settle(&mut node, now);
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
        poller.watch(card.claim(), READABLE, TOKEN_NIC);
        sockets.watch(&node, &poller);
        for p in pending.iter() {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().0 as u64);
        }

        // The node's next deadline: a retransmission, a lease's timer, a
        // lookup's wait, a connect's. Zero when one is due.
        let nanos = |left: Duration| u64::try_from(left.as_nanos()).unwrap_or(u64::MAX);
        let mut timeout = node.next_deadline().map_or(u64::MAX, |at| nanos(at.since(clock())));
        if let Some(left) = leases.due_in() {
            timeout = timeout.min(nanos(left));
        }
        // A card that never does what it owes sends no interrupt to say so.
        timeout = timeout.min(card.pass_due_in().unwrap_or(u64::MAX));
        // A client that connects and then says nothing wakes nothing, so the
        // deadline that removes it is a wake in its own right.
        if !pending.is_empty() {
            timeout = timeout.min(nanos(HANDSHAKE_TIMEOUT));
        }

        let mut ready: Vec<u64> = Vec::new();
        poller.wait_answers(1, timeout, |token, answer| {
            if !sockets.answered(&mut node, clock(), token, answer) {
                ready.push(token);
            }
        });
        sockets.bridge(&mut node, clock());

        // On a pass that found nothing ready too: otherwise a silent client
        // is only ever timed out by some other client's traffic.
        let now_wall = Wall::now();
        for p in pending.iter().filter(|p| now_wall.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
            say!(
                "netstack: dropping client {} — it never finished its request",
                p.conn.as_handle().0
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
                    conn.as_handle().0
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
            if !ready.contains(&(TOKEN_PENDING_BASE + handle.0 as u64)) {
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
                        pending[i].conn.as_handle().0
                    );
                    pending.remove(i);
                }
                RxStep::Frame { msg_type, payload_len } => {
                    let mut payload = [0u8; MAX_KEPT_REQUEST];
                    payload[..payload_len].copy_from_slice(pending[i].rx.payload(payload_len));
                    let p = pending.remove(i);
                    requests.push(Request {
                        client: Client { conn: p.conn },
                        msg_type,
                        payload,
                        payload_len,
                    });
                }
            }
        }

        for request in requests {
            if request.msg_type == toyos_inspect::MSG_INSPECT {
                answer_inspect(&request, &card, &leases, &sockets, &node);
                continue;
            }
            sockets.request(&mut node, clock(), request, draw);
        }
    }
}
