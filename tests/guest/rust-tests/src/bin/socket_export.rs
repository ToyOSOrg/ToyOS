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
//! hold both sockets.
//!
//! **Then the two race.** The parent spawns a second child, and both block in
//! an accept and then in a receive on the one queue each socket is: the harness
//! connects twice and sends two datagrams, and each process takes exactly one
//! of each, whichever loses a race waiting again on its bell. Every step waits
//! on the peer's next datagram or connection, under the harness's ceiling.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::os::fd::FromRawFd;
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
        Some(role @ ("child" | "racer")) => {
            let port = args.next().expect("the harness's port");
            let port = port.parse().unwrap_or_else(|e| panic!("a port, not {port:?}: {e}"));
            // SAFETY: the descriptors its parent gave it, a holder of each socket.
            let (listener, socket) = unsafe { (TcpListener::from_raw_fd(LISTENER_FD), UdpSocket::from_raw_fd(DATAGRAM_FD)) };
            match role {
                "child" => serve("child", &listener, &socket, port),
                _ => println!("{}", race("racer", &listener, &socket, port)),
            }
            return;
        }
        Some(port) => port.parse().unwrap_or_else(|e| panic!("a port, not {port:?}: {e}")),
        None => panic!("the harness's datagram port is this job's argument"),
    };
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, LISTENING)).expect("a listener through netstack");
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a datagram socket through netstack");
    let spawn = |role: &str| {
        Command::new(SELF_PATH)
            .arg(role)
            .arg(port.to_string())
            .inherit_handle(LISTENER_FD as u32, std::os::toyos::io::AsHandle::as_handle(&listener))
            .inherit_handle(DATAGRAM_FD as u32, std::os::toyos::io::AsHandle::as_handle(&socket))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn the {role} with a holder of each socket: {e}"))
    };
    let status = spawn("child").wait().expect("wait for the child");
    assert!(status.success(), "the child ended {status:?}");
    serve("parent", &listener, &socket, port);
    println!("a listener and a datagram socket exported to a child: it accepted and spoke on them, and so did the parent after it");

    let mut racer = spawn("racer");
    let mine = race("parent", &listener, &socket, port);
    let mut theirs = String::new();
    BufReader::new(racer.stdout.take().expect("the racer's stdout")).read_line(&mut theirs).expect("the racer's datagram");
    assert!(racer.wait().expect("wait for the racer").success(), "the racer failed");
    let mut taken = [mine, theirs.trim().to_string()];
    taken.sort();
    assert_eq!(taken, ["one", "two"], "each process takes one datagram of the two");
    println!("two holders blocked on one queue: each took one connection and one datagram");
}

/// `who`'s part of the race: say it is about to accept, accept one connection
/// and write a line on it, say it is about to receive, and receive one
/// datagram, whose bytes it answers.
fn race(who: &str, listener: &TcpListener, socket: &UdpSocket, port: u16) -> String {
    let peer = SocketAddr::from((HOST, port));
    let say = |what: &str| {
        let line = format!("{who} {what}");
        socket.send_to(line.as_bytes(), peer).unwrap_or_else(|e| panic!("{who}'s datagram: {e}"));
    };
    say("racing");
    let (mut stream, _) = listener.accept().unwrap_or_else(|e| panic!("{who}'s accept in the race: {e}"));
    writeln!(stream, "accepted by a racer").unwrap_or_else(|e| panic!("{who}'s line: {e}"));
    drop(stream);
    say("receiving");
    let mut heard = [0u8; 64];
    let (n, _) = socket.recv_from(&mut heard).unwrap_or_else(|e| panic!("{who}'s receive in the race: {e}"));
    String::from_utf8_lossy(&heard[..n]).into_owned()
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
