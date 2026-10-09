// BSD sockets — TCP and UDP use pipe-backed data transfer via netstack.

use alloc::alloc::{alloc as heap_alloc, dealloc as heap_dealloc};
use alloc::vec::Vec;
use core::ptr;
use toyos_abi::RawHandle;
use toyos_abi::syscall;
use toyos::net::{NetError, TcpSocketId, UdpSocketId, OPT_BROADCAST, OPT_NODELAY};

use crate::errno::{
    EACCES, EADDRINUSE, EAFNOSUPPORT, EBADF, ECONNREFUSED, ECONNRESET, EFAULT, EINVAL, EIO, ENOMEM, ENOPROTOOPT, ENOSPC,
    ENOTCONN, EOPNOTSUPP, ETIMEDOUT,
};
use crate::inaddr::{self, SockaddrIn, AF_INET};
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

#[derive(Clone, Copy)]
struct SocketEntry {
    kind: SocketKind,
    netstack_id: u32,       // netstack socket_id (0 = not yet connected/bound)
    connected: bool,
    bound: bool,
    local_port: u16,
    remote_addr: [u8; 4],
    remote_port: u16,
    // Pipe fds for data path (0 = not set)
    rx_fd: i32,         // read end of rx pipe (netstack→client)
    tx_fd: i32,         // write end of tx pipe (client→netstack)
    notify_fd: i32,     // read end of listener notify pipe
    // What netstack holds for the socket, which `getsockopt` answers from. A
    // socket netstack does not hold yet keeps each for the call that makes it
    // there to hand over: `broadcast` for a datagram socket's bind, `nodelay`
    // for a stream's `connect`.
    nodelay: bool,
    broadcast: bool,
}

const MAX_SOCKETS: usize = 128;
// Socket FDs start at 1024 to avoid collisions with file FDs
const SOCKET_FD_BASE: i32 = 1024;

static mut SOCKETS: [Option<SocketEntry>; MAX_SOCKETS] = [None; MAX_SOCKETS];

fn sock_from_fd(fd: i32) -> Option<&'static mut Option<SocketEntry>> {
    let idx = (fd - SOCKET_FD_BASE) as usize;
    if idx >= MAX_SOCKETS {
        return None;
    }
    unsafe { Some(&mut SOCKETS[idx]) }
}

fn alloc_socket(entry: SocketEntry) -> i32 {
    unsafe {
        for i in 0..MAX_SOCKETS {
            if SOCKETS[i].is_none() {
                SOCKETS[i] = Some(entry);
                return SOCKET_FD_BASE + i as i32;
            }
        }
    }
    -1
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

/// Bind a datagram socket netstack does not hold yet, handing over a
/// `SO_BROADCAST` set before it existed there.
fn bind_datagram(entry: &mut SocketEntry, ip: [u8; 4], port: u16) -> Result<(), NetError> {
    let bound = toyos::net::udp_bind(ip, port)?;
    if entry.broadcast {
        if let Err(e) = toyos::net::udp_set_option(bound.socket_id, OPT_BROADCAST, 1) {
            // `bound`'s pipe ends close where it drops.
            let _ = toyos::net::udp_close(bound.socket_id);
            return Err(e);
        }
    }
    entry.netstack_id = bound.socket_id.0;
    entry.local_port = bound.bound_port;
    entry.bound = true;
    entry.tx_fd = bound.tx.into_raw().0 as i32;
    entry.rx_fd = bound.rx.into_raw().0 as i32;
    Ok(())
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
    let entry = SocketEntry {
        kind,
        netstack_id: 0,
        connected: false,
        bound: false,
        local_port: 0,
        remote_addr: [0; 4],
        remote_port: 0,
        rx_fd: 0,
        tx_fd: 0,
        notify_fd: 0,
        nodelay: false,
        broadcast: false,
    };
    let fd = alloc_socket(entry);
    if fd < 0 {
        set_errno(ENOMEM);
    }
    fd
}

#[no_mangle]
pub unsafe extern "C" fn connect(fd: i32, addr: *const Sockaddr, addrlen: SocklenT) -> i32 {
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_mut() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };
    let (ip, port) = match parse_sockaddr(addr, addrlen) {
        Some(v) => v,
        None => { set_errno(EINVAL); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            let conn = match toyos::net::tcp_connect(ip, port, 30000) {
                Ok(c) => c,
                Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
            };
            if entry.nodelay {
                if let Err(e) = toyos::net::tcp_set_option(conn.socket_id, OPT_NODELAY, 1) {
                    // `conn`'s pipe ends close where it drops.
                    let _ = toyos::net::tcp_close(conn.socket_id);
                    set_errno(net_err_to_errno(e));
                    return -1;
                }
            }
            entry.netstack_id = conn.socket_id.0;
            entry.local_port = conn.local_port;
            entry.remote_addr = ip;
            entry.remote_port = port;
            entry.connected = true;
            entry.rx_fd = conn.rx.into_raw().0 as i32;
            entry.tx_fd = conn.tx.into_raw().0 as i32;
            0
        }
        SocketKind::Udp => {
            entry.remote_addr = ip;
            entry.remote_port = port;
            entry.connected = true;
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn bind(fd: i32, addr: *const Sockaddr, addrlen: SocklenT) -> i32 {
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_mut() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };
    let (ip, port) = match parse_sockaddr(addr, addrlen) {
        Some(v) => v,
        None => { set_errno(EINVAL); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            let bound = match toyos::net::tcp_bind(ip, port) {
                Ok(b) => b,
                Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
            };
            entry.netstack_id = bound.socket_id.0;
            entry.local_port = bound.bound_port;
            entry.bound = true;
            entry.notify_fd = bound.notify.into_raw().0 as i32;
            0
        }
        SocketKind::Udp => {
            if let Err(e) = bind_datagram(entry, ip, port) {
                set_errno(net_err_to_errno(e));
                return -1;
            }
            0
        }
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
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };
    let listener_id = TcpSocketId(entry.netstack_id);
    let notify_fd = entry.notify_fd;

    // Block until a connection arrives (read 1 byte from notify pipe)
    let mut notify_byte = [0u8; 1];
    let _ = syscall::read(RawHandle(notify_fd as u32), &mut notify_byte);

    let accepted = match toyos::net::tcp_accept(listener_id) {
        Ok(a) => a,
        Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
    };

    if !addr.is_null() && !addrlen.is_null() {
        fill_sockaddr(addr, addrlen, accepted.remote_addr, accepted.remote_port);
    }

    let new_entry = SocketEntry {
        kind: SocketKind::Tcp,
        netstack_id: accepted.socket_id.0,
        connected: true,
        bound: false,
        local_port: accepted.local_port,
        remote_addr: accepted.remote_addr,
        remote_port: accepted.remote_port,
        rx_fd: accepted.rx.into_raw().0 as i32,
        tx_fd: accepted.tx.into_raw().0 as i32,
        notify_fd: 0,
        nodelay: false,
        broadcast: false,
    };
    let new_fd = alloc_socket(new_entry);
    if new_fd < 0 {
        syscall::close(RawHandle(new_entry.rx_fd as u32));
        syscall::close(RawHandle(new_entry.tx_fd as u32));
        let _ = toyos::net::tcp_close(accepted.socket_id);
        set_errno(ENOMEM);
    }
    new_fd
}

#[no_mangle]
pub unsafe extern "C" fn send(fd: i32, buf: *const u8, len: usize, _flags: i32) -> isize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            let data = core::slice::from_raw_parts(buf, len);
            match syscall::write(RawHandle(entry.tx_fd as u32), data) {
                Ok(n) => n as isize,
                Err(_) => { set_errno(EIO); -1 }
            }
        }
        SocketKind::Udp => {
            if !entry.connected {
                set_errno(ENOTCONN);
                return -1;
            }
            sendto(fd, buf, len, _flags,
                &SockaddrIn::new(entry.remote_addr, entry.remote_port) as *const SockaddrIn as *const Sockaddr,
                core::mem::size_of::<SockaddrIn>() as SocklenT)
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn recv(fd: i32, buf: *mut u8, len: usize, _flags: i32) -> isize {
    if buf.is_null() || len == 0 {
        return 0;
    }
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            let data = core::slice::from_raw_parts_mut(buf, len);
            match syscall::read(RawHandle(entry.rx_fd as u32), data) {
                Ok(n) => n as isize,
                Err(_) => { set_errno(EIO); -1 }
            }
        }
        SocketKind::Udp => {
            recvfrom(fd, buf, len, _flags, ptr::null_mut(), ptr::null_mut())
        }
    }
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
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_mut() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            // TCP sendto ignores address, just send
            send(fd, buf, len, _flags)
        }
        SocketKind::Udp => {
            let (ip, port) = match parse_sockaddr(dest_addr, addrlen) {
                Some(v) => v,
                None => { set_errno(EINVAL); return -1; }
            };
            // POSIX: a socket not yet bound is bound at its first send, to a
            // port the stack chooses.
            if entry.netstack_id == 0 {
                if let Err(e) = bind_datagram(entry, [0; 4], 0) {
                    set_errno(net_err_to_errno(e));
                    return -1;
                }
            }
            // Write data to tx pipe
            let data = core::slice::from_raw_parts(buf, len);
            if let Err(_) = syscall::write(RawHandle(entry.tx_fd as u32), data) {
                set_errno(EIO);
                return -1;
            }
            // Send control message with metadata
            match toyos::net::udp_send_to(UdpSocketId(entry.netstack_id), ip, port, len as u16) {
                Ok(sent) => sent as isize,
                Err(e) => { set_errno(net_err_to_errno(e)); -1 }
            }
        }
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
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    match entry.kind {
        SocketKind::Tcp => {
            // TCP recvfrom ignores address, just recv
            recv(fd, buf, len, _flags)
        }
        SocketKind::Udp => {
            // Send control request and get metadata response
            let recv_resp = match toyos::net::udp_recv_from(UdpSocketId(entry.netstack_id), len as u32) {
                Ok(r) => r,
                Err(e) => { set_errno(net_err_to_errno(e)); return -1; }
            };

            if !src_addr.is_null() && !addrlen.is_null() {
                fill_sockaddr(src_addr, addrlen, recv_resp.addr, recv_resp.port);
            }

            let n = (recv_resp.len as usize).min(len);
            if n > 0 {
                // Read data from rx pipe
                let data = core::slice::from_raw_parts_mut(buf, n);
                match syscall::read(RawHandle(entry.rx_fd as u32), data) {
                    Ok(bytes_read) => bytes_read as isize,
                    Err(_) => { set_errno(EIO); -1 }
                }
            } else {
                0
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn shutdown(fd: i32, how: i32) -> i32 {
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    if let SocketKind::Tcp = entry.kind {
        if let Err(e) = toyos::net::tcp_shutdown(TcpSocketId(entry.netstack_id), how as u32) {
            set_errno(net_err_to_errno(e));
            return -1;
        }
    }
    0
}


// close (for socket fds)

#[no_mangle]
pub unsafe extern "C" fn close_socket(fd: i32) -> bool {
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => return false,
    };
    let entry = match slot.take() {
        Some(e) => e,
        None => return false,
    };

    // Close pipe fds
    if entry.rx_fd != 0 { syscall::close(RawHandle(entry.rx_fd as u32)); }
    if entry.tx_fd != 0 { syscall::close(RawHandle(entry.tx_fd as u32)); }
    if entry.notify_fd != 0 { syscall::close(RawHandle(entry.notify_fd as u32)); }

    // Tell netstack to close the socket
    if entry.netstack_id != 0 {
        match entry.kind {
            SocketKind::Tcp => { let _ = toyos::net::tcp_close(TcpSocketId(entry.netstack_id)); }
            SocketKind::Udp => { let _ = toyos::net::udp_close(UdpSocketId(entry.netstack_id)); }
        }
    }
    true
}

// setsockopt / getsockopt

fn option_errno(refusal: sockopt::Refusal) -> i32 {
    match refusal {
        sockopt::Refusal::Short => EINVAL,
        sockopt::Refusal::Fault => EFAULT,
        sockopt::Refusal::NoSuchOption => ENOPROTOOPT,
        sockopt::Refusal::NotSupported => EOPNOTSUPP,
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
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_mut() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    // All other options silently succeed (SO_REUSEADDR, SO_KEEPALIVE, etc.)
    let option = match sockopt::kept(level, optname, matches!(entry.kind, SocketKind::Udp), true) {
        Ok(Some(option)) => option,
        Ok(None) => return 0,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    let on = match sockopt::switch(optval, optlen) {
        Ok(on) => on,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    match option {
        Kept::NoDelay => {
            // netstack holds a connection from `connect` or `accept`, and a
            // listener from `bind`, whose id names no connection: it refuses
            // a listener's set, and nothing is kept for one.
            if entry.netstack_id != 0 {
                if let Err(e) = toyos::net::tcp_set_option(TcpSocketId(entry.netstack_id), OPT_NODELAY, on as u32) {
                    set_errno(net_err_to_errno(e));
                    return -1;
                }
            }
            entry.nodelay = on;
        }
        Kept::Broadcast => {
            // Only a datagram is ever sent to a broadcast address, and netstack
            // holds a datagram socket from its `bind`.
            if let (SocketKind::Udp, true) = (entry.kind, entry.netstack_id != 0) {
                if let Err(e) = toyos::net::udp_set_option(UdpSocketId(entry.netstack_id), OPT_BROADCAST, on as u32) {
                    set_errno(net_err_to_errno(e));
                    return -1;
                }
            }
            entry.broadcast = on;
        }
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn getsockopt(
    fd: i32,
    level: i32,
    optname: i32,
    optval: *mut u8,
    optlen: *mut SocklenT,
) -> i32 {
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };

    let option = match sockopt::kept(level, optname, matches!(entry.kind, SocketKind::Udp), false) {
        Ok(option) => option,
        Err(refusal) => { set_errno(option_errno(refusal)); return -1; }
    };
    let value = match option {
        Some(Kept::NoDelay) => entry.nodelay as i32,
        Some(Kept::Broadcast) => entry.broadcast as i32,
        // Every other option reads 0, `SO_ERROR` among them.
        None => 0,
    };
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
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };
    if !entry.connected {
        set_errno(ENOTCONN);
        return -1;
    }
    fill_sockaddr(addr, addrlen, entry.remote_addr, entry.remote_port);
    0
}

#[no_mangle]
pub unsafe extern "C" fn getsockname(
    fd: i32,
    addr: *mut Sockaddr,
    addrlen: *mut SocklenT,
) -> i32 {
    let slot = match sock_from_fd(fd) {
        Some(s) => s,
        None => { set_errno(EBADF); return -1; }
    };
    let entry = match slot.as_ref() {
        Some(e) => e,
        None => { set_errno(EBADF); return -1; }
    };
    // Local address: 10.0.2.15 (QEMU default)
    fill_sockaddr(addr, addrlen, [10, 0, 2, 15], entry.local_port);
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
