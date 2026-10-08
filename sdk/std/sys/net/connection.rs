use toyos::AsHandle;
use toyos::net::{NetError, TcpSocketId, UdpSocketId};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, SyscallError};

use crate::fmt;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, ToSocketAddrs};
use crate::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use crate::sync::Arc;
use crate::sync::atomic::Ordering::Relaxed;
use crate::sync::atomic::{AtomicBool, AtomicU32};
use crate::time::{Duration, Instant};

// --- Helpers ---

fn net_err_to_io(e: NetError) -> io::Error {
    let kind = match e {
        NetError::ConnectionRefused => io::ErrorKind::ConnectionRefused,
        NetError::ConnectionReset => io::ErrorKind::ConnectionReset,
        NetError::TimedOut => io::ErrorKind::TimedOut,
        NetError::AddrInUse => io::ErrorKind::AddrInUse,
        NetError::NotConnected => io::ErrorKind::NotConnected,
        NetError::InvalidInput => io::ErrorKind::InvalidInput,
        NetError::NetstackNotFound => io::ErrorKind::NotConnected,
        NetError::PermissionDenied => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, "netstack error")
}

fn addr_to_v4(addr: &SocketAddr) -> io::Result<([u8; 4], u16)> {
    match addr {
        SocketAddr::V4(v4) => Ok((v4.ip().octets(), v4.port())),
        SocketAddr::V6(_) => Err(io::Error::new(io::ErrorKind::InvalidInput, "IPv6 not supported")),
    }
}

fn duration_to_ms(d: Option<Duration>) -> u32 {
    match d {
        Some(d) => d.as_millis().min(u32::MAX as u128) as u32,
        None => 0,
    }
}

fn syscall_err(e: SyscallError) -> io::Error {
    match e {
        SyscallError::WouldBlock => io::ErrorKind::WouldBlock.into(),
        _ => io::Error::new(io::ErrorKind::Other, "syscall error"),
    }
}

/// How long `TcpStream::connect` waits for each address to answer.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Wait until `handle` is ready for `flags` or `left` passes.
fn wait_ready(handle: RawHandle, flags: u32, left: Duration) {
    let poller = Poller::new(1);
    poller.watch_raw(handle, flags, 0);
    poller.wait(1, left.as_nanos().min(u64::MAX as u128) as u64, |_| {});
}

/// Run `op` until it does not answer `WouldBlock`, waiting for `flags` on
/// `handle` in between; `None` once `timeout_ms` has passed.
fn with_timeout(
    handle: RawHandle,
    flags: u32,
    timeout_ms: u32,
    mut op: impl FnMut() -> Result<usize, SyscallError>,
) -> Option<Result<usize, SyscallError>> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    loop {
        match op() {
            Err(SyscallError::WouldBlock) => {}
            done => return Some(done),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        wait_ready(handle, flags, left);
    }
}

const TIMED_OUT: io::Error = io::const_error!(io::ErrorKind::TimedOut, "timed out");

/// The connection was reset, or ended by netstack: its send pipe has no reader.
const RESET: io::Error = io::const_error!(io::ErrorKind::ConnectionReset, "connection reset");

const SHUT_DOWN: io::Error =
    io::const_error!(io::ErrorKind::BrokenPipe, "the stream was shut down for writing");

/// Join rx/tx pipes into a single duplex kernel handle.
fn make_socket_fd(rx: toyos::Pipe, tx: toyos::Pipe) -> io::Result<OwnedFd> {
    let socket_fd =
        syscall::connection_join(rx.as_handle(), tx.as_handle()).map_err(syscall_err)?;
    // The join takes references of its own; these two are consumed here.
    drop(rx);
    drop(tx);
    Ok(unsafe { OwnedFd::from_raw_fd(socket_fd.0 as i32) })
}

// --- Shared socket ownership (prevents double-close on duplicate) ---

enum NetstackSocket {
    Tcp(TcpSocketId),
    Udp(UdpSocketId),
}

impl Drop for NetstackSocket {
    fn drop(&mut self) {
        let _ = match self {
            NetstackSocket::Tcp(id) => toyos::net::tcp_close(*id),
            NetstackSocket::Udp(id) => toyos::net::udp_close(*id),
        };
    }
}

// --- TcpStream ---

pub struct TcpStream {
    fd: OwnedFd,
    socket: Arc<NetstackSocket>,
    /// `shutdown` was asked for this half, on this stream or a duplicate.
    read_shut: Arc<AtomicBool>,
    write_shut: Arc<AtomicBool>,
    peer: SocketAddr,
    local_port: u16,
    read_timeout_ms: AtomicU32,
    write_timeout_ms: AtomicU32,
    nodelay: AtomicBool,
    nonblocking: AtomicBool,
}

impl TcpStream {
    fn new(fd: OwnedFd, id: TcpSocketId, peer: SocketAddr, local_port: u16) -> TcpStream {
        TcpStream {
            fd,
            socket: Arc::new(NetstackSocket::Tcp(id)),
            read_shut: Arc::new(AtomicBool::new(false)),
            write_shut: Arc::new(AtomicBool::new(false)),
            peer,
            local_port,
            read_timeout_ms: AtomicU32::new(0),
            write_timeout_ms: AtomicU32::new(0),
            nodelay: AtomicBool::new(false),
            nonblocking: AtomicBool::new(false),
        }
    }

    fn socket_id(&self) -> TcpSocketId {
        match *self.socket {
            NetstackSocket::Tcp(id) => id,
            _ => unreachable!(),
        }
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        super::each_addr(addr, |addr| Self::connect_timeout(addr, CONNECT_TIMEOUT))
    }

    pub fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        let (ip, port) = addr_to_v4(addr)?;
        let conn = toyos::net::tcp_connect(ip, port, duration_to_ms(Some(timeout)))
            .map_err(net_err_to_io)?;
        let fd = make_socket_fd(conn.rx, conn.tx)?;
        Ok(TcpStream::new(fd, conn.socket_id, *addr, conn.local_port))
    }

    fn raw_handle(&self) -> RawHandle {
        RawHandle(self.fd.as_raw_fd() as u32)
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.read_timeout_ms.store(duration_to_ms(dur), Relaxed);
        Ok(())
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.write_timeout_ms.store(duration_to_ms(dur), Relaxed);
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        let ms = self.read_timeout_ms.load(Relaxed);
        Ok(if ms == 0 { None } else { Some(Duration::from_millis(ms as u64)) })
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        let ms = self.write_timeout_ms.load(Relaxed);
        Ok(if ms == 0 { None } else { Some(Duration::from_millis(ms as u64)) })
    }

    pub fn peek(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "peek not supported"))
    }

    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.read_shut.load(Relaxed) {
            return Ok(0);
        }
        let handle = self.raw_handle();
        let read = if self.nonblocking.load(Relaxed) {
            syscall::read_nonblock(handle, buf)
        } else {
            match self.read_timeout_ms.load(Relaxed) {
                0 => syscall::read(handle, buf),
                ms => with_timeout(handle, READABLE, ms, || syscall::read_nonblock(handle, buf))
                    .ok_or(TIMED_OUT)?,
            }
        };
        match read.map_err(syscall_err)? {
            0 => self.ended().map(|()| 0),
            n => Ok(n),
        }
    }

    /// Whether the receive pipe's end was the peer's FIN or not. netstack ends
    /// the send pipe too, and first, when the connection did not end in
    /// order — a reset, a timeout — so a send pipe with no
    /// reader behind a receive pipe at its end is a reset.
    fn ended(&self) -> io::Result<()> {
        match syscall::write_nonblock(self.raw_handle(), &[]) {
            Ok(_) | Err(SyscallError::WouldBlock) => Ok(()),
            Err(SyscallError::Gone) => Err(RESET),
            Err(e) => Err(syscall_err(e)),
        }
    }

    pub fn read_buf(&self, mut cursor: BorrowedCursor<'_, u8>) -> io::Result<()> {
        let n = self.read(cursor.ensure_init())?;
        cursor.advance_checked(n);
        Ok(())
    }

    pub fn read_vectored(&self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        crate::io::default_read_vectored(|b| self.read(b), bufs)
    }

    pub fn is_read_vectored(&self) -> bool {
        false
    }

    pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.write_shut.load(Relaxed) {
            return Err(SHUT_DOWN);
        }
        let handle = self.raw_handle();
        let written = if self.nonblocking.load(Relaxed) {
            syscall::write_nonblock(handle, buf)
        } else {
            match self.write_timeout_ms.load(Relaxed) {
                0 => syscall::write(handle, buf),
                ms => with_timeout(handle, WRITABLE, ms, || syscall::write_nonblock(handle, buf))
                    .ok_or(TIMED_OUT)?,
            }
        };
        written.map_err(|e| match e {
            SyscallError::Gone => RESET,
            e => syscall_err(e),
        })
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        crate::io::default_write_vectored(|b| self.write(b), bufs)
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.peer)
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), self.local_port)))
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        let how_val = match how {
            Shutdown::Read => 0u32,
            Shutdown::Write => 1,
            Shutdown::Both => 2,
        };
        toyos::net::tcp_shutdown(self.socket_id(), how_val).map_err(net_err_to_io)?;
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            self.read_shut.store(true, Relaxed);
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            self.write_shut.store(true, Relaxed);
        }
        Ok(())
    }

    pub fn duplicate(&self) -> io::Result<TcpStream> {
        let new_fd = syscall::dup(self.raw_handle()).map_err(syscall_err)?;
        Ok(TcpStream {
            fd: unsafe { OwnedFd::from_raw_fd(new_fd.0 as i32) },
            socket: Arc::clone(&self.socket),
            read_shut: Arc::clone(&self.read_shut),
            write_shut: Arc::clone(&self.write_shut),
            peer: self.peer,
            local_port: self.local_port,
            read_timeout_ms: AtomicU32::new(self.read_timeout_ms.load(Relaxed)),
            write_timeout_ms: AtomicU32::new(self.write_timeout_ms.load(Relaxed)),
            nodelay: AtomicBool::new(self.nodelay.load(Relaxed)),
            nonblocking: AtomicBool::new(self.nonblocking.load(Relaxed)),
        })
    }

    pub fn set_linger(&self, _linger: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        toyos::net::tcp_set_option(self.socket_id(), toyos::net::OPT_NODELAY, nodelay as u32)
            .map_err(net_err_to_io)?;
        self.nodelay.store(nodelay, Relaxed);
        Ok(())
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        Ok(self.nodelay.load(Relaxed))
    }

    pub fn set_keepalive(&self, _keepalive: bool) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "keepalive not supported"))
    }

    pub fn keepalive(&self) -> io::Result<bool> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "keepalive not supported"))
    }

    pub fn set_ttl(&self, _ttl: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn ttl(&self) -> io::Result<u32> {
        Ok(64)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Relaxed);
        Ok(())
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

// No Drop impl — Arc<NetstackSocket> handles close on last drop.
// OwnedFd drop closes the pipe-backed socket fd.

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TcpStream(fd={}, peer={})", self.fd.as_raw_fd(), self.peer)
    }
}

// --- TcpListener ---

pub struct TcpListener {
    notify_fd: OwnedFd,
    socket: Arc<NetstackSocket>,
    local: SocketAddr,
    nonblocking: AtomicBool,
}

impl TcpListener {
    fn socket_id(&self) -> TcpSocketId {
        match *self.socket {
            NetstackSocket::Tcp(id) => id,
            _ => unreachable!(),
        }
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;
        let (ip, port) = addr_to_v4(&addr)?;
        let bound = toyos::net::tcp_bind(ip, port).map_err(net_err_to_io)?;
        Ok(TcpListener {
            notify_fd: unsafe { OwnedFd::from_raw_fd(bound.notify.into_raw().0 as i32) },
            socket: Arc::new(NetstackSocket::Tcp(bound.socket_id)),
            local: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), bound.bound_port)),
            nonblocking: AtomicBool::new(false),
        })
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let mut byte = [0u8; 1];
        let notify_fd = RawHandle(self.notify_fd.as_raw_fd() as u32);
        if self.nonblocking.load(Relaxed) {
            syscall::read_nonblock(notify_fd, &mut byte).map_err(syscall_err)?;
        } else {
            syscall::read(notify_fd, &mut byte).map_err(syscall_err)?;
        }

        let accepted = toyos::net::tcp_accept(self.socket_id()).map_err(net_err_to_io)?;
        let fd = make_socket_fd(accepted.rx, accepted.tx)?;

        let peer = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::from(accepted.remote_addr),
            accepted.remote_port,
        ));
        Ok((TcpStream::new(fd, accepted.socket_id, peer, accepted.local_port), peer))
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        let new_fd =
            syscall::dup(RawHandle(self.notify_fd.as_raw_fd() as u32)).map_err(syscall_err)?;
        Ok(TcpListener {
            notify_fd: unsafe { OwnedFd::from_raw_fd(new_fd.0 as i32) },
            socket: Arc::clone(&self.socket),
            local: self.local,
            nonblocking: AtomicBool::new(self.nonblocking.load(Relaxed)),
        })
    }

    pub fn set_ttl(&self, _ttl: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn ttl(&self) -> io::Result<u32> {
        Ok(64)
    }

    pub fn set_only_v6(&self, _only_v6: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn only_v6(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Relaxed);
        Ok(())
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.notify_fd.as_fd()
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.notify_fd.as_raw_fd()
    }
}

// No Drop impl — Arc<NetstackSocket> handles close on last drop.

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TcpListener(fd={}, local={})", self.notify_fd.as_raw_fd(), self.local)
    }
}

// --- UdpSocket ---

pub struct UdpSocket {
    socket: Arc<NetstackSocket>,
    tx_fd: OwnedFd,
    rx_fd: OwnedFd,
    local: SocketAddr,
    peer: crate::sync::Mutex<Option<SocketAddr>>,
    /// What netstack holds for the socket every duplicate names.
    broadcast: Arc<AtomicBool>,
    read_timeout_ms: AtomicU32,
    write_timeout_ms: AtomicU32,
}

impl UdpSocket {
    fn socket_id(&self) -> UdpSocketId {
        match *self.socket {
            NetstackSocket::Udp(id) => id,
            _ => unreachable!(),
        }
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;
        let (ip, port) = addr_to_v4(&addr)?;
        let bound = toyos::net::udp_bind(ip, port).map_err(net_err_to_io)?;
        Ok(UdpSocket {
            socket: Arc::new(NetstackSocket::Udp(bound.socket_id)),
            tx_fd: unsafe { OwnedFd::from_raw_fd(bound.tx.into_raw().0 as i32) },
            rx_fd: unsafe { OwnedFd::from_raw_fd(bound.rx.into_raw().0 as i32) },
            local: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), bound.bound_port)),
            peer: crate::sync::Mutex::new(None),
            broadcast: Arc::new(AtomicBool::new(false)),
            read_timeout_ms: AtomicU32::new(0),
            write_timeout_ms: AtomicU32::new(0),
        })
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.peer
            .lock()
            .unwrap()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "not connected"))
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let resp =
            toyos::net::udp_recv_from(self.socket_id(), buf.len() as u32).map_err(net_err_to_io)?;

        let n = (resp.len as usize).min(buf.len());
        if n > 0 {
            let rx_fd = RawHandle(self.rx_fd.as_raw_fd() as u32);
            syscall::read(rx_fd, &mut buf[..n]).map_err(syscall_err)?;
        }

        let addr = Ipv4Addr::new(resp.addr[0], resp.addr[1], resp.addr[2], resp.addr[3]);
        Ok((n, SocketAddr::V4(SocketAddrV4::new(addr, resp.port))))
    }

    pub fn peek_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "peek not supported"))
    }

    pub fn send_to(&self, buf: &[u8], addr: &SocketAddr) -> io::Result<usize> {
        let (ip, port) = addr_to_v4(addr)?;

        // Write data to TX pipe first, then send control message
        let tx_fd = RawHandle(self.tx_fd.as_raw_fd() as u32);
        if !buf.is_empty() {
            syscall::write(tx_fd, buf).map_err(syscall_err)?;
        }

        let sent = toyos::net::udp_send_to(self.socket_id(), ip, port, buf.len() as u16)
            .map_err(net_err_to_io)?;
        Ok(sent as usize)
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        let new_tx_fd =
            syscall::dup(RawHandle(self.tx_fd.as_raw_fd() as u32)).map_err(syscall_err)?;
        let new_rx_fd =
            syscall::dup(RawHandle(self.rx_fd.as_raw_fd() as u32)).map_err(syscall_err)?;
        Ok(UdpSocket {
            socket: Arc::clone(&self.socket),
            tx_fd: unsafe { OwnedFd::from_raw_fd(new_tx_fd.0 as i32) },
            rx_fd: unsafe { OwnedFd::from_raw_fd(new_rx_fd.0 as i32) },
            local: self.local,
            peer: crate::sync::Mutex::new(*self.peer.lock().unwrap()),
            broadcast: Arc::clone(&self.broadcast),
            read_timeout_ms: AtomicU32::new(self.read_timeout_ms.load(Relaxed)),
            write_timeout_ms: AtomicU32::new(self.write_timeout_ms.load(Relaxed)),
        })
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.read_timeout_ms.store(duration_to_ms(dur), Relaxed);
        Ok(())
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.write_timeout_ms.store(duration_to_ms(dur), Relaxed);
        Ok(())
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        let ms = self.read_timeout_ms.load(Relaxed);
        Ok(if ms == 0 { None } else { Some(Duration::from_millis(ms as u64)) })
    }

    pub fn write_timeout(&self) -> io::Result<Option<Duration>> {
        let ms = self.write_timeout_ms.load(Relaxed);
        Ok(if ms == 0 { None } else { Some(Duration::from_millis(ms as u64)) })
    }

    pub fn set_broadcast(&self, broadcast: bool) -> io::Result<()> {
        toyos::net::udp_set_option(self.socket_id(), toyos::net::OPT_BROADCAST, broadcast as u32)
            .map_err(net_err_to_io)?;
        self.broadcast.store(broadcast, Relaxed);
        Ok(())
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        Ok(self.broadcast.load(Relaxed))
    }

    pub fn set_multicast_loop_v4(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn multicast_loop_v4(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn set_multicast_ttl_v4(&self, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn multicast_ttl_v4(&self) -> io::Result<u32> {
        Ok(1)
    }

    pub fn set_multicast_loop_v6(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn multicast_loop_v6(&self) -> io::Result<bool> {
        Ok(false)
    }

    pub fn join_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "multicast not supported"))
    }

    pub fn join_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "multicast not supported"))
    }

    pub fn leave_multicast_v4(&self, _: &Ipv4Addr, _: &Ipv4Addr) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "multicast not supported"))
    }

    pub fn leave_multicast_v6(&self, _: &Ipv6Addr, _: u32) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "multicast not supported"))
    }

    pub fn set_ttl(&self, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub fn ttl(&self) -> io::Result<u32> {
        Ok(64)
    }

    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        Ok(None)
    }

    pub fn set_nonblocking(&self, _: bool) -> io::Result<()> {
        Ok(())
    }

    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let (n, _) = self.recv_from(buf)?;
        Ok(n)
    }

    pub fn peek(&self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "peek not supported"))
    }

    pub fn send(&self, buf: &[u8]) -> io::Result<usize> {
        let peer = self
            .peer
            .lock()
            .unwrap()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "not connected"))?;
        self.send_to(buf, &peer)
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;
        *self.peer.lock().unwrap() = Some(addr);
        Ok(())
    }
}

// No Drop impl — Arc<NetstackSocket> handles close on last drop.
// OwnedFd drops close the pipe fds.

impl fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UdpSocket(local={})", self.local)
    }
}

// --- LookupHost (DNS) ---

pub struct LookupHost {
    addrs: Vec<SocketAddr>,
    pos: usize,
}

impl Iterator for LookupHost {
    type Item = SocketAddr;
    fn next(&mut self) -> Option<SocketAddr> {
        if self.pos < self.addrs.len() {
            let addr = self.addrs[self.pos];
            self.pos += 1;
            Some(addr)
        } else {
            None
        }
    }
}

pub fn lookup_host(host: &str, port: u16) -> io::Result<LookupHost> {
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return Ok(LookupHost { addrs: vec![SocketAddr::V4(SocketAddrV4::new(ip, port))], pos: 0 });
    }

    let mut results = [[0u8; 4]; 16];
    let count = toyos::net::dns_lookup(host, &mut results).map_err(net_err_to_io)?;

    if count == 0 {
        return Err(io::Error::new(io::ErrorKind::Other, "DNS lookup failed: no results"));
    }

    let addrs = results[..count]
        .iter()
        .map(|ip| SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(*ip), port)))
        .collect();

    Ok(LookupHost { addrs, pos: 0 })
}
