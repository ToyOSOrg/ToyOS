//! Loopback networking through std's `net`: Linux's TCP and UDP in the
//! kernel. ToyOS has no loopback, so every benchmark here is `unsupported`
//! there: netstack refuses 127/8 and its own addresses as destinations
//! (`system/netstack/ip/src/route.rs`, `RouteLocalDestination`; the UDP
//! layer's `SendLoopback`). The link to another machine is `lan`'s.
//!
//! Every wait is bounded by [`PATIENCE`], so a stack that accepts a call and
//! never answers it is refused rather than hung.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use crate::measure::{each, io};
use crate::report::refuse;
use crate::sys;

const PATIENCE: Duration = Duration::from_secs(5);
const LOOPBACK: Ipv4Addr = Ipv4Addr::LOCALHOST;

fn accept(listener: &TcpListener) -> std::io::Result<TcpStream> {
    let start = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false)?;
                return Ok(stream);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock && start.elapsed() < PATIENCE => thread::yield_now(),
            Err(e) => return Err(e),
        }
    }
}

fn connect(addr: SocketAddr) -> std::io::Result<TcpStream> {
    let stream = TcpStream::connect_timeout(&addr, PATIENCE)?;
    stream.set_read_timeout(Some(PATIENCE))?;
    stream.set_write_timeout(Some(PATIENCE))?;
    Ok(stream)
}

/// A listener on the loopback, or the reason there is none.
fn listen() -> std::io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind((LOOPBACK, 0))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    // A system can open a socket and still never carry a segment to itself:
    // one connection, accepted, is the proof it carries.
    let probe = connect(addr)?;
    let accepted = accept(&listener)?;
    drop((probe, accepted));
    Ok((listener, addr))
}

fn refused(what: &str, e: std::io::Error) -> sys::Refused {
    sys::Refused::Failed(format!("{what}: {e} ({:?})", e.kind()))
}

const BENCHES: [&str; 5] =
    ["net.tcp.connect_accept_close", "net.tcp.reqrep.64", "net.tcp.stream.1", "net.tcp.stream.65536", "net.udp.reqrep.64"];

pub fn suite() {
    if cfg!(target_os = "toyos") {
        for bench in BENCHES {
            crate::report::unsupported(bench, "ToyOS's network stack has no loopback: netstack refuses 127/8 as a destination");
        }
        return;
    }
    let (listener, addr) = match listen() {
        Ok(pair) => pair,
        Err(e) => {
            let why = refused("a loopback TCP listener and one connection to it", e);
            for bench in &BENCHES[..4] {
                why.clone().report(bench);
            }
            return udp();
        }
    };

    // Connect, accept and both ends closed: 1,000 after 50.
    each("net.tcp.connect_accept_close", 50, 1000, &[], |_| {
        let client = io("connect", connect(addr))?;
        let server = io("accept", accept(&listener))?;
        drop((client, server));
        Ok(())
    });

    // 64 bytes sent and echoed back by another thread, no Nagle: 5,000 after 200.
    let pair = (|| -> std::io::Result<(TcpStream, TcpStream)> {
        let client = connect(addr)?;
        let server = accept(&listener)?;
        client.set_nodelay(true)?;
        server.set_nodelay(true)?;
        server.set_read_timeout(Some(PATIENCE))?;
        Ok((client, server))
    })();
    match pair {
        Ok((mut client, mut server)) => {
            let echo = thread::spawn(move || {
                let mut buf = [0u8; 64];
                while server.read_exact(&mut buf).is_ok() {
                    if server.write_all(&buf).is_err() {
                        break;
                    }
                }
            });
            let mut buf = [5u8; 64];
            each("net.tcp.reqrep.64", 200, 5000, &[], |_| {
                io("send", client.write_all(&buf))?;
                io("receive", client.read_exact(&mut buf))
            });
            drop(client);
            let _ = echo.join();
        }
        Err(e) => refuse("net.tcp.reqrep.64", e),
    }

    // One thread writing for 100 ms in `size`-byte writes, Nagle on as an
    // ordinary program has it, another reading in 64 KiB reads: MB/s as the
    // reader saw it, five rounds after one.
    for size in [1usize, 65536] {
        let bench = format!("net.tcp.stream.{size}");
        let mut rates = Vec::new();
        let mut failed = None;
        for _ in 0..6 {
            match stream_round(&listener, addr, size) {
                Ok(rate) => rates.push(rate),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
        }
        match failed {
            Some(e) => refuse(&bench, e),
            None => crate::report::result(&bench, "MB/s", &mut rates[1..].to_vec(), &[("write_bytes", size.to_string())]),
        }
    }
    udp();
}

fn stream_round(listener: &TcpListener, addr: SocketAddr, size: usize) -> Result<u64, String> {
    let mut writer = io("connect", connect(addr))?;
    let mut reader = io("accept", accept(listener))?;
    io("timeout", reader.set_read_timeout(Some(PATIENCE)))?;
    let writing = thread::spawn(move || -> std::io::Result<()> {
        let chunk = vec![0x5au8; size];
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            for _ in 0..64 {
                writer.write_all(&chunk)?;
            }
        }
        Ok(())
    });
    let mut buf = vec![0u8; 65536];
    io("read", reader.read(&mut buf))?;
    let (mut bytes, start) = (0u64, Instant::now());
    loop {
        match io("read", reader.read(&mut buf))? {
            0 => break,
            n => bytes += n as u64,
        }
    }
    let ns = crate::ns_since(start).max(1);
    io("write", writing.join().map_err(|_| "the writer panicked".to_string())?)?;
    Ok(bytes * 1000 / ns)
}

/// 64 bytes sent to a loopback UDP socket another thread echoes from:
/// 5,000 after 200.
fn udp() {
    let bench = "net.udp.reqrep.64";
    let sockets = (|| -> std::io::Result<(UdpSocket, UdpSocket)> {
        let server = UdpSocket::bind((LOOPBACK, 0))?;
        let client = UdpSocket::bind((LOOPBACK, 0))?;
        client.connect(server.local_addr()?)?;
        server.set_read_timeout(Some(PATIENCE))?;
        client.set_read_timeout(Some(PATIENCE))?;
        // As for TCP: one datagram there and back is the proof it carries.
        let mut buf = [0u8; 64];
        client.send(&buf)?;
        let (_, from) = server.recv_from(&mut buf)?;
        server.send_to(&buf, from)?;
        client.recv(&mut buf)?;
        Ok((server, client))
    })();
    let (server, client) = match sockets {
        Ok(pair) => pair,
        Err(e) => return refused("a loopback UDP exchange", e).report(bench),
    };
    let echo = thread::spawn(move || {
        let mut buf = [0u8; 64];
        while let Ok((n, from)) = server.recv_from(&mut buf) {
            if n == 0 || server.send_to(&buf[..n], from).is_err() {
                break;
            }
        }
    });
    let mut buf = [9u8; 64];
    each(bench, 200, 5000, &[], |_| {
        io("send", client.send(&buf))?;
        let n = io("receive", client.recv(&mut buf))?;
        if n == 64 { Ok(()) } else { Err(format!("received {n} bytes")) }
    });
    let _ = client.send(&[]);
    let _ = echo.join();
}
