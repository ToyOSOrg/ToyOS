//! ToyOS userland networking library.
//!
//! Owns the netstack IPC protocol and provides client functions for TCP, UDP, and DNS.
//! All networking in ToyOS goes through the `netstack` daemon: a TCP stream is
//! a region and a connection ([`stream`]), and a listener and a datagram
//! socket a connection and a bell ([`socket`]). **No request names a socket
//! by a number**: each is named by the connection only its holders hold.

mod ends;
pub mod socket;
pub mod stream;

/// A stream or a socket this process holds an end of, kept for as long as
/// this lives, which makes a new holder for each other process that is to
/// share it: what a spawn command keeps between spawns, where each child gets
/// a holder of its own.
pub struct Exporter {
    slot: usize,
    stream: bool,
}

impl Exporter {
    /// The stream or socket `handle` reaches; `None` for a handle that
    /// reaches neither.
    pub fn of(handle: crate::BorrowedHandle<'_>) -> Option<Self> {
        match stream::hold(handle) {
            Some(slot) => Some(Self { slot, stream: true }),
            None => socket::hold(handle).map(|slot| Self { slot, stream: false }),
        }
    }

    /// A new holder, its end of the connection to give another process
    /// ([`stream::export`], [`socket::export`]).
    pub fn export(&self) -> Result<OwnedHandle, NetError> {
        match self.stream {
            true => stream::export_held(self.slot),
            false => socket::export_held(self.slot),
        }
    }
}

impl Drop for Exporter {
    fn drop(&mut self) {
        match self.stream {
            true => stream::release(self.slot),
            false => socket::release(self.slot),
        }
    }
}

use crate::ipc::{Frame, FrameRx, IpcError, IpcPayload};
use crate::ipc_payload;
use crate::{Connection, OwnedHandle};

/// The requests a new connection to netstack carries. Each that makes a
/// socket makes the connection its first holder's; a request on a socket is
/// that holder's own ([`socket`], [`stream`]).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgType {
    /// [`UdpBindRequest`], answered [`BoundResponse`] with the holder's bell
    /// ([`socket::bind`]).
    UdpBind = 8,
    DnsLookup = 12,
    /// A stream's region with [`TcpConnectRequest`], answered with its two
    /// bells ([`stream::connect`]).
    TcpConnect = 23,
    /// [`TcpListenRequest`], answered [`BoundResponse`] with the holder's
    /// bell ([`socket::listen`]).
    TcpListen = 25,
}

impl MsgType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            8 => Some(Self::UdpBind),
            12 => Some(Self::DnsLookup),
            23 => Some(Self::TcpConnect),
            25 => Some(Self::TcpListen),
            _ => None,
        }
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RespType {
    Result = 128,
    Error = 129,
}

// Error codes (on the wire)

pub const ERR_CONNECTION_REFUSED: u32 = 1;
pub const ERR_CONNECTION_RESET: u32 = 2;
pub const ERR_TIMED_OUT: u32 = 3;
pub const ERR_ADDR_IN_USE: u32 = 4;
pub const ERR_NOT_CONNECTED: u32 = 5;
pub const ERR_INVALID_INPUT: u32 = 6;
/// netstack will not hold another connection of this kind right now.
///
/// Distinct from [`ERR_CONNECTION_REFUSED`] on purpose, and the distinction is
/// not cosmetic: the two ask the client for opposite responses. A peer that
/// refused the SYN will keep refusing it, so the right move is to give up on
/// that peer; netstack being full is a condition of this machine that clears when
/// something closes, so the right move is to back off and retry the same peer.
/// A client that cannot tell them apart cannot do either correctly.
///
/// The conflation was real, not hypothetical: `netstack`'s own pending-connect
/// path answers a socket that reached `Closed` with `ERR_CONNECTION_REFUSED`,
/// so a capacity refusal on that code is indistinguishable from an ordinary
/// failed connection — including to a test trying to find where the cap is.
pub const ERR_RESOURCE_EXHAUSTED: u32 = 7;
/// The socket was not given the permission the request needs: a datagram to a
/// broadcast address from a socket whose [`OPT_BROADCAST`] is off. The request
/// was well formed, which is what keeps this apart from [`ERR_INVALID_INPUT`].
pub const ERR_PERMISSION_DENIED: u32 = 8;
/// A holder's accept or receive found nothing to take: its bell rings once
/// there is something ([`socket`]).
pub const ERR_WOULD_BLOCK: u32 = 9;
pub const ERR_OTHER: u32 = 255;

/// Nagle's algorithm off, on a listener: its listen's [`TcpOptions`] and then
/// [`socket::set_option`], for every connection whose SYN arrives from then
/// on, and one that began before keeps what it has. A connection says what it
/// began with in its accept's answer; a stream's own is a word of its region
/// ([`stream::NODELAY`]).
pub const OPT_NODELAY: u32 = 1;
/// A datagram socket may send to a broadcast address: [`socket::set_option`].
pub const OPT_BROADCAST: u32 = 2;

/// The widest request payload netstack reads: a [`dns_lookup`]'s hostname,
/// since every typed request is far smaller.
pub const MAX_REQUEST: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    NetstackNotFound,
    ConnectionRefused,
    ConnectionReset,
    TimedOut,
    AddrInUse,
    NotConnected,
    InvalidInput,
    /// netstack is at its own limit. Retryable against the same peer, unlike
    /// [`NetError::ConnectionRefused`] — see [`ERR_RESOURCE_EXHAUSTED`].
    ResourceExhausted,
    /// See [`ERR_PERMISSION_DENIED`].
    PermissionDenied,
    /// See [`ERR_WOULD_BLOCK`], and a holder's call that does not wait.
    WouldBlock,
    /// A handle that reaches none of this process's sockets.
    NotASocket,
    Protocol(u32),
    Io,
}

impl NetError {
    pub fn from_error_code(code: u32) -> Self {
        match code {
            ERR_CONNECTION_REFUSED => NetError::ConnectionRefused,
            ERR_CONNECTION_RESET => NetError::ConnectionReset,
            ERR_TIMED_OUT => NetError::TimedOut,
            ERR_ADDR_IN_USE => NetError::AddrInUse,
            ERR_NOT_CONNECTED => NetError::NotConnected,
            ERR_INVALID_INPUT => NetError::InvalidInput,
            ERR_RESOURCE_EXHAUSTED => NetError::ResourceExhausted,
            ERR_PERMISSION_DENIED => NetError::PermissionDenied,
            ERR_WOULD_BLOCK => NetError::WouldBlock,
            ERR_OTHER => NetError::Io,
            // An older client meets a newer netstack here rather than at a panic:
            // an unknown code is still an error, and still says which one.
            code => NetError::Protocol(code),
        }
    }
}

ipc_payload! {
    /// [`MsgType::TcpConnect`], with the stream's region.
    pub struct TcpConnectRequest {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
        pub timeout_ms: u32,
    }

    /// The answer to [`MsgType::TcpConnect`], with the stream's bells
    /// (`net::stream::BELLS`): the address and port the stream left from.
    pub struct TcpConnectResponse {
        pub local_addr: [u8; 4],
        pub local_port: u16,
        pub _pad: u16,
    }

    pub struct TcpListenRequest {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
        /// What the listener holds from the moment its port listens.
        pub options: TcpOptions,
    }

    /// The answer to [`MsgType::TcpListen`] and [`MsgType::UdpBind`], with the
    /// first holder's bell.
    pub struct BoundResponse {
        pub port: u16,
        pub _pad: u16,
    }

    /// The options a listener holds, a word each: zero is off and anything
    /// else on.
    #[derive(Debug, PartialEq, Eq)]
    pub struct TcpOptions {
        nodelay: u32,
    }

    /// The answer to a listener holder's [`socket::ACCEPT`], with the stream's
    /// connection and its bells: the peer, and whether Nagle's algorithm
    /// is off for the stream as its listener had it, which netstack has
    /// written into the region's header too.
    pub struct TcpAcceptResponse {
        pub remote_addr: [u8; 4],
        pub remote_port: u16,
        pub local_port: u16,
        pub local_addr: [u8; 4],
        pub nodelay: u32,
    }

    pub struct UdpBindRequest {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
    }

    pub struct ErrorResponse {
        pub code: u32,
    }
}

impl TcpOptions {
    pub fn new(nodelay: bool) -> Self {
        Self { nodelay: nodelay as u32 }
    }

    /// [`OPT_NODELAY`].
    pub fn nodelay(&self) -> bool {
        self.nodelay != 0
    }
}

// NetstackConn — per-operation IPC connection (typestate protocol)

pub struct NetstackConn(Connection);

impl NetstackConn {
    /// One connection to netstack, through this process's own namespace.
    ///
    /// **There was a retry loop here and it is gone.** It spun a hundred times
    /// at ten milliseconds waiting for a name to appear in a global registry;
    /// a `netstack` connector is live from this process's first instruction, so
    /// there is nothing to wait for.
    ///
    /// [`NetError::ResourceExhausted`] is a separate answer and a retryable
    /// one: it is the *kernel's* port queue full of connections netstack has not
    /// accepted yet, which is backpressure and not a limit netstack chose. The
    /// retry loop used to hide it — it retried every error alike — and
    /// collapsing it into `NetstackNotFound` would leave a caller told the machine
    /// has no network because a burst outran one accept loop.
    pub fn connect() -> Result<Self, NetError> {
        crate::endow::service("netstack").map(Self).map_err(|e| match e {
            // Both are "there is no netstack to reach from here": one because the
            // manifest gave this program none, one because it has exited.
            crate::endow::EndowError::NotEndowed
            | crate::endow::EndowError::ServerGone => NetError::NetstackNotFound,
            crate::endow::EndowError::Refused(
                toyos_abi::syscall::SyscallError::ResourceExhausted,
            ) => NetError::ResourceExhausted,
            crate::endow::EndowError::Refused(_) => NetError::Io,
        })
    }

    pub fn request<Req: IpcPayload>(self, msg_type: MsgType, payload: &Req) -> Result<PendingResponse, NetError> {
        const { assert!(core::mem::size_of::<Req>() <= MAX_REQUEST) };
        self.0.send(msg_type as u32, payload).map_err(hangup)?;
        Ok(PendingResponse(self))
    }

    /// A request that hands netstack pipe ends, in the request's own frame,
    /// consumed whether or not this answers `Ok` ([`Connection::send_with`]).
    pub fn request_with_handles<Req: IpcPayload>(
        self,
        handles: impl IntoIterator<Item = OwnedHandle>,
        msg_type: MsgType,
        payload: &Req,
    ) -> Result<PendingResponse, NetError> {
        const { assert!(core::mem::size_of::<Req>() <= MAX_REQUEST) };
        self.0.send_with(msg_type as u32, payload, handles).map_err(hangup)?;
        Ok(PendingResponse(self))
    }

    pub fn request_bytes(self, msg_type: MsgType, data: &[u8]) -> Result<PendingResponse, NetError> {
        self.0.send_bytes(msg_type as u32, data).map_err(hangup)?;
        Ok(PendingResponse(self))
    }
}

/// The widest first frame an exported end carries.
const MAX_ADOPT: usize = {
    let (stream, socket) = (core::mem::size_of::<stream::Adopt>(), core::mem::size_of::<socket::Adopt>());
    if stream > socket { stream } else { socket }
};

/// An exported end taken up as this process's holder of what its first frame
/// says it holds, a socket ([`socket::ADOPT`]) or else a stream
/// (`stream::adopted`'s rules), as its descriptor's object. The frame
/// is read without waiting: netstack queued it before it answered the export,
/// so it is there or never sent. `None`, with everything closed, where the
/// process takes up no holder of it.
pub fn adopt_exported(end: OwnedHandle) -> Option<crate::fd::Object> {
    let connection = Connection::received(end);
    let mut frames: FrameRx<MAX_ADOPT> = FrameRx::new();
    let first = match frames.pump(&connection) {
        crate::ipc::RxStep::Frame { msg_type, payload_len, .. } => {
            let handles = frames.take_batch();
            Some((msg_type, frames.payload(payload_len), handles))
        }
        _ => None,
    };
    match first {
        Some((socket::ADOPT, payload, handles)) => socket::adopted(connection, payload, handles).map(socket::descriptor),
        first => {
            let first = first.filter(|&(msg_type, ..)| msg_type == stream::ADOPT).map(|(_, payload, handles)| (payload, handles));
            stream::adopted(connection, first).map(stream::descriptor)
        }
    }
}

/// A netstack that hung up mid-exchange is a netstack that is not there.
///
/// A connector is in the namespace from a program's first instruction, so
/// connecting to a netstack that has already exited *succeeds*: the
/// connection queues on a port nobody will accept from, and the hang-up
/// arrives at the first send or receive instead. The kernel answers a send
/// `Gone` once no receiver is left, and a receive `Gone` (`Disconnected`)
/// once the peer has gone with nothing queued. Either is
/// [`NetError::NetstackNotFound`], so a caller can tell "this machine has no
/// network" from "netstack failed".
pub(crate) fn hangup(e: IpcError) -> NetError {
    match e {
        IpcError::Disconnected
        | IpcError::Syscall(toyos_abi::syscall::SyscallError::Gone) => NetError::NetstackNotFound,
        _ => NetError::Io,
    }
}

/// netstack's answer on `connection`, taken into `rx`: a [`RespType::Result`],
/// whose payload and handles are the caller's, or the error a
/// [`RespType::Error`] carries.
pub(crate) fn answer<const MAX: usize>(
    connection: &impl crate::AsHandle,
    rx: &mut FrameRx<MAX>,
) -> Result<Frame, NetError> {
    let frame = rx.recv(connection).map_err(hangup)?;
    if frame.msg_type == RespType::Error as u32 {
        let err: ErrorResponse = crate::ipc::decode_payload(rx.payload(frame.payload_len)).map_err(hangup)?;
        return Err(NetError::from_error_code(err.code));
    }
    if frame.msg_type != RespType::Result as u32 {
        return Err(NetError::Protocol(frame.msg_type));
    }
    Ok(frame)
}

/// The widest typed answer netstack sends.
pub(crate) const MAX_ANSWER: usize = 64;

pub struct PendingResponse(NetstackConn);

impl PendingResponse {
    fn conn(&self) -> &Connection { &(self.0).0 }

    pub fn response<Resp: IpcPayload>(self) -> Result<Resp, NetError> {
        const { assert!(core::mem::size_of::<Resp>() <= MAX_ANSWER && core::mem::size_of::<ErrorResponse>() <= MAX_ANSWER) };
        let mut rx = FrameRx::<MAX_ANSWER>::new();
        let frame = answer(self.conn(), &mut rx)?;
        crate::ipc::decode_payload(rx.payload(frame.payload_len)).map_err(hangup)
    }

    /// The answer's bytes, taken straight into `buf`: their length. An
    /// answer longer than `buf` is [`NetError::Io`], an error answer
    /// included, so `buf` must hold an [`ErrorResponse`] too.
    pub fn response_bytes(self, buf: &mut [u8]) -> Result<usize, NetError> {
        let (frame, _) = self.conn().recv_into(buf).map_err(hangup)?;
        let payload = &buf[..frame.payload_len];
        match frame.msg_type {
            t if t == RespType::Error as u32 => {
                let err: ErrorResponse = crate::ipc::decode_payload(payload).map_err(hangup)?;
                Err(NetError::from_error_code(err.code))
            }
            t if t == RespType::Result as u32 => Ok(frame.payload_len),
            other => Err(NetError::Protocol(other)),
        }
    }

    /// The answer, the `N` handles it hands over, and the connection, which
    /// is then the caller's.
    pub fn answer_with_handles<Resp: IpcPayload, const N: usize>(self) -> Result<(Resp, [OwnedHandle; N], Connection), NetError> {
        const { assert!(core::mem::size_of::<Resp>() <= MAX_ANSWER && core::mem::size_of::<ErrorResponse>() <= MAX_ANSWER) };
        let mut rx = FrameRx::<MAX_ANSWER>::new();
        let frame = answer(self.conn(), &mut rx)?;
        let answer = crate::ipc::decode_payload(rx.payload(frame.payload_len)).map_err(hangup)?;
        let handles = rx.take_handles::<N>().ok_or(NetError::Protocol(frame.msg_type))?;
        Ok((answer, handles, (self.0).0))
    }

    /// An answer whose payload says nothing past its type.
    pub fn status(self) -> Result<(), NetError> {
        let mut rx = FrameRx::<MAX_ANSWER>::new();
        answer(self.conn(), &mut rx).map(drop)
    }
}

pub fn dns_lookup(hostname: &str, results: &mut [[u8; 4]]) -> Result<usize, NetError> {
    if hostname.len() > MAX_REQUEST {
        return Err(NetError::InvalidInput);
    }
    let mut buf = [0u8; 256];
    let n = NetstackConn::connect()?
        .request_bytes(MsgType::DnsLookup, hostname.as_bytes())?
        .response_bytes(&mut buf)?;

    if n == 0 {
        return Ok(0);
    }

    let count = buf[0] as usize;
    let mut written = 0;
    let mut offset = 1;
    for _ in 0..count {
        if written >= results.len() || offset >= n {
            break;
        }
        if buf[offset] == 4 && offset + 5 <= n {
            results[written] = [buf[offset + 1], buf[offset + 2], buf[offset + 3], buf[offset + 4]];
            written += 1;
            offset += 5;
        } else {
            break;
        }
    }
    Ok(written)
}

/// [`hangup`]'s whole table, which is what decides whether a program that needs
/// netstack leaves quietly or dies loudly.
///
/// **Both directions are asserted and the second is the point.** A guard that
/// accepted every error would pass the three tests above and would be the wrong
/// fix: a machine that *has* a NIC and cannot bind must still be loud, so the
/// refusals that are not a peer's absence have to stay [`NetError::Io`].
#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::syscall::SyscallError;

    /// `SYS_MESSAGE_SEND` into a connection whose server end has gone: the
    /// first refusal any request can meet, a listen's included.
    #[test]
    fn a_gone_handle_transfer_is_a_netstack_that_is_not_there() {
        assert_eq!(hangup(IpcError::Syscall(SyscallError::Gone)), NetError::NetstackNotFound);
    }

    /// `NotFound` is not this fact and does not reach it.
    #[test]
    fn a_not_found_is_not_a_netstack_that_is_not_there() {
        assert_eq!(hangup(IpcError::Syscall(SyscallError::NotFound)), NetError::Io);
    }

    /// The case that already worked: netstack was still there for the request and
    /// left before the response, so the hang-up arrives at a `read` of zero.
    #[test]
    fn a_read_that_hung_up_is_a_netstack_that_is_not_there() {
        assert_eq!(hangup(IpcError::Disconnected), NetError::NetstackNotFound);
    }

    /// Everything that is *not* a peer that has gone. Each of these on a
    /// machine with a live netstack is a real failure and must reach the caller as
    /// one — `sshserver` panics on `NetError::Io` by design.
    #[test]
    fn nothing_else_becomes_a_missing_netstack() {
        for e in [
            IpcError::Syscall(SyscallError::PermissionDenied),
            IpcError::Syscall(SyscallError::ResourceExhausted),
            IpcError::Syscall(SyscallError::InvalidArgument),
            IpcError::Syscall(SyscallError::BadAddress),
            IpcError::Syscall(SyscallError::WouldBlock),
            IpcError::Syscall(SyscallError::Io),
            IpcError::Malformed,
            IpcError::TooLarge,
        ] {
            assert_eq!(hangup(e), NetError::Io);
        }
    }

    /// netstack's own wire codes are a separate vocabulary and this change does not
    /// touch it: an `ErrorResponse` netstack chose to send is an answer from a netstack
    /// that is *there*, and only `ERR_NOT_CONNECTED` means the machine has no
    /// network.
    #[test]
    fn a_code_netstack_chose_is_still_its_own_answer() {
        assert_eq!(NetError::from_error_code(ERR_NOT_CONNECTED), NetError::NotConnected);
        assert_eq!(NetError::from_error_code(ERR_ADDR_IN_USE), NetError::AddrInUse);
        assert_eq!(
            NetError::from_error_code(ERR_CONNECTION_REFUSED),
            NetError::ConnectionRefused
        );
        assert_eq!(NetError::from_error_code(ERR_PERMISSION_DENIED), NetError::PermissionDenied);
        assert_eq!(NetError::from_error_code(ERR_INVALID_INPUT), NetError::InvalidInput);
        assert_eq!(NetError::from_error_code(ERR_WOULD_BLOCK), NetError::WouldBlock);
        assert_eq!(NetError::from_error_code(ERR_OTHER), NetError::Io);
        assert_eq!(NetError::from_error_code(4242), NetError::Protocol(4242));
    }

    /// The listen's request as netstack decodes it: the options last, after
    /// the address and the port, any non-zero word an option that is on, and
    /// a request without them refused.
    #[test]
    fn a_listens_request_ends_in_its_listeners_options() {
        let mut wire = [192, 0, 2, 7, 0x16, 0, 0, 0, 0, 0, 0, 0];
        let request: TcpListenRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!((request.addr, request.port, request.options), ([192, 0, 2, 7], 22, TcpOptions::new(false)));
        wire[8] = 1;
        let request: TcpListenRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!(request.options, TcpOptions::new(true));
        wire[8..].copy_from_slice(&[0, 0, 0, 0x80]);
        let request: TcpListenRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert!(request.options.nodelay());
        assert!(matches!(
            crate::ipc::decode_payload::<TcpListenRequest>(&wire[..8]),
            Err(IpcError::Malformed)
        ));
    }

    /// The accept's answer as a client decodes it: the peer, the local port
    /// and address, and `nodelay` last, any non-zero word on, and an answer
    /// without it refused.
    #[test]
    fn an_accepts_answer_ends_in_its_streams_nodelay() {
        let mut wire = [192, 0, 2, 7, 0x39, 0x30, 0x16, 0, 198, 51, 100, 9, 0, 0, 0, 0];
        let answer: TcpAcceptResponse = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!(
            (answer.remote_addr, answer.remote_port, answer.local_port, answer.local_addr),
            ([192, 0, 2, 7], 12345, 22, [198, 51, 100, 9])
        );
        assert_eq!(answer.nodelay, 0);
        wire[12..].copy_from_slice(&[0, 0, 0, 0x80]);
        let answer: TcpAcceptResponse = crate::ipc::decode_payload(&wire).unwrap();
        assert_ne!(answer.nodelay, 0);
        assert!(matches!(
            crate::ipc::decode_payload::<TcpAcceptResponse>(&wire[..12]),
            Err(IpcError::Malformed)
        ));
    }
}
