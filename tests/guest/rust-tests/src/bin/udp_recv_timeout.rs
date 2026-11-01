//! A datagram receive with a read timeout ends at it, through std and the
//! real netstack, as `WouldBlock`, std's word on Unix for a receive timeout:
//! a receive that nothing answers does not hang, and the next one waits
//! again. A timeout under a millisecond is still one: the protocol's
//! milliseconds read 0 as no bound, and std rounds such a timeout up to one,
//! never down to none. A zero timeout is refused `InvalidInput`, as std's
//! contract has it, by the one conversion every socket timeout and
//! `connect_timeout` takes.
//!
//! Nothing sends to the socket: no peer is needed, and the datagram path is
//! the node's host tests'.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

/// A hang ceiling under QEMU, whose clock bounds nothing else.
const CEILING: Duration = Duration::from_secs(30);

fn main() {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a datagram socket");
    let mut buf = [0u8; 64];
    let zero = |what: &str, got: std::io::Result<()>| {
        let err = got.expect_err(&format!("{what} took a zero timeout"));
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "{what} refused a zero timeout as {err:?}");
    };
    zero("set_read_timeout", socket.set_read_timeout(Some(Duration::ZERO)));
    zero("set_write_timeout", socket.set_write_timeout(Some(Duration::ZERO)));
    // Refused before anything is sent: the address answers nothing.
    let nowhere = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 9));
    zero("connect_timeout", TcpStream::connect_timeout(&nowhere, Duration::ZERO).map(drop));
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
