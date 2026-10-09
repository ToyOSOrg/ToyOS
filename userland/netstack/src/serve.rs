//! A client's requests onto the node's calls, and the node's answers back
//! onto the pipe ABI (`toyos::net`).
//!
//! **One request is one call of the node's**, and every decision about a
//! socket is the node's: what is here is the number a client names a socket
//! by, the clients that wait for an answer, the pipe ends netstack watches,
//! and the word each of the node's refusals is written in. Two requests reach
//! no call: a datagram's bytes travel in the socket's own pipes, which are
//! netstack's and not the node's, and a shutdown of the receiving half alone
//! asks nothing of the stack, std keeping that half's state itself.
//!
//! **Untrusted input.** A request's every field is its client's number: an id
//! that names nothing, or a socket of another kind, is refused
//! `ERR_NOT_CONNECTED`, a payload that is not the request's struct, a port of
//! zero and a name that is none `ERR_INVALID_INPUT`, and a length is a bound
//! on a read and never an allocation's size past what one frame carries.
//! Nothing a client sends panics netstack.
//!
//! **An id is a number any client can name**
//! (`issues/netstack-socket-ids-are-ambient.md`): the table starts at a
//! random one so that an id a client kept across a replaced netstack names
//! nothing in the next.

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;
use std::time::Duration;

use toyos::net::*;
use toyos::poller::{OTHER_END_GONE, READABLE, WRITABLE, Poller};
use toyos::{ipc, say, AsHandle, Pipe};
use toyos_abi::syscall::SyscallError;
use toyos_inspect::Snapshot;
use toyos_net_node::{
    AcceptRefused, ConnectRefused, DatagramId, Ended, Event, ListenRefused, ListenerId, LookupId, Node, NotStarted,
    PipeEnd, Pipes, Refused, StreamEvent, StreamId, WriteRefusal,
};
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{Endpoint, Failure};
use toyos_net_wire::{Instant, Port};

use crate::client::{Client, Request};
use crate::pipes::{self, Watched};

/// A watch's token names the socket it was asked of by its id, in the low
/// word, and never a place in a list: an answer can arrive after its socket
/// is gone, and must then name nothing.
const TOKEN_FROM_CLIENT: u64 = 1 << 32;
const TOKEN_TO_CLIENT: u64 = 2 << 32;
const TOKEN_LISTENER: u64 = 3 << 32;
const TOKEN_DATAGRAM: u64 = 4 << 32;
/// A client that waits for its lookup's answer, by its connection's handle.
/// Clear of the loop's own tokens and of a pending connection's.
const TOKEN_LOOKUP: u64 = 5 << 32;
const TOKEN_KIND: u64 = !(u32::MAX as u64);

/// Watches one place makes at most: a stream's two pipes.
pub const WATCHES_PER_PLACE: u32 = 2;

/// Watches the lookups make: each waiting client's connection.
pub const LOOKUP_WATCHES: u32 = toyos_dns::MAX_LOOKUPS as u32;

/// The most addresses one lookup's answer carries: what
/// `toyos::net::dns_lookup`'s 256-byte buffer holds, a count byte and five
/// bytes an address.
const MAX_ANSWERED: usize = (256 - 1) / 5;

/// What a client's id names.
enum Socket {
    Stream(StreamId),
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

/// The pipe ends of a stream the node holds, for as long as it holds either:
/// longer than its client's id names it, since a closed stream's pipe is
/// still read until it is empty.
struct Ends {
    stream: StreamId,
    to_client: Watched,
    from_client: Watched,
}

/// A client waiting for a datagram to reach its socket.
struct Receiving {
    client: Client,
    socket_id: u32,
    max_len: u32,
}

pub struct Sockets {
    ids: HashMap<u32, Socket>,
    next_id: u32,
    ends: HashMap<u32, Ends>,
    /// The id each stream's ends are kept under.
    by_stream: BTreeMap<StreamId, u32>,
    /// The client each unanswered connect is answered to, and the id it is
    /// told.
    connecting: BTreeMap<StreamId, (Client, u32)>,
    lookups: Vec<(LookupId, Client, toyos_dns::Name)>,
    receiving: Vec<Receiving>,
    /// The places the node was given.
    places: usize,
}

/// The two ends a connect or an accept moves, each split into the node's half
/// and the watched one.
fn data_pipes(client: &Client) -> Option<(Pipes, Watched, Watched)> {
    let [to_client, from_client] = client.conn.recv_handles_exact::<{ DATA_HANDLES }>()?;
    // SAFETY: the kernel moved both handles into this process with the frame
    // just read, and nothing else answers for either.
    let (to_client, from_client) = unsafe { (Pipe::from_raw(to_client), Pipe::from_raw(from_client)) };
    let (to_held, to_watched) = pipes::hold(to_client);
    let (from_held, from_watched) = pipes::hold(from_client);
    Some((Pipes { to_client: Box::new(to_held), from_client: Box::new(from_held) }, to_watched, from_watched))
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
/// those are named in the log and answered `ERR_OTHER`.
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
        AcceptRefused::NoPipes => ERR_INVALID_INPUT,
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

impl Sockets {
    pub fn new(first_id: u32, places: usize) -> Self {
        Self {
            ids: HashMap::new(),
            next_id: first_id.max(1),
            ends: HashMap::new(),
            by_stream: BTreeMap::new(),
            connecting: BTreeMap::new(),
            lookups: Vec::new(),
            receiving: Vec::new(),
            places,
        }
    }

    /// An id no socket and no stream's ends are kept under, and never 0.
    fn alloc_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = match self.next_id.wrapping_add(1) {
                0 => 1,
                next => next,
            };
            if !self.ids.contains_key(&id) && !self.ends.contains_key(&id) {
                return id;
            }
        }
    }

    fn hold_stream(&mut self, stream: StreamId, to_client: Watched, from_client: Watched) -> u32 {
        let id = self.alloc_id();
        self.ids.insert(id, Socket::Stream(stream));
        self.ends.insert(id, Ends { stream, to_client, from_client });
        self.by_stream.insert(stream, id);
        id
    }

    fn stream(&self, socket_id: u32) -> Option<StreamId> {
        match self.ids.get(&socket_id) {
            Some(Socket::Stream(id)) => Some(*id),
            Some(Socket::Listener { .. } | Socket::Datagram(_)) | None => None,
        }
    }

    fn listener(&self, socket_id: u32) -> Option<ListenerId> {
        match self.ids.get(&socket_id) {
            Some(Socket::Listener { id, .. }) => Some(*id),
            Some(Socket::Stream(_) | Socket::Datagram(_)) | None => None,
        }
    }

    fn datagram(&self, socket_id: u32) -> Option<&Datagram> {
        match self.ids.get(&socket_id) {
            Some(Socket::Datagram(socket)) => Some(socket),
            Some(Socket::Stream(_) | Socket::Listener { .. }) | None => None,
        }
    }

    /// One whole request. A call that answers at once drops its client where
    /// it answers; a connect, a lookup and a receive with nothing to hand
    /// over keep theirs until the node has the answer ([`Self::settle`]).
    pub fn request(&mut self, node: &mut Node, now: Instant, req: Request, draw: fn() -> u32) {
        match MsgType::from_u32(req.msg_type) {
            Some(MsgType::TcpClose) => self.close(node, now, &req),
            Some(MsgType::TcpShutdown) => self.shutdown(node, now, &req),
            Some(MsgType::UdpBind) => self.udp_bind(node, &req, draw),
            Some(MsgType::UdpSendTo) => self.udp_send_to(node, now, &req),
            Some(MsgType::UdpRecvFrom) => self.udp_recv_from(node, now, req),
            Some(MsgType::UdpClose) => self.udp_close(node, now, &req),
            Some(MsgType::DnsLookup) => self.lookup(node, now, req, draw),
            Some(MsgType::TcpSetOption) => self.set_option(node, now, &req),
            Some(MsgType::TcpListenerSetOption) => self.listener_set_option(node, &req),
            Some(MsgType::UdpSetOption) => self.udp_set_option(node, &req),
            Some(MsgType::TcpConnectPiped) => self.connect(node, now, req),
            Some(MsgType::TcpBindPiped) => self.listen(node, &req, draw),
            Some(MsgType::TcpAcceptPiped) => self.accept(node, now, &req),
            None => {
                say!("netstack: unknown message type {}", req.msg_type);
                req.client.error(ERR_INVALID_INPUT);
            }
        }
    }

    /// The client lets go of whatever its id names; an id that names nothing
    /// is let go already.
    fn close(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<SocketCloseRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        match self.ids.remove(&req.socket_id) {
            Some(Socket::Stream(id)) => node.close(now, id),
            Some(Socket::Listener { id, .. }) => {
                node.close_listener(now, id);
            }
            Some(Socket::Datagram(socket)) => self.end_datagram(node, now, socket),
            None => {}
        }
        msg.client.done();
    }

    fn shutdown(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<TcpShutdownRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(id) = self.stream(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        let known = match req.how {
            // The receiving half alone.
            0 => node.nodelay(id).is_some(),
            1 | 2 => node.shutdown_write(now, id),
            _ => {
                msg.client.error(ERR_INVALID_INPUT);
                return;
            }
        };
        if known {
            msg.client.done();
        } else {
            msg.client.error(ERR_NOT_CONNECTED);
        }
    }

    fn set_option(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<SocketOptionRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Some(id) = self.stream(req.socket_id) else {
            msg.client.error(ERR_NOT_CONNECTED);
            return;
        };
        match req.option {
            OPT_NODELAY if node.set_nodelay(now, id, req.value != 0) => msg.client.done(),
            OPT_NODELAY => msg.client.error(ERR_NOT_CONNECTED),
            _ => msg.client.error(ERR_INVALID_INPUT),
        }
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

    fn connect(&mut self, node: &mut Node, now: Instant, msg: Request) {
        let Ok(req) = ipc::decode_payload::<TcpConnectPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let (Some((pipes, to_client, from_client)), Some(port)) = (data_pipes(&msg.client), Port::new(req.port)) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let remote = Endpoint { addr: Ipv4Addr::from(req.addr), port };
        let timeout = (req.timeout_ms > 0).then(|| Duration::from_millis(u64::from(req.timeout_ms)));
        match node.connect(now, remote, timeout, pipes) {
            Ok(stream) => {
                let id = self.hold_stream(stream, to_client, from_client);
                self.connecting.insert(stream, (msg.client, id));
            }
            Err(refusal) => {
                if refusal == ConnectRefused::Full {
                    say!("netstack: refusing connect, {} of {} places held", node.held(), self.places);
                }
                msg.client.error(connect_refused(refusal));
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

    fn accept(&mut self, node: &mut Node, now: Instant, msg: &Request) {
        let Ok(req) = ipc::decode_payload::<TcpAcceptPipedRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let moved = data_pipes(&msg.client);
        let Some(listener) = self.listener(req.socket_id) else {
            msg.client.error(accept_refused(AcceptRefused::NoListener));
            return;
        };
        let (pipes, watched) = match moved {
            Some((pipes, to_client, from_client)) => (Some(pipes), Some((to_client, from_client))),
            None => (None, None),
        };
        match (node.accept(now, listener, pipes), watched) {
            (Ok(accepted), Some((to_client, from_client))) => {
                let socket_id = self.hold_stream(accepted.id, to_client, from_client);
                msg.client.result(&TcpAcceptPipedResponse {
                    socket_id,
                    remote_addr: accepted.remote.addr.octets(),
                    remote_port: accepted.remote.port.get(),
                    local_port: accepted.local.get(),
                    options: TcpOptions::new(accepted.nodelay),
                });
            }
            (Ok(_), None) => unreachable!("the node made a stream of an accept that moved no pipes"),
            (Err(refusal), _) => {
                if refusal == AcceptRefused::Full {
                    say!("netstack: refusing accept, {} of {} places held", node.held(), self.places);
                }
                msg.client.error(accept_refused(refusal));
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
        let waiting = Receiving { client: msg.client, socket_id: req.socket_id, max_len: req.max_len };
        if let Some(waiting) = self.deliver(node, now, waiting) {
            self.receiving.push(waiting);
        }
    }

    /// Hands `waiting` its socket's oldest datagram, or hands `waiting` back
    /// when none has arrived.
    ///
    /// **A datagram goes into the client's pipe whole, or its socket ends.**
    /// The answer names a length, and a write takes what the pipe has room
    /// for and cannot be taken back: a client reading that length out of a
    /// pipe holding part of this datagram would splice the next one onto it.
    fn deliver(&mut self, node: &mut Node, now: Instant, waiting: Receiving) -> Option<Receiving> {
        let Some(socket) = self.datagram(waiting.socket_id) else {
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
            "netstack: ending UDP socket {} — its receive pipe answered {wrote:?} to a {}-byte datagram",
            waiting.socket_id,
            datagram.len
        );
        if let Some(Socket::Datagram(socket)) = self.ids.remove(&waiting.socket_id) {
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

    /// Starts resolving the name `msg` carries, or answers at once where
    /// there is nothing to ask: an address written as one.
    fn lookup(&mut self, node: &mut Node, now: Instant, msg: Request, draw: fn() -> u32) {
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
        match node.resolve(now, name.clone(), draw) {
            Ok(id) => self.lookups.push((id, msg.client, name)),
            Err(NotStarted::NotConnected) => msg.client.error(ERR_NOT_CONNECTED),
            Err(NotStarted::ResourceExhausted) => msg.client.error(ERR_RESOURCE_EXHAUSTED),
        }
    }

    /// Everything the node has to say since the last call, to the log and to
    /// the clients that waited for it, and the table entries of what the node
    /// let go.
    pub fn settle(&mut self, node: &mut Node, now: Instant) {
        for waiting in std::mem::take(&mut self.receiving) {
            if let Some(waiting) = self.deliver(node, now, waiting) {
                self.receiving.push(waiting);
            }
        }

        let events: Vec<StreamEvent> = node.drain_stream_events().collect();
        for event in events {
            let (id, answer) = match event {
                StreamEvent::Connected { id, local } => (id, Ok(local)),
                StreamEvent::Failed { id, failure } => (id, Err(connect_failed(failure))),
                StreamEvent::TimedOut { id } => (id, Err(ERR_TIMED_OUT)),
                // Its client closed it from another connection.
                StreamEvent::Closed { id } => (id, Err(ERR_CONNECTION_REFUSED)),
                StreamEvent::Cut { .. } => {
                    say!("netstack: resetting a connection — its client is gone and its peer took none of its bytes in 100 s");
                    continue;
                }
            };
            let Some((client, socket_id)) = self.connecting.remove(&id) else {
                unreachable!("the node answered a connect nobody waits for")
            };
            match answer {
                Ok(local) => client.result(&TcpConnectResponse { socket_id, local_port: local.get(), _pad: 0 }),
                Err(code) => client.error(code),
            }
        }

        for resolved in node.take_resolved() {
            let Some(at) = self.lookups.iter().position(|(id, ..)| *id == resolved.id) else {
                unreachable!("the node ended a lookup nobody waits for")
            };
            let (_, client, name) = self.lookups.swap_remove(at);
            use toyos_dns::Failure as Dns;
            match resolved.result {
                Ok(addrs) => answer_lookup(&client, &addrs),
                // The protocol's one answer for a name with no address,
                // whether the name or only its address is missing.
                Err(Ended::Failed(Dns::NoSuchName | Dns::NoAddress)) => answer_lookup(&client, &[]),
                Err(Ended::Failed(Dns::TimedOut)) => client.error(ERR_TIMED_OUT),
                // No query found a way out, or the lease the lookup asked
                // under went: this machine is on no network that answers the
                // name, which a lease clears.
                Err(Ended::Failed(Dns::Unreachable) | Ended::LeaseChanged) => client.error(ERR_NOT_CONNECTED),
                Err(Ended::Failed(why @ (Dns::Truncated | Dns::ServerFailed(_) | Dns::TooManyAliases))) => {
                    say!("netstack: a lookup of {name} ended without an answer: {why:?}");
                    client.error(ERR_OTHER);
                }
                Err(Ended::NoPort) => {
                    say!("netstack: a lookup of {name} ended with every dynamic port bound, none left for its next query");
                    client.error(ERR_RESOURCE_EXHAUSTED);
                }
            }
        }

        let ended: Vec<(ListenerId, WriteRefusal)> = node.drain_ended_listeners().collect();
        for (ended, refusal) in ended {
            let found = self.ids.iter().find_map(|(socket_id, socket)| match socket {
                Socket::Listener { id, .. } if *id == ended => Some(*socket_id),
                Socket::Listener { .. } | Socket::Stream(_) | Socket::Datagram(_) => None,
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
                Event::Dhcp(refusal) => say!("netstack: DHCP: refused {refusal:?}"),
            }
        }

        // A stream the node let go holds neither end, and its id names
        // nothing from then.
        let (ids, by_stream) = (&mut self.ids, &mut self.by_stream);
        self.ends.retain(|socket_id, ends| {
            let held = ends.to_client.held().is_some() || ends.from_client.held().is_some();
            if !held {
                by_stream.remove(&ends.stream);
                if matches!(ids.get(socket_id), Some(Socket::Stream(_))) {
                    ids.remove(socket_id);
                }
            }
            held
        });
    }

    /// Asks the kernel about every pipe a pass could be owed for: a stream's
    /// as the node's last pass left them, the wake pipe of each listener and
    /// one pipe of each datagram socket for its owner's leaving, and the
    /// connection of each client that waits for a lookup, which hangs up by
    /// closing it.
    pub fn watch(&self, node: &Node, poller: &Poller) {
        for (stream, watch) in node.watches() {
            let Some((socket_id, ends)) = self.by_stream.get(&stream).and_then(|id| Some((*id, self.ends.get(id)?))) else {
                unreachable!("the node holds a stream whose ends netstack does not")
            };
            let from = if watch.readable { READABLE } else { 0 } | if watch.writer { OTHER_END_GONE } else { 0 };
            if let (true, Some(pipe)) = (from != 0, ends.from_client.held()) {
                poller.watch(&*pipe, from, TOKEN_FROM_CLIENT | u64::from(socket_id));
            }
            let to = if watch.writable { WRITABLE } else { 0 } | if watch.reader { OTHER_END_GONE } else { 0 };
            if let (true, Some(pipe)) = (to != 0, ends.to_client.held()) {
                poller.watch(&*pipe, to, TOKEN_TO_CLIENT | u64::from(socket_id));
            }
        }
        for (socket_id, socket) in &self.ids {
            match socket {
                Socket::Listener { wakes, .. } => {
                    if let Some(pipe) = wakes.held() {
                        poller.watch(&*pipe, OTHER_END_GONE, TOKEN_LISTENER | u64::from(*socket_id));
                    }
                }
                Socket::Datagram(socket) => poller.watch(&socket.to_client, OTHER_END_GONE, TOKEN_DATAGRAM | u64::from(*socket_id)),
                Socket::Stream(_) => {}
            }
        }
        for (_, client, _) in &self.lookups {
            poller.watch(&client.conn, READABLE, TOKEN_LOOKUP | u64::from(client.conn.as_handle().0));
        }
    }

    /// What the kernel answered one of [`Self::watch`]'s watches, or `false`
    /// for a token that is none of them. An answer about a socket that is
    /// gone since, or a pipe end the node let go, says nothing: closing an
    /// end ends its watch, and that end is an answer too.
    pub fn answered(&mut self, node: &mut Node, now: Instant, token: u64, answer: Result<u32, SyscallError>) -> bool {
        let socket_id = token as u32;
        match token & TOKEN_KIND {
            kind @ (TOKEN_FROM_CLIENT | TOKEN_TO_CLIENT) => {
                let Some(ends) = self.ends.get(&socket_id) else { return true };
                let (end, held) = if kind == TOKEN_FROM_CLIENT {
                    (PipeEnd::FromClient, ends.from_client.held().is_some())
                } else {
                    (PipeEnd::ToClient, ends.to_client.held().is_some())
                };
                match answer {
                    _ if !held => {}
                    Err(why) => {
                        say!("netstack: resetting a connection — the kernel refused the watch of its {end:?} pipe: {why:?}");
                        node.pipe_broken(now, ends.stream, end);
                    }
                    Ok(met) if met & OTHER_END_GONE != 0 => node.pipe_gone(now, ends.stream, end),
                    Ok(_) => node.bridge(now),
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
                self.lookups.retain(|(id, client, _)| {
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

    /// The socket table as `inspect` reads it: counts, and no endpoint,
    /// because every client holding `netstack` can ask.
    pub fn inspect(&self, node: &Node, snap: &mut Snapshot) {
        let (mut streams, mut listeners, mut udp) = (0u32, 0u32, 0u32);
        for socket in self.ids.values() {
            match socket {
                Socket::Stream(_) => streams += 1,
                Socket::Listener { .. } => listeners += 1,
                Socket::Datagram(_) => udp += 1,
            }
        }
        snap.put("sockets.tcp", streams);
        snap.put("sockets.listeners", listeners);
        snap.put("sockets.udp", udp);
        snap.put("piped.live", node.streams());
        snap.put("places.held", node.held());
        snap.put("places.max", self.places);
    }
}
