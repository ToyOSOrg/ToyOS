use toyos::net::socket;
use toyos::net::stream::{self, Refused, Wait};
use toyos::net::NetError;

use crate::fmt;
use crate::io::{self, BorrowedCursor, IoSlice, IoSliceMut};
use crate::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, ToSocketAddrs};
use crate::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use crate::sync::atomic::Ordering::Relaxed;
use crate::sync::atomic::{AtomicBool, AtomicU32};
use crate::time::Duration;

// --- Helpers ---

/// A stream's handle this process just got, as an owned descriptor.
fn stream_fd(handle: toyos::OwnedHandle) -> io::Result<OwnedFd> {
    let fd = toyos::fd::install(stream::descriptor(handle), false, 0).map_err(crate::sys::fd_error)?;
    // SAFETY: a descriptor just installed for this stream alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A listener's or a datagram socket's handle this process just got, as an
/// owned descriptor.
fn socket_fd(handle: toyos::OwnedHandle) -> io::Result<OwnedFd> {
    let fd = toyos::fd::install(socket::descriptor(handle), false, 0).map_err(crate::sys::fd_error)?;
    // SAFETY: a descriptor just installed for this socket alone.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The kernel handle an owned descriptor names. Its owner alone closes it,
/// so one gone is another owner having closed it, against I/O safety.
fn kernel(fd: &OwnedFd) -> toyos::BorrowedHandle<'_> {
    let raw = toyos::fd::with(fd.as_raw_fd(), |o| o.handle().map(|h| h.raw()))
        .ok()
        .flatten()
        .expect("std::net: an owned descriptor was closed by another owner");
    // SAFETY: `fd` owns the descriptor, which names the handle until it closes.
    unsafe { toyos::BorrowedHandle::borrow_raw(raw) }
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
        NetError::WouldBlock => return io::ErrorKind::WouldBlock.into(),
        NetError::NotASocket => io::ErrorKind::InvalidInput,
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

/// A timeout as the protocol's milliseconds, where 0 is no bound: a zero
/// timeout is refused, as std's contract has it, and one under a millisecond
/// waits one, not forever.
fn timeout_ms(d: Option<Duration>) -> io::Result<u32> {
    match d {
        Some(Duration::ZERO) => Err(io::Error::ZERO_TIMEOUT),
        Some(d) => Ok(d.as_millis().clamp(1, u32::MAX as u128) as u32),
        None => Ok(0),
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
        Refused::AddressGone => io::const_error!(io::ErrorKind::AddrNotAvailable, "this machine gave up the connection's address"),
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
        let connected = stream::connect(ip, port, timeout_ms(Some(timeout))?, false).map_err(net_err_to_io)?;
        Ok(TcpStream::from_fd(stream_fd(connected.handle)?))
    }

    fn handle(&self) -> toyos::BorrowedHandle<'_> {
        kernel(&self.fd)
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.read_timeout_ms.store(timeout_ms(dur)?, Relaxed);
        Ok(())
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.write_timeout_ms.store(timeout_ms(dur)?, Relaxed);
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
        let (_, (addr, port)) = stream::endpoints(self.handle()).map_err(stream_refused)?;
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(addr), port)))
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
        Ok(bell.raw().0)
    }

    /// The stream's receive bell: the handle a watch for bytes is made on,
    /// for reading.
    pub fn receive_bell(&self) -> io::Result<u32> {
        let bell = stream::doorbell(self.handle(), stream::Way::Receive).map_err(stream_refused)?;
        Ok(bell.raw().0)
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

/// A listener's handle, which `toyos::net::socket` holds this process's holder
/// of the listener by: every descriptor of it shares the holder. The handle's
/// drop lets the holder go, and the listener goes with its last holder.
pub struct TcpListener {
    fd: OwnedFd,
    nonblocking: AtomicBool,
}

impl TcpListener {
    /// A listener on `fd`, a handle `toyos::net::socket` holds a listener's end for.
    pub(crate) fn from_fd(fd: OwnedFd) -> TcpListener {
        TcpListener { fd, nonblocking: AtomicBool::new(false) }
    }

    pub(crate) fn into_fd(self) -> OwnedFd {
        self.fd
    }

    fn handle(&self) -> toyos::BorrowedHandle<'_> {
        kernel(&self.fd)
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<TcpListener> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;
        let (ip, port) = addr_to_v4(&addr)?;
        let bound = socket::listen(ip, port, toyos::net::TcpOptions::new(false)).map_err(net_err_to_io)?;
        Ok(TcpListener::from_fd(socket_fd(bound.handle)?))
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        let (addr, port) = socket::local(self.handle()).map_err(net_err_to_io)?;
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(addr), port)))
    }

    pub fn accept(&self) -> io::Result<(TcpStream, SocketAddr)> {
        let wait = if self.nonblocking.load(Relaxed) { Wait::Nonblocking } else { Wait::Blocking(None) };
        let accepted = socket::accept(self.handle(), wait).map_err(net_err_to_io)?;
        let peer = SocketAddr::V4(SocketAddrV4::new(
            Ipv4Addr::from(accepted.remote_addr),
            accepted.remote_port,
        ));
        Ok((TcpStream::from_fd(stream_fd(accepted.handle)?), peer))
    }

    pub fn duplicate(&self) -> io::Result<TcpListener> {
        Ok(TcpListener { fd: self.fd.try_clone()?, nonblocking: AtomicBool::new(self.nonblocking.load(Relaxed)) })
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
        self.fd.as_fd()
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

impl fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TcpListener(fd={})", self.fd.as_raw_fd())
    }
}

// --- UdpSocket ---

/// A datagram socket's handle, as [`TcpListener`]'s is a listener's. Its peer,
/// once connected, is netstack's, shared by every holder.
pub struct UdpSocket {
    fd: OwnedFd,
    read_timeout_ms: AtomicU32,
    /// Kept for its getter: a send is not bounded by it (`SO_SNDTIMEO`).
    write_timeout_ms: AtomicU32,
    nonblocking: AtomicBool,
}

impl UdpSocket {
    /// A datagram socket on `fd`, a handle `toyos::net::socket` holds a
    /// datagram socket's end for.
    pub(crate) fn from_fd(fd: OwnedFd) -> UdpSocket {
        UdpSocket { fd, read_timeout_ms: AtomicU32::new(0), write_timeout_ms: AtomicU32::new(0), nonblocking: AtomicBool::new(false) }
    }

    pub(crate) fn into_fd(self) -> OwnedFd {
        self.fd
    }

    fn handle(&self) -> toyos::BorrowedHandle<'_> {
        kernel(&self.fd)
    }

    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<UdpSocket> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;
        let (ip, port) = addr_to_v4(&addr)?;
        let bound = socket::bind(ip, port).map_err(net_err_to_io)?;
        Ok(UdpSocket::from_fd(socket_fd(bound.handle)?))
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match socket::peer(self.handle()).map_err(net_err_to_io)? {
            Some((addr, port)) => Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(addr), port))),
            None => Err(io::Error::new(io::ErrorKind::NotConnected, "not connected")),
        }
    }

    pub fn socket_addr(&self) -> io::Result<SocketAddr> {
        let (addr, port) = socket::local(self.handle()).map_err(net_err_to_io)?;
        Ok(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(addr), port)))
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let wait = wait(self.nonblocking.load(Relaxed), self.read_timeout_ms.load(Relaxed));
        let received = socket::recv_from(self.handle(), buf, wait).map_err(|e| match e {
            NetError::TimedOut if wait != Wait::Nonblocking => io::ErrorKind::WouldBlock.into(),
            e => net_err_to_io(e),
        })?;
        Ok((received.len, SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(received.addr), received.port))))
    }

    pub fn peek_from(&self, _buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "peek not supported"))
    }

    pub fn send_to(&self, buf: &[u8], addr: &SocketAddr) -> io::Result<usize> {
        let (ip, port) = addr_to_v4(addr)?;
        socket::send_to(self.handle(), ip, port, buf).map_err(net_err_to_io)
    }

    pub fn duplicate(&self) -> io::Result<UdpSocket> {
        Ok(UdpSocket {
            fd: self.fd.try_clone()?,
            read_timeout_ms: AtomicU32::new(self.read_timeout_ms.load(Relaxed)),
            write_timeout_ms: AtomicU32::new(self.write_timeout_ms.load(Relaxed)),
            nonblocking: AtomicBool::new(self.nonblocking.load(Relaxed)),
        })
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.read_timeout_ms.store(timeout_ms(dur)?, Relaxed);
        Ok(())
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        self.write_timeout_ms.store(timeout_ms(dur)?, Relaxed);
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
        socket::set_option(self.handle(), toyos::net::OPT_BROADCAST, broadcast as u32).map_err(net_err_to_io)
    }

    pub fn broadcast(&self) -> io::Result<bool> {
        socket::option(self.handle(), toyos::net::OPT_BROADCAST).map(|on| on != 0).map_err(net_err_to_io)
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

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.nonblocking.store(nonblocking, Relaxed);
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
        socket::send(self.handle(), buf).map_err(net_err_to_io)
    }

    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let addr = addr
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no addresses found"))?;
        let (ip, port) = addr_to_v4(&addr)?;
        socket::connect(self.handle(), ip, port).map_err(net_err_to_io)
    }

    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

impl fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UdpSocket(fd={})", self.fd.as_raw_fd())
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
