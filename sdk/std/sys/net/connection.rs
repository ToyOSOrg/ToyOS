use toyos::net::stream::{self, Refused, Wait};
use toyos::net::{NetError, TcpSocketId, UdpSocketId};
use toyos_abi::RawHandle;
use toyos_abi::syscall::{self, SyscallError};

use crate::fmt;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, ToSocketAddrs};
use crate::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use crate::sync::Arc;
use crate::sync::atomic::Ordering::Relaxed;
use crate::sync::atomic::{AtomicBool, AtomicU32};
use crate::time::Duration;

// --- Helpers ---

/// A stream's handle this process just got, as an owned descriptor.
fn stream_fd(handle: RawHandle) -> io::Result<OwnedFd> {
    let fd = toyos::fd::install(stream::descriptor(handle), false, 0).map_err(crate::sys::fd_error)?;
    // SAFETY: a descriptor just installed for this stream alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A kernel handle this process just got, as an owned descriptor.
fn owned(handle: RawHandle) -> io::Result<OwnedFd> {
    let fd = crate::sys::new_fd(handle)?;
    // SAFETY: a descriptor just installed for this handle alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The kernel handle an owned descriptor names. Its owner alone closes it,
/// so one gone is another owner having closed it, against I/O safety.
fn kernel(fd: &OwnedFd) -> RawHandle {
    toyos::fd::with(fd.as_raw_fd(), |o| o.handle).expect("std::net: an owned descriptor was closed by another owner")
}

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
        NetError::ResourceExhausted | NetError::Protocol(_) | NetError::Io => io::ErrorKind::Other,
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

const TIMED_OUT: io::Error = io::const_error!(io::ErrorKind::TimedOut, "timed out");

const SHUT_DOWN: io::Error =
    io::const_error!(io::ErrorKind::BrokenPipe, "the stream was shut down for writing");

/// What std answers for a stream's refusal. netstack gone is
/// `ConnectionAborted`: neither the peer's reset nor its FIN.
pub fn stream_refused(refusal: Refused) -> io::Error {
    match refusal {
        Refused::NotThisProcess(handle) => {
            io::Error::new(io::ErrorKind::InvalidInput, stream::Misuse::NotThisProcess(handle).to_string())
        }
        Refused::WouldBlock => io::ErrorKind::WouldBlock.into(),
        Refused::TimedOut => TIMED_OUT,
        Refused::Reset => io::const_error!(io::ErrorKind::ConnectionReset, "connection reset"),
        Refused::ConnectionTimedOut => io::const_error!(io::ErrorKind::TimedOut, "the connection timed out"),
        Refused::Unreachable => io::const_error!(io::ErrorKind::HostUnreachable, "the peer is unreachable"),
        Refused::Aborted => io::const_error!(io::ErrorKind::ConnectionAborted, "netstack ended the stream"),
        Refused::Shut => SHUT_DOWN,
    }
}

/// How a call waits: not at all on a nonblocking stream, and otherwise until
/// its direction is ready or `timeout_ms`, 0 for no timeout, passes.
fn wait(nonblocking: bool, timeout_ms: u32) -> Wait {
    match (nonblocking, timeout_ms) {
        (true, _) => Wait::Nonblocking,
        (false, 0) => Wait::Blocking(None),
        (false, ms) => {
            let now = toyos_abi::clock::nanos_since_boot();
            Wait::Blocking(Some(now.saturating_add(u64::from(ms) * 1_000_000)))
        }
    }
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

/// A stream's handle, which `toyos::net::stream` holds its one end by: every
/// handle of it in this process shares the region, its cursors and its
/// waiters. The handle's drop lets the end go (`OwnedFd`'s ToyOS arm).
pub struct TcpStream {
    fd: OwnedFd,
    read_timeout_ms: AtomicU32,
    write_timeout_ms: AtomicU32,
    nonblocking: AtomicBool,
}

impl TcpStream {
    /// A stream on `fd`, a handle `toyos::net::stream` holds an end for.
    pub(crate) fn from_fd(fd: OwnedFd) -> TcpStream {
        TcpStream {
            fd,
            read_timeout_ms: AtomicU32::new(0),
            write_timeout_ms: AtomicU32::new(0),
            nonblocking: AtomicBool::new(false),
        }
    }

    pub(crate) fn into_fd(self) -> OwnedFd {
        self.fd
    }

    pub fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<TcpStream> {
        super::each_addr(addr, |addr| Self::connect_timeout(addr, CONNECT_TIMEOUT))
    }

    pub fn connect_timeout(addr: &SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
        let (ip, port) = addr_to_v4(addr)?;
        let connected = stream::connect(ip, port, duration_to_ms(Some(timeout)), false).map_err(net_err_to_io)?;
        Ok(TcpStream::from_fd(stream_fd(connected.handle)?))
    }

    fn handle(&self) -> RawHandle {
        kernel(&self.fd)
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
        let wait = wait(self.nonblocking.load(Relaxed), self.read_timeout_ms.load(Relaxed));
        stream::read(self.handle(), buf, wait).map_err(stream_refused)
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
        let wait = wait(self.nonblocking.load(Relaxed), self.write_timeout_ms.load(Relaxed));
        stream::write(self.handle(), buf, wait).map_err(stream_refused)
    }

    pub fn write_vectored(&self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        crate::io::default_write_vectored(|b| self.write(b), bufs)
    }

    pub fn is_write_vectored(&self) -> bool {
        false
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        let ((addr, port), _) = stream::endpoints(self.handle()).map_err(stream_refused)?;
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(addr), port)))
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        let (_, local_port) = stream::endpoints(self.handle()).map_err(stream_refused)?;
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), local_port)))
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        if matches!(how, Shutdown::Read | Shutdown::Both) {
            stream::shutdown_read(self.handle()).map_err(stream_refused)?;
        }
        if matches!(how, Shutdown::Write | Shutdown::Both) {
            stream::shutdown_write(self.handle()).map_err(stream_refused)?;
        }
        Ok(())
    }

    /// A second descriptor on the same stream's end.
    pub fn duplicate(&self) -> io::Result<TcpStream> {
        let duplicate = TcpStream {
            fd: self.fd.try_clone()?,
            read_timeout_ms: AtomicU32::new(self.read_timeout_ms.load(Relaxed)),
            write_timeout_ms: AtomicU32::new(self.write_timeout_ms.load(Relaxed)),
            nonblocking: AtomicBool::new(self.nonblocking.load(Relaxed)),
        };
        Ok(duplicate)
    }

    pub fn set_linger(&self, _linger: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    pub fn linger(&self) -> io::Result<Option<Duration>> {
        Ok(None)
    }

    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        stream::set_nodelay(self.handle(), nodelay).map_err(stream_refused)
    }

    pub fn nodelay(&self) -> io::Result<bool> {
        stream::nodelay(self.handle()).map_err(stream_refused)
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

    /// The failure that ended the stream: the receive ring's `end`.
    pub fn take_error(&self) -> io::Result<Option<io::Error>> {
        let failure = stream::failure(self.handle()).map_err(stream_refused)?;
        Ok(failure.map(stream_refused))
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

    /// The stream's send bell: the handle a watch for room is made on, for
    /// reading.
    pub fn bell(&self) -> io::Result<u32> {
        let bell = stream::doorbell(self.handle(), stream::Way::Send).map_err(stream_refused)?;
        Ok(bell.0)
    }

    /// The stream's receive bell: the handle a watch for bytes is made on,
    /// for reading.
    pub fn receive_bell(&self) -> io::Result<u32> {
        let bell = stream::doorbell(self.handle(), stream::Way::Receive).map_err(stream_refused)?;
        Ok(bell.0)
    }

    /// Registered for readiness: each named way's want armed, and netstack rung
    /// where one is ready, so the first event comes.
    pub fn register(&self, readable: bool, writable: bool) -> io::Result<()> {
        stream::register(self.handle(), readable, writable).map_err(stream_refused)
    }
}

impl fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TcpStream(fd={})", self.fd.as_raw_fd())
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
            notify_fd: owned(bound.notify.into_raw())?,
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
        let notify_fd = kernel(&self.notify_fd);
        if self.nonblocking.load(Relaxed) {
            syscall::read_nonblock(notify_fd, &mut byte).map_err(syscall_err)?;
        } else {
            syscall::read(notify_fd, &mut byte).map_err(syscall_err)?;
        }

        let accepted = stream::accept(self.socket_id()).map_err(net_err_to_io)?;
        let peer = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::from(accepted.remote_addr),
            accepted.remote_port,
        ));
        Ok((TcpStream::from_fd(stream_fd(accepted.handle)?), peer))
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        Ok(TcpListener {
            notify_fd: self.notify_fd.try_clone()?,
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
    /// What netstack holds for the socket every duplicate names, locked across
    /// the request that changes it so the two cannot come to differ.
    broadcast: Arc<crate::sync::Mutex<bool>>,
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
            tx_fd: owned(bound.tx.into_raw())?,
            rx_fd: owned(bound.rx.into_raw())?,
            local: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(ip), bound.bound_port)),
            peer: crate::sync::Mutex::new(None),
            broadcast: Arc::new(crate::sync::Mutex::new(false)),
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
            let rx_fd = kernel(&self.rx_fd);
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
        let tx_fd = kernel(&self.tx_fd);
        if !buf.is_empty() {
            syscall::write(tx_fd, buf).map_err(syscall_err)?;
        }

        let sent = toyos::net::udp_send_to(self.socket_id(), ip, port, buf.len() as u16)
            .map_err(net_err_to_io)?;
        Ok(sent as usize)
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        Ok(UdpSocket {
            socket: Arc::clone(&self.socket),
            tx_fd: self.tx_fd.try_clone()?,
            rx_fd: self.rx_fd.try_clone()?,
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
        let mut held = self.broadcast.lock().unwrap();
        toyos::net::udp_set_option(self.socket_id(), toyos::net::OPT_BROADCAST, broadcast as u32)
            .map_err(net_err_to_io)?;
        *held = broadcast;
        Ok(())
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        Ok(*self.broadcast.lock().unwrap())
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
