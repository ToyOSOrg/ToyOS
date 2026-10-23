//! A listener and a datagram socket a real netstack holds, each exported to a
//! child through std's `Command` and adopted there: the path from a bind to a
//! second process's accept and datagrams that only a guest with netstack shows.
//!
//! The harness is the peer: it listens for datagrams on its host's loopback,
//! which QEMU's user network reaches from the guest as `10.0.2.2`, names that
//! port as this job's argument, and connects to [`LISTENING`] through the port
//! QEMU forwards to it. The parent binds both sockets and spawns a child given
//! them as descriptors 3 and 4 (`CommandExt::inherit_handle`, which exports
//! each); the child says it is ready in a datagram, is told to go in one,
//! accepts the harness's connection and writes a line on it. Once the child
//! has ended, the parent does the same through its own holders, which still
//! hold both sockets. Every step waits on the peer's next datagram or
//! connection, under the harness's ceiling.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::os::fd::FromRawFd;
use std::os::toyos::io::AsRawHandle;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

const SELF_PATH: &str = "/system/bin/test_rs_socket_export";

/// QEMU's user network's address of its host.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// The port the guest listens on, which the harness's boot forwards to
/// (`tests/toyos.rs`'s `SOCKET_EXPORT_PORT`).
const LISTENING: u16 = 7070;

/// The descriptors the child is given its holders under.
const LISTENER_FD: i32 = 3;
const DATAGRAM_FD: i32 = 4;

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = match args.next().as_deref() {
        Some("child") => {
            let port = args.next().expect("the harness's port");
            return child(port.parse().unwrap_or_else(|e| panic!("a port, not {port:?}: {e}")));
        }
        Some(port) => port.parse().unwrap_or_else(|e| panic!("a port, not {port:?}: {e}")),
        None => panic!("the harness's datagram port is this job's argument"),
    };
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, LISTENING)).expect("a listener through netstack");
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a datagram socket through netstack");
    let status = Command::new(SELF_PATH)
        .arg("child")
        .arg(port.to_string())
        .inherit_handle(LISTENER_FD as u32, listener.as_raw_handle())
        .inherit_handle(DATAGRAM_FD as u32, socket.as_raw_handle())
        .stdout(Stdio::null())
        .status()
        .expect("spawn the child with a holder of each socket");
    assert!(status.success(), "the child ended {status:?}");
    serve("parent", &listener, &socket, port);
    println!("a listener and a datagram socket exported to a child: it accepted and spoke on them, and so did the parent after it");
}

fn child(port: u16) {
    // SAFETY: the descriptors its parent gave it, a holder of each socket.
    let (listener, socket) = unsafe { (TcpListener::from_raw_fd(LISTENER_FD), UdpSocket::from_raw_fd(DATAGRAM_FD)) };
    serve("child", &listener, &socket, port);
}

/// `who`'s turn: ready said and go heard in datagrams, then one connection
/// accepted and a line written on it.
fn serve(who: &str, listener: &TcpListener, socket: &UdpSocket, port: u16) {
    let peer = SocketAddr::from((HOST, port));
    let ready = format!("{who} ready");
    let sent = socket.send_to(ready.as_bytes(), peer).unwrap_or_else(|e| panic!("{who}'s datagram: {e}"));
    assert_eq!(sent, ready.len());
    let mut heard = [0u8; 64];
    let (n, from) = socket.recv_from(&mut heard).unwrap_or_else(|e| panic!("{who}'s receive: {e}"));
    assert_eq!((&heard[..n], from), (&b"go"[..], peer), "{who} heard another datagram");
    let (mut stream, _) = listener.accept().unwrap_or_else(|e| panic!("{who}'s accept: {e}"));
    writeln!(stream, "accepted by the {who}").unwrap_or_else(|e| panic!("{who}'s line: {e}"));
}
