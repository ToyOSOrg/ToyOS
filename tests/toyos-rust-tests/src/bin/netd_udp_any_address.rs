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

use netd_stream::HOST;

fn main() {
    let echo: u16 = std::env::args()
        .nth(2)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_udp_any_address <host port> <echo port>");

    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind the unspecified address");
    let to = SocketAddr::from((HOST, echo));
    let datagram: Vec<u8> = (0..200u8).collect();
    assert_eq!(socket.send_to(&datagram, to).expect("send to the echo"), datagram.len());

    // No deadline: a reply that never comes is a hang the harness ceiling reds.
    let mut buf = [0u8; 512];
    let (n, from) = socket.recv_from(&mut buf).expect("the receive");
    let got = &buf[..n];
    assert_eq!(from, to, "the reply came from somewhere other than the echo");
    assert_eq!(got, &datagram[..], "the echo's reply is not the datagram sent");
    println!("netd_udp_any_address: ok");
}
