//! A client's requests onto the node's calls, and the node's answers back
//! onto `toyos::net`'s protocol.
//!
//! **One request is one call of the node's**, and every decision about a
//! socket is the node's: what is here is the number a client names a listener
//! or a datagram socket by, the clients that wait for an answer, the handles
//! netstack watches, and the word each of the node's refusals is written in. A
//! datagram's bytes travel in the socket's own pipes, which are netstack's and
//! not the node's, and reach no call.
//!
//! **A stream is its region and the connection its connect or accept came on**
//! (`toyos::net::stream`): netstack judges the region before it maps it
//! (`lent`), makes the stream's bell, answers once with the bell, and from then
//! watches the connection once, for the client's rings and its hang-up, which
//! is the stream's close. A client names no stream by a number.
//!
//! **Untrusted input.** A request's every field is its client's number: an id
//! that names nothing, or a socket of another kind, is refused
//! `ERR_NOT_CONNECTED`, a payload that is not the request's struct, a port of
//! zero and a name that is none `ERR_INVALID_INPUT`, and a length is a bound
//! on a read and never an allocation's size past what one frame carries. A
//! request that waits holds its client's connection, so what waits is
//! bounded: one receive a socket, a second refused `ERR_RESOURCE_EXHAUSTED`.
//! Nothing a client sends panics netstack.
//!
//! **A lookup's view is the badge on its connection**, which the supervisor
//! minted with the row and session of the program it endowed the connector,
//! and which is bytes here: what the node keeps of one view's answers answers
//! no other. A lookup on a connection with no badge is refused
//! `ERR_PERMISSION_DENIED`. Neither the name a lookup asks nor anything kept is
//! said in the log or to `inspect`, which every holder of `netstack` reads.
//!
//! **An id is a number any client can name**
//! (`issues/netstack-socket-ids-are-ambient.md`): the table starts at a
//! random one so that an id a client kept across a replaced netstack names
//! nothing in the next.

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;
use std::time::Duration;

use std::rc::{Rc, Weak};

use toyos::net::*;
use toyos::poller::{OTHER_END_GONE, READABLE};
use toyos::port::Acceptor;
use toyos::{ipc, say, AsHandle, Connection, OwnedHandle, Pipe};
use toyos_abi::syscall::{SyscallError, MAX_BADGE};
use toyos_inspect::Snapshot;
use toyos_net_node::{
    AcceptRefused, ConnectRefused, DatagramId, Ended, Event, ListenRefused, ListenerId, LookupId, Node, NotStarted,
    Refused, Resolving, StreamEvent, StreamId, WriteRefusal,
};
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{Endpoint, Failure};
use toyos_net_wire::{Instant, Port};

use crate::client::{Client, Request};
use crate::lent::Region;
use crate::pipes::{self, Watched};
use crate::watches::Watches;
use crate::HOSTNAME;

/// A watch's token names the socket it was asked of by its id, in the low
/// word, and never a place in a list: an answer can arrive after its socket
/// is gone, and must then name nothing.
const TOKEN_STREAM: u64 = 1 << 32;
const TOKEN_LISTENER: u64 = 3 << 32;
const TOKEN_DATAGRAM: u64 = 4 << 32;
/// A client that waits for its lookup's answer, by its connection's handle.
/// Clear of the loop's own tokens and of a pending connection's.
const TOKEN_LOOKUP: u64 = 5 << 32;
const TOKEN_KIND: u64 = !(u32::MAX as u64);

/// Watches the lookups make: each waiting client's connection.
pub const LOOKUP_WATCHES: u32 = toyos_dns::MAX_LOOKUPS as u32;

/// The most addresses one lookup's answer carries: what
/// `toyos::net::dns_lookup`'s 256-byte buffer holds, a count byte and five
/// bytes an address.
const MAX_ANSWERED: usize = (256 - 1) / 5;

/// What a client's id names.
enum Socket {
    Listener { id: ListenerId, wakes: Watched },
    Datagram(Datagram),
}

/// A datagram socket's two pipes, which carry its payloads between its client
/// and netstack: the node holds a datagram only between two calls.
struct Datagram {
    id: DatagramId,
    to_client: Pipe,
    from_client: Pipe,
}

/// A stream's connection as netstack watches it, for as long as the node
/// holds the stream's session.
struct Stream {
    id: StreamId,
    connection: Weak<Connection>,
    /// The connect not yet answered: its connection, and the bell's read end
    /// its answer hands over.
    answer: Option<(Rc<Connection>, Pipe)>,
}

/// A client waiting for a datagram to reach its socket.
struct Receiving {
    client: Client,
    max_len: u32,
}

pub struct Sockets {
    ids: HashMap<u32, Socket>,
    next_id: u32,
    /// Each stream by the key its watch's token carries.
    streams: HashMap<u32, Stream>,
    /// The key each stream is kept under.
    by_stream: BTreeMap<StreamId, u32>,
    lookups: Vec<(LookupId, Client)>,
    /// At most one receive waits on a socket, so a client's requests alone
    /// hold no more of netstack's handles than its sockets do.
    receiving: BTreeMap<u32, Receiving>,
    /// The places the node was given.
    places: usize,
    /// A stream's connection was answered in this wake: one pass over the
    /// streams follows the wake, however many answers it carried.
    bridge: bool,
    /// The streams whose client rang in this wake.
    rung: Vec<StreamId>,
}

/// A stream's region off a request's connection, judged, and its bell made:
/// the session the node is lent and the bell's read end the answer hands
/// over. Refused by name, nothing is mapped.
fn lend(client: Client) -> Result<(Region, Pipe, Rc<Connection>), (Client, u32)> {
    let Some([region]) = client.conn.recv_handles_exact::<1>() else { return Err((client, ERR_INVALID_INPUT)) };
    let Ok((bell, ring)) = toyos::pipe_pair() else {
        // The handle is consumed either way.
        toyos_abi::syscall::close(region);
        return Err((client, ERR_RESOURCE_EXHAUSTED));
    };
    let connection = Rc::new(client.conn);
    match Region::lent(region, connection.clone(), ring) {
        Ok(session) => Ok((session, bell, connection)),
        Err(why) => {
            say!("netstack: refusing a stream's region — {why}");
            let Ok(conn) = Rc::try_unwrap(connection) else { unreachable!("a refused region shares its connection") };
            Err((Client { conn }, ERR_INVALID_INPUT))
        }
    }
}

/// Answers a stream's request with its bell and `answer`, on the connection
/// that is the stream's from here on.
fn hand_over<T: ipc::IpcPayload>(connection: &Connection, bell: Pipe, answer: &T) {
    let sent = connection.send_handles([OwnedHandle::from(bell)]).map_err(ipc::TrySendError::Syscall);
    if let Err(why) = sent.and_then(|()| connection.try_send(RespType::Result as u32, answer)) {
        say!("netstack: a stream's client left before its answer: {why:?}");
    }
}

/// The pipe ABI's word for each of the node's.
fn refused(refusal: Refused) -> u32 {
    match refusal {
        Refused::AddrInUse => ERR_ADDR_IN_USE,
        Refused::NotConnected => ERR_NOT_CONNECTED,
        Refused::InvalidInput => ERR_INVALID_INPUT,
        Refused::PermissionDenied => ERR_PERMISSION_DENIED,
        Refused::ResourceExhausted => ERR_RESOURCE_EXHAUSTED,
    }
}

/// A connect that made no stream. A machine with no address or no route yet
/// is `ERR_NOT_CONNECTED`, which clears when a lease lands, and never a
/// peer's refusal; no free port is `ERR_ADDR_IN_USE`.
fn connect_refused(refusal: ConnectRefused) -> u32 {
    use toyos_net_tcp::Error;
    match refusal {
        ConnectRefused::Full => ERR_RESOURCE_EXHAUSTED,
        ConnectRefused::Stack(ConnectError::Route(_)) => ERR_NOT_CONNECTED,
        ConnectRefused::Stack(ConnectError::NotUnicast | ConnectError::Tcp(Error::InvalidRemote)) => ERR_INVALID_INPUT,
        ConnectRefused::Stack(ConnectError::Tcp(Error::AddrInUse)) => ERR_ADDR_IN_USE,
        ConnectRefused::Stack(ConnectError::Tcp(
            why @ (Error::NoSuchSocket | Error::NotConnected | Error::Exists | Error::Closing | Error::WouldBlock | Error::Failed(_)),
        )) => {
            say!("netstack: the stack refused an active open as {why:?}, which is no refusal of one");
            ERR_OTHER
        }
    }
}

/// A handshake that ended without a connection. The pipe ABI has a word for a
/// peer's refusal, its reset and its silence, and none for an ICMP error's:
/// those are named in the log and answered `ERR_OTHER`
/// (`issues/the-pipe-abi-has-no-word-for-an-unreachable-host-or-a-lookup-to-try-again.md`).
fn connect_failed(failure: Failure) -> u32 {
    match failure {
        Failure::Refused => ERR_CONNECTION_REFUSED,
        Failure::Reset => ERR_CONNECTION_RESET,
        Failure::TimedOut => ERR_TIMED_OUT,
        Failure::Unreachable(_) | Failure::Prohibited => {
            say!("netstack: a connect ended {failure:?}");
            ERR_OTHER
        }
    }
}

fn listen_refused(refusal: ListenRefused) -> u32 {
    match refusal {
        ListenRefused::Full => ERR_RESOURCE_EXHAUSTED,
        // The word a datagram socket's bind to such an address is answered in.
        ListenRefused::NotLocal => ERR_INVALID_INPUT,
        ListenRefused::InUse => ERR_ADDR_IN_USE,
    }
}

fn accept_refused(refusal: AcceptRefused) -> u32 {
    match refusal {
        AcceptRefused::NoListener | AcceptRefused::Nothing => ERR_NOT_CONNECTED,
        AcceptRefused::NoRegion => ERR_INVALID_INPUT,
        AcceptRefused::Full => ERR_RESOURCE_EXHAUSTED,
    }
}

/// A lookup's answer: a count, then each address behind the family tag 4. A
/// resolver may answer with a subset of a name's addresses, and these are the
/// ones the server put first.
fn answer_lookup(client: &Client, addrs: &[[u8; 4]]) {
    let mut answer = vec![addrs.len().min(MAX_ANSWERED) as u8];
    for addr in addrs.iter().take(MAX_ANSWERED) {
        answer.push(4);
        answer.extend_from_slice(addr);
    }
    client.result_bytes(&answer);
}

/// How a lookup ended, kept or asked, in the pipe ABI's words.
fn answer_resolved(client: &Client, result: Result<Vec<[u8; 4]>, Ended>) {
    use toyos_dns::Failure as Dns;
    match result {
        Ok(addrs) => answer_lookup(client, &addrs),
        // The protocol's one answer for a name with no address, whether the
        // name or only its address is missing.
        Err(Ended::Failed(Dns::NoSuchName | Dns::NoAddress)) => answer_lookup(client, &[]),
        Err(Ended::Failed(Dns::TimedOut)) => client.error(ERR_TIMED_OUT),
        // No query found a way out, or the lease the lookup asked under went:
        // this machine is on no network that answers the name, which a lease
        // clears. Neither is that word's (see `connect_failed`).
        Err(Ended::Failed(Dns::Unreachable) | Ended::LeaseChanged) => client.error(ERR_NOT_CONNECTED),
        Err(Ended::Failed(why @ (Dns::Truncated | Dns::ServerFailed(_) | Dns::TooManyAliases))) => {
            say!("netstack: a lookup ended without an answer: {why:?}");
            client.error(ERR_OTHER);
        }
        Err(Ended::NoPort) => {
            say!("netstack: a lookup ended with every dynamic port bound, none left for its next query");
            client.error(ERR_RESOURCE_EXHAUSTED);
        }
    }
}

impl Sockets {
    pub fn new(first_id: u32, places: usize) -> Self {
        Self {
            ids: HashMap::new(),
            next_id: first_id.max(1),
            streams: HashMap::new(),
            by_stream: BTreeMap::new(),
            lookups: Vec::new(),
            receiving: BTreeMap::new(),
            places,
            bridge: false,
            rung: Vec::new(),
        }
    }

    /// An id no socket and no stream is kept under, and never 0.
    fn alloc_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = match self.next_id.wrapping_add(1) {
                0 => 1,
                next => next,
            };
            if !self.ids.contains_key(&id) && !self.streams.contains_key(&id) {
                return id;
            }
        }
    }

    fn hold_stream(&mut self, id: StreamId, connection: &Rc<Connection>, answer: Option<Pipe>) {
        let key = self.alloc_id();
        let answer = answer.map(|bell| (connection.clone(), bell));
        self.streams.insert(key, Stream { id, connection: Rc::downgrade(connection), answer });
        self.by_stream.insert(id, key);
    }

    fn listener(&self, socket_id: u32) -> Option<ListenerId> {
        match self.ids.get(&socket_id) {
            Some(Socket::Listener { id, .. }) => Some(*id),
            Some(Socket::Datagram(_)) | None => None,
        }
    }

    fn datagram(&self, socket_id: u32) -> Option<&Datagram> {
        match self.ids.get(&socket_id) {
            Some(Socket::Datagram(socket)) => Some(socket),
            Some(Socket::Listener { .. }) | None => None,
        }
    }

    /// One whole request. A call that answers at once drops its client where
    /// it answers; a connect, a lookup and a receive with nothing to hand
    /// over keep theirs until the node has the answer ([`Self::settle`]).
    pub fn request(&mut self, node: &mut Node, now: Instant, req: Request, acceptor: &Acceptor, draw: fn() -> u32) {
        match MsgType::from_u32(req.msg_type) {
            Some(MsgType::TcpClose) => self.close(node, now, &req),
            Some(MsgType::UdpBind) => self.udp_bind(node, &req, draw),
            Some(MsgType::UdpSendTo) => self.udp_send_to(node, now, &req),
            Some(MsgType::UdpRecvFrom) => self.udp_recv_from(node, now, req),
            Some(MsgType::UdpClose) => self.udp_close(node, now, &req),
            Some(MsgType::DnsLookup) => self.lookup(node, now, req, acceptor, draw),
            Some(MsgType::TcpListenerSetOption) => self.listener_set_option(node, &req),
            Some(MsgType::UdpSetOption) => self.udp_set_option(node, &req),
            Some(MsgType::TcpConnect) => self.connect(node, now, req),
            Some(MsgType::TcpBindPiped) => self.listen(node, &req, draw),
            Some(MsgType::TcpAccept) => self.accept(node, now, req),
            None => {
                say!("netstack: unknown message type {}", req.msg_type);
                req.client.error(ERR_INVALID_INPUT);
            }
        }
    }

    /// The client lets go of the listener or datagram socket its id names; an
    /// id that names nothing is let go already.
    fn close(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        match self.ids.remove(&req.socket_id) {
            Some(Socket::Listener { id, .. }) => {
                node.close_listener(now, id);
            }
            Some(Socket::Datagram(socket)) => self.end_datagram(node, now, socket),
            None => {}
        }
        msg.client.done();
    }

    fn listener_set_option(&mut self, node: &mut Node, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(id) = self.listener(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        match req.option {
            OPT_NODELAY if node.set_listener_nodelay(id, req.value != 0) => msg.client.done(),
            OPT_NODELAY => msg.client.error(ERR_NOT_CONNECTED),
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    fn udp_set_option(&mut self, node: &mut Node, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(socket) = self.datagram(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        match req.option {
            OPT_BROADCAST => match node.udp_set_broadcast(socket.id, req.value != 0) {
                Ok(()) => msg.client.done(),
                Err(refusal) => msg.client.error(refused(refusal)),
            },
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
    }

    /// A stream's connect: its region lent, its connection the stream's from
    /// here, and its answer once the node has one ([`Self::settle`]).
    fn connect(&mut self, node: &mut Node, now: Instant, msg: Request) {
        let Ok(req) = ipc::decode_payload::<TcpConnectRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(port) = Port::new(req.port) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let (session, bell, connection) = match lend(msg.client) {
            Ok(lent) => lent,
            Err((client, code)) => return client.error(code),
        };
        let remote = Endpoint { addr: Ipv4Addr::from(req.addr), port };
        let timeout = (req.timeout_ms > 0).then(|| Duration::from_millis(u64::from(req.timeout_ms)));
        match node.connect(now, remote, timeout, Box::new(session)) {
            Ok(stream) => self.hold_stream(stream, &connection, Some(bell)),
            Err(refusal) => {
                if refusal == ConnectRefused::Full {
                    say!("netstack: refusing connect, {} of {} places held", node.held(), self.places);
                }
                let Ok(conn) = Rc::try_unwrap(connection) else { unreachable!("a refused connect kept its session") };
                Client { conn }.error(connect_refused(refusal));
            }
        }
    }

    fn listen(&mut self, node: &mut Node, msg: &Request, draw: fn() -> u32) {
        let Ok(req) = ipc::decode_payload::<TcpBindPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some([wakes]) = msg.client.conn.recv_handles_exact::<{ NOTIFY_HANDLES }>() else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // SAFETY: the kernel moved the handle into this process with the frame
        // just read, and nothing else answers for it.
        let (held, wakes) = pipes::hold(unsafe { Pipe::from_raw(wakes) });
        match node.listen(Ipv4Addr::from(req.addr), Port::new(req.port), req.options.nodelay(), Box::new(held), draw) {
            Ok((listener, port)) => {
                let socket_id = self.alloc_id();
                self.ids.insert(socket_id, Socket::Listener { id: listener, wakes });
                msg.client.result(&TcpBindResponse { socket_id, bound_port: port.get(), _pad: 0 });
            }
            Err(refusal) => msg.client.error(listen_refused(refusal)),
        }
    }

    /// A stream's accept: its region lent, and the oldest connection waiting
    /// at the listener the stream on it, answered at once.
    fn accept(&mut self, node: &mut Node, now: Instant, msg: Request) {
        let Ok(req) = ipc::decode_payload::<TcpAcceptRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(listener) = self.listener(req.listener) else {
            msg.client.error(accept_refused(AcceptRefused::NoListener));
            return;
        };
        let (session, bell, connection) = match lend(msg.client) {
            Ok(lent) => lent,
            Err((client, code)) => {
                // Refused, the accept still spends its wake.
                let _ = node.accept(now, listener, None);
                return client.error(code);
            }
        };
        match node.accept(now, listener, Some(Box::new(session))) {
            Ok(accepted) => {
                hand_over(&connection, bell, &TcpAcceptResponse {
                    remote_addr: accepted.remote.addr.octets(),
                    remote_port: accepted.remote.port.get(),
                    local_port: accepted.local.get(),
                    nodelay: u32::from(accepted.nodelay),
                });
                self.hold_stream(accepted.id, &connection, None);
            }
            Err(refusal) => {
                if refusal == AcceptRefused::Full {
                    say!("netstack: refusing accept, {} of {} places held", node.held(), self.places);
                }
                let Ok(conn) = Rc::try_unwrap(connection) else { unreachable!("a refused accept kept its session") };
                Client { conn }.error(accept_refused(refusal));
            }
        }
    }

    fn udp_bind(&mut self, node: &mut Node, msg: &Request, draw: fn() -> u32) {
        let Ok(req) = ipc::decode_payload::<UdpBindRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some([to_client, from_client]) = msg.client.conn.recv_handles_exact::<{ DATA_HANDLES }>() else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // SAFETY: the kernel moved both handles into this process with the
        // frame just read, and nothing else answers for either.
        let (to_client, from_client) = unsafe { (Pipe::from_raw(to_client), Pipe::from_raw(from_client)) };
        match node.udp_bind(Ipv4Addr::from(req.addr), Port::new(req.port), draw) {
            Ok((id, port)) => {
                let socket_id = self.alloc_id();
                self.ids.insert(socket_id, Socket::Datagram(Datagram { id, to_client, from_client }));
                msg.client.result(&UdpBindResponse { socket_id, bound_port: port.get(), _pad: 0 });
            }
            Err(refusal) => msg.client.error(refused(refusal)),
        }
    }

    /// The client wrote the datagram into its socket's pipe and then sent this
    /// request, so the bytes the request names are there: a pipe that holds
    /// fewer is a client naming bytes it never wrote, and a read that would
    /// wait for them would wait on that client.
    fn udp_send_to(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<UdpSendToRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(socket) = self.datagram(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let mut payload = vec![0u8; usize::from(req.len)];
        // A datagram of no bytes put none in the pipe.
        let read = if payload.is_empty() { Ok(0) } else { socket.from_client.read_nonblock(&mut payload) };
        match read {
            Ok(read) if read == payload.len() => {}
            Ok(_) | Err(SyscallError::WouldBlock) => {
                msg.client.error(ERR_INVALID_INPUT);
                return;
            }
            Err(_) => {
                msg.client.error(ERR_OTHER);
                return;
            }
        }
        match node.udp_send_to(now, socket.id, Ipv4Addr::from(req.addr), req.port, &payload) {
            Ok(()) => msg.client.result(&u32::from(req.len)),
            Err(refusal) => msg.client.error(refused(refusal)),
        }
    }

    fn udp_recv_from(&mut self, node: &mut Node, now: Instant, msg: Request) {
        let Ok(req) = ipc::decode_payload::<UdpRecvFromRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        // A waiting client that hung up holds the socket's one wait for nobody.
        if let Some(waiting) = self.receiving.remove(&req.socket_id) {
            if !waiting.client.gone() {
                self.receiving.insert(req.socket_id, waiting);
                msg.client.error(ERR_RESOURCE_EXHAUSTED);
                return;
            }
        }
        let waiting = Receiving { client: msg.client, max_len: req.max_len };
        if let Some(waiting) = self.deliver(node, now, req.socket_id, waiting) {
            self.receiving.insert(req.socket_id, waiting);
        }
    }

    /// Hands `waiting` its socket's oldest datagram, or hands `waiting` back
    /// when none has arrived.
    ///
    /// **A datagram goes into the client's pipe whole, or its socket ends.**
    /// The answer names a length, and a write takes what the pipe has room
    /// for and cannot be taken back: a client reading that length out of a
    /// pipe holding part of this datagram would splice the next one onto it.
    fn deliver(&mut self, node: &mut Node, now: Instant, socket_id: u32, waiting: Receiving) -> Option<Receiving> {
        let Some(socket) = self.datagram(socket_id) else {
            waiting.client.error(ERR_NOT_CONNECTED);
            return None;
        };
        // The client's number bounds what it is handed and never what is
        // allocated: no datagram is longer than [udp] delivers.
        let room = usize::try_from(waiting.max_len).unwrap_or(usize::MAX).min(toyos_net_udp::limits::MAX_PAYLOAD);
        let mut payload = vec![0u8; room];
        let datagram = match node.udp_recv_from(socket.id, &mut payload) {
            Ok(Some(datagram)) => datagram,
            Ok(None) => return Some(waiting),
            Err(refusal) => {
                waiting.client.error(refused(refusal));
                return None;
            }
        };
        let wrote = socket.to_client.write_nonblock(&payload[..datagram.len]);
        if wrote == Ok(datagram.len) {
            waiting.client.result(&UdpRecvResponse {
                addr: datagram.source.octets(),
                port: datagram.source_port.map_or(0, Port::get),
                len: datagram.len as u16,
            });
            return None;
        }
        say!(
            "netstack: ending UDP socket {socket_id} — its receive pipe answered {wrote:?} to a {}-byte datagram",
            datagram.len
        );
        if let Some(Socket::Datagram(socket)) = self.ids.remove(&socket_id) {
            self.end_datagram(node, now, socket);
        }
        waiting.client.error(ERR_CONNECTION_RESET);
        None
    }

    fn end_datagram(&mut self, node: &mut Node, now: Instant, socket: Datagram) {
        if let Err(refusal) = node.udp_close(now, socket.id) {
            unreachable!("the node refused the close of a datagram socket its table held: {refusal:?}");
        }
    }

    fn udp_close(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if self.datagram(req.socket_id).is_some() {
            if let Some(Socket::Datagram(socket)) = self.ids.remove(&req.socket_id) {
                self.end_datagram(node, now, socket);
            }
        }
        msg.client.done();
    }

    /// Starts resolving the name `msg` carries for the view its connection's
    /// badge names, or answers at once where there is nothing to ask: an
    /// address written as one, or a name the view keeps an answer for.
    fn lookup(&mut self, node: &mut Node, now: Instant, msg: Request, acceptor: &Acceptor, draw: fn() -> u32) {
        let Ok(hostname) = std::str::from_utf8(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        if let Ok(ip) = hostname.parse::<Ipv4Addr>() {
            answer_lookup(&msg.client, &[ip.octets()]);
            return;
        }
        let Ok(name) = toyos_dns::Name::parse(hostname) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let mut badge = [0u8; MAX_BADGE];
        let view = match acceptor.badge(&msg.client.conn, &mut badge) {
            Ok(view) => view,
            Err(SyscallError::NotFound) => {
                msg.client.error(ERR_PERMISSION_DENIED);
                return;
            }
            Err(why) => panic!("netstack: the kernel refused the badge of a connection its acceptor accepted: {why:?}"),
        };
        match node.resolve(now, view, name, draw) {
            Ok(Resolving::Kept(result)) => answer_resolved(&msg.client, result),
            Ok(Resolving::Asking(id)) => self.lookups.push((id, msg.client)),
            Err(NotStarted::NotConnected) => msg.client.error(ERR_NOT_CONNECTED),
            Err(NotStarted::ResourceExhausted) => msg.client.error(ERR_RESOURCE_EXHAUSTED),
        }
    }

    /// Everything the node has to say since the last call, to the log and to
    /// the clients that waited for it, and the table entries of what the node
    /// let go.
    pub fn settle(&mut self, node: &mut Node, now: Instant) {
        for (socket_id, waiting) in std::mem::take(&mut self.receiving) {
            if let Some(waiting) = self.deliver(node, now, socket_id, waiting) {
                self.receiving.insert(socket_id, waiting);
            }
        }

        let events: Vec<StreamEvent> = node.drain_stream_events().collect();
        for event in events {
            let (id, answer) = match event {
                StreamEvent::Connected { id, local } => (id, Ok(local)),
                StreamEvent::Failed { id, failure } => (id, Err(connect_failed(failure))),
                StreamEvent::TimedOut { id } => (id, Err(ERR_TIMED_OUT)),
                // Its client left first, and nobody reads the answer.
                StreamEvent::Closed { id } => (id, Err(ERR_CONNECTION_REFUSED)),
                StreamEvent::Violated { violation, .. } => {
                    say!("netstack: resetting a stream — its client wrote what no client writes: {violation:?}");
                    continue;
                }
            };
            let answer_to = self.by_stream.get(&id).and_then(|key| self.streams.get_mut(key)?.answer.take());
            let Some((connection, bell)) = answer_to else { unreachable!("the node answered a connect nobody waits for") };
            match answer {
                Ok(local) => hand_over(&connection, bell, &TcpConnectResponse { local_port: local.get(), _pad: 0 }),
                Err(code) => {
                    if let Err(why) = connection.try_send(RespType::Error as u32, &ErrorResponse { code }) {
                        say!("netstack: a stream's client left before its answer: {why:?}");
                    }
                }
            }
        }

        for resolved in node.take_resolved() {
            let Some(at) = self.lookups.iter().position(|(id, _)| *id == resolved.id) else {
                unreachable!("the node ended a lookup nobody waits for")
            };
            let (_, client) = self.lookups.swap_remove(at);
            answer_resolved(&client, resolved.result);
        }

        let ended: Vec<(ListenerId, WriteRefusal)> = node.drain_ended_listeners().collect();
        for (ended, refusal) in ended {
            let found = self.ids.iter().find_map(|(socket_id, socket)| match socket {
                Socket::Listener { id, .. } if *id == ended => Some(*socket_id),
                Socket::Listener { .. } | Socket::Datagram(_) => None,
            });
            // None where its owner's close arrived in the pass that ended it.
            let Some(socket_id) = found else { continue };
            self.ids.remove(&socket_id);
            // Its owner gone is the ordinary end of a listener.
            if refusal != WriteRefusal::Gone {
                say!("netstack: closing listener {socket_id} — its notify pipe refused a wake: {refusal:?}");
            }
        }

        let events: Vec<Event> = node.drain_events().collect();
        for event in events {
            match event {
                Event::Stack { refusal, suppressed: 0 } => say!("netstack: refused {refusal:?}"),
                Event::Stack { refusal, suppressed } => say!("netstack: refused {refusal:?}, and {suppressed} more by its rule"),
                Event::Dhcp { refusal, suppressed: 0 } => say!("netstack: DHCP: refused {refusal:?}"),
                Event::Dhcp { refusal, suppressed } => say!("netstack: DHCP: refused {refusal:?}, and {suppressed} more by its rule"),
                Event::Name(toyos_mdns::Event::Claimed) => {
                    say!("netstack: mDNS: no host answered for {HOSTNAME}.local; this machine answers as it")
                }
                Event::Name(toyos_mdns::Event::Lost) => say!(
                    "netstack: mDNS: another host answered for {HOSTNAME}.local; this machine answers to no name and asks for {HOSTNAME}.local again every {} s",
                    toyos_mdns::RETRY_MS / 1000
                ),
            }
        }

        // A stream the node let go holds no session, and its key names nothing
        // from then.
        let by_stream = &mut self.by_stream;
        self.streams.retain(|_, stream| {
            let held = stream.connection.strong_count() > 0;
            if !held {
                by_stream.remove(&stream.id);
            }
            held
        });
    }

    /// Asks the kernel about everything a pass could be owed for: each
    /// stream's connection, for its client's rings once it is answered and its
    /// hang-up always, the wake pipe of each listener and one pipe of each
    /// datagram socket for its owner's leaving, and the connection of each
    /// client that waits for a lookup, which hangs up by closing it.
    pub fn watch(&self, poller: &mut Watches) {
        for (key, stream) in &self.streams {
            let Some(connection) = stream.connection.upgrade() else { continue };
            let flags = if stream.answer.is_some() { OTHER_END_GONE } else { READABLE | OTHER_END_GONE };
            poller.watch(&*connection, flags, TOKEN_STREAM | u64::from(*key));
        }
        for (socket_id, socket) in &self.ids {
            match socket {
                Socket::Listener { wakes, .. } => {
                    if let Some(pipe) = wakes.held() {
                        poller.watch(&*pipe, OTHER_END_GONE, TOKEN_LISTENER | u64::from(*socket_id));
                    }
                }
                Socket::Datagram(socket) => poller.watch(&socket.to_client, OTHER_END_GONE, TOKEN_DATAGRAM | u64::from(*socket_id)),
            }
        }
        for (_, client) in &self.lookups {
            poller.watch(&client.conn, READABLE, TOKEN_LOOKUP | u64::from(client.conn.as_handle().0));
        }
    }

    /// What the kernel answered one of [`Self::watch`]'s watches, or `false`
    /// for a token that is none of them. An answer about a socket or a stream
    /// that is gone since says nothing.
    pub fn answered(&mut self, node: &mut Node, now: Instant, token: u64, answer: Result<u32, SyscallError>) -> bool {
        let socket_id = token as u32;
        match token & TOKEN_KIND {
            TOKEN_STREAM => {
                let Some(stream) = self.streams.get(&socket_id) else { return true };
                let Some(connection) = stream.connection.upgrade() else { return true };
                let left = match answer {
                    Err(why) => {
                        say!("netstack: closing a stream — the kernel refused the watch of its connection: {why:?}");
                        true
                    }
                    Ok(met) if met & OTHER_END_GONE != 0 => true,
                    // Doorbells, all taken: what they rang for is the pass's to find.
                    Ok(_) => drained(&connection),
                };
                if left {
                    node.left(now, stream.id);
                } else {
                    self.rung.push(stream.id);
                    self.bridge = true;
                }
            }
            TOKEN_LISTENER => {
                let gone = answer.map_or(true, |met| met & OTHER_END_GONE != 0);
                if let (true, Some(id)) = (gone, self.listener(socket_id)) {
                    if let Err(why) = answer {
                        say!("netstack: closing listener {socket_id} — the kernel refused the watch of its notify pipe: {why:?}");
                    }
                    self.ids.remove(&socket_id);
                    node.close_listener(now, id);
                }
            }
            TOKEN_DATAGRAM => {
                let gone = answer.map_or(true, |met| met & OTHER_END_GONE != 0);
                if gone && self.datagram(socket_id).is_some() {
                    if let Err(why) = answer {
                        say!("netstack: ending UDP socket {socket_id} — the kernel refused the watch of its receive pipe: {why:?}");
                    }
                    if let Some(Socket::Datagram(socket)) = self.ids.remove(&socket_id) {
                        self.end_datagram(node, now, socket);
                    }
                }
            }
            TOKEN_LOOKUP => {
                // A lookup whose client has left ends now: its sockets and
                // its place are another client's.
                let spoke = |client: &Client| u64::from(client.conn.as_handle().0) == u64::from(socket_id);
                self.lookups.retain(|(id, client)| {
                    let left = spoke(client) && client.gone();
                    if left {
                        node.let_go(now, *id);
                    }
                    !left
                });
            }
            _ => return false,
        }
        true
    }

    /// The pass over the streams the last wake's answers asked for, once.
    pub fn bridge(&mut self, node: &mut Node, now: Instant) {
        if std::mem::take(&mut self.bridge) {
            node.bridge(now, self.rung.drain(..));
        }
    }

    /// The socket table as `inspect` reads it: counts, and no endpoint,
    /// because every client holding `netstack` can ask.
    pub fn inspect(&self, node: &Node, snap: &mut Snapshot) {
        let (mut listeners, mut udp) = (0u32, 0u32);
        for socket in self.ids.values() {
            match socket {
                Socket::Listener { .. } => listeners += 1,
                Socket::Datagram(_) => udp += 1,
            }
        }
        snap.put("sockets.tcp", self.streams.len() as u32);
        snap.put("sockets.listeners", listeners);
        snap.put("sockets.udp", udp);
        snap.put("streams.live", node.streams());
        snap.put("places.held", node.held());
        snap.put("places.max", self.places);
    }
}

/// Takes every ring a stream's client wrote on its connection: `true` where
/// the client hung up instead.
fn drained(connection: &Connection) -> bool {
    let mut bytes = [0u8; 64];
    loop {
        match connection.read_nonblock(&mut bytes) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(SyscallError::WouldBlock) => return false,
            Err(_) => return true,
        }
    }
}
