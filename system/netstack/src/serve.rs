//! A client's requests onto the node's calls, and the node's answers back
//! onto `toyos::net`'s protocol.
//!
//! **One request is one call of the node's**, and every decision about a
//! socket is the node's: what is here is which socket and holder each
//! connection is, the clients that wait for an answer, the handles netstack
//! watches, and the word each of the node's refusals is written in.
//!
//! **A listener and a datagram socket are their holders** (`toyos::net::socket`):
//! the connection its listen or bind came on, which is handed its bell with the
//! answer, and one more for each export. netstack watches each holder's
//! connection for its frames, every one a request on that holder's own socket
//! answered at once, and its hang-up, which is that holder's leaving and, for
//! the last, the socket's close. **No frame names a socket**, so a connection
//! reaches only what it holds: a holder's frame on a new connection is a
//! message type that does not exist there, and one a holder's protocol does
//! not have drops that holder. An accept's stream gets a connection of the
//! private port's, handed over in the accept's answer.
//!
//! **A stream is its region and its holders** (`toyos::net::stream`): the
//! connection its connect or accept came on, and one more for each export.
//! netstack judges the region before it maps it (`lent`), makes the first
//! holder's bells and answers once with them, and from then watches each
//! holder's rung bell for its rings and exports, and its wakers' end, which is
//! that holder's leaving and, for the last, the stream's close. Until a
//! connect is answered netstack itself holds the rung's waker, so it watches
//! the first holder's connection instead, whose hang-up is that leaving. A
//! holder netstack makes is a connection of its private port's, its `ADOPT`
//! queued on it before the asker is answered. A client names no stream by a
//! number, and a frame no holder of the protocol sends drops that holder.
//!
//! **Untrusted input.** A request's every field is its client's number: a
//! payload that is not the request's struct, an option no socket of its kind
//! has, a port of zero and a name that is none are refused `ERR_INVALID_INPUT`,
//! and a length is a bound on a read and never an allocation's size past what
//! one frame carries. Only a connect and a lookup wait, each holding its own
//! connection. Nothing a client sends panics netstack.
//!
//! **A lookup's view is the badge on its connection**, which the supervisor
//! minted with the row and session of the program it endowed the connector,
//! and which is bytes here: what the node keeps of one view's answers answers
//! no other. A lookup on a connection with no badge is refused
//! `ERR_PERMISSION_DENIED`. Neither the name a lookup asks nor anything kept is
//! said in the log or to `inspect`, which every holder of `netstack` reads.

use std::collections::{BTreeMap, HashMap};
use std::net::Ipv4Addr;
use std::time::Duration;

use std::rc::{Rc, Weak};

use toyos::net::*;
use toyos::net::socket::{self, Kind, ACCEPT, CONNECT, OPTION, PEER, RECEIVE, SEND, SEND_PEER, SET_OPTION};
use toyos::net::stream::{Adopt, ADOPT, BELLS, EXPORT, EXPORTED, REFUSED};
use toyos::transport::session::Holder;
use toyos::poller::READABLE;
use toyos::port::Acceptor;
use toyos::ipc::RxStep;
use toyos::wake::{self, Bell, Hungup, Waker};
use toyos::{ipc, say, AsHandle, Connection, OwnedHandle};
use toyos_abi::syscall::{SyscallError, MAX_BADGE};
use toyos_inspect::Snapshot;
use toyos_net_node::{
    AcceptRefused, ConnectRefused, DatagramId, Ended, Event, ExportRefused, HolderId, ListenRefused, ListenerId, LookupId,
    Node, NotStarted, Refused, Resolving, StreamEvent, StreamId,
};
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{Endpoint, Failure};
use toyos_net_wire::{Instant, Port};

use crate::client::{Client, Request};
use crate::lent::{Holders, Holding, Region};
use crate::bells;
use crate::watches::Watches;
use crate::HOSTNAME;

/// A watch's token names the holder it was asked of by its key, in the low
/// word, and never a place in a list: an answer can arrive after its holder
/// is gone, and must then name nothing.
const TOKEN_STREAM: u64 = 1 << 32;
const TOKEN_SOCKET: u64 = 3 << 32;
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

/// The socket a holder holds.
#[derive(Clone, Copy, Debug)]
enum Held {
    Listener(ListenerId),
    Datagram(DatagramId),
}

/// One holder of a listener or a datagram socket, as netstack watches it.
struct SocketHolder {
    socket: Held,
    holder: HolderId,
    /// The address the socket's listen or bind named, for each holder's
    /// adoption.
    addr: [u8; 4],
    /// What it asks on, and whose hang-up is its leaving.
    connection: Connection,
    frames: ipc::FrameRx<{ socket::MAX_FRAME }>,
}

/// A stream's holders as netstack watches them, for as long as the node holds
/// the stream's session.
struct Stream {
    id: StreamId,
    holders: Weak<Holders>,
    /// The connect not yet answered: the bells its answer hands over, and the
    /// holders, kept for the answer past a session the node let go of.
    answer: Option<([OwnedHandle; BELLS], Rc<Holders>)>,
    /// The number the next holder made is given.
    next: u16,
}

/// The most frames one holder's connection is read for in a wake: one that
/// always has another cannot keep the rest from being served.
const FRAMES_A_WAKE: usize = 64;

/// The name the private port holders are made on has in netstack's own
/// namespace.
const EXPORT_PORT: &str = "export";

/// The private port every holder netstack makes is a connection of, but the
/// first of a connect, a listen and a bind, which is the request's own: its
/// acceptor, and the namespace holding netstack's one connector to it. It
/// dies with netstack, so nothing pending reaches a restarted one.
struct Exports {
    acceptor: Acceptor,
    names: toyos::namespace::Namespace,
}

impl Exports {
    fn new() -> Self {
        let (acceptor, connector) = toyos::port::create().unwrap_or_else(|e| panic!("netstack: no export port ({e:?})"));
        let names = toyos::namespace::build()
            .add(EXPORT_PORT, &connector)
            .finish()
            .unwrap_or_else(|e| panic!("netstack: no namespace for the export port ({e:?})"));
        Self { acceptor, names }
    }

    /// A connection to netstack: the end to hand away, which keeps every right
    /// a connection is made with, `TRANSFER` among them, and netstack's.
    fn connect(&self) -> Result<(Connection, Connection), SyscallError> {
        let theirs = self.names.open(EXPORT_PORT)?;
        let ours = self.acceptor.accept().unwrap_or_else(|e| panic!("netstack: the export port refused an accept ({e:?})"));
        Ok((theirs, ours))
    }
}

pub struct Sockets {
    /// Each holder of a listener or a datagram socket, by the key its watch's
    /// token carries.
    sockets: HashMap<u32, SocketHolder>,
    next_id: u32,
    /// Each stream by its key.
    streams: HashMap<u32, Stream>,
    /// The key each stream is kept under.
    by_stream: BTreeMap<StreamId, u32>,
    /// Each holder, by the key its watch's token carries: its stream's key
    /// and its number.
    holders: HashMap<u32, (u32, Holder)>,
    exports: Exports,
    lookups: Vec<(LookupId, Client)>,
    /// The places the node was given.
    places: usize,
    /// A stream's connection was answered in this wake: one pass over the
    /// streams follows the wake, however many answers it carried.
    bridge: bool,
    /// The streams whose client rang in this wake.
    rung: Vec<StreamId>,
}

/// What netstack keeps of a holder's three bells: the wakers of its receive
/// and send bells, and the bell it rings netstack on.
struct Kept {
    receive: Waker,
    send: Waker,
    rung: Bell,
}

/// A holder's three bells: what it is handed, in [`BELLS`]'s order, and what
/// netstack keeps.
fn stream_bells() -> Result<([OwnedHandle; BELLS], Kept), SyscallError> {
    let (receive, receive_bell) = wake::pair()?;
    let (send, send_bell) = wake::pair()?;
    let (rung_waker, rung) = wake::pair()?;
    Ok(([receive_bell.into(), send_bell.into(), rung_waker.into()], Kept { receive, send, rung }))
}

/// A holder on `connection` with the bells netstack keeps of it.
fn holding(connection: Connection, kept: Kept) -> Holding {
    let Kept { receive, send, rung } = kept;
    Holding { connection, frames: ipc::FrameRx::new(), receive, send, rung }
}

/// What a lent region makes: the session, the bells its holder is handed and
/// the holders.
type Lent = (Region, [OwnedHandle; BELLS], Rc<Holders>);

/// A stream's region, judged, and its first holder's bells made, the holder's
/// connection being `connection`: the session the node is lent, the bells the
/// answer hands over, and the holders. Refused by name, nothing is
/// mapped, and the connection is handed back with the refusal's word.
fn lend(connection: Connection, region: toyos::OwnedHandle) -> Result<Lent, (Connection, u32)> {
    let mapping = match Region::judged(region) {
        Ok(mapping) => mapping,
        Err(why) => {
            say!("netstack: refusing a stream's region — {why}");
            return Err((connection, ERR_INVALID_INPUT));
        }
    };
    let Ok((theirs, kept)) = stream_bells() else { return Err((connection, ERR_RESOURCE_EXHAUSTED)) };
    let first = holding(connection, kept);
    match Region::new(mapping, first) {
        Ok((session, holders)) => Ok((session, theirs, holders)),
        Err(why) => panic!("netstack: the kernel would not duplicate a region it mapped: {why:?}"),
    }
}

/// Answers a stream's request with its bells and `answer`, on its first
/// holder's connection.
fn hand_over<T: ipc::IpcPayload>(holders: &Holders, bells: [OwnedHandle; BELLS], answer: &T) {
    let by_number = holders.by_number.borrow();
    let Some(first) = by_number.get(&Holder::FIRST) else { return };
    if let Err(why) = first.connection.try_send_with(RespType::Result as u32, answer, bells) {
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
        Refused::ConnectionRefused => ERR_CONNECTION_REFUSED,
        // No finer word exists yet, as for a connect's (`connect_failed`).
        Refused::Unreachable => ERR_OTHER,
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
        // The address it was made from went under it: as no address at all.
        Failure::AddressGone => ERR_NOT_CONNECTED,
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
        AcceptRefused::NoListener => ERR_NOT_CONNECTED,
        AcceptRefused::Nothing => ERR_WOULD_BLOCK,
        AcceptRefused::NoRegion => ERR_INVALID_INPUT,
        AcceptRefused::Full => ERR_RESOURCE_EXHAUSTED,
    }
}

fn export_refused(refusal: ExportRefused) -> u32 {
    match refusal {
        ExportRefused::Full => ERR_RESOURCE_EXHAUSTED,
        ExportRefused::Gone => ERR_NOT_CONNECTED,
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
    pub fn new(places: usize) -> Self {
        Self {
            sockets: HashMap::new(),
            next_id: 1,
            streams: HashMap::new(),
            by_stream: BTreeMap::new(),
            holders: HashMap::new(),
            exports: Exports::new(),
            lookups: Vec::new(),
            places,
            bridge: false,
            rung: Vec::new(),
        }
    }

    /// A key no holder and no stream is kept under, and never 0.
    fn alloc_id(&mut self) -> u32 {
        loop {
            let id = self.next_id;
            self.next_id = match self.next_id.wrapping_add(1) {
                0 => 1,
                next => next,
            };
            if !self.sockets.contains_key(&id) && !self.streams.contains_key(&id) && !self.holders.contains_key(&id) {
                return id;
            }
        }
    }

    fn hold_stream(&mut self, id: StreamId, holders: &Rc<Holders>, answer: Option<[OwnedHandle; BELLS]>) {
        let key = self.alloc_id();
        let answer = answer.map(|bells| (bells, holders.clone()));
        self.streams.insert(key, Stream { id, holders: Rc::downgrade(holders), answer, next: 2 });
        self.by_stream.insert(id, key);
        let watch = self.alloc_id();
        self.holders.insert(watch, (key, Holder::FIRST));
    }

    /// A holder of `socket` on `connection`, watched from here.
    fn hold_socket(&mut self, socket: Held, holder: HolderId, addr: [u8; 4], connection: Connection) {
        let key = self.alloc_id();
        self.sockets.insert(key, SocketHolder { socket, holder, addr, connection, frames: ipc::FrameRx::new() });
    }

    /// One whole request on a new connection. A call that answers at once
    /// drops its client where it answers; a connect and a lookup keep theirs
    /// until the node has the answer ([`Self::settle`]); a listen and a bind
    /// make theirs the socket's first holder.
    pub fn request(&mut self, node: &mut Node, now: Instant, req: Request, acceptor: &Acceptor, draw: fn() -> u32) {
        match MsgType::from_u32(req.msg_type) {
            Some(MsgType::UdpBind) => self.udp_bind(node, now, req, draw),
            Some(MsgType::DnsLookup) => self.lookup(node, now, req, acceptor, draw),
            Some(MsgType::TcpConnect) => self.connect(node, now, req),
            Some(MsgType::TcpListen) => self.listen(node, now, req, draw),
            None => {
                say!("netstack: unknown message type {}", req.msg_type);
                req.client.error(ERR_INVALID_INPUT);
            }
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
        let Some([region]) = msg.handles.exact::<1>() else { return msg.client.error(ERR_INVALID_INPUT) };
        let (session, bells, holders) = match lend(msg.client.conn, region) {
            Ok(lent) => lent,
            Err((conn, code)) => return Client { conn }.error(code),
        };
        let remote = Endpoint { addr: Ipv4Addr::from(req.addr), port };
        let timeout = (req.timeout_ms > 0).then(|| Duration::from_millis(u64::from(req.timeout_ms)));
        match node.connect(now, remote, timeout, Box::new(session)) {
            Ok(stream) => self.hold_stream(stream, &holders, Some(bells)),
            Err(refusal) => {
                if refusal == ConnectRefused::Full {
                    say!("netstack: refusing connect, {} of {} places held", node.held(), self.places);
                }
                answer_refused(&holders, connect_refused(refusal));
            }
        }
    }

    /// A listen: its connection the listener's first holder, answered with its
    /// bell.
    fn listen(&mut self, node: &mut Node, now: Instant, msg: Request, draw: fn() -> u32) {
        let Ok(req) = ipc::decode_payload::<TcpListenRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Ok((theirs, bell)) = bells::bell() else { return msg.client.error(ERR_RESOURCE_EXHAUSTED) };
        match node.listen(Ipv4Addr::from(req.addr), Port::new(req.port), req.options.nodelay(), Box::new(bell), draw) {
            Ok((id, port)) => self.bound(node, now, Held::Listener(id), req.addr, msg.client, port, theirs),
            Err(refusal) => msg.client.error(listen_refused(refusal)),
        }
    }

    /// A datagram socket's bind: its connection the socket's first holder,
    /// answered with its bell.
    fn udp_bind(&mut self, node: &mut Node, now: Instant, msg: Request, draw: fn() -> u32) {
        let Ok(req) = ipc::decode_payload::<UdpBindRequest>(msg.payload()) else {
            msg.client.error(ERR_INVALID_INPUT);
            return;
        };
        let Ok((theirs, bell)) = bells::bell() else { return msg.client.error(ERR_RESOURCE_EXHAUSTED) };
        match node.udp_bind(Ipv4Addr::from(req.addr), Port::new(req.port), Box::new(bell), draw) {
            Ok((id, port)) => self.bound(node, now, Held::Datagram(id), req.addr, msg.client, port, theirs),
            Err(refusal) => msg.client.error(refused(refusal)),
        }
    }

    /// A socket the node made, answered to its first holder with its bell.
    fn bound(&mut self, node: &mut Node, now: Instant, socket: Held, addr: [u8; 4], client: Client, port: Port, bell: Bell) {
        let answer = BoundResponse { port: port.get(), _pad: 0 };
        if let Err(why) = client.conn.try_send_with(RespType::Result as u32, &answer, [OwnedHandle::from(bell)]) {
            say!("netstack: a socket's client left before its answer: {why:?}");
            return departed(node, now, socket, HolderId::FIRST);
        }
        self.hold_socket(socket, HolderId::FIRST, addr, client.conn);
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
    /// let go. `label` names a link in a line.
    pub fn settle(&mut self, node: &mut Node, label: impl Fn(toyos_net_node::IfIndex) -> String) {
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
            let Some((bells, holders)) = answer_to else { unreachable!("the node answered a connect nobody waits for") };
            match answer {
                Ok(local) => hand_over(&holders, bells, &TcpConnectResponse { local_port: local.get(), _pad: 0 }),
                Err(code) => answer_refused(&holders, code),
            }
        }

        for resolved in node.take_resolved() {
            let Some(at) = self.lookups.iter().position(|(id, _)| *id == resolved.id) else {
                unreachable!("the node ended a lookup nobody waits for")
            };
            let (_, client) = self.lookups.swap_remove(at);
            answer_resolved(&client, resolved.result);
        }

        let events: Vec<Event> = node.drain_events().collect();
        for event in events {
            match event {
                Event::Stack { refusal, suppressed: 0 } => say!("netstack: refused {refusal:?}"),
                Event::Stack { refusal, suppressed } => say!("netstack: refused {refusal:?}, and {suppressed} more by its rule"),
                Event::Dhcp { link, refusal, suppressed: 0 } => say!("netstack: DHCP on {}: refused {refusal:?}", label(link)),
                Event::Dhcp { link, refusal, suppressed } => {
                    say!("netstack: DHCP on {}: refused {refusal:?}, and {suppressed} more by its rule", label(link))
                }
                Event::Name { link, event: toyos_mdns::Event::Claimed } => {
                    say!("netstack: mDNS on {}: no host answered for {HOSTNAME}.local; this machine answers as it there", label(link))
                }
                Event::Name { link, event: toyos_mdns::Event::Lost } => say!(
                    "netstack: mDNS on {}: another host answered for {HOSTNAME}.local; this machine answers to no name there and asks for {HOSTNAME}.local again every {} s",
                    label(link),
                    toyos_mdns::RETRY_MS / 1000
                ),
            }
        }

        // A stream the node let go holds no session, and its key and its
        // holders' name nothing from then.
        let by_stream = &mut self.by_stream;
        self.streams.retain(|_, stream| {
            let held = stream.holders.strong_count() > 0;
            if !held {
                by_stream.remove(&stream.id);
            }
            held
        });
        let streams = &self.streams;
        self.holders.retain(|_, (key, _)| streams.contains_key(key));
    }

    /// Asks the kernel about everything a pass could be owed for: each
    /// stream holder's bell, for its rings and its leaving, or for a connect
    /// not yet answered its connection, for its hang-up; each socket
    /// holder's connection, for its requests and its hang-up, and the
    /// connection of each client that waits for a lookup, which hangs up by
    /// closing it.
    pub fn watch(&self, poller: &mut Watches) {
        for (key, (stream, number)) in &self.holders {
            let Some(stream) = self.streams.get(stream) else { continue };
            let Some(holders) = stream.holders.upgrade() else { continue };
            let token = TOKEN_STREAM | u64::from(*key);
            match holders.by_number.borrow().get(number) {
                Some(holding) if stream.answer.is_some() => poller.watch(&holding.connection, READABLE, token),
                Some(holding) => poller.watch(&holding.rung, READABLE, token),
                None => {}
            }
        }
        for (key, holder) in &self.sockets {
            poller.watch(&holder.connection, READABLE, TOKEN_SOCKET | u64::from(*key));
        }
        for (_, client) in &self.lookups {
            poller.watch(&client.conn, READABLE, TOKEN_LOOKUP | u64::from(client.conn.as_handle().raw().0));
        }
    }

    /// What the kernel answered one of [`Self::watch`]'s watches, or `false`
    /// for a token that is none of them. An answer about a socket or a stream
    /// that is gone since says nothing.
    pub fn answered(&mut self, node: &mut Node, now: Instant, token: u64, answer: Result<u32, SyscallError>) -> bool {
        let key = token as u32;
        match token & TOKEN_KIND {
            TOKEN_STREAM => self.holder_answered(node, now, key, answer),
            TOKEN_SOCKET => self.socket_answered(node, now, key, answer),
            TOKEN_LOOKUP => {
                // A lookup whose client has left ends now: its sockets and
                // its place are another client's.
                let spoke = |client: &Client| u64::from(client.conn.as_handle().raw().0) == u64::from(key);
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

    /// What a holder's bell, or its connection while its connect is
    /// unanswered, said: a ring for the pass and its exports answered, read
    /// frame by frame off its connection, or its leaving, which a hang-up and
    /// a frame no holder sends are too.
    fn holder_answered(&mut self, node: &mut Node, now: Instant, key: u32, answer: Result<u32, SyscallError>) {
        let Some(&(stream_key, number)) = self.holders.get(&key) else { return };
        let Some(stream) = self.streams.get(&stream_key) else { return };
        let id = stream.id;
        let Some(holders) = stream.holders.upgrade() else { return };
        let (mut rang, mut exports, mut left) = (false, 0usize, answer.is_err());
        if let Err(why) = answer {
            say!("netstack: dropping a stream's holder — the kernel refused the watch of its bell or connection: {why:?}");
        }
        if let Some(holding) = holders.by_number.borrow_mut().get_mut(&number).filter(|_| !left) {
            match holding.rung.try_take() {
                Ok(woken) => rang = woken,
                Err(Hungup) => left = true,
            }
            for _ in 0..FRAMES_A_WAKE {
                if left {
                    break;
                }
                match heard(holding.frames.pump(&holding.connection)) {
                    Heard::Nothing => break,
                    Heard::Export => exports += 1,
                    Heard::Leaving(why) => {
                        if let Some(step) = why {
                            say!("netstack: dropping a stream's holder — it sent {step:?}, which no holder sends");
                        }
                        left = true;
                        break;
                    }
                }
            }
        }
        for _ in 0..exports {
            self.export(node, now, stream_key, number);
        }
        if left {
            holders.by_number.borrow_mut().remove(&number);
            self.holders.remove(&key);
            node.departed(now, id, number);
        } else if rang {
            self.rung.push(id);
            self.bridge = true;
        }
    }

    /// A holder more of the stream `stream_key` names, for `asker`, which
    /// is answered [`EXPORTED`] with the new holder's end, its `ADOPT` queued
    /// on it first, or [`REFUSED`].
    fn export(&mut self, node: &mut Node, now: Instant, stream_key: u32, asker: Holder) {
        let Some(stream) = self.streams.get(&stream_key) else { return };
        let (id, next) = (stream.id, stream.next);
        let Some(holders) = stream.holders.upgrade() else { return };
        let refuse = |code: u32| {
            if let Some(holding) = holders.by_number.borrow().get(&asker)
                && let Err(why) = holding.connection.try_send(REFUSED, &code)
            {
                say!("netstack: a holder left before its export's answer: {why:?}");
            }
        };
        let Some(number) = Holder::new(next) else { return refuse(ERR_RESOURCE_EXHAUSTED) };
        // The place first: a holder asking past them costs no handle.
        let (peer, local) = match node.export(id) {
            Ok(endpoints) => endpoints,
            Err(ExportRefused::Full) => {
                say!("netstack: refusing an export, {} of {} places held", node.held(), self.places);
                return refuse(ERR_RESOURCE_EXHAUSTED);
            }
            Err(refusal @ ExportRefused::Gone) => return refuse(export_refused(refusal)),
        };
        let made = (|| {
            let (theirs, ours) = self.exports.connect()?;
            let (handed, kept) = stream_bells()?;
            Ok::<_, SyscallError>((theirs, ours, handed, kept, holders.region()?))
        })();
        let Ok((theirs, ours, [receive_bell, send_bell, rung_waker], kept, region)) = made else {
            say!("netstack: refusing an export — the kernel made no connection, bells or region for it: {made:?}", made = made.err());
            node.departed(now, id, number);
            return refuse(ERR_RESOURCE_EXHAUSTED);
        };
        let adopt = Adopt {
            holder: number.number(),
            peer_port: peer.port.get(),
            peer_addr: peer.addr.octets(),
            local_port: local.get(),
            _pad: 0,
        };
        let handles = [region, receive_bell, send_bell, rung_waker];
        if let Err(why) = ours.try_send_with(ADOPT, &adopt, handles) {
            say!("netstack: refusing an export — its ADOPT would not queue: {why:?}");
            node.departed(now, id, number);
            return refuse(ERR_RESOURCE_EXHAUSTED);
        }
        holders.by_number.borrow_mut().insert(number, holding(ours, kept));
        if let Some(stream) = self.streams.get_mut(&stream_key) {
            stream.next = next.wrapping_add(1);
        }
        let watch = self.alloc_id();
        self.holders.insert(watch, (stream_key, number));
        if let Some(holding) = holders.by_number.borrow().get(&asker)
            && let Err(why) = holding.connection.try_signal_with(EXPORTED, [theirs.into_owned()])
        {
            say!("netstack: a holder left before its export's answer: {why:?}");
        }
    }

    /// What a socket holder's connection had, frame by frame: each request
    /// answered, and its hang-up, a frame no holder sends or an answer it
    /// would not take, its leaving.
    fn socket_answered(&mut self, node: &mut Node, now: Instant, key: u32, answer: Result<u32, SyscallError>) {
        let Some(holder) = self.sockets.get(&key) else { return };
        let (socket, number) = (holder.socket, holder.holder);
        if let Err(why) = answer {
            say!("netstack: dropping a socket's holder — the kernel refused the watch of its connection: {why:?}");
            return self.socket_left(node, now, key);
        }
        for _ in 0..FRAMES_A_WAKE {
            let Some(holder) = self.sockets.get_mut(&key) else { return };
            let step = holder.frames.pump(&holder.connection);
            let asked = match step {
                RxStep::Idle => return,
                RxStep::Frame { msg_type, payload_len, handles } => asked(kind_of(socket), msg_type, holder.frames.payload(payload_len), handles),
                RxStep::Eof | RxStep::Malformed => None,
            };
            let Some(asked) = asked else {
                if !matches!(step, RxStep::Eof) {
                    say!("netstack: dropping a socket's holder — it sent {step:?}, which no holder sends");
                }
                return self.socket_left(node, now, key);
            };
            let answered = match asked {
                Asked::Accept => {
                    let region = holder.frames.take_batch();
                    self.accept(node, now, key, region)
                }
                Asked::Export => self.export_socket(node, now, key),
                asked => {
                    let reply = answer_socket(node, now, socket, number, asked);
                    let Some(holder) = self.sockets.get(&key) else { return };
                    let sent = match reply {
                        Ok(Reply::Bare) => holder.connection.try_signal(RespType::Result as u32),
                        Ok(Reply::Word(word)) => holder.connection.try_send(RespType::Result as u32, &word),
                        Ok(Reply::Peer(peer)) => holder.connection.try_send(RespType::Result as u32, &peer),
                        Ok(Reply::Bytes(bytes)) => holder.connection.try_send_bytes(RespType::Result as u32, &bytes),
                        Err(code) => holder.connection.try_send(RespType::Error as u32, &ErrorResponse { code }),
                    };
                    sent.is_ok()
                }
            };
            if !answered {
                say!("netstack: dropping a socket's holder — it would not take an answer");
                return self.socket_left(node, now, key);
            }
        }
    }

    fn socket_left(&mut self, node: &mut Node, now: Instant, key: u32) {
        if let Some(holder) = self.sockets.remove(&key) {
            departed(node, now, holder.socket, holder.holder);
        }
    }

    /// A listener holder's accept: the oldest connection waiting at its
    /// listener the stream on `region`, its first holder a connection of the
    /// private port's, handed to the asker with the stream's bells. Whether the
    /// asker took its answer.
    fn accept(&mut self, node: &mut Node, now: Instant, key: u32, region: ipc::Batch) -> bool {
        let Some(&SocketHolder { socket: Held::Listener(id), holder, .. }) = self.sockets.get(&key) else {
            unreachable!("an accept is a listener holder's")
        };
        // Closed unread where the node asks for no session.
        let mut region = Some(region);
        let mut made = None;
        let mut code = ERR_INVALID_INPUT;
        let exports = &self.exports;
        let accepted = node.accept(now, id, holder, || {
            let [region] = region.take()?.exact::<1>()?;
            let (theirs, ours) = match exports.connect() {
                Ok(pair) => pair,
                Err(_) => {
                    code = ERR_RESOURCE_EXHAUSTED;
                    return None;
                }
            };
            match lend(ours, region) {
                Ok((session, bells, holders)) => {
                    made = Some((theirs, bells, holders));
                    Some(Box::new(session) as Box<dyn toyos_net_node::Session>)
                }
                Err((_, refused)) => {
                    code = refused;
                    None
                }
            }
        });
        drop(region);
        let (answer, handles) = match (accepted, made) {
            (Ok(accepted), Some((theirs, [receive, send, rung], holders))) => {
                let answer = TcpAcceptResponse {
                    remote_addr: accepted.remote.addr.octets(),
                    remote_port: accepted.remote.port.get(),
                    local_port: accepted.local.get(),
                    nodelay: u32::from(accepted.nodelay),
                };
                self.hold_stream(accepted.id, &holders, None);
                (Ok(answer), Some([theirs.into_owned(), receive, send, rung]))
            }
            (Ok(_), None) => unreachable!("the node took a stream it was given no session for"),
            (Err(AcceptRefused::NoRegion), _) => (Err(code), None),
            (Err(refusal), _) => {
                if refusal == AcceptRefused::Full {
                    say!("netstack: refusing accept, {} of {} places held", node.held(), self.places);
                }
                (Err(accept_refused(refusal)), None)
            }
        };
        let Some(asker) = self.sockets.get(&key) else { return true };
        match (answer, handles) {
            (Ok(answer), Some(handles)) => asker.connection.try_send_with(RespType::Result as u32, &answer, handles).is_ok(),
            (Err(code), _) => asker.connection.try_send(RespType::Error as u32, &ErrorResponse { code }).is_ok(),
            (Ok(_), None) => unreachable!("an accepted stream's answer carries its handles"),
        }
    }

    /// A holder more of the socket the holder `key` names holds, answered
    /// [`EXPORTED`] with the new holder's end, its [`socket::ADOPT`] queued on
    /// it first, or [`REFUSED`]. Whether the asker took its answer.
    fn export_socket(&mut self, node: &mut Node, now: Instant, key: u32) -> bool {
        let Some(&SocketHolder { socket, addr, .. }) = self.sockets.get(&key) else { return true };
        let made = (|| {
            let (theirs, ours) = self.exports.connect()?;
            let (handed, bell) = bells::bell()?;
            Ok::<_, SyscallError>((theirs, ours, handed, bell))
        })();
        let reply = match made {
            Err(why) => {
                say!("netstack: refusing an export — the kernel made no connection or bell for it: {why:?}");
                Err(ERR_RESOURCE_EXHAUSTED)
            }
            Ok((theirs, ours, handed, bell)) => {
                let exported = match socket {
                    Held::Listener(id) => node.export_listener(id, Box::new(bell)),
                    Held::Datagram(id) => node.export_datagram(id, Box::new(bell)),
                };
                match exported {
                    Err(refusal) => {
                        if refusal == ExportRefused::Full {
                            say!("netstack: refusing an export, {} of {} places held", node.held(), self.places);
                        }
                        Err(export_refused(refusal))
                    }
                    Ok((holder, port)) => {
                        let adopt = socket::Adopt { kind: kind_of(socket) as u32, addr, port: port.get(), _pad: 0 };
                        match ours.try_send_with(socket::ADOPT, &adopt, [OwnedHandle::from(handed)]) {
                            Ok(()) => {
                                self.hold_socket(socket, holder, addr, ours);
                                Ok(theirs)
                            }
                            Err(why) => {
                                say!("netstack: refusing an export — its ADOPT would not queue: {why:?}");
                                departed(node, now, socket, holder);
                                Err(ERR_RESOURCE_EXHAUSTED)
                            }
                        }
                    }
                }
            }
        };
        let Some(asker) = self.sockets.get(&key) else { return true };
        match reply {
            Ok(theirs) => asker.connection.try_signal_with(EXPORTED, [theirs.into_owned()]).is_ok(),
            Err(code) => asker.connection.try_send(REFUSED, &code).is_ok(),
        }
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
        snap.put("sockets.tcp", self.streams.len() as u32);
        snap.put("streams.holders", self.holders.len() as u32);
        snap.put("sockets.listeners", node.listeners() as u32);
        snap.put("sockets.udp", node.datagrams() as u32);
        snap.put("sockets.holders", self.sockets.len() as u32);
        snap.put("streams.live", node.streams());
        snap.put("places.held", node.held());
        snap.put("places.max", self.places);
        tcp_counters(node.shard().tcp_counters(), snap);
    }
}

/// Every [tcp] counter, at `net.<its name>`.
fn tcp_counters(counters: &toyos_net_tcp::Counters, snap: &mut Snapshot) {
    for (name, value) in counters.iter() {
        snap.put(name, value);
    }
}

/// What one read of a holder's connection said.
#[derive(Debug)]
enum Heard {
    Nothing,
    Export,
    /// It hung up, or, with what it sent, wrote what no holder writes.
    Leaving(Option<RxStep>),
}

/// A holder's frame as netstack takes it: [`EXPORT`], bare, and anything else
/// its leaving.
fn heard(step: RxStep) -> Heard {
    match step {
        RxStep::Idle => Heard::Nothing,
        RxStep::Frame { msg_type: EXPORT, payload_len: 0, handles: 0 } => Heard::Export,
        RxStep::Eof => Heard::Leaving(None),
        step @ (RxStep::Malformed | RxStep::Frame { .. }) => Heard::Leaving(Some(step)),
    }
}

fn kind_of(socket: Held) -> Kind {
    match socket {
        Held::Listener(_) => Kind::Listener,
        Held::Datagram(_) => Kind::Datagram,
    }
}

/// `holder` of `socket` left.
fn departed(node: &mut Node, now: Instant, socket: Held, holder: HolderId) {
    match socket {
        Held::Listener(id) => node.listener_departed(now, id, holder),
        Held::Datagram(id) => node.datagram_departed(now, id, holder),
    }
}

/// A socket holder's request, as its frame said it: only what its kind of
/// socket is asked, in exactly its shape.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    /// With one handle, the stream's region.
    Accept,
    Export,
    SetOption(u32, u32),
    Option(u32),
    Send { to: [u8; 4], port: u16, data: Vec<u8> },
    Receive,
    Connect { to: [u8; 4], port: u16 },
    Peer,
    SendPeer(Vec<u8>),
}

/// The request a holder of a `kind` socket sent, or `None` for a frame no
/// such holder sends, which is its leaving.
fn asked(kind: Kind, msg_type: u32, payload: &[u8], handles: usize) -> Option<Asked> {
    match (kind, msg_type, handles) {
        (Kind::Listener, ACCEPT, 1) if payload.is_empty() => Some(Asked::Accept),
        (_, EXPORT, 0) if payload.is_empty() => Some(Asked::Export),
        (_, SET_OPTION, 0) => {
            let option: socket::OptionRequest = ipc::decode_payload(payload).ok()?;
            Some(Asked::SetOption(option.option, option.value))
        }
        (_, OPTION, 0) => ipc::decode_payload::<u32>(payload).ok().map(Asked::Option),
        (Kind::Datagram, SEND, 0) => {
            let (to, data) = socket::datagram_of(payload)?;
            Some(Asked::Send { to: to.addr, port: to.port, data: data.to_vec() })
        }
        (Kind::Datagram, RECEIVE, 0) if payload.is_empty() => Some(Asked::Receive),
        (Kind::Datagram, CONNECT, 0) => {
            let peer: socket::Datagram = ipc::decode_payload(payload).ok()?;
            Some(Asked::Connect { to: peer.addr, port: peer.port })
        }
        (Kind::Datagram, PEER, 0) if payload.is_empty() => Some(Asked::Peer),
        (Kind::Datagram, SEND_PEER, 0) if payload.len() <= socket::MAX_DATAGRAM => Some(Asked::SendPeer(payload.to_vec())),
        _ => None,
    }
}

/// What a socket request is answered with.
enum Reply {
    Bare,
    Word(u32),
    Peer(socket::Datagram),
    Bytes(Vec<u8>),
}

/// A request that is one call of the node's, on `holder`'s own `socket`.
fn answer_socket(node: &mut Node, now: Instant, socket: Held, holder: HolderId, asked: Asked) -> Result<Reply, u32> {
    match (socket, asked) {
        (Held::Listener(id), Asked::SetOption(OPT_NODELAY, value)) => {
            node.set_listener_nodelay(id, value != 0).then_some(Reply::Bare).ok_or(ERR_NOT_CONNECTED)
        }
        (Held::Listener(id), Asked::Option(OPT_NODELAY)) => node.listener_nodelay(id).map(|on| Reply::Word(u32::from(on))).ok_or(ERR_NOT_CONNECTED),
        (Held::Datagram(id), Asked::SetOption(OPT_BROADCAST, value)) => {
            node.udp_set_broadcast(id, value != 0).map(|()| Reply::Bare).map_err(refused)
        }
        (Held::Datagram(id), Asked::Option(OPT_BROADCAST)) => node.udp_broadcast(id).map(|on| Reply::Word(u32::from(on))).map_err(refused),
        (_, Asked::SetOption(..) | Asked::Option(_)) => Err(ERR_INVALID_INPUT),
        (Held::Datagram(id), Asked::Send { to, port, data }) => {
            node.udp_send_to(now, id, Ipv4Addr::from(to), port, &data).map(|()| Reply::Bare).map_err(refused)
        }
        (Held::Datagram(id), Asked::Connect { to, port }) => {
            node.udp_connect(now, id, Ipv4Addr::from(to), port).map(|()| Reply::Bare).map_err(refused)
        }
        (Held::Datagram(id), Asked::Peer) => match node.udp_peer(id).map_err(refused)? {
            Some((addr, port)) => Ok(Reply::Peer(socket::Datagram { addr: addr.octets(), port: port.get(), _pad: 0 })),
            None => Err(ERR_NOT_CONNECTED),
        },
        (Held::Datagram(id), Asked::SendPeer(data)) => node.udp_send(now, id, &data).map(|()| Reply::Bare).map_err(refused),
        (Held::Datagram(id), Asked::Receive) => {
            let mut payload = [0u8; toyos_net_udp::limits::MAX_PAYLOAD];
            match node.udp_recv_from(id, holder, &mut payload) {
                Ok(Some(datagram)) => {
                    let mut frame = [0u8; socket::MAX_FRAME];
                    let source = datagram.source_port.map_or(0, Port::get);
                    let Some(frame) = socket::datagram(datagram.source.octets(), source, &payload[..datagram.len], &mut frame) else {
                        unreachable!("[udp] delivered a datagram longer than one frame carries")
                    };
                    Ok(Reply::Bytes(frame.to_vec()))
                }
                Ok(None) => Err(ERR_WOULD_BLOCK),
                Err(refusal) => Err(refused(refusal)),
            }
        }
        (
            _,
            asked @ (Asked::Accept
            | Asked::Export
            | Asked::Send { .. }
            | Asked::Receive
            | Asked::Connect { .. }
            | Asked::Peer
            | Asked::SendPeer(_)),
        ) => {
            unreachable!("{asked:?} is answered by its own call, or was refused as no holder's of its kind")
        }
    }
}

const _: () = assert!(toyos_net_udp::limits::MAX_PAYLOAD <= socket::MAX_DATAGRAM, "a received datagram is one frame");

/// A stream's request refused, on its first holder's connection.
fn answer_refused(holders: &Holders, code: u32) {
    if let Some(first) = holders.by_number.borrow().get(&Holder::FIRST)
        && let Err(why) = first.connection.try_send(RespType::Error as u32, &ErrorResponse { code })
    {
        say!("netstack: a stream's client left before its answer: {why:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every [tcp] counter fits the snapshot with half of a frame to spare, which is more than
    /// the card's, the lease's and the sockets' few dozen entries take. A counter is eight bytes
    /// whatever its value.
    #[test]
    fn the_tcp_counters_leave_the_snapshot_half_its_frame() {
        let mut snap = Snapshot::new(toyos_inspect::NET);
        tcp_counters(&toyos_net_tcp::Counters::default(), &mut snap);
        let bytes = snap.encode().expect("the counters encode").len();
        assert!(bytes <= toyos_inspect::MAX_SNAPSHOT_BYTES / 2, "{bytes} bytes");
    }

    /// A new connection's requests make a socket or ask a name and act on
    /// none: every request a holder makes of its own socket is no request
    /// there, so a connection that holds nothing reaches nothing.
    #[test]
    fn a_new_connection_names_no_socket() {
        for holders in [ACCEPT, OPTION, SET_OPTION, SEND, RECEIVE, CONNECT, PEER, SEND_PEER, EXPORT, EXPORTED, REFUSED, ADOPT, socket::ADOPT] {
            assert_eq!(MsgType::from_u32(holders), None, "{holders}");
        }
    }

    /// A holder asks only what its kind of socket is asked, in exactly its
    /// shape: anything else is its leaving, and none panics.
    #[test]
    fn a_holder_asks_only_its_own_kind_of_socket() {
        let option = [1, 0, 0, 0, 1, 0, 0, 0];
        let mut frame = [0u8; socket::MAX_FRAME];
        let datagram = socket::datagram([192, 0, 2, 7], 53, b"hi", &mut frame).unwrap().to_vec();
        assert_eq!(asked(Kind::Listener, ACCEPT, &[], 1), Some(Asked::Accept));
        for kind in [Kind::Listener, Kind::Datagram] {
            assert_eq!(asked(kind, EXPORT, &[], 0), Some(Asked::Export));
            assert_eq!(asked(kind, SET_OPTION, &option, 0), Some(Asked::SetOption(1, 1)));
            assert_eq!(asked(kind, OPTION, &[2, 0, 0, 0], 0), Some(Asked::Option(2)));
        }
        assert_eq!(asked(Kind::Datagram, SEND, &datagram, 0), Some(Asked::Send { to: [192, 0, 2, 7], port: 53, data: b"hi".to_vec() }));
        assert_eq!(asked(Kind::Datagram, RECEIVE, &[], 0), Some(Asked::Receive));
        assert_eq!(asked(Kind::Datagram, CONNECT, &datagram[..8], 0), Some(Asked::Connect { to: [192, 0, 2, 7], port: 53 }));
        assert_eq!(asked(Kind::Datagram, PEER, &[], 0), Some(Asked::Peer));
        assert_eq!(asked(Kind::Datagram, SEND_PEER, b"hi", 0), Some(Asked::SendPeer(b"hi".to_vec())));

        for (kind, msg_type, payload, handles) in [
            (Kind::Datagram, ACCEPT, &[][..], 1),
            (Kind::Listener, SEND, &datagram[..], 0),
            (Kind::Listener, RECEIVE, &[], 0),
            (Kind::Listener, ACCEPT, &[], 0),
            (Kind::Listener, ACCEPT, &[], 2),
            (Kind::Listener, ACCEPT, &[0], 1),
            (Kind::Datagram, RECEIVE, &[], 1),
            (Kind::Datagram, RECEIVE, &[0], 0),
            (Kind::Datagram, SEND, &datagram[..7], 0),
            (Kind::Datagram, SEND, &datagram[..], 1),
            (Kind::Listener, EXPORT, &[0], 0),
            (Kind::Listener, EXPORT, &[], 1),
            (Kind::Datagram, SET_OPTION, &option[..7], 0),
            (Kind::Datagram, OPTION, &option[..], 0),
            (Kind::Listener, CONNECT, &datagram[..8], 0),
            (Kind::Listener, PEER, &[], 0),
            (Kind::Listener, SEND_PEER, b"hi", 0),
            (Kind::Datagram, CONNECT, &datagram[..], 0),
            (Kind::Datagram, PEER, &[0], 0),
            (Kind::Datagram, SEND_PEER, &[0; socket::MAX_DATAGRAM + 1], 0),
            (Kind::Datagram, SEND_PEER, b"hi", 1),
            (Kind::Datagram, socket::ADOPT, &[0; 8], 1),
            (Kind::Listener, u32::MAX, &[], 0),
        ] {
            assert_eq!(asked(kind, msg_type, payload, handles), None, "{kind:?} {msg_type} {payload:?} {handles}");
        }
    }

    /// Only a bare export is a holder's frame: any payload, any handle, any
    /// other type and broken framing are its leaving, and none panics.
    #[test]
    fn a_holder_frame_no_holder_sends_is_its_leaving() {
        let frame = |msg_type, payload_len, handles| RxStep::Frame { msg_type, payload_len, handles };
        assert!(matches!(heard(RxStep::Idle), Heard::Nothing));
        assert!(matches!(heard(frame(EXPORT, 0, 0)), Heard::Export));
        assert!(matches!(heard(RxStep::Eof), Heard::Leaving(None)));
        for step in [
            RxStep::Malformed,
            frame(1, 0, 0),
            frame(EXPORT, 4, 0),
            frame(EXPORT, 0, 1),
            frame(EXPORTED, 0, 1),
            frame(ADOPT, 12, 3),
            frame(0, 0, 0),
            frame(u32::MAX, 0, 0),
        ] {
            assert!(matches!(heard(step), Heard::Leaving(Some(_))), "{step:?}");
        }
    }
}
