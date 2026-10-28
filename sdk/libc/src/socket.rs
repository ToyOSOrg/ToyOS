// BSD sockets over netstack: a TCP stream is `toyos::net::stream`'s region
// and connection, and a listener and a datagram socket `toyos::net::socket`'s
// holder. A socket is an entry of this library's until it binds or connects;
// from then its descriptor names netstack's object as std's does, so a spawn
// passes it as a holder of its own (`toyos::fd::copy`).

use alloc::alloc::{alloc as heap_alloc, dealloc as heap_dealloc};
use alloc::vec::Vec;
use core::ptr;
use toyos::net::socket::{self, Kind};
use toyos::net::stream::{self, Wait};
use toyos::net::{NetError, TcpOptions, OPT_BROADCAST, OPT_NODELAY};

use crate::errcode::{
    EACCES, EADDRINUSE, EAFNOSUPPORT, EAGAIN, EBADF, ECONNREFUSED, ECONNRESET, EFAULT, EINVAL, EIO, EISCONN, ENOMEM,
    ENOPROTOOPT, ENOSPC, ENOTCONN, ETIMEDOUT,
};
use crate::inaddr::{self, SockaddrIn, AF_INET};
use crate::dgramreq;
use crate::sockopt::{self, Kept};

// C types matching POSIX

type SocklenT = u32;

#[repr(C)]
pub struct Sockaddr {
    sa_family: u16,
    sa_data: [u8; 14],
}

#[repr(C)]
pub struct Addrinfo {
    ai_flags: i32,
    ai_family: i32,
    ai_socktype: i32,
    ai_protocol: i32,
    ai_addrlen: SocklenT,
    ai_addr: *mut Sockaddr,
    ai_canonname: *mut u8,
    ai_next: *mut Addrinfo,
}

const AF_UNSPEC: i32 = 0;
const SOCK_STREAM: i32 = 1;
const SOCK_DGRAM: i32 = 2;

// Internal socket table

#[derive(Clone, Copy)]
enum SocketKind {
    Tcp,
    Udp,
}

/// A socket netstack does not hold yet: what its bind or connect makes it
/// with. A listener's `nodelay` goes in its bind's request, a datagram
/// socket's `broadcast` is handed over after its bind, and a stream's
/// `nodelay` into its region before its connect.
#[derive(Clone, Copy)]
struct SocketEntry {
    kind: SocketKind,
    nodelay: bool,
    broadcast: bool,
}

const MAX_SOCKETS: usize = 128;

/// A socket's descriptor names an object of this library's own
/// (`toyos::fd::Object::opaque`), whose word is this tag, `libc`, beside the
/// socket's index here, its upper half unlike socket2's.
const SOCKET_TAG: u64 = 0x6c69_6263 << 32;

static mut SOCKETS: [Option<SocketEntry>; MAX_SOCKETS] = [None; MAX_SOCKETS];

/// The socket descriptor `fd` names, held for the call: a `close` on another
/// thread meanwhile lets the socket go only once this drops, so a call never
/// meets a handle closed under it. `None` for a descriptor that is no socket.
fn sock_from_fd(fd: i32) -> Option<Sock> {
    let hold = toyos::fd::hold(fd).ok()?;
    let object = hold.object();
    match (object.tag(), object.handle()) {
        (toyos::fd::STREAM, Some(_)) => Some(Sock::Stream(Held(hold))),
        (toyos::fd::SOCKET, Some(handle)) => match socket::kind(handle) {
            Some(Kind::Listener) => Some(Sock::Listener(Held(hold))),
            Some(Kind::Datagram) => Some(Sock::Datagram(Held(hold))),
            // Another copy of `toyos`'s: this library holds no end of it.
            None => None,
        },
        _ => {
            let idx = index_of(&object)?;
            Some(Sock::Entry(Socket { _hold: hold, idx }))
        }
    }
}

/// What a socket descriptor names, held.
enum Sock {
    /// A socket netstack does not hold yet: this library's entry.
    Entry(Socket),
    /// A stream, whose descriptor names its handle.
    Stream(Held),
    /// A listener, whose descriptor names its holder's.
    Listener(Held),
    /// A datagram socket, likewise.
    Datagram(Held),
}

/// A descriptor of netstack's object, held for the call.
struct Held(toyos::fd::Hold);

impl Held {
    /// The stream's or the socket's handle, lent for the hold.
    fn handle(&self) -> toyos::BorrowedHandle<'_> {
        self.0.object().handle().expect("a stream's or a socket's descriptor names its handle")
    }
}

/// The index of the socket `object` is, or `None` for an object that is no
/// socket of this library's.
fn index_of(object: &toyos::fd::Lent<'_>) -> Option<usize> {
    if object.handle().is_some() || object.word() & !0xffff_ffff != SOCKET_TAG {
        return None;
    }
    usize::try_from(object.word() & 0xffff_ffff).ok().filter(|&i| i < MAX_SOCKETS)
}

/// A socket descriptor's entry, for as long as its descriptor is held.
struct Socket {
    _hold: toyos::fd::Hold,
    idx: usize,
}

impl Socket {
    /// The entry, which lives as long as the hold.
    ///
    /// Nothing makes the `&mut` exclusive: two calls on one socket each make
    /// one (`issues/concurrent-libc-calls-on-one-socket-alias-its-entry.md`).
    fn slot(&mut self) -> &mut Option<SocketEntry> {
        // SAFETY: the hold keeps `release_socket` from taking the entry for as
        // long as the borrow lives; exclusivity is the defect above.
        unsafe { &mut *core::ptr::addr_of_mut!(SOCKETS[self.idx]) }
    }
}

/// `entry` as a socket descriptor, the lowest free and inherited, or `-1`
/// with `errno` set.
fn alloc_socket(entry: SocketEntry) -> i32 {
    let Some(idx) = (0..MAX_SOCKETS).find(|&i| unsafe { SOCKETS[i].is_none() }) else {
        set_errno(ENOMEM);
        return -1;
    };
    unsafe { SOCKETS[idx] = Some(entry) };
    match toyos::fd::install(toyos::fd::Object::opaque(SOCKET_TAG | idx as u64, release_socket), true, 0) {
        Ok(fd) => fd,
        // The table let the object go, and with it the socket.
        Err(_) => {
            set_errno(crate::errcode::EMFILE);
            -1
        }
    }
}

/// A socket no descriptor names any more: its entry goes.
extern "C" fn release_socket(word: u64) {
    let idx = (word & 0xffff_ffff) as usize;
    if idx < MAX_SOCKETS {
        unsafe { SOCKETS[idx] = None };
    }
}

use crate::errno::set as set_errno;

// netstack error conversion

fn net_err_to_errno(e: NetError) -> i32 {
    match e {
        NetError::ConnectionRefused => ECONNREFUSED,
        NetError::ConnectionReset => ECONNRESET,
        NetError::TimedOut => ETIMEDOUT,
        NetError::AddrInUse => EADDRINUSE,
        NetError::NotConnected => ENOTCONN,
        NetError::InvalidInput => EINVAL,
        NetError::PermissionDenied => EACCES,
        NetError::WouldBlock => EAGAIN,
        NetError::NotASocket => EBADF,
        NetError::NetstackNotFound | NetError::ResourceExhausted | NetError::Protocol(_) | NetError::Io => EIO,
    }
}

/// Parse sockaddr_in into (ipv4_octets, port).
unsafe fn parse_sockaddr(addr: *const Sockaddr, len: SocklenT) -> Option<([u8; 4], u16)> {
    if addr.is_null() || (len as usize) < core::mem::size_of::<SockaddrIn>() {
        return None;
    }
    (*(addr as *const SockaddrIn)).endpoint()
}

/// Fill a sockaddr_in from ip + port.
unsafe fn fill_sockaddr(addr: *mut Sockaddr, addrlen: *mut SocklenT, ip: [u8; 4], port: u16) {
    if addr.is_null() || addrlen.is_null() {
        return;
    }
    SockaddrIn::new(ip, port).answer(addr as *mut u8, addrlen);
}

/// Binds the datagram socket descriptor `fd` names, whose entry is `entry`,
/// handing over what was set before netstack held it and connecting it to
/// `peer`: from here `fd` names netstack's socket, and the entry goes once the
/// caller's hold does.
fn bind_datagram(fd: i32, entry: &SocketEntry, ip: [u8; 4], port: u16, peer: Option<([u8; 4], u16)>) -> Result<toyos::fd::Hold, i32> {
    let bound = socket::bind(ip, port).map_err(net_err_to_errno)?;
    let handle = toyos::AsHandle::as_handle(&bound.handle);
    let handed = (|| {
        if entry.broadcast {
            socket::set_option(handle, OPT_BROADCAST, 1)?;
        }
        match peer {
            Some((addr, port)) => socket::connect(handle, addr, port),
            None => Ok(()),
        }
    })();
    if let Err(e) = handed {
        socket::let_go(bound.handle);
        return Err(net_err_to_errno(e));
    }
    // SAFETY: `fd` names a socket entry, libc's own object, which its
    // netstack socket replaces at the number C keeps using.
    unsafe { toyos::fd::replace(fd, socket::descriptor(bound.handle)) }.map_err(|_| EBADF)?;
    toyos::fd::hold(fd).map_err(|_| EBADF)
}

/// A datagram socket's sends: one netstack holds, or one it does not yet,
/// which its first send binds.
struct Sending<'a> {
    fd: i32,
    entry: Option<&'a SocketEntry>,
    /// The socket's descriptor, held for the sends.
    bound: Option<toyos::fd::Hold>,
}

impl dgramreq::Datagrams for Sending<'_> {
    type Error = i32;
    fn held(&self) -> bool {
        self.bound.is_some()
    }
    fn bind(&mut self, ip: [u8; 4], port: u16) -> Result<(), i32> {
        let entry = self.entry.ok_or(EBADF)?;
        self.bound = Some(bind_datagram(self.fd, entry, ip, port, None)?);
        Ok(())
    }
    fn send(&mut self, data: &[u8], ip: [u8; 4], port: u16) -> Result<usize, i32> {
        let held = self.bound.as_ref().ok_or(EBADF)?;
        let handle = held.object().handle().ok_or(EBADF)?;
        socket::send_to(handle, ip, port, data).map_err(net_err_to_errno)
    }
}

/// `data` to `ip`:`port` from the datagram socket `sending` is.
fn send_datagram(mut sending: Sending<'_>, data: &[u8], ip: [u8; 4], port: u16) -> isize {
    match dgramreq::send_to(&mut sending, data, ip, port) {
        Ok(sent) => sent as isize,
        Err(errno) => { set_errno(errno); -1 }
    }
}

// BSD socket API

#[no_mangle]
pub unsafe extern "C" fn socket(domain: i32, sock_type: i32, _protocol: i32) -> i32 {
    if domain != AF_INET && domain != AF_UNSPEC {
        set_errno(EAFNOSUPPORT);
        return -1;
    }
    let kind = match sock_type & 0xf {
        SOCK_STREAM => SocketKind::Tcp,
        SOCK_DGRAM => SocketKind::Udp,
        _ => {
            set_errno(EINVAL);
            return -1;
        }
    };
    alloc_socket(SocketEntry { kind, nodelay: false, broadcast: false })
}

#[no_mangle]
pub unsafe extern "C" fn connect(fd: i32, addr: *const Sockaddr, addrlen: SocklenT) -> i32 {
    let (ip, port) = match parse_sockaddr(addr, addrlen) {
        Some(v) => v,
        None => { set_errno(EINVAL); return -1; }
    };
    let mut socket = match sock_from_fd(fd) {
        Some(Sock::Entry(socket)) => socket,
        Some(Sock::Datagram(held)) => {
            return match socket::connect(held.handle(), ip, port) {
                Ok(()) => 0,
                Err(e) => { set_errno(net_err_to_errno(e)); -1 }
            };
        }
        Some(Sock::Stream(_) | Sock::Listener(_)) => { set_errno(EISCONN); return -1; }
        None => { set_errno(EBADF); return -1; }
    };
    let slot = socket.slot();
    let entry = match slot.as_mut() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            // Nagle's algorithm off from the first segment where it was set before.
            let connected = match stream::connect(ip, port, 30000, entry.nodelay) {
                Ok(c) => c,
                Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
            };
            // The descriptor names the stream from here on; the entry goes once
            // this call's hold does.
            // SAFETY: `fd` names a socket entry, libc's own object, which its
            // stream replaces at the number C keeps using.
            match unsafe { toyos::fd::replace(fd, stream::descriptor(connected.handle)) } {
                Ok(()) => 0,
                Err(_) => { set_errno(EBADF); -1 }
            }
        }
        // A connect binds a socket not yet bound, at any address to a port the
        // stack chooses, as a first send does.
        SocketKind::Udp => match bind_datagram(fd, entry, [0; 4], 0, Some((ip, port))) {
            Ok(_) => 0,
            Err(errno) => { set_errno(errno); -1 }
        },
    }
}

#[no_mangle]
pub unsafe extern "C" fn bind(fd: i32, addr: *const Sockaddr, addrlen: SocklenT) -> i32 {
    let mut socket = match sock_from_fd(fd) {
        Some(Sock::Entry(socket)) => socket,
        Some(Sock::Stream(_)) => { set_errno(EISCONN); return -1; }
        Some(Sock::Listener(_) | Sock::Datagram(_)) => { set_errno(EINVAL); return -1; }
        None => { set_errno(EBADF); return -1; }
    };
    let Some(entry) = *socket.slot() else { set_errno(EBADF); return -1; };
    let (ip, port) = match parse_sockaddr(addr, addrlen) {
        Some(v) => v,
        None => { set_errno(EINVAL); return -1; }
    };

    let bound = match entry.kind {
        SocketKind::Tcp => socket::listen(ip, port, TcpOptions::new(entry.nodelay))
            .map_err(net_err_to_errno)
            // SAFETY: `fd` names a socket entry, libc's own object, which its
            // listener replaces at the number C keeps using.
            .and_then(|bound| unsafe { toyos::fd::replace(fd, socket::descriptor(bound.handle)) }.map_err(|_| EBADF)),
        SocketKind::Udp => bind_datagram(fd, &entry, ip, port, None).map(drop),
    };
    match bound {
        Ok(()) => 0,
        Err(errno) => { set_errno(errno); -1 }
    }
}

#[no_mangle]
pub unsafe extern "C" fn listen(_fd: i32, _backlog: i32) -> i32 {
    // netstack handles listen implicitly via bind — no separate listen step needed
    0
}

#[no_mangle]
pub unsafe extern "C" fn accept(
    fd: i32,
    addr: *mut Sockaddr,
    addrlen: *mut SocklenT,
) -> i32 {
    let listener = match sock_from_fd(fd) {
        Some(Sock::Listener(held)) => held,
        Some(_) => { set_errno(EINVAL); return -1; }
        None => { set_errno(EBADF); return -1; }
    };
    let handle = listener.handle();

    // The descriptor's handle is the holder's bell: a connection that waits rings it.
    let mut poller = None;
    let accepted = interruptibly(
        |block| match socket::accept(handle, if block { Wait::Blocking(None) } else { Wait::Nonblocking }) {
            Err(NetError::WouldBlock) if !block => None,
            answer => Some(answer),
        },
        |wake| {
            let poller = poller.get_or_insert_with(|| toyos::poller::Poller::new(2));
            poller.watch(&handle, toyos::poller::READABLE, 0);
            poller.watch(&wake, toyos::poller::READABLE, 1);
            poller.wait(1, u64::MAX, |_| {});
            Ok(())
        },
    );
    let accepted = match accepted {
        Ok(Ok(accepted)) => accepted,
        Ok(Err(e)) => { set_errno(net_err_to_errno(e)); return -1; }
        Err(errno) => { set_errno(errno); return -1; }
    };

    if !addr.is_null() && !addrlen.is_null() {
        fill_sockaddr(addr, addrlen, accepted.remote_addr, accepted.remote_port);
    }

    // A socket descriptor is inherited, as `socket`'s is; on a full table the
    // stream goes with its handle.
    match toyos::fd::install(stream::descriptor(accepted.handle), true, 0) {
        Ok(fd) => fd,
        Err(_) => { set_errno(crate::errcode::EMFILE); -1 }
    }
}

#[no_mangle]
pub unsafe extern "C" fn send(fd: i32, buf: *const u8, len: usize, _flags: i32) -> isize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let data = core::slice::from_raw_parts(buf, len);
    match sock_from_fd(fd) {
        Some(Sock::Stream(held)) => write_stream(held.handle(), data),
        Some(Sock::Datagram(held)) => match socket::send(held.handle(), data) {
            Ok(sent) => sent as isize,
            Err(e) => { set_errno(net_err_to_errno(e)); -1 }
        },
        // A socket netstack does not hold was never connected.
        Some(Sock::Entry(_)) => { set_errno(ENOTCONN); -1 }
        Some(Sock::Listener(_)) => { set_errno(ENOTCONN); -1 }
        None => { set_errno(EBADF); -1 }
    }
}

/// `send` on a stream, and `write` on a stream's descriptor.
fn write_stream(handle: toyos::BorrowedHandle<'_>, data: &[u8]) -> isize {
    match stream::write(handle, data, Wait::Blocking(None)) {
        Ok(n) => n as isize,
        Err(refused) => { set_errno(crate::errcode::of_stream(refused)); -1 }
    }
}

/// `recv` on a stream, and `read` of its descriptor: a handler interrupts it
/// as [`interruptibly`] says.
fn read_stream(handle: toyos::BorrowedHandle<'_>, data: &mut [u8]) -> isize {
    let mut poller = None;
    let read = interruptibly(
        |block| {
            let wait = if block { Wait::Blocking(None) } else { Wait::Nonblocking };
            match stream::read(handle, data, wait) {
                Err(stream::Refused::WouldBlock) if !block => None,
                answer => Some(answer),
            }
        },
        |wake| {
            let poller = poller.get_or_insert_with(|| toyos::poller::Poller::new(2));
            stream::wait_ready(handle, stream::Way::Receive, wake, poller).map_err(crate::errcode::of_stream)
        },
    );
    match read {
        Ok(Ok(n)) => n as isize,
        Ok(Err(refused)) => { set_errno(crate::errcode::of_stream(refused)); -1 }
        Err(errno) => { set_errno(errno); -1 }
    }
}

/// A datagram into `data` from the datagram socket `handle` reaches, and where
/// it came from: a handler interrupts the wait for it as [`interruptibly`]
/// says.
fn read_datagram(handle: toyos::BorrowedHandle<'_>, data: &mut [u8]) -> Result<socket::Received, i32> {
    let mut poller = None;
    let received = interruptibly(
        |block| match socket::recv_from(handle, data, if block { Wait::Blocking(None) } else { Wait::Nonblocking }) {
            Err(NetError::WouldBlock) if !block => None,
            answer => Some(answer),
        },
        |wake| {
            let poller = poller.get_or_insert_with(|| toyos::poller::Poller::new(2));
            poller.watch(&handle, toyos::poller::READABLE, 0);
            poller.watch(&wake, toyos::poller::READABLE, 1);
            poller.wait(1, u64::MAX, |_| {});
            Ok(())
        },
    );
    received?.map_err(net_err_to_errno)
}

#[no_mangle]
pub unsafe extern "C" fn recv(fd: i32, buf: *mut u8, len: usize, _flags: i32) -> isize {
    recvfrom(fd, buf, len, _flags, ptr::null_mut(), ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn sendto(
    fd: i32,
    buf: *const u8,
    len: usize,
    _flags: i32,
    dest_addr: *const Sockaddr,
    addrlen: SocklenT,
) -> isize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let data = core::slice::from_raw_parts(buf, len);
    let sock = sock_from_fd(fd);
    // A stream's `sendto` ignores the address, as `send`.
    if let Some(Sock::Stream(held)) = &sock {
        return write_stream(held.handle(), data);
    }
    let Some((ip, port)) = parse_sockaddr(dest_addr, addrlen) else { set_errno(EINVAL); return -1; };
    match sock {
        Some(Sock::Datagram(held)) => send_datagram(Sending { fd, entry: None, bound: Some(held.0) }, data, ip, port),
        Some(Sock::Entry(mut socket)) => match *socket.slot() {
            Some(entry @ SocketEntry { kind: SocketKind::Udp, .. }) => send_datagram(Sending { fd, entry: Some(&entry), bound: None }, data, ip, port),
            Some(_) => { set_errno(ENOTCONN); -1 }
            None => { set_errno(EBADF); -1 }
        },
        Some(Sock::Listener(_)) => { set_errno(ENOTCONN); -1 }
        Some(Sock::Stream(_)) => unreachable!("a stream wrote above"),
        None => { set_errno(EBADF); -1 }
    }
}

#[no_mangle]
pub unsafe extern "C" fn recvfrom(
    fd: i32,
    buf: *mut u8,
    len: usize,
    _flags: i32,
    src_addr: *mut Sockaddr,
    addrlen: *mut SocklenT,
) -> isize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let data = core::slice::from_raw_parts_mut(buf, len);
    match sock_from_fd(fd) {
        // A stream's `recvfrom` names no address, as `recv`.
        Some(Sock::Stream(held)) => read_stream(held.handle(), data),
        Some(Sock::Datagram(held)) => match read_datagram(held.handle(), data) {
            Ok(received) => {
                if !src_addr.is_null() && !addrlen.is_null() {
                    fill_sockaddr(src_addr, addrlen, received.addr, received.port);
                }
                received.len as isize
            }
            Err(errno) => { set_errno(errno); -1 }
        },
        Some(Sock::Entry(_) | Sock::Listener(_)) => { set_errno(ENOTCONN); -1 }
        None => { set_errno(EBADF); -1 }
    }
}

#[no_mangle]
pub unsafe extern "C" fn shutdown(fd: i32, how: i32) -> i32 {
    let held = match sock_from_fd(fd) {
        Some(Sock::Stream(held)) => held,
        Some(Sock::Listener(_)) => { set_errno(ENOTCONN); return -1; }
        Some(Sock::Datagram(_)) => return 0,
        Some(Sock::Entry(mut socket)) => {
            let tcp = socket.slot().as_ref().is_some_and(|entry| matches!(entry.kind, SocketKind::Tcp));
            if tcp {
                set_errno(ENOTCONN);
                return -1;
            }
            return 0;
        }
        None => { set_errno(EBADF); return -1; }
    };
    if !matches!(how, SHUT_RD | SHUT_WR | SHUT_RDWR) {
        set_errno(EINVAL);
        return -1;
    }
    let handle = held.handle();
    if matches!(how, SHUT_RD | SHUT_RDWR) {
        let _ = stream::shutdown_read(handle);
    }
    if matches!(how, SHUT_WR | SHUT_RDWR) {
        let _ = stream::shutdown_write(handle);
    }
    0
}

const SHUT_RD: i32 = 0;
const SHUT_WR: i32 = 1;
const SHUT_RDWR: i32 = 2;

/// `attempt`'s answer, asked without blocking until it has one, with `wait`
/// between: given what a handler's signal makes readable, it waits for that
/// or for what the call holds to be ready. A handler that interrupts the
/// wait restarts it if it was installed with `SA_RESTART`, and answers
/// `EINTR` if not. With no handler kept nothing interrupts it, and
/// `attempt(true)` blocks for the answer.
fn interruptibly<T>(
    mut attempt: impl FnMut(bool) -> Option<T>,
    mut wait: impl FnMut(toyos::BorrowedHandle<'static>) -> Result<(), i32>,
) -> Result<T, i32> {
    let Some(mut call) = crate::signal::Call::enter(None, false, false) else {
        return Ok(attempt(true).expect("a blocking attempt answers"));
    };
    loop {
        if let Some(delivery) = call.take() {
            if !call.interrupted(delivery) {
                return Err(crate::errcode::EINTR);
            }
            continue;
        }
        if let Some(answer) = attempt(false) {
            return Ok(answer);
        }
        wait(call.wake())?;
    }
}

/// `read` of `fd` where it names a stream of this library's, as `recv`:
/// `None` for any other descriptor. In a Rust program the descriptor table's
/// owner is std's copy of `toyos`, which holds no end of libc's streams.
pub(crate) fn read_if_stream(fd: i32, data: &mut [u8]) -> Option<isize> {
    match sock_from_fd(fd)? {
        Sock::Stream(held) if stream::is_stream(held.handle()) => Some(read_stream(held.handle(), data)),
        _ => None,
    }
}

/// `write` of `fd` where it names a stream of this library's, as `send`.
pub(crate) fn write_if_stream(fd: i32, data: &[u8]) -> Option<isize> {
    match sock_from_fd(fd)? {
        Sock::Stream(held) if stream::is_stream(held.handle()) => Some(write_stream(held.handle(), data)),
        _ => None,
    }
}

/// The handle of the stream a descriptor names, for `poll`, given the object
/// of a descriptor `poll` holds.
pub(crate) fn stream_of<'a>(object: &toyos::fd::Lent<'a>) -> Option<toyos::BorrowedHandle<'a>> {
    object.handle().filter(|_| object.tag() == toyos::fd::STREAM)
}

/// The kernel handle a socket that is no stream is watched by, for `poll`:
/// its holder's bell, readable while a connection or a datagram waits.
pub(crate) fn watched_of<'a>(object: &toyos::fd::Lent<'a>) -> Option<toyos::BorrowedHandle<'a>> {
    object.handle().filter(|_| object.tag() == toyos::fd::SOCKET)
}


// setsockopt / getsockopt

fn option_errno(refusal: sockopt::Refusal) -> i32 {
    match refusal {
        sockopt::Refusal::Short => EINVAL,
        sockopt::Refusal::Fault => EFAULT,
        sockopt::Refusal::NoSuchOption => ENOPROTOOPT,
    }
}

/// The option netstack holds for `kept` on a listener or a datagram socket:
/// a listener's Nagle switch and a datagram socket's broadcast permission. A
/// listener sends to no broadcast address, and a datagram socket has no Nagle
/// switch.
fn held_option(kind: Kind, kept: Kept) -> Option<u32> {
    match (kind, kept) {
        (Kind::Listener, Kept::NoDelay) => Some(OPT_NODELAY),
        (Kind::Datagram, Kept::Broadcast) => Some(OPT_BROADCAST),
        (Kind::Listener, Kept::Broadcast) | (Kind::Datagram, Kept::NoDelay) => None,
    }
}

#[no_mangle]
pub unsafe extern "C" fn setsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *const u8,
    optlen: SocklenT,
) -> i32 {
    let mut socket = match sock_from_fd(fd) {
        Some(Sock::Stream(held)) => return set_stream_option(held.handle(), level, optname, optval, optlen),
        Some(Sock::Listener(held)) => return set_held_option(held.handle(), Kind::Listener, level, optname, optval, optlen),
        Some(Sock::Datagram(held)) => return set_held_option(held.handle(), Kind::Datagram, level, optname, optval, optlen),
        Some(Sock::Entry(socket)) => socket,
        None => { set_errno(EBADF); return -1; }
    };
    let slot = socket.slot();
    let entry = match slot.as_mut() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    // All other options silently succeed (SO_REUSEADDR, SO_KEEPALIVE, etc.)
    let option = match sockopt::kept(level, optname, matches!(entry.kind, SocketKind::Udp)) {
        Ok(Some(option)) => option,
        Ok(None) => return 0,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    let on = match sockopt::switch(optval, optlen) {
        Ok(on) => on,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    // Kept for the bind or the connect that hands it over.
    match option {
        Kept::NoDelay => entry.nodelay = on,
        Kept::Broadcast => entry.broadcast = on,
    }
    0
}

/// `setsockopt` on a listener or a datagram socket: a request of netstack's.
unsafe fn set_held_option(handle: toyos::BorrowedHandle<'_>, kind: Kind, level: i32, optname: i32, optval: *const u8, optlen: SocklenT) -> i32 {
    let option = match sockopt::kept(level, optname, kind == Kind::Datagram) {
        Ok(Some(option)) => option,
        Ok(None) => return 0,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    let on = match sockopt::switch(optval, optlen) {
        Ok(on) => on,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    let Some(option) = held_option(kind, option) else { return 0 };
    match socket::set_option(handle, option, on as u32) {
        Ok(()) => 0,
        Err(e) => { set_errno(net_err_to_errno(e)); -1 }
    }
}

/// `setsockopt` on a stream: its Nagle switch is a word of its region.
unsafe fn set_stream_option(handle: toyos::BorrowedHandle<'_>, level: i32, optname: i32, optval: *const u8, optlen: SocklenT) -> i32 {
    let option = match sockopt::kept(level, optname, false) {
        Ok(Some(option)) => option,
        Ok(None) => return 0,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    let on = match sockopt::switch(optval, optlen) {
        Ok(on) => on,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    match option {
        Kept::NoDelay => match stream::set_nodelay(handle, on) {
            Ok(()) => 0,
            Err(refused) => { set_errno(crate::errcode::of_stream(refused)); -1 }
        },
        // A stream sends to no broadcast address.
        Kept::Broadcast => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn getsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *mut u8,
    optlen: *mut SocklenT,
) -> i32 {
    let (option, entry_value) = match sock_from_fd(fd) {
        Some(Sock::Stream(held)) => match sockopt::kept(level, optname, false) {
            Ok(Some(Kept::NoDelay)) => match stream::nodelay(held.handle()) {
                Ok(on) => (Some(Kept::NoDelay), on as i32),
                Err(refused) => { set_errno(crate::errcode::of_stream(refused)); return -1; }
            },
            Ok(option) => (option, 0),
            Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
        },
        Some(sock @ (Sock::Listener(_) | Sock::Datagram(_))) => {
            let (kind, held) = match sock {
                Sock::Listener(held) => (Kind::Listener, held),
                Sock::Datagram(held) => (Kind::Datagram, held),
                Sock::Entry(_) | Sock::Stream(_) => unreachable!("matched as netstack's socket"),
            };
            match sockopt::kept(level, optname, kind == Kind::Datagram) {
                Ok(Some(kept)) => match held_option(kind, kept).map(|option| socket::option(held.handle(), option)) {
                    Some(Ok(value)) => (Some(kept), (value != 0) as i32),
                    Some(Err(e)) => { set_errno(net_err_to_errno(e)); return -1; }
                    None => (Some(kept), 0),
                },
                Ok(None) => (None, 0),
                Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
            }
        }
        Some(Sock::Entry(mut socket)) => {
            let Some(entry) = *socket.slot() else { set_errno(EBADF); return -1; };
            match sockopt::kept(level, optname, matches!(entry.kind, SocketKind::Udp)) {
                Ok(Some(Kept::NoDelay)) => (Some(Kept::NoDelay), entry.nodelay as i32),
                Ok(Some(Kept::Broadcast)) => (Some(Kept::Broadcast), entry.broadcast as i32),
                Ok(None) => (None, 0),
                Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
            }
        }
        None => { set_errno(EBADF); return -1; }
    };
    // Every other option reads 0, `SO_ERROR` among them.
    let value = if option.is_some() { entry_value } else { 0 };
    match sockopt::answer(value, optval, optlen) {
        Ok(()) => 0,
        Err(refusal) => { set_errno(option_errno(refusal)); -1 }
    }
}

// getpeername / getsockname

#[no_mangle]
pub unsafe extern "C" fn getpeername(
    fd: i32,
    addr: *mut Sockaddr,
    addrlen: *mut SocklenT,
) -> i32 {
    let peer = match sock_from_fd(fd) {
        Some(Sock::Stream(held)) => match stream::endpoints(held.handle()) {
            Ok((peer, _)) => Some(peer),
            Err(refused) => { set_errno(crate::errcode::of_stream(refused)); return -1; }
        },
        Some(Sock::Datagram(held)) => match socket::peer(held.handle()) {
            Ok(peer) => peer,
            Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
        },
        Some(Sock::Listener(_) | Sock::Entry(_)) => None,
        None => { set_errno(EBADF); return -1; }
    };
    let Some((ip, port)) = peer else { set_errno(ENOTCONN); return -1; };
    fill_sockaddr(addr, addrlen, ip, port);
    0
}

#[no_mangle]
pub unsafe extern "C" fn getsockname(
    fd: i32,
    addr: *mut Sockaddr,
    addrlen: *mut SocklenT,
) -> i32 {
    let local_port = match sock_from_fd(fd) {
        Some(Sock::Stream(held)) => match stream::endpoints(held.handle()) {
            Ok((_, local_port)) => local_port,
            Err(refused) => { set_errno(crate::errcode::of_stream(refused)); return -1; }
        },
        Some(Sock::Listener(held) | Sock::Datagram(held)) => match socket::local(held.handle()) {
            Ok((_, port)) => port,
            Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
        },
        Some(Sock::Entry(mut socket)) => match *socket.slot() {
            Some(_) => 0,
            None => { set_errno(EBADF); return -1; }
        },
        None => { set_errno(EBADF); return -1; }
    };
    // Local address: 10.0.2.15 (QEMU default)
    fill_sockaddr(addr, addrlen, [10, 0, 2, 15], local_port);
    0
}

// DNS: getaddrinfo / freeaddrinfo

#[no_mangle]
pub unsafe extern "C" fn getaddrinfo(
    node: *const u8,
    _service: *const u8,
    _hints: *const Addrinfo,
    res: *mut *mut Addrinfo,
) -> i32 {
    if node.is_null() || res.is_null() {
        return -1; // EAI_NONAME
    }

    let name_len = super::string::strlen(node);
    let name = core::slice::from_raw_parts(node, name_len);

    // Try parsing as IPv4 literal first
    if let Some(ip) = inaddr::dotted_quad(name) {
        return build_addrinfo_result(res, &[(ip, 0)]);
    }

    // Use toyos::net dns_lookup
    let hostname = match core::str::from_utf8(name) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let mut results = [[0u8; 4]; 16];
    let count = match toyos::net::dns_lookup(hostname, &mut results) {
        Ok(n) => n,
        Err(_) => return -1,
    };
    if count == 0 {
        return -1; // EAI_NONAME
    }

    let addrs: Vec<([u8; 4], u16)> = results[..count].iter().map(|ip| (*ip, 0u16)).collect();
    build_addrinfo_result(res, &addrs)
}

unsafe fn build_addrinfo_result(res: *mut *mut Addrinfo, addrs: &[([u8; 4], u16)]) -> i32 {
    let mut prev: *mut Addrinfo = ptr::null_mut();
    // Build linked list in reverse so first result is first in list
    for &(ip, port) in addrs.iter().rev() {
        let layout_ai = core::alloc::Layout::new::<Addrinfo>();
        let ai = heap_alloc(layout_ai) as *mut Addrinfo;
        if ai.is_null() { return -1; }

        let layout_sa = core::alloc::Layout::new::<SockaddrIn>();
        let sa = heap_alloc(layout_sa) as *mut SockaddrIn;
        if sa.is_null() {
            heap_dealloc(ai as *mut u8, layout_ai);
            return -1;
        }

        sa.write(SockaddrIn::new(ip, port));

        (*ai).ai_flags = 0;
        (*ai).ai_family = AF_INET;
        (*ai).ai_socktype = SOCK_STREAM;
        (*ai).ai_protocol = 0;
        (*ai).ai_addrlen = core::mem::size_of::<SockaddrIn>() as SocklenT;
        (*ai).ai_addr = sa as *mut Sockaddr;
        (*ai).ai_canonname = ptr::null_mut();
        (*ai).ai_next = prev;

        prev = ai;
    }
    *res = prev;
    0
}

#[no_mangle]
pub unsafe extern "C" fn freeaddrinfo(mut res: *mut Addrinfo) {
    while !res.is_null() {
        let next = (*res).ai_next;
        if !(*res).ai_addr.is_null() {
            heap_dealloc((*res).ai_addr as *mut u8, core::alloc::Layout::new::<SockaddrIn>());
        }
        heap_dealloc(res as *mut u8, core::alloc::Layout::new::<Addrinfo>());
        res = next;
    }
}

#[no_mangle]
pub unsafe extern "C" fn gai_strerror(_errcode: i32) -> *const u8 {
    b"DNS lookup failed\0".as_ptr()
}

// inet_pton / inet_ntop / inet_addr / htons / ntohs / htonl / ntohl

#[no_mangle]
pub unsafe extern "C" fn inet_pton(af: i32, src: *const u8, dst: *mut u8) -> i32 {
    let text = core::slice::from_raw_parts(src, super::string::strlen(src));
    match inaddr::pton(af, text) {
        Ok(Some(ip)) => {
            ptr::copy_nonoverlapping(ip.as_ptr(), dst, 4);
            1
        }
        Ok(None) => 0,
        Err(refusal) => { set_errno(address_errno(refusal)); -1 }
    }
}

fn address_errno(refusal: inaddr::Refusal) -> i32 {
    match refusal {
        inaddr::Refusal::Family => EAFNOSUPPORT,
        inaddr::Refusal::Room => ENOSPC,
    }
}

#[no_mangle]
pub unsafe extern "C" fn inet_ntop(
    af: i32,
    src: *const u8,
    dst: *mut u8,
    size: SocklenT,
) -> *const u8 {
    match inaddr::ntop(af, src, dst, size) {
        Ok(()) => dst as *const u8,
        Err(refusal) => { set_errno(address_errno(refusal)); ptr::null() }
    }
}

#[no_mangle]
pub unsafe extern "C" fn htons(hostshort: u16) -> u16 {
    hostshort.to_be()
}

#[no_mangle]
pub unsafe extern "C" fn ntohs(netshort: u16) -> u16 {
    u16::from_be(netshort)
}

#[no_mangle]
pub unsafe extern "C" fn htonl(hostlong: u32) -> u32 {
    hostlong.to_be()
}

#[no_mangle]
pub unsafe extern "C" fn ntohl(netlong: u32) -> u32 {
    u32::from_be(netlong)
}

/// `INADDR_NONE` for a text that is no address, which is also what
/// 255.255.255.255 reads as.
#[no_mangle]
pub unsafe extern "C" fn inet_addr(cp: *const u8) -> u32 {
    let text = core::slice::from_raw_parts(cp, super::string::strlen(cp));
    inaddr::numbers_and_dots(text).map_or(u32::MAX, u32::from_ne_bytes)
}
