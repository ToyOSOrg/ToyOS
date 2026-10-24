//! A datagram receive with a read timeout ends at it, through std and the
//! real netstack, as `WouldBlock`, std's word on Unix for a receive timeout:
//! a receive that nothing answers does not hang, and the next one waits
//! again.
//!
//! Nothing sends to the socket: no peer is needed, and the datagram path is
//! the node's host tests'.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_millis(300);
/// A hang ceiling under QEMU, whose clock bounds nothing else.
const CEILING: Duration = Duration::from_secs(30);

fn main() {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a datagram socket");
    socket.set_read_timeout(Some(TIMEOUT)).expect("a read timeout");
    let mut buf = [0u8; 64];
    for receive in 1..=2 {
        let t = Instant::now();
        let got = socket.recv_from(&mut buf);
        let took = t.elapsed();
        let err = got.expect_err("nothing sent to the socket, and a receive answered a datagram");
        assert_eq!(err.kind(), ErrorKind::WouldBlock, "receive {receive} ended {err:?} after {took:?}");
        assert!(took >= TIMEOUT, "receive {receive} timed out after {took:?}, before its {TIMEOUT:?}");
        assert!(took < CEILING, "receive {receive} took {took:?}");
        println!("udp_recv_timeout: receive {receive} timed out after {} ms", took.as_millis());
    }
}
