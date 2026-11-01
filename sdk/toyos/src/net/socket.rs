//! A TCP listener and a datagram socket: objects of netstack's, each named by what only its
//! holders hold.
//!
//! **A socket has holders, each a process's connection to netstack** and a bell netstack rings
//! it on. The process that listened or bound is the first; [`export`] asks netstack for another,
//! whose connection end a spawn endows and the child takes up (`super::adopt_exported`). A
//! request on the connection acts on its own socket and nothing else: no frame names a socket,
//! so a holder has nothing to name another's by. The socket ends with its last holder's hang-up.
//!
//! **A holder's connection carries frames**, each answered before the next is sent, under the
//! end's turn: [`ACCEPT`], with a stream's region, answered with the stream's connection and
//! bells ([`super::TcpAcceptResponse`]); [`SEND`], one datagram; [`RECEIVE`], answered with one
//! datagram; [`SET_OPTION`] and [`OPTION`]; and [`EXPORT`], answered [`EXPORTED`] or
//! [`REFUSED`]; a datagram socket's [`CONNECT`], [`PEER`] and [`SEND_PEER`], whose peer is the
//! socket's and so every holder's. An exported end's first frame is [`ADOPT`], carrying
//! [`Adopt`] and the new holder's bell. **A datagram is one frame** each way, [`Datagram`] then
//! its bytes, so two holders' datagrams never interleave.
//!
//! **The bell says "look"**: netstack rings a holder while a connection, a datagram or a
//! datagram socket's pending error waits and the holder has not asked since it was last rung,
//! a new holder at once. So an accept and a receive ask only while the bell is readable: one that
//! finds it empty waits, or answers [`NetError::WouldBlock`], with no request; one that finds it
//! rung makes what its request needs and reads the bell empty just before the request goes,
//! and what another holder took first is answered `WouldBlock` too. A call that fails before
//! its request goes leaves the ring in the bell, where a poller still sees it. Nothing else
//! takes the bell; a look observes it and takes nothing.
//!
//! **A handle with no end here is refused** [`NetError::NotASocket`]. A descriptor's handle is
//! a copy of its end's bell, so a poller watches it for reading; the end keeps its own
//! connection and bell, which only the end's last handle and call let go. Like a stream's, an
//! end stays in its process: its bell carries no `TRANSFER`, nor a datagram socket's
//! connection. A listener's connection keeps it, since the kernel carries an accept's region
//! only on a connection that has it; no descriptor names that handle, so nothing hands it on.

use toyos_abi::handle::Rights;
use toyos_abi::syscall;
use toyos_abi::RawHandle;

use crate::ipc::{self, Connection, FrameRx};
use crate::sync::Lock;
use crate::{AsHandle, BorrowedHandle, OwnedHandle};

use super::ends::{close_kept, kept, narrowed, Ends, HELD_BELL, HELD_CONNECTION};
use super::stream::{self, Wait};
use super::{answer, hangup, BoundResponse, MAX_ANSWER, MsgType, NetError, NetstackConn, TcpListenRequest, TcpOptions, UdpBindRequest};

pub use super::stream::{EXPORT, EXPORTED, REFUSED};

/// The first frame on a socket's exported end, carrying [`Adopt`] and the new
/// holder's bell: unlike a stream's (`stream::ADOPT`), so an exported end says
/// what it holds.
pub const ADOPT: u32 = 0x84;

/// A listener holder's accept: a bare frame carrying the stream's region.
pub const ACCEPT: u32 = 3;
/// [`OptionRequest`]: [`super::OPT_NODELAY`] on a listener, [`super::OPT_BROADCAST`] on a
/// datagram socket. Answered bare.
pub const SET_OPTION: u32 = 4;
/// A `u32`, the option, answered with a `u32`, its value.
pub const OPTION: u32 = 5;
/// A datagram holder's send: [`Datagram`], its destination, then its bytes. Answered bare.
pub const SEND: u32 = 6;
/// A datagram holder's receive, bare: answered [`Datagram`], its source, then its bytes, or
/// [`super::ERR_WOULD_BLOCK`].
pub const RECEIVE: u32 = 7;
/// A datagram holder's connect: [`Datagram`], the peer, its `_pad` and nothing more. Answered
/// bare.
pub const CONNECT: u32 = 13;
/// A datagram holder's ask for its socket's peer, bare: answered [`Datagram`], or
/// [`super::ERR_NOT_CONNECTED`].
pub const PEER: u32 = 14;
/// A datagram holder's send to its socket's peer: the datagram's bytes alone. Answered bare.
pub const SEND_PEER: u32 = 15;

/// The most bytes one datagram carries: an Ethernet frame's 1500, past the IPv4 header and the
/// UDP header (RFC 894, RFC 791, RFC 768).
pub const MAX_DATAGRAM: usize = 1500 - 20 - 8;
/// The widest frame a holder sends or is answered: a datagram's.
pub const MAX_FRAME: usize = core::mem::size_of::<Datagram>() + MAX_DATAGRAM;

crate::ipc_payload! {
    /// A datagram's far end: where a [`SEND`] goes, and where a [`RECEIVE`]'s came from.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Datagram {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
    }

    pub struct OptionRequest {
        pub option: u32,
        pub value: u32,
    }

    /// [`PEER`]'s answer: the peer a connect named, and the address the socket sends there from,
    /// which the connect fixed.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Peer {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
        pub local_addr: [u8; 4],
    }

    /// An exported end's [`ADOPT`]: what kind of socket it holds, and the
    /// address and port it is at.
    pub struct Adopt {
        pub kind: u32,
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
    }
}

/// `data` to `addr`:`port` as one frame's payload, in `out`: `None` for more than a datagram
/// carries.
pub fn datagram<'a>(addr: [u8; 4], port: u16, data: &[u8], out: &'a mut [u8; MAX_FRAME]) -> Option<&'a [u8]> {
    if data.len() > MAX_DATAGRAM {
        return None;
    }
    let at = core::mem::size_of::<Datagram>();
    out[..at].copy_from_slice(ipc::as_bytes(&Datagram { addr, port, _pad: 0 }));
    out[at..at + data.len()].copy_from_slice(data);
    Some(&out[..at + data.len()])
}

/// A frame's payload as one datagram: its far end and its bytes, or `None` for a payload
/// shorter than a [`Datagram`] or longer than [`MAX_FRAME`].
pub fn datagram_of(payload: &[u8]) -> Option<(Datagram, &[u8])> {
    let at = core::mem::size_of::<Datagram>();
    if payload.len() < at || payload.len() > MAX_FRAME {
        return None;
    }
    let header = ipc::decode_payload(&payload[..at]).ok()?;
    Some((header, &payload[at..]))
}

/// What a socket is.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Listener = 1,
    Datagram = 2,
}

impl Kind {
    pub fn from_u32(word: u32) -> Option<Self> {
        match word {
            1 => Some(Self::Listener),
            2 => Some(Self::Datagram),
            _ => None,
        }
    }
}

// ---- the process's ends ----

/// The most sockets one process holds: the per-process bound libc's socket table has, as
/// [`stream::MAX_STREAMS`]. An estimate: no measurement sets it.
pub const MAX_SOCKETS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct End {
    kind: Kind,
    /// The end's own handle of its connection, which every request is made on and only [`free`]
    /// closes, kept as [`kept`] says.
    connection: BorrowedHandle<'static>,
    /// The end's own handle of its bell.
    bell: BorrowedHandle<'static>,
    /// The address its listen or bind named, and the port it holds.
    addr: [u8; 4],
    port: u16,
}

static TABLE: Lock<Ends<End, MAX_SOCKETS>> = Lock::new(Ends::new());

/// Each end's turn on its connection, one request and its answer at a time.
static TURNS: [Lock<()>; MAX_SOCKETS] = [const { Lock::new(()) }; MAX_SOCKETS];


/// Lets go of an end no handle or call reaches: its connection, which is its holder's leaving,
/// and its bell.
fn free(slot: usize, end: End) {
    // SAFETY: no handle and no call reaches the end, whose own these are.
    unsafe {
        close_kept(end.connection.raw());
        close_kept(end.bell.raw());
    }
    TABLE.lock().unreserve(slot);
}

/// Runs `f` as a call through `handle`, its end kept until it returns.
fn call<R>(handle: BorrowedHandle<'_>, f: impl FnOnce(usize, End) -> Result<R, NetError>) -> Result<R, NetError> {
    let (slot, end) = TABLE.lock().call(handle.raw()).ok_or(NetError::NotASocket)?;
    let answer = f(slot, end);
    if let Some(end) = TABLE.lock().returned(slot) {
        free(slot, end);
    }
    answer
}

/// A call on a socket of `kind` only.
fn call_on<R>(handle: BorrowedHandle<'_>, kind: Kind, f: impl FnOnce(usize, End) -> Result<R, NetError>) -> Result<R, NetError> {
    call(handle, |slot, end| if end.kind == kind { f(slot, end) } else { Err(NetError::InvalidInput) })
}

/// Whether `bell` is readable, waiting for it at most `nanos`: rung, or hung up with netstack.
/// Nothing is taken.
fn rung(bell: BorrowedHandle<'_>, nanos: u64) -> Result<bool, NetError> {
    match syscall::handle_wait(bell.raw(), crate::poller::READABLE, nanos) {
        Ok(ready) => Ok(ready != 0),
        Err(_) => Err(NetError::Io),
    }
}

/// One request's read of the bell, handed out by [`look`] and spent once: spending it gives the
/// [`Drained`] that [`Drained::accept`] and [`Drained::receive`] send their request
/// with. Spend it after every step that can fail, so a call refused before its request leaves
/// the ring in the bell. A raw [`ipc`] send of [`ACCEPT`] or [`RECEIVE`] bypasses it.
pub(super) struct Drain<'a>(&'a dyn Fn());

impl Drain<'_> {
    pub(super) fn now(self) -> Drained {
        (self.0)();
        Drained(())
    }
}

/// The bell read for one request, which sending the request spends.
#[must_use = "the bell was read for a request: send it"]
pub(super) struct Drained(());

impl Drained {
    /// [`ACCEPT`] on `listener`, lending the stream's `region`.
    pub(super) fn accept(self, listener: BorrowedHandle<'_>, region: OwnedHandle) -> Result<(), ipc::IpcError> {
        ipc::signal_with(listener, ACCEPT, [region])
    }

    /// [`RECEIVE`] on `connection`.
    pub(super) fn receive(self, connection: BorrowedHandle<'_>) -> Result<(), ipc::IpcError> {
        ipc::signal(connection, RECEIVE)
    }
}

/// A holder's bell, as [`look`] reads it.
trait Bell {
    /// `f` under the end's turn, told whether the bell is rung now.
    fn look_now<R>(&self, f: impl FnOnce(bool) -> R) -> Result<R, NetError>;
    /// Until the bell is rung, or for at most `nanos`, out of the turn.
    fn wait(&self, nanos: u64) -> Result<(), NetError>;
    fn drain(&self);
}

/// An end's own bell: looked at under the end's turn, waited on out of it.
struct EndBell {
    slot: usize,
    bell: BorrowedHandle<'static>,
}

impl Bell for EndBell {
    fn look_now<R>(&self, f: impl FnOnce(bool) -> R) -> Result<R, NetError> {
        let _turn = TURNS[self.slot].lock();
        rung(self.bell, 0).map(f)
    }

    fn wait(&self, nanos: u64) -> Result<(), NetError> {
        // Netstack gone leaves the bell readable for good, and the next look says so.
        rung(self.bell, nanos).map(drop)
    }

    fn drain(&self) {
        // A bell netstack let go of answers `Gone`, which the request it was read for answers.
        let _ = syscall::event_take(self.bell.raw());
    }
}

/// `ask` while `bell` is rung, until it takes something, waiting for the bell between as
/// `wait` says. Each `ask` is handed one [`Drain`].
fn look<T>(bell: &impl Bell, wait: Wait, mut ask: impl FnMut(Drain<'_>) -> Result<T, NetError>) -> Result<T, NetError> {
    let drain = || bell.drain();
    loop {
        if let Some(answer) = bell.look_now(|rung| rung.then(|| ask(Drain(&drain))))? {
            match answer {
                Err(NetError::WouldBlock) => {}
                answer => return answer,
            }
        }
        let until = match wait {
            Wait::Nonblocking => return Err(NetError::WouldBlock),
            Wait::Blocking(until) => until,
        };
        let left = until.map_or(u64::MAX, |until| until.saturating_sub(toyos_abi::clock::nanos_since_boot()));
        if left == 0 {
            return Err(NetError::TimedOut);
        }
        bell.wait(left)?;
    }
}

// ---- listen and bind ----

/// A socket this process made, its first holder: the descriptor's handle, and its port.
#[derive(Debug)]
pub struct Bound {
    pub handle: OwnedHandle,
    pub port: u16,
}

/// A listener at `addr`:`port`, port 0 asking netstack to choose one, holding `options` before
/// any SYN can reach it.
pub fn listen(addr: [u8; 4], port: u16, options: TcpOptions) -> Result<Bound, NetError> {
    open(Kind::Listener, addr, |netstack| {
        netstack.request(MsgType::TcpListen, &TcpListenRequest { addr, port, _pad: 0, options })?.answer_with_handles::<BoundResponse, 1>()
    })
}

/// A datagram socket at `addr`:`port`, port 0 asking netstack to choose one.
pub fn bind(addr: [u8; 4], port: u16) -> Result<Bound, NetError> {
    open(Kind::Datagram, addr, |netstack| {
        netstack.request(MsgType::UdpBind, &UdpBindRequest { addr, port, _pad: 0 })?.answer_with_handles::<BoundResponse, 1>()
    })
}

/// Makes a socket: a slot, then `ask`'s request on a new connection to netstack, whose answer
/// hands over the bell and makes the connection this process's holder. Refused
/// [`NetError::ResourceExhausted`] before anything is made where the process holds
/// [`MAX_SOCKETS`].
fn open(kind: Kind, addr: [u8; 4], ask: impl FnOnce(NetstackConn) -> Result<(BoundResponse, [OwnedHandle; 1], Connection), NetError>) -> Result<Bound, NetError> {
    let slot = TABLE.lock().reserve().ok_or(NetError::ResourceExhausted)?;
    let made = NetstackConn::connect().and_then(ask).and_then(|(bound, [bell], connection)| {
        let (descriptor, connection, bell) = take_up(kind, connection.into_owned(), bell)?;
        Ok((descriptor, End { kind, connection, bell, addr, port: bound.port }))
    });
    let mut table = TABLE.lock();
    match made {
        Ok((handle, end)) => {
            table.enter(slot, end, handle.as_handle().raw());
            Ok(Bound { handle, port: end.port })
        }
        Err(refused) => {
            table.unreserve(slot);
            Err(refused)
        }
    }
}

/// A holder's connection and bell, narrowed to what a `kind` socket's holder uses, and a copy of
/// the bell for the descriptor: the descriptor's, the connection, the bell. Refused, every handle
/// is closed.
fn take_up(kind: Kind, connection: OwnedHandle, bell: OwnedHandle) -> Result<(OwnedHandle, BorrowedHandle<'static>, BorrowedHandle<'static>), NetError> {
    let rights = match kind {
        Kind::Listener => HELD_CONNECTION.union(Rights::TRANSFER),
        Kind::Datagram => HELD_CONNECTION,
    };
    let connection = narrowed(connection, rights);
    let bell = narrowed(bell, HELD_BELL);
    let refused = match (connection, bell) {
        // SAFETY: narrowed here for the end being made, which keeps them until its `free`.
        (Ok(connection), Ok(bell)) => match unsafe { kept(bell) }.try_clone_to_owned() {
            // SAFETY: as above.
            Ok(descriptor) => return Ok((descriptor, unsafe { kept(connection) }, unsafe { kept(bell) })),
            Err(_) => ([Some(connection), Some(bell)], NetError::ResourceExhausted),
        },
        (connection, bell) => ([connection.ok(), bell.ok()], NetError::Io),
    };
    let (made, why) = refused;
    // SAFETY: each narrowed here and kept by no end yet.
    made.into_iter().flatten().for_each(|handle| unsafe { close_kept(handle) });
    Err(why)
}

// ---- handles ----

/// The kind of socket `handle` reaches, if it reaches one of this process's. A lookup, and
/// nothing is written.
pub fn kind(handle: BorrowedHandle<'_>) -> Option<Kind> {
    TABLE.lock().get(handle.raw()).map(|end| end.kind)
}

pub fn is_socket(handle: BorrowedHandle<'_>) -> bool {
    kind(handle).is_some()
}

/// The address the socket `handle` reaches is at and the port it holds: a
/// connected datagram socket's is the one its connect fixed, as netstack answers
/// it, and any other's is the one its listen or bind named.
pub fn local(handle: BorrowedHandle<'_>) -> Result<([u8; 4], u16), NetError> {
    let end = TABLE.lock().get(handle.raw()).ok_or(NetError::NotASocket)?;
    let connected = match end.kind {
        Kind::Datagram => connected(handle)?.map(|peer| peer.local_addr),
        Kind::Listener => None,
    };
    Ok((connected.unwrap_or(end.addr), end.port))
}

/// `handle`, which reaches one of this process's sockets, as a descriptor's object: let go by
/// this copy of the crate, which holds the end (`crate::fd::SOCKET`).
pub fn descriptor(handle: OwnedHandle) -> crate::fd::Object {
    crate::fd::Object::socket(handle, released)
}

extern "C" fn released(word: u64) {
    if let Ok(handle) = u32::try_from(word) {
        // SAFETY: the table gives up a descriptor's handle when it releases
        // its object, and names it no more.
        let_go(unsafe { OwnedHandle::from_raw(RawHandle(handle)) });
    }
}

/// Closes `handle`; the end goes with its last handle and call.
pub fn let_go(handle: OwnedHandle) {
    let forgotten = TABLE.lock().forget(handle.as_handle().raw());
    drop(handle);
    if let Some(Some((slot, end))) = forgotten {
        free(slot, end);
    }
}

// ---- a listener's ----

/// The oldest connection waiting at the listener `handle` reaches, as a stream: waiting for one
/// as `wait` says, [`NetError::WouldBlock`] or [`NetError::TimedOut`] where none came.
pub fn accept(handle: BorrowedHandle<'_>, wait: Wait) -> Result<stream::Accepted, NetError> {
    call_on(handle, Kind::Listener, |slot, end| look(&EndBell { slot, bell: end.bell }, wait, |drain| stream::accept(end.connection, drain)))
}

// ---- a datagram socket's ----

/// A datagram [`recv_from`] took: the bytes it wrote, and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received {
    /// The datagram's bytes, cut to the buffer: the rest is gone.
    pub len: usize,
    pub addr: [u8; 4],
    pub port: u16,
}

/// `data` as one datagram to `addr`:`port`: the bytes it took, all of them.
pub fn send_to(handle: BorrowedHandle<'_>, addr: [u8; 4], port: u16, data: &[u8]) -> Result<usize, NetError> {
    let mut frame = [0u8; MAX_FRAME];
    let frame = datagram(addr, port, data, &mut frame).ok_or(NetError::InvalidInput)?;
    call_on(handle, Kind::Datagram, |slot, end| {
        let _turn = TURNS[slot].lock();
        ipc::send_bytes(end.connection, SEND, frame).map_err(hangup)?;
        bare(end.connection)?;
        Ok(data.len())
    })
}

/// The oldest datagram waiting at the socket `handle` reaches, into `buf`: waiting for one as
/// `wait` says, [`NetError::WouldBlock`] or [`NetError::TimedOut`] where none came.
pub fn recv_from(handle: BorrowedHandle<'_>, buf: &mut [u8], wait: Wait) -> Result<Received, NetError> {
    call_on(handle, Kind::Datagram, |slot, end| {
        look(&EndBell { slot, bell: end.bell }, wait, |drain| {
            drain.now().receive(end.connection).map_err(hangup)?;
            let mut rx = FrameRx::<MAX_FRAME>::new();
            let frame = answer(&end.connection, &mut rx)?;
            let (from, data) = datagram_of(rx.payload(frame.payload_len)).ok_or(NetError::Protocol(RECEIVE))?;
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            Ok(Received { len, addr: from.addr, port: from.port })
        })
    })
}

/// The socket `handle` reaches connected to `addr`:`port` (POSIX's `connect`), for every holder:
/// from here it receives only the peer's datagrams, and [`send`] goes there.
pub fn connect(handle: BorrowedHandle<'_>, addr: [u8; 4], port: u16) -> Result<(), NetError> {
    call_on(handle, Kind::Datagram, |slot, end| {
        let _turn = TURNS[slot].lock();
        ipc::send(end.connection, CONNECT, &Datagram { addr, port, _pad: 0 }).map_err(hangup)?;
        bare(end.connection)
    })
}

/// The peer the socket `handle` reaches is connected to, by any of its holders.
pub fn peer(handle: BorrowedHandle<'_>) -> Result<Option<([u8; 4], u16)>, NetError> {
    Ok(connected(handle)?.map(|peer| (peer.addr, peer.port)))
}

/// netstack's [`PEER`] answer for the datagram socket `handle` reaches, `None` while no
/// holder connected it. One naming a local address no socket sends from is refused.
fn connected(handle: BorrowedHandle<'_>) -> Result<Option<Peer>, NetError> {
    call_on(handle, Kind::Datagram, |slot, end| {
        let _turn = TURNS[slot].lock();
        ipc::signal(end.connection, PEER).map_err(hangup)?;
        let mut rx = FrameRx::<MAX_ANSWER>::new();
        match answer(&end.connection, &mut rx) {
            Ok(frame) => match ipc::decode_payload::<Peer>(rx.payload(frame.payload_len)) {
                Ok(peer) if super::unicast(peer.local_addr) => Ok(Some(peer)),
                _ => Err(NetError::Protocol(PEER)),
            },
            Err(NetError::NotConnected) => Ok(None),
            Err(e) => Err(e),
        }
    })
}

/// `data` as one datagram to the connected socket's peer: the bytes it took, all of them.
/// [`NetError::NotConnected`] for a socket no holder connected.
pub fn send(handle: BorrowedHandle<'_>, data: &[u8]) -> Result<usize, NetError> {
    if data.len() > MAX_DATAGRAM {
        return Err(NetError::InvalidInput);
    }
    call_on(handle, Kind::Datagram, |slot, end| {
        let _turn = TURNS[slot].lock();
        ipc::send_bytes(end.connection, SEND_PEER, data).map_err(hangup)?;
        bare(end.connection)?;
        Ok(data.len())
    })
}

// ---- either's ----

pub fn set_option(handle: BorrowedHandle<'_>, option: u32, value: u32) -> Result<(), NetError> {
    call(handle, |slot, end| {
        let _turn = TURNS[slot].lock();
        ipc::send(end.connection, SET_OPTION, &OptionRequest { option, value }).map_err(hangup)?;
        bare(end.connection)
    })
}

/// The value netstack holds for `option`, which any holder may have set.
pub fn option(handle: BorrowedHandle<'_>, option: u32) -> Result<u32, NetError> {
    call(handle, |slot, end| {
        let _turn = TURNS[slot].lock();
        ipc::send(end.connection, OPTION, &option).map_err(hangup)?;
        let mut rx = FrameRx::<MAX_ANSWER>::new();
        let frame = answer(&end.connection, &mut rx)?;
        ipc::decode_payload(rx.payload(frame.payload_len)).map_err(|_| NetError::Protocol(OPTION))
    })
}

/// An answer that carries nothing.
fn bare(connection: BorrowedHandle<'_>) -> Result<(), NetError> {
    let mut rx = FrameRx::<MAX_ANSWER>::new();
    let frame = answer(&connection, &mut rx)?;
    match frame.payload_len == 0 && frame.handles == 0 {
        true => Ok(()),
        false => Err(NetError::Protocol(frame.msg_type)),
    }
}

// ---- export and adoption ----

/// A new holder of the socket `handle` reaches, for another process: the end of its connection,
/// which carries `TRANSFER` so a spawn can endow it, and which `super::adopt_exported` takes up
/// there.
/// netstack out of places for it is [`NetError::ResourceExhausted`].
pub fn export(handle: BorrowedHandle<'_>) -> Result<OwnedHandle, NetError> {
    call(handle, |slot, end| {
        let _turn = TURNS[slot].lock();
        stream::ask_export(end.connection, None)
    })
}

/// A call through `handle` begun and left open, so its end stays until [`release`]: the slot.
/// `None` for a handle that reaches no socket.
pub(super) fn hold(handle: BorrowedHandle<'_>) -> Option<usize> {
    TABLE.lock().call(handle.raw()).map(|(slot, _)| slot)
}

/// [`export`] of the end a [`hold`] keeps at `slot`.
pub(super) fn export_held(slot: usize) -> Result<OwnedHandle, NetError> {
    let end = TABLE.lock().at(slot).expect("a held end is live");
    let _turn = TURNS[slot].lock();
    stream::ask_export(end.connection, None)
}

/// The call a [`hold`] began, ended.
pub(super) fn release(slot: usize) {
    if let Some(end) = TABLE.lock().returned(slot) {
        free(slot, end);
    }
}

/// An exported end taken up as this process's holder of its socket, `payload` and `handles` the
/// [`ADOPT`] netstack queued on it (`super::adopt_exported`): the end and its bell narrowed to
/// keep no `TRANSFER`. The descriptor's handle; `None`, with everything closed, where the frame
/// is malformed, the process holds [`MAX_SOCKETS`] or the kernel will not narrow them.
pub(super) fn adopted(connection: Connection, payload: &[u8], handles: ipc::Batch) -> Option<OwnedHandle> {
    let adopt: Adopt = ipc::decode_payload(payload).ok()?;
    let [bell] = handles.exact::<1>()?;
    let kind = Kind::from_u32(adopt.kind)?;
    let slot = TABLE.lock().reserve()?;
    let made = take_up(kind, connection.into_owned(), bell);
    let mut table = TABLE.lock();
    match made {
        Ok((descriptor, connection, bell)) => {
            table.enter(slot, End { kind, connection, bell, addr: adopt.addr, port: adopt.port }, descriptor.as_handle().raw());
            Some(descriptor)
        }
        Err(_) => {
            table.unreserve(slot);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`PEER`]'s answer as a holder decodes it: the peer, its port, then the address the
    /// socket sends from; an answer without that address is refused.
    #[test]
    fn a_peers_answer_ends_in_the_address_it_is_sent_from() {
        let wire = [192, 0, 2, 7, 53, 0, 0, 0, 198, 51, 100, 9];
        let peer: Peer = ipc::decode_payload(&wire).unwrap();
        assert_eq!(peer, Peer { addr: [192, 0, 2, 7], port: 53, _pad: 0, local_addr: [198, 51, 100, 9] });
        assert!(ipc::decode_payload::<Peer>(&wire[..8]).is_err());
    }

    /// A datagram is one frame's payload: its far end, then exactly its bytes, read back as it
    /// was written, whatever bytes it carries, an empty one too.
    #[test]
    fn a_datagram_is_one_frame_read_back_whole() {
        let mut out = [0u8; MAX_FRAME];
        for data in [&b""[..], b"one", &[0xff; MAX_DATAGRAM]] {
            let frame = datagram([192, 0, 2, 7], 5_353, data, &mut out).expect("a datagram that fits").to_vec();
            assert_eq!(frame.len(), core::mem::size_of::<Datagram>() + data.len());
            let (from, read) = datagram_of(&frame).expect("a frame of one datagram");
            assert_eq!((from, read), (Datagram { addr: [192, 0, 2, 7], port: 5_353, _pad: 0 }, data));
        }
    }

    /// One byte past a datagram is refused at the sender, before any frame; a payload shorter
    /// than the far end, or past the widest frame, is no datagram.
    #[test]
    fn what_no_datagram_carries_is_refused_both_ways() {
        let mut out = [0u8; MAX_FRAME];
        assert!(datagram([0; 4], 1, &[0; MAX_DATAGRAM + 1], &mut out).is_none());
        for len in 0..core::mem::size_of::<Datagram>() {
            assert!(datagram_of(&[0; 8][..len]).is_none(), "{len} bytes");
        }
        assert!(datagram_of(&[0; MAX_FRAME + 1]).is_none());
    }

    /// netstack's side of two holders' bells over one queue (`toyos-net-node`'s `holders`):
    /// each holder is rung while something waits and it has not been rung since it last asked,
    /// and every ask, whatever it takes, is followed by that telling.
    #[derive(Default)]
    struct Server {
        queue: alloc::collections::VecDeque<u32>,
        told: [bool; 2],
        /// The rings in each holder's bell, unread.
        bells: [u32; 2],
        /// What happens while holder 1 waits, a batch a wait.
        while_waiting: alloc::collections::VecDeque<alloc::vec::Vec<Event>>,
        /// What arrives once an ask that found nothing is answered, before its holder reads
        /// the answer.
        after_nothing: Option<u32>,
        taken: [Option<u32>; 2],
    }

    enum Event {
        Arrives(u32),
        /// Holder 0 makes its whole call, which must take something.
        Looks,
    }

    impl Server {
        fn tell(&mut self) {
            for holder in 0..2 {
                if !self.queue.is_empty() && !self.told[holder] {
                    self.bells[holder] += 1;
                    self.told[holder] = true;
                }
            }
        }

        fn arrives(&mut self, item: u32) {
            self.queue.push_back(item);
            self.tell();
        }

        fn ask(&mut self, holder: usize, _: Drained) -> Result<u32, NetError> {
            self.told[holder] = false;
            let taken = self.queue.pop_front();
            self.tell();
            if taken.is_none() {
                if let Some(item) = self.after_nothing.take() {
                    self.arrives(item);
                }
            }
            taken.ok_or(NetError::WouldBlock)
        }
    }

    struct Held<'a> {
        holder: usize,
        server: &'a core::cell::RefCell<Server>,
    }

    impl Bell for Held<'_> {
        fn look_now<R>(&self, f: impl FnOnce(bool) -> R) -> Result<R, NetError> {
            let rung = self.server.borrow().bells[self.holder] > 0;
            Ok(f(rung))
        }

        /// Runs what the scenario says happens meanwhile; a holder still unrung when nothing is
        /// left to happen would wait for good.
        fn wait(&self, _: u64) -> Result<(), NetError> {
            while self.server.borrow().bells[self.holder] == 0 {
                let Some(batch) = self.server.borrow_mut().while_waiting.pop_front() else {
                    panic!("holder {} waits for good with {:?} queued", self.holder, self.server.borrow().queue)
                };
                for event in batch {
                    match event {
                        Event::Arrives(item) => self.server.borrow_mut().arrives(item),
                        Event::Looks => {
                            let taken = call(0, self.server).expect("holder 0 takes what it was rung for");
                            self.server.borrow_mut().taken[0] = Some(taken);
                        }
                    }
                }
            }
            Ok(())
        }

        fn drain(&self) {
            self.server.borrow_mut().bells[self.holder] = 0;
        }
    }

    fn call(holder: usize, server: &core::cell::RefCell<Server>) -> Result<u32, NetError> {
        look(&Held { holder, server }, Wait::Blocking(None), |drain| {
            let drained = drain.now();
            server.borrow_mut().ask(holder, drained)
        })
    }

    /// Two holders blocked on one queue: both are rung for the first arrival, the other takes
    /// it, and holder 1's ask finds nothing. The second arrives as that answer goes, before
    /// holder 1 reads it, so its ring is already in the bell: a holder that read its bell after
    /// asking would wait for good.
    #[test]
    fn the_holder_that_found_nothing_takes_the_next_arrival() {
        let server = core::cell::RefCell::new(Server {
            while_waiting: [alloc::vec![Event::Arrives(1), Event::Looks]].into(),
            after_nothing: Some(2),
            ..Server::default()
        });
        let taken = call(1, &server).expect("holder 1 takes the second arrival");
        assert_eq!((server.borrow().taken[0], taken), (Some(1), 2));
        assert!(server.borrow().queue.is_empty());
    }

    #[test]
    fn a_kind_is_its_word_and_no_other_word_is_one() {
        for kind in [Kind::Listener, Kind::Datagram] {
            assert_eq!(Kind::from_u32(kind as u32), Some(kind));
        }
        assert_eq!((Kind::from_u32(0), Kind::from_u32(3)), (None, None));
    }
}
