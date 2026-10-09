use std::collections::HashMap;
use std::time::{Duration, Instant};
use toyos::poller::{OTHER_END_GONE, READABLE, WRITABLE, Poller};
use toyos::ipc;
use toyos::AsHandle;
use toyos::ipc::RxStep;
use toyos::say;

mod card;
mod client;
mod device;
mod dhcp;
mod i219;
mod listen;
mod mdns;
mod resolve;
mod virtio_net;

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

use toyos::endow;
use toyos::Pipe;
use toyos_abi::syscall::PciId;
use toyos_i219::Part;
use toyos_inspect::Snapshot;
use virtio_net::VirtioNet;

use card::Card;
use client::{Client, ClientRx, HANDSHAKE_TIMEOUT, MAX_KEPT_REQUEST, MAX_PENDING_CONNS, PendingConn, Request};

use toyos::net::*;

use smoltcp::iface::{Config, Interface, PollResult, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::Instant as SmoltcpInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpEndpoint, IpListenEndpoint};

use std::net::Ipv4Addr;

// --- smoltcp Device wrapper ---

/// The driver, as smoltcp's `Device`.
///
/// A thin adapter: every token below borrows the driver rather than a claim
/// handle, because the ring the token gives back to is this process's own.
struct DmaNic {
    nic: Card,
}

impl DmaNic {
    /// Whether the card takes a frame now. Where it does not, its claim reads
    /// ready when it will.
    fn room(&self) -> bool {
        self.nic.tx_room() > 0 || self.nic.wake_on_room() > 0
    }
}

impl Device for DmaNic {
    type RxToken<'a> = DmaRxToken<'a>;
    type TxToken<'a> = DmaTxToken<'a>;

    fn receive(&mut self, _timestamp: SmoltcpInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // A frame is taken off the receive ring only with room to answer it:
        // smoltcp takes a transmit token with every frame it receives.
        if !self.room() {
            return None;
        }
        let token = match &self.nic {
            Card::Virtio(nic) => {
                nic.poll_rx().map(|(index, len)| DmaRxToken::Virtio { nic, index, len })
            }
            Card::Intel(nic) => nic.poll_rx().map(|frame| DmaRxToken::Intel { nic, frame }),
        }?;
        Some((token, DmaTxToken { nic: &self.nic }))
    }

    fn transmit(&mut self, _timestamp: SmoltcpInstant) -> Option<Self::TxToken<'_>> {
        self.room().then_some(DmaTxToken { nic: &self.nic })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1514;
        caps.medium = Medium::Ethernet;
        caps
    }
}

/// One received frame, holding the driver it came from rather than a tag that
/// says which — so a frame and a driver that do not go together is not a state
/// this program can be in.
enum DmaRxToken<'a> {
    Virtio { nic: &'a VirtioNet, index: usize, len: usize },
    Intel { nic: &'a i219::Nic, frame: toyos_i219::Frame },
}

impl phy::RxToken for DmaRxToken<'_> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        // The borrow ends with `f`, and the buffer goes back to the device only
        // afterwards: smoltcp's `consume` takes `FnOnce(&[u8])`, so the
        // callback cannot keep the reference past its own return.
        match self {
            Self::Virtio { nic, index, len } => {
                let result = f(nic.rx_frame(index, len));
                nic.rx_done(index);
                result
            }
            Self::Intel { nic, frame } => {
                let result = f(nic.rx_frame(&frame));
                nic.rx_done(frame);
                result
            }
        }
    }
}

struct DmaTxToken<'a> {
    nic: &'a Card,
}

impl phy::TxToken for DmaTxToken<'_> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        // Filling and sending are one call, because the buffer belongs to the
        // descriptor the driver picks: a frame written before one was taken
        // would be written into a buffer the device may still be reading.
        self.nic.tx(len, f)
    }
}

// --- Socket tracking ---

enum SocketKind {
    TcpStream(SocketHandle),
    TcpListener(SocketHandle),
    Udp(SocketHandle),
}

struct UdpPipes {
    tx_read: Pipe,
    rx_write: Pipe,
}

struct PendingUdpRecv {
    client: Client,
    socket_id: u32,
    max_len: u32,
}

/// A piped TCP connection: data flows through kernel pipes instead of IPC messages.
///
/// **The socket and its id live exactly as long as this does.** What ends a
/// connection is what the kernel says of the client's pipe ends and what the
/// peer says on the wire; a close request only asks for that end early, and a
/// client that dies sends none.
///
/// **A client is gone when the kernel says so of both its ends**
/// ([`OTHER_END_GONE`], watched on each pipe for as long as netstack holds
/// it), whatever its send pipe still holds and whether or not the socket
/// takes bytes. A direction netstack itself has closed counts as gone. Such a
/// connection is [`ownerless`]: the wire has [`OWNERLESS_LIFE`] to finish it,
/// and a reset that then cannot leave has [`RESET_LIFE`]. A client holding an
/// end of a direction still open is never timed.
struct PipedConnection {
    socket_id: u32,
    handle: SocketHandle,
    rx_write: Option<Pipe>,
    tx_read: Option<Pipe>,
    /// The kernel said no holder of the send pipe's write end is left. What
    /// the pipe holds is still the peer's.
    writer_gone: bool,
    /// When a pass first found the client gone, or cut the connection.
    ownerless: Option<Instant>,
    /// [`Ownerless::Cut`] was this connection's answer.
    cut: bool,
    /// The client's receive pipe refused bytes the socket still holds, so the
    /// pipe is watched for room.
    held: bool,
}

impl PipedConnection {
    /// **A client's handle that refuses netstack for any reason but a full pipe or
    /// a vanished reader ends that client's connection, never netstack.** The
    /// ends are whatever the client moved, and nothing checks their kind at
    /// intake: a read end or a handle with no `WRITE` right answers a refusal
    /// here, and one that is no pipe end has its watch refused. So does a pipe
    /// whose ring page could not be allocated, which no wait cures.
    fn refuse(&mut self, socket: &mut tcp::Socket, end: &str, e: toyos_abi::syscall::SyscallError) {
        say!("netstack: resetting a connection — its {end} pipe refused netstack: {e:?}");
        socket.abort();
        self.close_all();
    }

    fn close_rx(&mut self) {
        self.rx_write.take();
    }

    fn close_tx(&mut self) {
        self.tx_read.take();
    }

    fn close_all(&mut self) {
        self.close_rx();
        self.close_tx();
    }

    /// The client holds no end of a direction that is still open.
    fn clientless(&self) -> bool {
        self.rx_write.is_none() && (self.tx_read.is_none() || self.writer_gone)
    }

    /// What the kernel answered a watch on the client's send pipe, or on its
    /// receive pipe. **Of a pipe netstack still holds**: closing one ends its
    /// watch, and that end is an answer too.
    fn pipe_answered(
        &mut self,
        socket: &mut tcp::Socket,
        send: bool,
        answer: Result<u32, toyos_abi::syscall::SyscallError>,
    ) {
        let (held, end) = if send { (&self.tx_read, "send") } else { (&self.rx_write, "receive") };
        match answer {
            _ if held.is_none() => {}
            Err(e) => self.refuse(socket, end, e),
            Ok(met) if met & OTHER_END_GONE == 0 => {}
            Ok(_) if send => self.writer_gone = true,
            // Nobody is left to read it.
            Ok(_) => self.close_rx(),
        }
    }
}

/// A piped TCP listener: netstack writes 1 byte to notify pipe on new connection.
struct PipedListener {
    handle: SocketHandle,
    notify_write: Pipe,
    listening: listen::Listening,
}

struct PendingPipedConnect {
    client: Client,
    socket_id: u32,
    handle: SocketHandle,
    /// Held from the moment the request arrived. The ends came *with* it, so
    /// there is nothing left to open when the handshake completes and nothing
    /// to fail there — where a pipe id could still be refused after netstack had
    /// already told smoltcp to connect.
    pipes: DataPipes,
    deadline: Option<Instant>,
}

/// The two ends of a client's data path, as the client's request handed them
/// over.
///
/// A pipe end travels as itself now: the client makes both pipes, keeps the
/// ends facing itself, and moves these two. They used to be ids in the request
/// payload, which netstack reopened by number — and any peer of the pipe's creator
/// could have named the same one.
struct DataPipes {
    to_client: Pipe,
    from_client: Pipe,
}

impl DataPipes {
    /// Take the pair the frame just read off `client` promised.
    fn take(client: &Client) -> Option<Self> {
        let [to_client, from_client] = client.conn.recv_handles_exact::<{ DATA_HANDLES }>()?;
        Some(Self {
            to_client: unsafe { Pipe::from_raw(to_client) },
            from_client: unsafe { Pipe::from_raw(from_client) },
        })
    }
}

/// Whether `socket` takes a client's bytes now: it is sending, and its send
/// buffer has room.
fn send_room(socket: &tcp::Socket) -> bool {
    socket.can_send() && socket.send_capacity() > socket.send_queue()
}

/// Whether `socket` has said its last to its peer: it is closed, and the reset
/// an abort owes has left. `TimeWait` only waits.
fn spent(socket: &tcp::Socket) -> bool {
    !socket.is_open() && !(socket.state() == tcp::State::Closed && socket.remote_endpoint().is_some())
}

/// How long the wire has to finish a connection whose client's pipe ends are
/// both gone, before netstack resets it: R2, the time RFC 9293 §3.8.3 gives a
/// segment's retransmission before the connection is closed, at the 100
/// seconds it asks for at least.
///
/// **From the client's leaving and not from the peer's last word**, so a peer
/// that keeps answering holds a slot no longer than one that says nothing:
/// RFC 9293 §3.8.6.1 lets a system reclaim a connection its peer holds open.
const OWNERLESS_LIFE: Duration = Duration::from_secs(100);

/// How long the reset of a connection cut at [`OWNERLESS_LIFE`] has to leave.
/// A connection that sent and heard nothing for that long has outlived its
/// next hop's neighbour entry, so the reset waits on an ARP answer.
///
/// Address resolution's own budget: RFC 4861 §7.2.2 fails it after
/// `MAX_MULTICAST_SOLICIT` solicitations `RETRANS_TIMER` apart, 3 and 1,000
/// milliseconds in §10. That is IPv6's; RFC 1122 §2.3.2.1 gives ARP a rate of
/// one request a second per destination and no count, and smoltcp asks at that
/// rate for as long as the socket lives.
const RESET_LIFE: Duration = Duration::from_secs(3);

/// What a pass makes of a connection whose client is gone.
#[derive(Debug, PartialEq, Eq)]
enum Ownerless {
    /// The wire still owes something, and has time left.
    Waits,
    /// Reset at [`OWNERLESS_LIFE`] with the wire unfinished, and kept until
    /// the reset has left or [`RESET_LIFE`] is over.
    Cut,
    /// The wire is finished.
    Over,
    /// Let go with a reset that never left: no next hop took it.
    Unsaid,
}

/// The pass's answer for `socket`, whose client has been gone for `waited`, or
/// which was cut `waited` ago.
fn ownerless(socket: &mut tcp::Socket, waited: Duration, cut: bool) -> Ownerless {
    if spent(socket) {
        Ownerless::Over
    } else if waited < if cut { RESET_LIFE } else { OWNERLESS_LIFE } {
        Ownerless::Waits
    } else if cut {
        Ownerless::Unsaid
    } else {
        socket.abort();
        Ownerless::Cut
    }
}

fn piped_connection(socket_id: u32, handle: SocketHandle, pipes: DataPipes) -> PipedConnection {
    PipedConnection {
        socket_id,
        handle,
        rx_write: Some(pipes.to_client),
        tx_read: Some(pipes.from_client),
        writer_gone: false,
        ownerless: None,
        cut: false,
        held: false,
    }
}

/// Poll registrations that are not piped connections: the service listener and
/// the NIC claim.
const FIXED_POLL_HANDLES: u32 = 2;

/// Registrations one piped connection can make in a batch: its send pipe and
/// its receive pipe.
const POLL_HANDLES_PER_PIPED: u32 = 2;

/// Registrations the lookups make in a batch: each waiting client's
/// connection, which is how netstack hears it hang up.
const LOOKUP_POLL_HANDLES: u32 = resolve::MAX_LOOKUPS as u32;

/// Hard ceiling on live piped connections, from the poller rather than from
/// memory: netstack registers every connection's pipes in the same batch as the two
/// fixed registrations, the pending connections and the lookups' clients, and
/// `Poller::MAX_HANDLES` is the widest set one poller can carry. The memory
/// budget below binds first on a machine whose eighth holds fewer connections.
const MAX_PIPED_SLOTS: u64 = ((Poller::MAX_HANDLES - FIXED_POLL_HANDLES - MAX_PENDING_CONNS - LOOKUP_POLL_HANDLES)
    / POLL_HANDLES_PER_PIPED) as u64;

/// Payload bytes a UDP socket's receive buffer holds, and therefore the longest
/// datagram netstack can ever hand back — which is what bounds the buffer
/// [`Netstack::deliver_datagram`] sizes from a client's `max_len`.
const UDP_SOCKET_BUFFER: usize = 65536;

/// Payload bytes each direction of a TCP socket buffers inside netstack, before the
/// window closes and the peer is asked to wait.
const TCP_SOCKET_BUFFER: usize = 65536;

/// Physical memory one piped connection costs. A kernel pipe is exactly one
/// 2 MiB page (`kernel/src/pipe.rs`: `PIPE_SIZE = PAGE_2M`) and a piped socket
/// is two of them, one per direction. The client allocates them, but netstack
/// holding the far ends is what keeps them alive, so this is netstack's to bound.
const PIPED_CONNECTION_BYTES: u64 = 2 * 2 * 1024 * 1024;

/// Share of physical memory netstack will keep tied up in client pipes.
///
/// Policy, not derivation, and the same eighth the compositor takes for the
/// same reason: nothing in the kernel says what a process may use — no
/// per-process limit, no pressure signal, no OOM killer — so the quantity that
/// would make this derivable does not exist yet.
const PIPE_BUDGET_SHARE: u64 = 8;

/// How many piped connections netstack will hold, given total physical memory.
///
/// An eighth of memory divided by the two pipes a connection costs, floored at
/// one and capped at what one poller can watch.
///
/// **A mitigation, not a policy anyone chose.** A piped connection's 4 MiB is
/// charged to nobody — no per-process limit, no pressure signal, no OOM killer
/// (`issues/no-physical-memory-fairness.md`) — so without a cap a client that opens sockets
/// in a loop walks the machine into exhaustion, and netstack has no way to tell
/// that from ordinary use. Delete this in favour of a kernel memory limit, not
/// in favour of a bigger number.
fn max_piped_connections(total_mem: u64) -> usize {
    let budget = total_mem / PIPE_BUDGET_SHARE;
    (budget / PIPED_CONNECTION_BYTES).clamp(1, MAX_PIPED_SLOTS) as usize
}

/// Total physical memory, as the kernel reports it.
fn total_memory() -> u64 {
    let mut buf = [0u8; toyos::system::SYSINFO_HEADER_SIZE];
    let n = toyos::system::sysinfo(&mut buf);
    assert!(n >= toyos::system::SYSINFO_HEADER_SIZE, "sysinfo returned {n} bytes");
    toyos_abi::syscall::SysinfoHeader::decode(&buf).memory_total
}

/// Where this netstack's socket ids start: at random, and never 0.
///
/// **A client holds a socket id across netstack being replaced** (`toyos-swap`): it
/// learns the old netstack is gone when a request fails, and closing what it held
/// is its first reaction. Every netstack counting from 1 made that stale number
/// another client's live socket in the new one. A random start makes two
/// instances' ranges overlap only by a chance the size of their lengths over
/// 2^32, which bounds the harm and does not remove it: an id is a number any
/// client can name (`issues/netstack-socket-ids-are-ambient.md`).
fn first_socket_id() -> u32 {
    let mut bytes = [0u8; 4];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("netstack: the kernel's random source refused the first socket id: {e:?}"));
    u32::from_le_bytes(bytes).max(1)
}

/// smoltcp's one random source, which every TCP connection's initial sequence
/// number (RFC 6528 asks for one an off-path sender cannot predict) and every
/// DHCP transaction ID are drawn from. `Config::new` seeds it with 0, which
/// is the same sequence on every boot of every machine.
fn smoltcp_seed() -> u64 {
    let mut bytes = [0u8; 8];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("netstack: the kernel's random source refused smoltcp's seed: {e:?}"));
    u64::from_le_bytes(bytes)
}

struct Netstack {
    sockets: HashMap<u32, SocketKind>,
    next_id: u32,
    next_local_port: u16,
    pending_udp_recvs: Vec<PendingUdpRecv>,
    resolver: resolve::Resolver<Client, fn() -> u16>,
    piped_connections: Vec<PipedConnection>,
    piped_listeners: HashMap<u32, PipedListener>,
    pending_piped_connects: Vec<PendingPipedConnect>,
    udp_pipes: HashMap<u32, UdpPipes>,
    max_piped_connections: usize,
}

impl Netstack {
    fn new(max_piped_connections: usize) -> Self {
        Self {
            sockets: HashMap::new(),
            next_id: first_socket_id(),
            next_local_port: 49152,
            pending_udp_recvs: Vec::new(),
            resolver: resolve::Resolver::new(Instant::now(), resolve::random_u16),
            piped_connections: Vec::new(),
            piped_listeners: HashMap::new(),
            pending_piped_connects: Vec::new(),
            udp_pipes: HashMap::new(),
            max_piped_connections,
        }
    }

    /// Is there room for one more piped connection?
    ///
    /// Counts the connects still waiting for their SYN-ACK: they each already
    /// name a pair of pipes, so leaving them out would let a burst of
    /// `TCP_CONNECT_PIPED` overshoot the cap by the whole burst.
    fn piped_room(&self) -> bool {
        self.piped_live() < self.max_piped_connections
    }

    /// Connections the cap is counting. Reported by both refusals, because
    /// `piped_connections.len()` alone reads as "0 already, max 126" when a
    /// burst of connects fills the pending list — a refusal that looks like a
    /// bug in the check rather than the check working.
    fn piped_live(&self) -> usize {
        self.piped_connections.len() + self.pending_piped_connects.len()
    }

    /// The socket table's size, as `inspect` reads it: counts, and no
    /// endpoint, because every client holding `netstack` can ask.
    ///
    /// `sockets.untabled` is every socket the stack holds that no table entry
    /// names, the resolver's left out: netstack's own, and any that outlived its
    /// entry, which moves it and no other count. The resolver's are left out
    /// because their number is every program's lookups in flight.
    fn inspect(&self, snap: &mut Snapshot, socket_set: &SocketSet<'_>) {
        let untabled = socket_set
            .iter()
            .count()
            .checked_sub(self.sockets.len() + self.resolver.sockets())
            .expect("netstack: a table entry or a lookup names a socket the stack does not hold");
        snap.put("sockets.untabled", untabled);
        let (mut streams, mut listeners, mut udp) = (0u32, 0u32, 0u32);
        for kind in self.sockets.values() {
            match kind {
                SocketKind::TcpStream(_) => streams += 1,
                SocketKind::TcpListener(_) => listeners += 1,
                SocketKind::Udp(_) => udp += 1,
            }
        }
        snap.put("sockets.tcp", streams);
        snap.put("sockets.listeners", listeners);
        snap.put("sockets.udp", udp);
        snap.put("piped.live", self.piped_live());
        snap.put("piped.ownerless", self.piped_connections.iter().filter(|c| c.ownerless.is_some()).count());
        snap.put("piped.max", self.max_piped_connections);
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = match self.next_id.wrapping_add(1) {
            0 => 1,
            next => next,
        };
        id
    }

    fn alloc_port(&mut self) -> u16 {
        let port = self.next_local_port;
        self.next_local_port = if self.next_local_port >= 65535 { 49152 } else { self.next_local_port + 1 };
        port
    }

    /// The first port from [`alloc_port`](Self::alloc_port)'s cursor that no
    /// UDP socket holds, and the cursor moves past it; `None` once every one
    /// is held.
    fn alloc_free_udp_port(&mut self, socket_set: &SocketSet<'_>) -> Option<u16> {
        self.next_local_port = resolve::free_port(socket_set, self.next_local_port)?;
        Some(self.alloc_port())
    }

    /// Dispatch one whole request.
    ///
    /// A synchronous handler answers and lets the connection close where it
    /// stands; an asynchronous one moves the [`Client`] into its pending list
    /// and answers when what it started finishes.
    fn handle_message(
        &mut self,
        req: Request,
        socket_set: &mut SocketSet<'_>,
        iface: &mut Interface,
    ) {
        match MsgType::from_u32(req.msg_type) {
            Some(MsgType::TcpClose) => self.handle_tcp_close(&req, socket_set),
            Some(MsgType::TcpShutdown) => self.handle_tcp_shutdown(&req, socket_set),
            Some(MsgType::UdpBind) => self.handle_udp_bind(&req, socket_set),
            Some(MsgType::UdpSendTo) => self.handle_udp_send_to(&req, socket_set),
            Some(MsgType::UdpRecvFrom) => self.handle_udp_recv_from(req, socket_set),
            Some(MsgType::UdpClose) => self.handle_udp_close(&req, socket_set),
            Some(MsgType::DnsLookup) => self.handle_dns_lookup(req, socket_set),
            Some(MsgType::TcpSetOption) => self.handle_tcp_set_option(&req, socket_set),
            Some(MsgType::TcpGetOption) => self.handle_tcp_get_option(&req, socket_set),
            Some(MsgType::TcpConnectPiped) => self.handle_tcp_connect_piped(req, socket_set, iface),
            Some(MsgType::TcpBindPiped) => self.handle_tcp_bind_piped(&req, socket_set),
            Some(MsgType::TcpAcceptPiped) => self.handle_tcp_accept_piped(&req, socket_set),
            None => {
                say!("netstack: unknown message type {}", req.msg_type);
                req.client.error(ERR_INVALID_INPUT);
            }
        }
    }

    fn handle_tcp_close(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(kind) = self.sockets.remove(&req.socket_id) {
            match kind {
                SocketKind::TcpStream(handle) => {
                    socket_set.get_mut::<tcp::Socket>(handle).close();
                    socket_set.remove(handle);
                    if let Some(pos) = self.piped_connections.iter().position(|c| c.handle == handle) {
                        self.piped_connections.swap_remove(pos).close_all();
                    }
                    // A connect still waiting for its SYN-ACK names the
                    // handle just removed, and the pass that would read it
                    // next is a panic; its client is answered instead.
                    if let Some(pos) =
                        self.pending_piped_connects.iter().position(|c| c.handle == handle)
                    {
                        self.pending_piped_connects.swap_remove(pos).client.error(ERR_CONNECTION_REFUSED);
                    }
                }
                SocketKind::TcpListener(handle) => {
                    socket_set.get_mut::<tcp::Socket>(handle).abort();
                    socket_set.remove(handle);
                    self.piped_listeners.remove(&req.socket_id);
                }
                SocketKind::Udp(handle) => {
                    socket_set.get_mut::<udp::Socket>(handle).close();
                    socket_set.remove(handle);
                    self.udp_pipes.remove(&req.socket_id);
                }
            }
        }
        msg.client.done();
    }

    fn handle_tcp_shutdown(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<TcpShutdownRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::TcpStream(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let socket = socket_set.get_mut::<tcp::Socket>(*handle);
        if req.how == 1 || req.how == 2 {
            socket.close();
        }
        msg.client.done();
    }

    fn handle_udp_bind(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<UdpBindRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(pipes) = DataPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let (rx_write, tx_read) = (pipes.to_client, pipes.from_client);
        let port = match req.port {
            0 => match self.alloc_free_udp_port(socket_set) {
                Some(port) => port,
                None => {
                    msg.client.error(ERR_ADDR_IN_USE);
                    return;
                }
            },
            port if resolve::udp_port_taken(socket_set, port) => {
                msg.client.error(ERR_ADDR_IN_USE);
                return;
            }
            port => port,
        };

        let rx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 16],
            vec![0u8; UDP_SOCKET_BUFFER],
        );
        let tx_buf = udp::PacketBuffer::new(
            vec![udp::PacketMetadata::EMPTY; 16],
            vec![0u8; UDP_SOCKET_BUFFER],
        );
        let mut socket = udp::Socket::new(rx_buf, tx_buf);
        // **The unspecified address binds as no address at all.** smoltcp
        // reads `Some(0.0.0.0)` as a socket for datagrams addressed to
        // 0.0.0.0, and none ever is, so a socket bound the ordinary way to
        // receive on every address would receive nothing but broadcast.
        let addr = Ipv4Addr::from(req.addr);
        let endpoint = IpListenEndpoint { addr: (!addr.is_unspecified()).then_some(IpAddress::Ipv4(addr)), port };
        socket
            .bind(endpoint)
            .unwrap_or_else(|e| panic!("netstack: a fresh socket refused to bind the free port {port}: {e:?}"));

        let handle = socket_set.add(socket);
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::Udp(handle));
        self.udp_pipes.insert(socket_id, UdpPipes { tx_read, rx_write });

        msg.client.result(&UdpBindResponse {
            socket_id,
            bound_port: port,
            _pad: 0,
        });
    }

    fn handle_udp_send_to(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<UdpSendToRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };

        let Some(SocketKind::Udp(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let handle = *handle;

        let Some(pipes) = self.udp_pipes.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };

        let mut buf = vec![0u8; req.len as usize];
        let n = match toyos_abi::syscall::read_nonblock(pipes.tx_read.as_handle(), &mut buf) {
            Ok(n) => n,
            // The client writes the datagram into the pipe and *then* sends this
            // request, so an empty pipe is a client naming bytes it never put
            // there. A blocking read here waits for a second write that a
            // conforming client never makes.
            Err(toyos_abi::syscall::SyscallError::WouldBlock) => {
                msg.client.error(ERR_INVALID_INPUT);
                return;
            }
            Err(_) => {
                msg.client.error(ERR_OTHER);
                return;
            }
        };

        let addr = Ipv4Addr::from(req.addr);
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(addr), req.port);
        let socket = socket_set.get_mut::<udp::Socket>(handle);
        match socket.send_slice(&buf[..n], endpoint) {
            Ok(()) => msg.client.result(&(n as u32)),
            Err(_) => msg.client.error(ERR_OTHER),
        }
    }

    /// Take one waiting datagram off `socket_id` for `client`, or hand the
    /// client back when none has arrived.
    ///
    /// **A datagram goes into the client's pipe whole, or its socket ends.**
    /// The answer names a length, and a write takes as much as the pipe has
    /// room for and cannot be taken back: a client reading that length out of
    /// a pipe holding part of this datagram would splice the next one onto it.
    /// So a pipe that will not take one whole — full, gone, or not a pipe netstack
    /// can write — ends the socket by name and answers its client a reset, and
    /// nothing can follow the part it did take.
    fn deliver_datagram(
        &mut self,
        client: Client,
        socket_id: u32,
        max_len: u32,
        socket_set: &mut SocketSet<'_>,
    ) -> Option<Client> {
        let (Some(&SocketKind::Udp(handle)), Some(pipes)) =
            (self.sockets.get(&socket_id), self.udp_pipes.get(&socket_id))
        else {
            client.error(ERR_NOT_CONNECTED);
            return None;
        };
        let socket = socket_set.get_mut::<udp::Socket>(handle);
        if !socket.can_recv() {
            return Some(client);
        }
        // `max_len` is the client's number. Clamped rather than trusted: the
        // socket's own receive buffer is 65536 bytes, so no datagram it can hand
        // back is longer, and an unclamped `vec!` here is a 4 GiB allocation any
        // client can ask netstack to make.
        let mut bytes = vec![0u8; (max_len as usize).min(UDP_SOCKET_BUFFER)];
        let (n, meta) = match socket.recv_slice(&mut bytes) {
            Ok(got) => got,
            Err(_) => {
                client.error(ERR_OTHER);
                return None;
            }
        };
        let wrote = toyos_abi::syscall::write_nonblock(pipes.rx_write.as_handle(), &bytes[..n]);
        if wrote == Ok(n) {
            let IpAddress::Ipv4(addr) = meta.endpoint.addr;
            client.result(&UdpRecvResponse { addr: addr.octets(), port: meta.endpoint.port, len: n as u16 });
            return None;
        }
        match wrote {
            Ok(took) => say!("netstack: ending UDP socket {socket_id} — its receive pipe took {took} of a {n}-byte datagram"),
            Err(e) => say!("netstack: ending UDP socket {socket_id} — its receive pipe refused a {n}-byte datagram: {e:?}"),
        }
        socket.close();
        socket_set.remove(handle);
        self.sockets.remove(&socket_id);
        self.udp_pipes.remove(&socket_id);
        client.error(ERR_CONNECTION_RESET);
        None
    }

    fn handle_udp_recv_from(&mut self, msg: Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<UdpRecvFromRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(client) = self.deliver_datagram(msg.client, req.socket_id, req.max_len, socket_set) {
            // Nothing has arrived yet: keep the connection open until one does.
            self.pending_udp_recvs.push(PendingUdpRecv {
                client,
                socket_id: req.socket_id,
                max_len: req.max_len,
            });
        }
    }

    fn handle_udp_close(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Some(SocketKind::Udp(handle)) = self.sockets.remove(&req.socket_id) {
            socket_set.get_mut::<udp::Socket>(handle).close();
            socket_set.remove(handle);
            self.udp_pipes.remove(&req.socket_id);
        }
        msg.client.done();
    }

    /// Start resolving the name `msg` carries, or answer at once where there
    /// is nothing to ask: an address written as one, or a name no server can
    /// be asked for.
    fn handle_dns_lookup(&mut self, msg: Request, socket_set: &mut SocketSet<'_>) {
        let Ok(hostname) = std::str::from_utf8(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Ok(ip) = hostname.parse::<std::net::Ipv4Addr>() {
            answer_lookup(&msg.client, &[ip.octets()]);
            return;
        }
        let Ok(name) = toyos_dns::Name::parse(hostname) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Err((client, why)) = self.resolver.start(msg.client, name, socket_set, Instant::now()) {
            client.error(why.code());
        }
    }

    fn handle_tcp_set_option(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::TcpStream(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let socket = socket_set.get_mut::<tcp::Socket>(*handle);
        match req.option {
            OPT_NODELAY => {
                socket.set_nagle_enabled(req.value == 0);
                msg.client.done();
            }
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    fn handle_tcp_get_option(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(SocketKind::TcpStream(handle)) = self.sockets.get(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let socket = socket_set.get_mut::<tcp::Socket>(*handle);
        match req.option {
            OPT_NODELAY => {
                let val = if socket.nagle_enabled() { 0u32 } else { 1u32 };
                msg.client.result(&SocketOptionResponse { value: val });
            }
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    // --- Piped socket handlers ---

    fn handle_tcp_connect_piped(
        &mut self,
        msg: Request,
        socket_set: &mut SocketSet<'_>,
        iface: &mut Interface,
    ) {
        let Ok(req) = ipc::decode_payload::<TcpConnectPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // Refused before the socket exists, so a refusal leaves nothing to
        // unwind and no SYN on the wire. An error return, never a panic: the
        // request is a client's and asking for one connection too many is not
        // a bug in netstack.
        //
        // Not `ERR_CONNECTION_REFUSED`, which this file already uses below for
        // a pending connect whose socket reached `Closed` — the peer's answer.
        // On one code a client cannot tell "this machine is full, back off"
        // from "that peer says no, give up".
        if !self.piped_room() {
            say!(
                "netstack: refusing connect, {} piped connections already (max {})",
                self.piped_live(),
                self.max_piped_connections,
            );
            msg.client.error(ERR_RESOURCE_EXHAUSTED);
            return;
        }
        // Taken before the socket exists, for the same reason the capacity
        // check is: a missing pair leaves nothing to unwind and no SYN on the
        // wire.
        let Some(pipes) = DataPipes::take(&msg.client) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let remote = IpEndpoint::new(
            IpAddress::Ipv4(Ipv4Addr::from(req.addr)),
            req.port,
        );
        if req.port == 0 || remote.addr.is_unspecified() {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        }
        // **This machine holding no address is not a peer's refusal.** Before
        // the lease there is no source for a SYN, and the socket's own
        // `Unaddressable` would reach the client as `ERR_CONNECTION_REFUSED`,
        // which says "that peer says no, give up" about a condition of this
        // machine that clears when the lease lands.
        if iface.ipv4_addr().is_none() {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        }
        let local_port = self.alloc_port();

        let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let mut socket = tcp::Socket::new(rx_buf, tx_buf);
        if socket.connect(iface.context(), remote, local_port).is_err() {
            msg.client.error(ERR_CONNECTION_REFUSED);
            return;
        }

        let handle = socket_set.add(socket);
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::TcpStream(handle));

        let deadline = if req.timeout_ms > 0 {
            Some(Instant::now() + Duration::from_millis(req.timeout_ms as u64))
        } else {
            None
        };

        // Async — hold the connection until the handshake completes.
        self.pending_piped_connects.push(PendingPipedConnect {
            client: msg.client,
            socket_id,
            handle,
            pipes,
            deadline,
        });
    }

    fn handle_tcp_bind_piped(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<TcpBindPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let port = if req.port == 0 { self.alloc_port() } else { req.port };

        // Take the pipe before the socket goes into socket_set: a missing one
        // then has no half-built socket to unwind.
        let Some([notify]) = msg.client.conn.recv_handles_exact::<{ NOTIFY_HANDLES }>() else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let notify_write = unsafe { Pipe::from_raw(notify) };

        let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let mut socket = tcp::Socket::new(rx_buf, tx_buf);
        if socket.listen(port).is_err() {
            msg.client.error(ERR_ADDR_IN_USE);
            return;
        }

        let handle = socket_set.add(socket);
        let socket_id = self.alloc_id();
        self.sockets.insert(socket_id, SocketKind::TcpListener(handle));

        self.piped_listeners.insert(socket_id, PipedListener {
            handle,
            notify_write,
            listening: listen::Listening::new(port),
        });

        msg.client.result(&TcpBindResponse {
            socket_id,
            bound_port: port,
            _pad: 0,
        });
    }

    fn handle_tcp_accept_piped(&mut self, msg: &Request, socket_set: &mut SocketSet<'_>) {
        let Ok(req) = ipc::decode_payload::<TcpAcceptPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let (room, pipes) = (self.piped_room(), DataPipes::take(&msg.client));
        let Some(listener) = self.piped_listeners.get_mut(&req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let (old_handle, local_port) = (listener.handle, listener.listening.port());
        let pipes = match listener.listening.accept(socket_set.get_mut::<tcp::Socket>(old_handle), room, pipes) {
            listen::Accept::Take(pipes) => pipes,
            listen::Accept::NoPipes => {
                msg.client.error(ERR_INVALID_INPUT);
                return;
            }
            listen::Accept::NoRoom => {
                say!(
                    "netstack: refusing accept, {} piped connections already (max {})",
                    self.piped_live(),
                    self.max_piped_connections,
                );
                msg.client.error(ERR_RESOURCE_EXHAUSTED);
                return;
            }
            listen::Accept::Nothing => {
                msg.client.error(ERR_NOT_CONNECTED);
                return;
            }
        };

        let remote = socket_set.get_mut::<tcp::Socket>(old_handle).remote_endpoint().unwrap();
        let remote_addr = match remote.addr {
            IpAddress::Ipv4(a) => a.octets(),
        };

        let stream_id = self.alloc_id();
        self.sockets.insert(stream_id, SocketKind::TcpStream(old_handle));

        self.piped_connections.push(piped_connection(stream_id, old_handle, pipes));

        // Create replacement listener
        let rx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; TCP_SOCKET_BUFFER]);
        let mut new_listener = tcp::Socket::new(rx_buf, tx_buf);
        // A fresh socket on the port an established one holds: neither refusal
        // `listen` has (port 0, a socket not closed) can be this one.
        new_listener
            .listen(local_port)
            .unwrap_or_else(|e| panic!("netstack: a fresh socket refused to listen on {local_port}: {e:?}"));
        let new_handle = socket_set.add(new_listener);
        self.sockets.insert(req.socket_id, SocketKind::TcpListener(new_handle));

        self.piped_listeners
            .get_mut(&req.socket_id)
            .expect("looked up above; nothing between there and here removes a piped_listeners entry")
            .handle = new_handle;

        msg.client.result(&TcpAcceptPipedResponse {
            socket_id: stream_id,
            remote_addr,
            remote_port: remote.port,
            local_port,
        });
    }

    /// Bridge data between smoltcp sockets and kernel pipes for piped connections.
    /// Drains both directions as far as the other side takes — when a pipe is
    /// full, data stays in smoltcp's buffer and the TCP window shrinks.
    fn bridge_piped(&mut self, socket_set: &mut SocketSet<'_>) {
        use toyos_abi::syscall::SyscallError;
        let mut closed = Vec::new();
        for i in 0..self.piped_connections.len() {
            let conn = &mut self.piped_connections[i];
            let socket = socket_set.get_mut::<tcp::Socket>(conn.handle);

            // smoltcp rx → the client's pipe. **Nothing leaves the socket that
            // the pipe did not take**: a byte dequeued here has already been
            // acknowledged to the peer, so one the pipe refused is cut out of
            // the middle of the client's stream with nothing saying so. The
            // rest waits in the socket, and the pipe's room is what wakes the
            // pass that moves it.
            conn.held = false;
            if let Some(ref pipe) = conn.rx_write {
                let mut refused = None;
                while socket.can_recv() {
                    let moved = socket.recv(|queued| {
                        match toyos_abi::syscall::write_nonblock(pipe.as_handle(), queued) {
                            Ok(n) => (n, n),
                            Err(e) => {
                                refused = Some(e);
                                (0, 0)
                            }
                        }
                    });
                    if !matches!(moved, Ok(n) if n > 0) {
                        break;
                    }
                }
                match refused {
                    None => {}
                    // Full: the client has not read yet.
                    Some(SyscallError::WouldBlock) => conn.held = true,
                    Some(SyscallError::Gone) => conn.close_rx(),
                    Some(e) => conn.refuse(socket, "receive", e),
                }
            }

            // pipe read → smoltcp tx. Ok(0) is the kernel's EOF — ring drained,
            // no writer — which says the client stopped writing; not the
            // forgeable closed flags. [`send_room`] and never `can_send` alone:
            // a zero-length read answers `Ok(0)`, which the arm below reads as
            // the client hanging up.
            while send_room(socket) {
                if let Some(ref pipe) = conn.tx_read {
                    // **No more is taken out of the pipe than the socket will
                    // take from us.** `send_slice` answers how many bytes it
                    // enqueued and takes fewer when the send buffer is short of
                    // room; bytes read past that are gone, and the peer's stream
                    // is short in the middle with nothing saying so. The pipe is
                    // where the rest belongs until there is room.
                    let mut buf = [0u8; 4096];
                    let want = (socket.send_capacity() - socket.send_queue()).min(buf.len());
                    match toyos_abi::syscall::read_nonblock(pipe.as_handle(), &mut buf[..want]) {
                        Ok(0) => {
                            socket.close();
                            conn.close_tx();
                            break;
                        }
                        Ok(n) => {
                            // Both refusals are bytes the pipe has already given
                            // up, so neither may be swallowed here of all places.
                            let sent = socket.send_slice(&buf[..n]).unwrap_or_else(|e| {
                                panic!("netstack: a socket that could send refused {n} byte(s): {e:?}")
                            });
                            assert_eq!(sent, n, "netstack: the send buffer took {sent} of {n} byte(s) it had room for");
                        }
                        Err(SyscallError::WouldBlock) => break,
                        Err(e) => {
                            conn.refuse(socket, "send", e);
                            break;
                        }
                    }
                } else {
                    break;
                }
            }

            // Signal EOF to client when remote has closed and all data is drained
            if !socket.may_recv() && !socket.can_recv() && conn.rx_write.is_some() {
                conn.close_rx();
            }

            // **A connection that is over takes no more of the client's bytes.**
            // A peer's reset leaves the socket `Closed` and `can_send` false for
            // good, so the loop above never reads the pipe again; left open, the
            // client's writes fill a pipe nobody drains and then block, and a
            // writer that is never told its peer is gone cannot say so.
            if !socket.is_open() && conn.tx_read.is_some() {
                conn.close_tx();
            }

            if conn.clientless() {
                let waited = conn.ownerless.get_or_insert_with(Instant::now).elapsed();
                match ownerless(socket, waited, conn.cut) {
                    Ownerless::Waits => {}
                    Ownerless::Cut => {
                        say!(
                            "netstack: resetting a connection — its client left {}s ago and its peer has not finished it",
                            waited.as_secs()
                        );
                        (conn.ownerless, conn.cut) = (Some(Instant::now()), true);
                    }
                    Ownerless::Over => {
                        if conn.cut {
                            say!("netstack: the reset has left");
                        }
                        closed.push(i);
                    }
                    Ownerless::Unsaid => {
                        say!(
                            "netstack: letting a connection go with its reset unsent — no next hop took it in {}s",
                            RESET_LIFE.as_secs()
                        );
                        closed.push(i);
                    }
                }
            }
        }

        for &i in closed.iter().rev() {
            let conn = self.piped_connections.swap_remove(i);
            socket_set.remove(conn.handle);
            self.sockets.remove(&conn.socket_id);
        }
    }

    /// How long until the first connection with no client left reaches
    /// [`OWNERLESS_LIFE`], or a cut one [`RESET_LIFE`], which nothing on the
    /// wire wakes a pass for.
    fn ownerless_wake_in(&self) -> Option<Duration> {
        self.piped_connections
            .iter()
            .filter_map(|c| Some((c.ownerless?, if c.cut { RESET_LIFE } else { OWNERLESS_LIFE })))
            .map(|(since, life)| life.saturating_sub(since.elapsed()))
            .min()
    }

    /// What the kernel answered a watch on a pipe of connection `socket_id`,
    /// which may be gone since: the watch of a pipe closed with it ends too.
    fn pipe_answered(
        &mut self,
        socket_set: &mut SocketSet<'_>,
        socket_id: u32,
        send: bool,
        answer: Result<u32, toyos_abi::syscall::SyscallError>,
    ) {
        if let Some(conn) = self.piped_connections.iter_mut().find(|c| c.socket_id == socket_id) {
            conn.pipe_answered(socket_set.get_mut::<tcp::Socket>(conn.handle), send, answer);
        }
    }

    /// Tell each piped listener's owner about a connection it can accept, and
    /// close every listener whose notify pipe refuses netstack.
    ///
    /// **One write a pass, and any refusal ends the listener.** A listener
    /// owed a wake writes its byte; the rest write zero bytes, which move
    /// nothing and are still refused by name once the owner has gone. A wake
    /// the pipe will not take is one the owner never gets, so its `accept`
    /// would wait forever on a listener netstack still held; closing the listener
    /// is what tells it instead — its notify pipe reads EOF. A full pipe is
    /// that refusal too: it is an owner that has left a whole pipe of wakes
    /// unread.
    fn serve_piped_listeners(&mut self, socket_set: &mut SocketSet<'_>, room: bool) {
        use toyos_abi::syscall::SyscallError;
        let mut dead = Vec::new();
        for (&socket_id, listener) in &mut self.piped_listeners {
            let wake = listener.listening.wake(socket_set.get_mut::<tcp::Socket>(listener.handle), room);
            match toyos_abi::syscall::write_nonblock(listener.notify_write.as_handle(), wake) {
                Ok(_) => {}
                Err(SyscallError::WouldBlock) if wake.is_empty() => {}
                // Its owner has gone, which is the ordinary end of a listener.
                Err(SyscallError::Gone) => dead.push(socket_id),
                Err(e) => {
                    say!("netstack: closing listener {socket_id} — its notify pipe refused netstack: {e:?}");
                    dead.push(socket_id);
                }
            }
        }
        for socket_id in dead {
            if let Some(_listener) = self.piped_listeners.remove(&socket_id) {
                if let Some(kind) = self.sockets.remove(&socket_id) {
                    if let SocketKind::TcpListener(handle) = kind {
                        socket_set.get_mut::<tcp::Socket>(handle).abort();
                        socket_set.remove(handle);
                    }
                }
            }
        }
    }

    /// Process pending async operations (UDP recvs, lookups, piped connects),
    /// and say whether there is room for another piped connection after them.
    fn process_pending(&mut self, socket_set: &mut SocketSet<'_>) -> bool {
        let now = Instant::now();

        for pr in std::mem::take(&mut self.pending_udp_recvs) {
            if let Some(client) = self.deliver_datagram(pr.client, pr.socket_id, pr.max_len, socket_set) {
                self.pending_udp_recvs.push(PendingUdpRecv { client, ..pr });
            }
        }

        for (client, name, ended) in self.resolver.pass(socket_set, now) {
            use resolve::Ended;
            use toyos_dns::Failure;
            match ended {
                Ok(addrs) => answer_lookup(&client, &addrs),
                // The protocol's one answer for a name with no address,
                // whether the name or only its address is missing.
                Err(Ended::Failed(Failure::NoSuchName | Failure::NoAddress)) => answer_lookup(&client, &[]),
                Err(Ended::Failed(Failure::TimedOut)) => client.error(ERR_TIMED_OUT),
                Err(Ended::Failed(why @ (Failure::Truncated | Failure::ServerFailed(_) | Failure::TooManyAliases))) => {
                    say!("netstack: a lookup of {name} ended without an answer: {why:?}");
                    client.error(ERR_OTHER);
                }
                Err(Ended::NoPort) => {
                    say!("netstack: a lookup of {name} ended with every dynamic port bound, none left for its next query");
                    client.error(ERR_RESOURCE_EXHAUSTED);
                }
            }
        }

        // Pending piped connects
        let mut i = 0;
        while i < self.pending_piped_connects.len() {
            let pc = &self.pending_piped_connects[i];
            let socket = socket_set.get_mut::<tcp::Socket>(pc.handle);
            if socket.may_send() {
                let local_port = socket.local_endpoint().map(|e| e.port).unwrap_or(0);
                let resp = TcpConnectResponse {
                    socket_id: pc.socket_id,
                    local_port,
                    _pad: 0,
                };
                pc.client.result(&resp);
                let pc = self.pending_piped_connects.swap_remove(i);
                self.piped_connections.push(piped_connection(pc.socket_id, pc.handle, pc.pipes));
                continue;
            }
            if socket.state() == tcp::State::Closed {
                pc.client.error(ERR_CONNECTION_REFUSED);
                let (socket_id, handle) = (pc.socket_id, pc.handle);
                self.sockets.remove(&socket_id);
                socket_set.remove(handle);
                self.pending_piped_connects.swap_remove(i);
                continue;
            }
            if pc.deadline.is_some_and(|d| now >= d) {
                pc.client.error(ERR_TIMED_OUT);
                socket.abort();
                let (socket_id, handle) = (pc.socket_id, pc.handle);
                self.sockets.remove(&socket_id);
                socket_set.remove(handle);
                self.pending_piped_connects.swap_remove(i);
                continue;
            }
            i += 1;
        }
        self.piped_room()
    }
}

/// The most addresses one lookup's answer carries: what
/// `toyos::net::dns_lookup`'s 256-byte buffer holds, a count byte and five
/// bytes an address. A resolver may answer with a subset of a name's
/// addresses, and these are the ones the server put first.
const MAX_ANSWERED: usize = (256 - 1) / 5;

/// A lookup's answer: a count, then each address behind the family tag 4.
fn answer_lookup(client: &Client, addrs: &[[u8; 4]]) {
    let addrs = &addrs[..addrs.len().min(MAX_ANSWERED)];
    let mut answer = vec![addrs.len() as u8];
    for addr in addrs {
        answer.push(4);
        answer.extend_from_slice(addr);
    }
    client.result_bytes(&answer);
}

/// Answer `inspect` with what this pass knows, in one non-blocking write, and
/// let the connection close as every other answer does.
///
/// Here and not in [`Netstack::handle_message`] because the card and the
/// lease are the loop's and not the socket table's.
fn answer_inspect(request: &Request, daemon: &Netstack, card: &Card, dhcp: &dhcp::Dhcp, socket_set: &SocketSet<'_>) {
    // The request is a bare header, and anything riding on one is not this
    // protocol.
    if request.payload_len != 0 {
        request.client.error(ERR_INVALID_INPUT);
        return;
    }
    let mut snap = Snapshot::new(toyos_inspect::NET);
    card.inspect(&mut snap);
    dhcp.inspect(&mut snap);
    daemon.inspect(&mut snap, socket_set);
    let encoded = snap.encode().unwrap_or_else(|why| panic!("netstack: its snapshot: {why}"));
    request.client.snapshot(&encoded);
}

const _: () = assert!(
    toyos_inspect::MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame"
);

fn main() {
    // **The order this used to have was load-bearing and is now moot.** The
    // device was claimed before the name was published, because a client that
    // connected while netstack was still in `DmaNic::open` reached a listener owned
    // by a process about to return and got its request answered by nobody —
    // sshserver found it, took its `panic!` arm and put a tokio backtrace across the
    // boot. There is no window left to order around: the `netstack` port exists
    // before either process does, a client's connection is queued on it whether
    // or not this program ever reaches `accept`, and if netstack exits the queued
    // client sees `Gone` rather than silence.
    let Some((open, claim)) = CARDS
        .iter()
        .find_map(|(id, open)| endow::pci_function::<toyos::PciDev>(*id).map(|c| (*open, c)))
    else {
        say!("netstack: no NIC on this machine, exiting");
        return;
    };
    let acceptor = endow::acceptor("netstack")
        .expect("the manifest declares this program serves `netstack`");
    let nic = open(claim);
    // The link as the card came up with it, which the first change a pass
    // reports is measured against. Virtio reports no link changes at all.
    let mut link_up = match &nic {
        Card::Intel(intel) => intel.link().is_up(),
        Card::Virtio(_) => true,
    };
    let mac = nic.mac();
    let mut device = DmaNic { nic };

    say!(
        "netstack: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    );
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    config.random_seed = smoltcp_seed();
    let epoch = Instant::now();
    let now = SmoltcpInstant::from_millis(0);
    let mut iface = Interface::new(config, &mut device, now);

    let mut socket_set = SocketSet::new(vec![]);

    let dhcp_handle = socket_set.add(dhcp::socket());
    let mut dhcp = dhcp::Dhcp::new();
    if let Card::Intel(nic) = &device.nic {
        nic.accept_multicast(toyos_mdns::GROUP_MAC);
    }
    let mut mdns = mdns::Responder::new(dhcp::HOSTNAME, &mut iface, &mut socket_set);

    let total_mem = total_memory();
    let max_piped = max_piped_connections(total_mem);
    let mut daemon = Netstack::new(max_piped);

    // Sized for the slot ceiling rather than for `max_piped`: the batch
    // between two `wait` calls is the two fixed registrations, at most two per live
    // piped connection, one per pending connection and one per lookup, and the
    // ceiling is what that can never exceed.
    let poller = Poller::new(
        FIXED_POLL_HANDLES
            + POLL_HANDLES_PER_PIPED * MAX_PIPED_SLOTS as u32
            + MAX_PENDING_CONNS
            + LOOKUP_POLL_HANDLES,
    );
    const TOKEN_LISTENER: u64 = 0;
    const TOKEN_NIC: u64 = 1;
    // A piped connection's two pipes, by its socket id in the low word: an
    // answer names the connection it was asked of and no place in a list,
    // which a connection let go since would hand to another.
    const TOKEN_SEND_PIPE: u64 = 1 << 32;
    const TOKEN_RECEIVE_PIPE: u64 = 2 << 32;
    // Clear of a connection's own handle by more than `MAX_HANDLES` (4096,
    // `kernel/src/object/handle.rs`).
    const TOKEN_PENDING_BASE: u64 = 0x1_0000;
    // Clear of the pending range by the same margin.
    const TOKEN_LOOKUP_BASE: u64 = 0x2_0000;

    let mut pending: Vec<PendingConn> = Vec::new();

    loop {
        // Before `iface.poll`, because it is what makes the interrupt taken and
        // what gives a driver with a per-pass receive budget that budget back.
        if let Some(link) = device.nic.begin_pass() {
            // Down to up only, and only with no lease held: a speed change is
            // no new network, and a bound lease is kept across a flap rather
            // than given up — `dhcp::restart`'s own header. Before the poll
            // below, so the DISCOVER goes out on this pass.
            if link.is_up() && !link_up && !dhcp.leased() {
                dhcp::restart(socket_set.get_mut::<dhcpv4::Socket>(dhcp_handle));
            }
            link_up = link.is_up();
        }
        let now = SmoltcpInstant::from_millis(epoch.elapsed().as_millis() as i64);
        while iface.poll(now, &mut device, &mut socket_set) != PollResult::None {}
        device.nic.report();

        // **After the poll and before anything is served.** The lease is what
        // gives this machine an address, a route and its resolvers, so a client
        // answered before it was applied would be answered on a machine that is
        // on no network.
        let change = dhcp::Change::of(socket_set.get_mut::<dhcpv4::Socket>(dhcp_handle));
        if dhcp.pass(change, &mut iface, &mut daemon.resolver) {
            say!(
                "netstack: ready, at most {max_piped} piped connections \
                 ({} MiB each of {} MiB total)",
                PIPED_CONNECTION_BYTES / (1024 * 1024),
                total_mem / (1024 * 1024),
            );
        }

        mdns.pass(&iface, &mut socket_set, Instant::now());

        daemon.bridge_piped(&mut socket_set);

        let room = daemon.process_pending(&mut socket_set);
        daemon.serve_piped_listeners(&mut socket_set, room);

        // smoltcp's own next deadline — a retransmit, a persist probe, a
        // delayed ACK — and zero when it has a frame to send now. A piped
        // connection needs nothing else: its peer's bytes wake the NIC, and its
        // client's bytes and room wake the watches below.
        //
        // **None of them while the card has no room.** Each is a frame to send
        // or to take, the card refuses both, and smoltcp's "now" would be a
        // pass every time round until it stops; the card's claim begins the
        // pass that can.
        let smoltcp_due = if device.room() {
            iface.poll_delay(now, &socket_set).map_or(u64::MAX, |d| d.total_micros().saturating_mul(1000))
        } else {
            u64::MAX
        };

        // A pending UDP receive or connect has no wake of its own.
        let has_pending_async = !daemon.pending_udp_recvs.is_empty()
            || !daemon.pending_piped_connects.is_empty();
        let timeout = if has_pending_async {
            smoltcp_due.min(Duration::from_millis(1).as_nanos() as u64)
        } else {
            smoltcp_due
        };

        poller.watch(&acceptor, READABLE, TOKEN_LISTENER);
        poller.watch(device.nic.claim(), READABLE, TOKEN_NIC);

        // The client's bytes to send, room in a receive pipe that is holding
        // the peer's back, and the client letting go of either end: each is a
        // pass's worth of work.
        for conn in daemon.piped_connections.iter() {
            // Readable only while the socket can take the bytes: a pipe
            // holding some is readable until read, so its watch would complete
            // on every pass while the peer's window is shut. The ACK that makes
            // room wakes the NIC. Its writer's leaving is asked until answered,
            // for the same reason: it stays so.
            let room = send_room(socket_set.get::<tcp::Socket>(conn.handle));
            let send = if room { READABLE } else { 0 } | if conn.writer_gone { 0 } else { OTHER_END_GONE };
            if let (true, Some(pipe)) = (send != 0, &conn.tx_read) {
                poller.watch(pipe, send, TOKEN_SEND_PIPE | u64::from(conn.socket_id));
            }
            if let Some(pipe) = &conn.rx_write {
                let room = if conn.held { WRITABLE } else { 0 };
                poller.watch(pipe, room | OTHER_END_GONE, TOKEN_RECEIVE_PIPE | u64::from(conn.socket_id));
            }
        }

        for p in pending.iter() {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING_BASE + p.conn.as_handle().0 as u64);
        }

        // A client waiting on a lookup hangs up by closing its connection,
        // which makes it readable.
        for client in daemon.resolver.clients() {
            poller.watch(&client.conn, READABLE, TOKEN_LOOKUP_BASE + client.conn.as_handle().0 as u64);
        }

        let timeout = match mdns.wake_in(Instant::now()) {
            Some(left) => timeout.min(left.as_nanos() as u64),
            None => timeout,
        };
        // A lookup waiting on its answer is woken when its wait is over, to
        // ask the next server.
        let timeout = match daemon.resolver.wake_in(Instant::now()) {
            Some(left) => timeout.min(left.as_nanos() as u64),
            None => timeout,
        };
        let timeout = match daemon.ownerless_wake_in() {
            Some(left) => timeout.min(left.as_nanos() as u64),
            None => timeout,
        };
        // A card that never does what it owes sends no interrupt to say so.
        let timeout = timeout.min(device.nic.pass_due_in().unwrap_or(u64::MAX));
        // A client that connects and then says nothing wakes nothing, so the
        // deadline that removes it has to be a wake in its own right: without
        // this netstack can sit in `wait` forever with `pending` full of clients
        // whose handshake is already over its time.
        let timeout = if pending.is_empty() {
            timeout
        } else {
            timeout.min(HANDSHAKE_TIMEOUT.as_nanos() as u64)
        };

        let mut ready: Vec<u64> = Vec::new();
        poller.wait_answers(1, timeout, |token, answer| match token & !u64::from(u32::MAX) {
            TOKEN_SEND_PIPE => daemon.pipe_answered(&mut socket_set, token as u32, true, answer),
            TOKEN_RECEIVE_PIPE => daemon.pipe_answered(&mut socket_set, token as u32, false, answer),
            _ => ready.push(token),
        });

        // A handshake that never completes is why this deadline exists, and the
        // sweep has to happen on a pass that found nothing ready too —
        // otherwise a silent client is only ever timed out by some *other*
        // client's traffic.
        let now_wall = Instant::now();
        for p in pending.iter().filter(|p| now_wall.duration_since(p.since) >= HANDSHAKE_TIMEOUT) {
            say!(
                "netstack: dropping client {} — it never finished its request",
                p.conn.as_handle().0
            );
        }
        pending.retain(|p| now_wall.duration_since(p.since) < HANDSHAKE_TIMEOUT);

        // A lookup whose client has left ends now, not when its servers are
        // done with it: its sockets and its slot are another client's.
        let spoke = |c: &Client| ready.contains(&(TOKEN_LOOKUP_BASE + c.conn.as_handle().0 as u64));
        daemon.resolver.let_go(&mut socket_set, |c| spoke(c) && c.gone());

        // Accept and the request are two events. Nothing is read here: a client
        // that connects and then says nothing costs a slot and a deadline, not
        // the network stack.
        if ready.contains(&TOKEN_LISTENER) {
            let conn = acceptor.accept().expect("accept failed");
            if pending.len() >= MAX_PENDING_CONNS as usize {
                say!(
                    "netstack: refusing client {} — {MAX_PENDING_CONNS} connections are already \
                     waiting to say what they want",
                    conn.as_handle().0
                );
            } else {
                pending.push(PendingConn { conn, rx: ClientRx::new(), since: Instant::now() });
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
                answer_inspect(&request, &daemon, &device.nic, &dhcp, &socket_set);
                continue;
            }
            daemon.handle_message(request, &mut socket_set, &mut iface);
        }
    }
}
