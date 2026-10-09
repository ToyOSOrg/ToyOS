//! Piped TCP connections that end with no close request from their client, as
//! a client that died leaves them: once netstack has let each connection go,
//! its stream count is what it was before the first, and so is its count of
//! places held, once the stack has finished each connection alone.
//!
//! argv[1] is the port of the harness's host server, which ends every
//! connection it accepts at once. Each round reads the peer's end of stream,
//! drops both pipe ends, and waits for netstack's own count of live piped
//! connections to return.
//!
//! argv[2] is the port of a second, which holds every connection it accepts
//! and reads nothing. On it, a client that moves netstack a handle that is no
//! pipe end where its receive end belongs, and keeps its send end: the kernel
//! refuses netstack's watch of it, and that refusal is all that ends the
//! connection.
//!
//! On the same server, a client that shut its sending half down and left:
//! netstack let its send pipe go at the shutdown and its peer sends nothing,
//! so nothing but the kernel's word on its receive pipe's other end can tell
//! netstack it is gone.
//!
//! A listener and a datagram socket whose owner drops its pipe ends with no
//! close request, and no peer near either: the kernel's word on the pipe's
//! other end is all that ends them, and netstack's count of each returns.
//!
//! argv[3] is the port of a third, which answers each datagram with itself. A
//! receive is asked of netstack before any datagram has been sent, so it waits
//! there, and is answered by the datagram the server sends back; a second
//! receive on that socket while the first waits is refused.
//!
//! Last, connects to the holding server until netstack refuses one: it holds
//! as many places as it said it has, and the connect past them is answered
//! `ResourceExhausted`. Last because the stack keeps each of those
//! connections' places until it has finished them with a peer that never
//! ends its half.

use std::time::{Duration, Instant};

use toyos::net::{
    MsgType, NetError, NetstackConn, TcpConnectPipedRequest, TcpConnectResponse, UdpRecvFromRequest, UdpRecvResponse,
};
use toyos::OwnedHandle;
use toyos_inspect::{Value, NET};

/// The host, as QEMU's user network names it to a guest.
const HOST: [u8; 4] = [10, 0, 2, 2];

const ROUNDS: usize = 4;

/// A hang ceiling on netstack letting one ended connection go: it does so on
/// the pass the client's leaving wakes, and the stack finishes the connection
/// when the peer acknowledges its FIN.
const LET_GO: Duration = Duration::from_secs(20);

fn count(key: &str) -> u64 {
    let snapshot = inspect::ask(NET).unwrap_or_else(|why| panic!("netstack's inspect answer: {why}"));
    match snapshot.get(key) {
        Some(Value::U64(n)) => *n,
        other => panic!("netstack's snapshot carries {key} as {other:?}"),
    }
}

/// Asks netstack for `key` until it is `wanted`: each question is a round
/// trip netstack answers on a pass of its own.
fn returns(key: &str, wanted: u64, what: &str) {
    let asked = Instant::now();
    loop {
        let found = count(key);
        if found == wanted {
            return;
        }
        assert!(asked.elapsed() < LET_GO, "netstack's {key} is {found} and not {wanted} {LET_GO:?} after {what}");
    }
}

/// A connection the peer holds open, whose client shuts its sending half down
/// and is gone.
fn left_after_a_shutdown(port: u16) {
    let live = count("net.piped.live");
    let conn = toyos::net::tcp_connect(HOST, port, 0).unwrap_or_else(|e| panic!("the holding server: {e:?}"));
    assert_eq!(count("net.piped.live"), live + 1, "netstack counts the connection it just answered");
    toyos::net::tcp_shutdown(conn.socket_id, 1).unwrap_or_else(|e| panic!("shutting the sending half: {e:?}"));
    drop(conn);
    returns("net.piped.live", live, "a client that shut its sending half down dropped both its ends");
}

fn owners_that_left_are_let_go() {
    let (listeners, udp) = (count("net.sockets.listeners"), count("net.sockets.udp"));
    let listener = toyos::net::tcp_bind([0, 0, 0, 0], 0).unwrap_or_else(|e| panic!("a listener: {e:?}"));
    let socket = toyos::net::udp_bind([0, 0, 0, 0], 0).unwrap_or_else(|e| panic!("a datagram socket: {e:?}"));
    assert_eq!(count("net.sockets.listeners"), listeners + 1, "netstack counts the listener it just answered");
    assert_eq!(count("net.sockets.udp"), udp + 1, "netstack counts the datagram socket it just answered");
    // Their pipe ends, and no close request.
    drop(listener);
    drop(socket);
    returns("net.sockets.listeners", listeners, "a listener's owner dropped its wake pipe");
    returns("net.sockets.udp", udp, "a datagram socket's owner dropped its pipes");
}

fn a_receive_that_waits_is_answered(port: u16) {
    const SAID: &[u8] = b"a datagram for a receive that waits";
    let socket = toyos::net::udp_bind([0, 0, 0, 0], 0).unwrap_or_else(|e| panic!("a datagram socket: {e:?}"));
    let waiting = NetstackConn::connect()
        .and_then(|netstack| {
            netstack.request(MsgType::UdpRecvFrom, &UdpRecvFromRequest { socket_id: socket.socket_id.0, max_len: 64 })
        })
        .unwrap_or_else(|e| panic!("asking for a datagram: {e:?}"));
    // netstack reads its clients' requests in the order they connected, so
    // this answer says the receive above is waiting there.
    count("net.sockets.udp");
    let second = toyos::net::udp_recv_from(socket.socket_id, 64);
    assert_eq!(second.err(), Some(NetError::ResourceExhausted), "a second receive on a socket whose first waits");
    assert_eq!(socket.tx.write_nonblock(SAID), Ok(SAID.len()), "a datagram into the socket's send pipe");
    toyos::net::udp_send_to(socket.socket_id, HOST, port, SAID.len() as u16)
        .unwrap_or_else(|e| panic!("sending to the answering server: {e:?}"));
    let answer: UdpRecvResponse = waiting.response().unwrap_or_else(|e| panic!("the receive that waited: {e:?}"));
    assert_eq!((answer.addr, answer.port, usize::from(answer.len)), (HOST, port, SAID.len()), "the answer's source and length");
    let mut back = [0u8; 64];
    assert_eq!(socket.rx.read(&mut back), Ok(SAID.len()), "the answer's bytes in the socket's receive pipe");
    assert_eq!(&back[..SAID.len()], SAID);
    toyos::net::udp_close(socket.socket_id).unwrap_or_else(|e| panic!("closing the datagram socket: {e:?}"));
}

fn a_connect_past_the_places_is_refused(port: u16) {
    let (max, held) = (count("net.places.max"), count("net.places.held"));
    let mut kept = Vec::new();
    let refused = loop {
        match toyos::net::tcp_connect(HOST, port, 0) {
            Ok(conn) => kept.push(conn),
            Err(refusal) => break refusal,
        }
        assert!(kept.len() as u64 <= max, "netstack holds more connections than the {max} places it said it has");
    };
    assert_eq!(refused, NetError::ResourceExhausted, "the connect past netstack's places");
    assert_eq!(
        kept.len() as u64,
        max - held,
        "netstack said it has {max} places with {held} held, and refused the connect after {} more",
        kept.len()
    );
    assert_eq!(count("net.places.held"), max, "every place is held where the connect was refused");
}

/// A connection the peer holds open, whose client keeps its send end and moved
/// netstack an acceptor for the end netstack writes the peer's bytes into. The
/// peer sends none, so netstack never writes it: its watch is refused, and the
/// connection is reset and let go.
fn a_receive_end_that_is_no_pipe_end_is_refused(port: u16) {
    let live = count("net.piped.live");
    let (acceptor, _connector) = toyos::port::create().expect("a port");
    // SAFETY: the acceptor's one handle, which nothing else answers for.
    let no_pipe_end = unsafe { OwnedHandle::from_raw(acceptor.into_raw()) };
    let (from_client, kept) = toyos::pipe_pair().expect("a pipe");
    let connected: TcpConnectResponse = NetstackConn::connect()
        .and_then(|netstack| {
            netstack.request_with_handles(
                [no_pipe_end, OwnedHandle::from(from_client)],
                MsgType::TcpConnectPiped,
                &TcpConnectPipedRequest { addr: HOST, port, _pad: 0, timeout_ms: 0 },
            )
        })
        .and_then(|pending| pending.response())
        .unwrap_or_else(|e| panic!("the holding server, with an acceptor for a receive end: {e:?}"));
    returns(
        "net.piped.live",
        live,
        &format!("the kernel refused the watch of connection {}'s receive end, which is no pipe end", connected.socket_id),
    );
    drop(kept);
}

fn main() {
    let port = |at: usize| -> u16 {
        std::env::args()
            .nth(at)
            .and_then(|p| p.parse().ok())
            .expect("usage: netstack_socket_churn <ending host port> <holding host port> <answering host port>")
    };
    let (port, holding, answering) = (port(1), port(2), port(3));
    let (streams, live) = (count("net.sockets.tcp"), count("net.piped.live"));
    let held = count("net.places.held");
    for round in 1..=ROUNDS {
        let conn = toyos::net::tcp_connect(HOST, port, 0)
            .unwrap_or_else(|e| panic!("connection {round} to the host server: {e:?}"));
        let mut byte = [0u8; 1];
        assert_eq!(conn.rx.read(&mut byte), Ok(0), "connection {round}: the host ends it without a byte");
        // Both pipe ends, and no close request.
        drop(conn);
        returns("net.piped.live", live, &format!("both ends of connection {round} closed"));
    }
    let left = count("net.sockets.tcp");
    println!(
        "netstack_socket_churn: {ROUNDS} connections ended and let go; netstack held {streams} stream(s) before \
         them and holds {left} after"
    );
    assert_eq!(left, streams, "netstack keeps a stream for a connection it let go");
    returns("net.places.held", held, "the last of the connections it let go ended on the wire");
    a_receive_end_that_is_no_pipe_end_is_refused(holding);
    println!("netstack_socket_churn: a connection whose receive end is no pipe end was reset");
    left_after_a_shutdown(holding);
    println!("netstack_socket_churn: a client that shut down and left was let go");
    owners_that_left_are_let_go();
    println!("netstack_socket_churn: a listener and a datagram socket whose owner left were let go");
    a_receive_that_waits_is_answered(answering);
    println!("netstack_socket_churn: a receive that waited was answered by its datagram");
    a_connect_past_the_places_is_refused(holding);
    println!("netstack_socket_churn: the connect past netstack's places was refused");
    println!("netstack_socket_churn: ok");
}
