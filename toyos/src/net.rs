//! ToyOS userland networking library.
//!
//! Owns the netstack IPC protocol and provides client functions for TCP, UDP, and DNS.
//! All networking in ToyOS goes through the `netstack` daemon via message passing
//! and kernel pipes.

use crate::ipc::{IpcError, IpcHeader, IpcPayload};
use crate::ipc_payload;
use crate::{Connection, OwnedHandle, Pipe};

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MsgType {
    TcpClose = 4,
    TcpShutdown = 7,
    UdpBind = 8,
    UdpSendTo = 9,
    UdpRecvFrom = 10,
    UdpClose = 11,
    DnsLookup = 12,
    TcpSetOption = 13,
    TcpListenerSetOption = 14,
    UdpSetOption = 15,
    TcpConnectPiped = 20,
    TcpBindPiped = 21,
    TcpAcceptPiped = 22,
}

impl MsgType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            4 => Some(Self::TcpClose),
            7 => Some(Self::TcpShutdown),
            8 => Some(Self::UdpBind),
            9 => Some(Self::UdpSendTo),
            10 => Some(Self::UdpRecvFrom),
            11 => Some(Self::UdpClose),
            12 => Some(Self::DnsLookup),
            13 => Some(Self::TcpSetOption),
            14 => Some(Self::TcpListenerSetOption),
            15 => Some(Self::UdpSetOption),
            20 => Some(Self::TcpConnectPiped),
            21 => Some(Self::TcpBindPiped),
            22 => Some(Self::TcpAcceptPiped),
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
pub const ERR_OTHER: u32 = 255;

/// Nagle's algorithm off. On a stream, [`tcp_set_option`]. On a listener,
/// its bind's [`TcpOptions`] and then [`tcp_listener_set_option`]: for every
/// connection whose SYN arrives from then on, and one that began before keeps
/// what it has. A connection says what it began with in its accept's answer
/// ([`TcpOptions`]); one a connect made begins with the algorithm on.
pub const OPT_NODELAY: u32 = 1;
/// A datagram socket may send to a broadcast address: [`udp_set_option`].
pub const OPT_BROADCAST: u32 = 2;

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
            ERR_OTHER => NetError::Io,
            // An older client meets a newer netstack here rather than at a panic:
            // an unknown code is still an error, and still says which one.
            code => NetError::Protocol(code),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TcpSocketId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UdpSocketId(pub u32);

// Protocol request/response structs

/// A duplex data path is **two pipe ends, sent with the request that opens
/// it**, in this order. The client makes both pipes and keeps the ends facing
/// itself.
///
/// Stated once because the two sides of the swap are in different programs: a
/// reversed pair is two working pipes carrying each other's bytes, which no
/// type here can catch.
pub const DATA_HANDLES: usize = 2;
/// The end netstack writes into and the client reads from.
pub const DATA_TO_CLIENT: usize = 0;
/// The end the client writes into and netstack reads from.
pub const DATA_FROM_CLIENT: usize = 1;

/// A bind sends one end: the one netstack writes an accept notification into.
pub const NOTIFY_HANDLES: usize = 1;

ipc_payload! {
    pub struct TcpConnectPipedRequest {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
        pub timeout_ms: u32,
    }

    pub struct TcpConnectResponse {
        pub socket_id: u32,
        pub local_port: u16,
        pub _pad: u16,
    }

    pub struct SocketCloseRequest {
        pub socket_id: u32,
    }

    pub struct TcpBindPipedRequest {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
        /// What the listener holds from the moment its port listens.
        pub options: TcpOptions,
    }

    pub struct TcpBindResponse {
        pub socket_id: u32,
        pub bound_port: u16,
        pub _pad: u16,
    }

    pub struct TcpShutdownRequest {
        pub socket_id: u32,
        pub how: u32,
    }

    pub struct TcpAcceptPipedRequest {
        pub socket_id: u32,
    }

    /// The options a listener or a stream holds, a word each: zero is off and
    /// anything else on.
    #[derive(Debug, PartialEq, Eq)]
    pub struct TcpOptions {
        nodelay: u32,
    }

    pub struct TcpAcceptPipedResponse {
        pub socket_id: u32,
        pub remote_addr: [u8; 4],
        pub remote_port: u16,
        pub local_port: u16,
        /// What the connection holds as it is handed over.
        pub options: TcpOptions,
    }

    pub struct UdpBindRequest {
        pub addr: [u8; 4],
        pub port: u16,
        pub _pad: u16,
    }

    pub struct UdpBindResponse {
        pub socket_id: u32,
        pub bound_port: u16,
        pub _pad: u16,
    }

    pub struct UdpSendToRequest {
        pub socket_id: u32,
        pub addr: [u8; 4],
        pub port: u16,
        pub len: u16,
    }

    pub struct UdpRecvFromRequest {
        pub socket_id: u32,
        pub max_len: u32,
    }

    pub struct UdpRecvResponse {
        pub addr: [u8; 4],
        pub port: u16,
        pub len: u16,
    }

    /// [`MsgType::TcpSetOption`], [`MsgType::TcpListenerSetOption`] and
    /// [`MsgType::UdpSetOption`]: the request's type says which kind of socket
    /// `socket_id` names.
    pub struct SocketOptionRequest {
        pub socket_id: u32,
        pub option: u32,
        pub value: u32,
    }

    pub struct ErrorResponse {
        pub code: u32,
    }

    struct SentBytes {
        value: u32,
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

// Return types

pub struct TcpConnection {
    pub rx: Pipe,
    pub tx: Pipe,
    pub socket_id: TcpSocketId,
    pub local_port: u16,
}

pub struct TcpBound {
    pub notify: Pipe,
    pub socket_id: TcpSocketId,
    pub bound_port: u16,
}

pub struct TcpAccepted {
    pub rx: Pipe,
    pub tx: Pipe,
    pub socket_id: TcpSocketId,
    pub remote_addr: [u8; 4],
    pub remote_port: u16,
    pub local_port: u16,
    pub options: TcpOptions,
}

pub struct UdpBound {
    pub socket_id: UdpSocketId,
    pub bound_port: u16,
    pub tx: Pipe,
    pub rx: Pipe,
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
        self.0.send(msg_type as u32, payload).map_err(hangup)?;
        Ok(PendingResponse(self))
    }

    /// A request that hands netstack pipe ends, consumed whether or not this
    /// answers `Ok` ([`Connection::send_handles`]).
    pub fn request_with_handles<Req: IpcPayload>(
        self,
        handles: impl IntoIterator<Item = OwnedHandle>,
        msg_type: MsgType,
        payload: &Req,
    ) -> Result<PendingResponse, NetError> {
        self.0.send_handles(handles).map_err(|e| hangup(IpcError::Syscall(e)))?;
        self.0.send(msg_type as u32, payload).map_err(hangup)?;
        Ok(PendingResponse(self))
    }

    pub fn request_bytes(self, msg_type: MsgType, data: &[u8]) -> Result<PendingResponse, NetError> {
        self.0.send_bytes(msg_type as u32, data).map_err(hangup)?;
        Ok(PendingResponse(self))
    }
}

/// A netstack that hung up mid-exchange is a netstack that is not there.
///
/// **[`NetstackConn::connect`] already says so and the exchange did not, which is
/// a distinction this architecture removed.** A connector is in the namespace
/// from a program's first instruction, so connecting to a netstack that has
/// already exited *succeeds* — the connection queues on a port nobody will
/// ever accept from — and the hang-up arrives at the first send or the first
/// read instead. Reporting that as [`NetError::Io`] left every caller unable
/// to tell "this machine has no network" from "netstack failed", and `sshserver`
/// panicked across the boot of every NIC-less machine that lost the race
/// rather than exiting with the line it has for exactly this.
///
/// **`Disconnected` is only the word for a hang-up this endpoint *read*, and a
/// request writes twice before it reads at all.** `IpcError::Disconnected` is
/// raised in one place — `ipc::read_exact`, on a `read` that answered zero — so
/// it is what a peer that left while this endpoint was waiting for the response
/// looks like. A peer that left *before* the request went out is refused by the
/// kernel at one of the two writes instead, and both answer `Gone` on a
/// connection whose handle is still live and still this process's: when the
/// server end's last handle goes, `port::Acceptor::on_zero_handles` closes
/// every queued connection's inbox — this end's *outbox*, so `HandleQueue::push`
/// refuses `SYS_HANDLE_SEND` rather than filling a queue nobody will drain —
/// and drops the connection, which drops the server's read end, so `SYS_WRITE`
/// is a pipe with no readers.
///
/// `Gone` cannot mean anything else here. This is applied only to `read`,
/// `write` and `handle_send` on a connection this process holds, and a handle a
/// process does not hold ends it at the kernel rather than answering a word.
fn hangup(e: IpcError) -> NetError {
    match e {
        IpcError::Disconnected
        | IpcError::Syscall(toyos_abi::syscall::SyscallError::Gone) => NetError::NetstackNotFound,
        _ => NetError::Io,
    }
}

pub struct PendingResponse(NetstackConn);

impl PendingResponse {
    fn conn(&self) -> &Connection { &(self.0).0 }

    fn recv_checked_header(&self) -> Result<IpcHeader, NetError> {
        let header = self.conn().recv_header().map_err(hangup)?;
        if header.msg_type == RespType::Error as u32 {
            let err: ErrorResponse = self.conn().recv_payload(&header).map_err(hangup)?;
            return Err(NetError::from_error_code(err.code));
        }
        if header.msg_type != RespType::Result as u32 {
            return Err(NetError::Protocol(header.msg_type));
        }
        Ok(header)
    }

    pub fn response<Resp: IpcPayload>(self) -> Result<Resp, NetError> {
        let header = self.recv_checked_header()?;
        self.conn().recv_payload(&header).map_err(hangup)
    }

    pub fn response_bytes(self, buf: &mut [u8]) -> Result<usize, NetError> {
        let header = self.recv_checked_header()?;
        self.conn().recv_bytes(&header, buf).map_err(hangup)
    }

    pub fn status(self) -> Result<(), NetError> {
        let header = self.recv_checked_header()?;
        if header.len() > 0 {
            let mut skip = [0u8; 128];
            let _ = self.conn().recv_bytes(&header, &mut skip);
        }
        Ok(())
    }
}

/// The two pipes behind a duplex data path, split into what the caller keeps
/// and what netstack is given.
///
/// The `to_netstack` ends are owned here only until the send; a caller that errors
/// out before then drops this and both pipes go with it.
struct DataPath {
    rx: Pipe,
    tx: Pipe,
    to_netstack: [Pipe; DATA_HANDLES],
}

impl DataPath {
    fn create() -> Result<Self, NetError> {
        let (rx, netstack_tx) = crate::pipe_pair().map_err(|_| NetError::Io)?;
        let (netstack_rx, tx) = crate::pipe_pair().map_err(|_| NetError::Io)?;
        Ok(Self { rx, tx, to_netstack: [netstack_tx, netstack_rx] })
    }

    fn split(self) -> (Pipe, Pipe, [OwnedHandle; DATA_HANDLES]) {
        (self.rx, self.tx, self.to_netstack.map(OwnedHandle::from))
    }
}

// TCP client functions

pub fn tcp_connect(
    addr: [u8; 4],
    port: u16,
    timeout_ms: u32,
) -> Result<TcpConnection, NetError> {
    let netstack = NetstackConn::connect()?;
    let (rx, tx, handles) = DataPath::create()?.split();

    let resp: TcpConnectResponse = netstack
        .request_with_handles(handles, MsgType::TcpConnectPiped, &TcpConnectPipedRequest {
            addr,
            port,
            _pad: 0,
            timeout_ms,
        })?
        .response()?;

    Ok(TcpConnection { rx, tx, socket_id: TcpSocketId(resp.socket_id), local_port: resp.local_port })
}

/// A listener that holds no option.
pub fn tcp_bind(addr: [u8; 4], port: u16) -> Result<TcpBound, NetError> {
    tcp_bind_with(addr, port, TcpOptions::new(false))
}

/// A listener that holds `options` before any SYN can reach its port.
pub fn tcp_bind_with(addr: [u8; 4], port: u16, options: TcpOptions) -> Result<TcpBound, NetError> {
    let netstack = NetstackConn::connect()?;
    let (notify, netstack_notify) = crate::pipe_pair().map_err(|_| NetError::Io)?;

    let resp: TcpBindResponse = netstack
        .request_with_handles(
            [netstack_notify.into()],
            MsgType::TcpBindPiped,
            &TcpBindPipedRequest { addr, port, _pad: 0, options },
        )?
        .response()?;

    Ok(TcpBound { notify, socket_id: TcpSocketId(resp.socket_id), bound_port: resp.bound_port })
}

pub fn tcp_accept(socket_id: TcpSocketId) -> Result<TcpAccepted, NetError> {
    let netstack = NetstackConn::connect()?;
    let (rx, tx, handles) = DataPath::create()?.split();

    let resp: TcpAcceptPipedResponse = netstack
        .request_with_handles(handles, MsgType::TcpAcceptPiped, &TcpAcceptPipedRequest {
            socket_id: socket_id.0,
        })?
        .response()?;

    Ok(TcpAccepted {
        rx,
        tx,
        socket_id: TcpSocketId(resp.socket_id),
        remote_addr: resp.remote_addr,
        remote_port: resp.remote_port,
        local_port: resp.local_port,
        options: resp.options,
    })
}

pub fn tcp_shutdown(socket_id: TcpSocketId, how: u32) -> Result<(), NetError> {
    NetstackConn::connect()?
        .request(MsgType::TcpShutdown, &TcpShutdownRequest { socket_id: socket_id.0, how })?
        .status()
}

pub fn tcp_close(socket_id: TcpSocketId) -> Result<(), NetError> {
    NetstackConn::connect()?
        .request(MsgType::TcpClose, &SocketCloseRequest { socket_id: socket_id.0 })?
        .status()
}

pub fn tcp_set_option(socket_id: TcpSocketId, option: u32, value: u32) -> Result<(), NetError> {
    NetstackConn::connect()?
        .request(MsgType::TcpSetOption, &SocketOptionRequest { socket_id: socket_id.0, option, value })?
        .status()
}

pub fn tcp_listener_set_option(socket_id: TcpSocketId, option: u32, value: u32) -> Result<(), NetError> {
    NetstackConn::connect()?
        .request(MsgType::TcpListenerSetOption, &SocketOptionRequest { socket_id: socket_id.0, option, value })?
        .status()
}

// UDP client functions

pub fn udp_bind(addr: [u8; 4], port: u16) -> Result<UdpBound, NetError> {
    let netstack = NetstackConn::connect()?;
    let (rx, tx, handles) = DataPath::create()?.split();

    let resp: UdpBindResponse = netstack
        .request_with_handles(handles, MsgType::UdpBind, &UdpBindRequest { addr, port, _pad: 0 })?
        .response()?;

    Ok(UdpBound { socket_id: UdpSocketId(resp.socket_id), bound_port: resp.bound_port, tx, rx })
}

pub fn udp_send_to(socket_id: UdpSocketId, addr: [u8; 4], port: u16, len: u16) -> Result<u32, NetError> {
    let resp: SentBytes = NetstackConn::connect()?
        .request(MsgType::UdpSendTo, &UdpSendToRequest {
            socket_id: socket_id.0,
            addr,
            port,
            len,
        })?
        .response()?;
    Ok(resp.value)
}

pub fn udp_recv_from(socket_id: UdpSocketId, max_len: u32) -> Result<UdpRecvResponse, NetError> {
    NetstackConn::connect()?
        .request(MsgType::UdpRecvFrom, &UdpRecvFromRequest {
            socket_id: socket_id.0,
            max_len,
        })?
        .response()
}

pub fn udp_close(socket_id: UdpSocketId) -> Result<(), NetError> {
    NetstackConn::connect()?
        .request(MsgType::UdpClose, &SocketCloseRequest { socket_id: socket_id.0 })?
        .status()
}

pub fn udp_set_option(socket_id: UdpSocketId, option: u32, value: u32) -> Result<(), NetError> {
    NetstackConn::connect()?
        .request(MsgType::UdpSetOption, &SocketOptionRequest { socket_id: socket_id.0, option, value })?
        .status()
}

pub fn dns_lookup(hostname: &str, results: &mut [[u8; 4]]) -> Result<usize, NetError> {
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

    /// `SYS_HANDLE_SEND` into a connection whose server end has gone. The first
    /// refusal a `tcp_bind` can meet, because a request that hands netstack pipe
    /// ends moves the handles before it writes the frame.
    #[test]
    fn a_gone_handle_transfer_is_a_netstack_that_is_not_there() {
        assert_eq!(hangup(IpcError::Syscall(SyscallError::Gone)), NetError::NetstackNotFound);
    }

    /// `SYS_WRITE` into the same connection a moment later reaches the same
    /// word, so `NotFound` is not this fact and no longer reaches it.
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
        assert_eq!(NetError::from_error_code(ERR_OTHER), NetError::Io);
        assert_eq!(NetError::from_error_code(4242), NetError::Protocol(4242));
    }

    /// The option request as netstack decodes it: three words in the order
    /// socket, option, value, and nothing shorter.
    #[test]
    fn an_option_request_is_three_words_on_the_wire() {
        let wire = [0x44, 0x33, 0x22, 0x11, 2, 0, 0, 0, 1, 0, 0, 0];
        let request: SocketOptionRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!(request.socket_id, 0x1122_3344);
        assert_eq!(request.option, OPT_BROADCAST);
        assert_eq!(request.value, 1);
        assert!(matches!(
            crate::ipc::decode_payload::<SocketOptionRequest>(&wire[..11]),
            Err(IpcError::Malformed)
        ));
    }

    /// The bind's request as netstack decodes it: the options last, after
    /// the address and the port, any non-zero word an option that is on, and
    /// a request without them refused.
    #[test]
    fn a_binds_request_ends_in_its_listeners_options() {
        let mut wire = [192, 0, 2, 7, 0x16, 0, 0, 0, 0, 0, 0, 0];
        let request: TcpBindPipedRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!((request.addr, request.port, request.options), ([192, 0, 2, 7], 22, TcpOptions::new(false)));
        wire[8] = 1;
        let request: TcpBindPipedRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!(request.options, TcpOptions::new(true));
        wire[8..].copy_from_slice(&[0, 0, 0, 0x80]);
        let request: TcpBindPipedRequest = crate::ipc::decode_payload(&wire).unwrap();
        assert!(request.options.nodelay());
        assert!(matches!(
            crate::ipc::decode_payload::<TcpBindPipedRequest>(&wire[..8]),
            Err(IpcError::Malformed)
        ));
    }

    /// The accept's answer as a client decodes it: the options last, after
    /// the ports, any non-zero word an option that is on, and an answer
    /// without them refused.
    #[test]
    fn an_accepts_answer_ends_in_its_connections_options() {
        let mut wire = [0x44, 0x33, 0x22, 0x11, 192, 0, 2, 7, 0x39, 0x30, 0x16, 0, 0, 0, 0, 0];
        let answer: TcpAcceptPipedResponse = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!(answer.socket_id, 0x1122_3344);
        assert_eq!((answer.remote_addr, answer.remote_port, answer.local_port), ([192, 0, 2, 7], 12345, 22));
        assert!(!answer.options.nodelay());
        assert_eq!(answer.options, TcpOptions::new(false));
        wire[12] = 1;
        let answer: TcpAcceptPipedResponse = crate::ipc::decode_payload(&wire).unwrap();
        assert_eq!(answer.options, TcpOptions::new(true));
        wire[12..].copy_from_slice(&[0, 0, 0, 0x80]);
        let answer: TcpAcceptPipedResponse = crate::ipc::decode_payload(&wire).unwrap();
        assert!(answer.options.nodelay());
        assert!(matches!(
            crate::ipc::decode_payload::<TcpAcceptPipedResponse>(&wire[..12]),
            Err(IpcError::Malformed)
        ));
    }
}
