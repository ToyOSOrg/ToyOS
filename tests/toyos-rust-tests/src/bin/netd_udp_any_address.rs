//! A UDP socket bound to the unspecified address receives the unicast reply to
//! what it sent, which is how every client that is not a server binds one: a
//! resolver's socket among them.
//!
//! Through `std::net::UdpSocket`, the path a Rust program takes. The reply is
//! the harness's UDP echo on `HOST` sending the datagram back to the address
//! it came from, which is this machine's leased address and not `0.0.0.0`.
//!
//! argv[1] is the port of the harness's host server, which this program does
//! not use; argv[2] is the port of the harness's UDP echo.
//! `netd_udp_any_address: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc;
use std::time::Duration;

use netd_stream::HOST;

/// How long netd may take to answer a receive whose datagram is on its way.
/// A bound, said by name; not a pace. std's `UdpSocket` does not honour a read
/// timeout, so the receive runs on a thread and this bounds the wait for it.
const WITHIN: Duration = Duration::from_secs(20);

fn main() {
    let echo: u16 = std::env::args()
        .nth(2)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_udp_any_address <host port> <echo port>");

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind the unspecified address");
    let to = SocketAddr::from((HOST, echo));
    let datagram: Vec<u8> = (0..200u8).collect();
    assert_eq!(socket.send_to(&datagram, to).expect("send to the echo"), datagram.len());

    let (answered, answer) = mpsc::channel();
    let receiver = socket.try_clone().expect("a second handle for the receive");
    std::thread::spawn(move || {
        let mut buf = [0u8; 512];
        answered.send(receiver.recv_from(&mut buf).map(|(n, from)| (buf[..n].to_vec(), from)))
    });
    let (got, from) = answer
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("a socket bound to 0.0.0.0 received no reply within {WITHIN:?}"))
        .expect("the receive");
    assert_eq!(from, to, "the reply came from somewhere other than the echo");
    assert_eq!(got, datagram, "the echo's reply is not the datagram sent");
    println!("netd_udp_any_address: ok");
}
