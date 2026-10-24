//! A datagram receive with a read timeout ends at it, through std and the
//! real netstack, as `WouldBlock`, std's word on Unix for a receive timeout:
//! a receive that nothing answers does not hang, and the next one waits
//! again. A timeout under a millisecond is still one: the protocol's
//! milliseconds read 0 as no bound, and std rounds such a timeout up to one,
//! never down to none.
//!
//! Nothing sends to the socket: no peer is needed, and the datagram path is
//! the node's host tests'.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, Instant};

/// A hang ceiling under QEMU, whose clock bounds nothing else.
const CEILING: Duration = Duration::from_secs(30);

fn main() {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a datagram socket");
    let mut buf = [0u8; 64];
    // 300 ms twice, then 500 us, which a truncation to whole milliseconds
    // would make an unbounded wait.
    for (receive, timeout) in [Duration::from_millis(300), Duration::from_millis(300), Duration::from_micros(500)].into_iter().enumerate() {
        socket.set_read_timeout(Some(timeout)).expect("a read timeout");
        let t = Instant::now();
        let got = socket.recv_from(&mut buf);
        let took = t.elapsed();
        let err = got.expect_err("nothing sent to the socket, and a receive answered a datagram");
        assert_eq!(err.kind(), ErrorKind::WouldBlock, "receive {receive} under {timeout:?} ended {err:?} after {took:?}");
        assert!(took >= timeout, "receive {receive} timed out after {took:?}, before its {timeout:?}");
        assert!(took < CEILING, "receive {receive} under {timeout:?} took {took:?}");
        println!("udp_recv_timeout: receive {receive} under {timeout:?} timed out after {took:?}");
    }
}
