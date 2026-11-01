//! A stream carries 4 MiB each way, every byte where it was written, while
//! this end's send ring grows, at the first write its room cannot take, here
//! the third: each page committed by this process before it writes it, and
//! by netstack before it reads it. A missing commit ends this process or
//! netstack; a byte laid out by the wrong size arrives out of place. The
//! receive ring grows only as [tcp] wants more buffer, which QEMU's user
//! network's round trips seldom ask for: the node's host test
//! `a_download_a_reader_keeps_up_with_grows_the_receive_ring_and_keeps_every_byte`
//! holds that growth.
//!
//! It needs the guest: the commits and the refused touch are the kernel's.
//! ToyOS has no loopback, so the harness is the peer, as for `socket_export`:
//! this job's argument is the port of its datagram socket, which QEMU's user
//! network reaches as `10.0.2.2`. The job listens on the port the boot
//! forwards, says it is ready in a datagram, accepts the harness's
//! connection, and both ends send [`BYTES`] and check what they receive.
//! `netstack_holders_exported_to_children`'s boot runs it, on
//! tests/images/netcase.toml.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::thread;

/// QEMU's user network's address of its host.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
/// The port the boot forwards to (`tests/toyos.rs`'s `SOCKET_EXPORT_PORT`).
const LISTENING: u16 = 7070;
/// What each end sends, as `tests/toyos.rs`'s `STREAM_GROWS_BYTES`.
const BYTES: usize = 4 << 20;
/// Write sizes in turn: a byte, a part of a ring, one ring and more than one,
/// so a write meets a partial room, a full ring and a growth pending.
const WRITES: [usize; 4] = [1, 1000, 64 << 10, 300 << 10];

/// The byte at stream position `at`, which repeats at no power of two a ring
/// could be: `tests/toyos.rs`'s `stream_grows_byte`.
fn byte(at: usize) -> u8 {
    ((at as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 56) as u8
}

fn send(mut stream: &TcpStream) {
    let all: Vec<u8> = (0..BYTES).map(byte).collect();
    let mut at = 0;
    for size in WRITES.iter().cycle() {
        if at == BYTES {
            break;
        }
        let end = (at + size).min(BYTES);
        stream.write_all(&all[at..end]).expect("a write the stream takes");
        at = end;
    }
    stream.shutdown(Shutdown::Write).expect("this end's FIN");
}

fn receive(mut stream: &TcpStream) {
    let mut buf = vec![0u8; 200 << 10];
    let mut at = 0;
    loop {
        // Reads of a varying size: some take less than is ready.
        let want = [7usize, 4096, 200 << 10][at % 3];
        let n = stream.read(&mut buf[..want]).expect("a read the stream answers");
        if n == 0 {
            break;
        }
        if let Some(wrong) = (0..n).find(|&i| buf[i] != byte(at + i)) {
            panic!("the byte at {} is {:#04x}, sent {:#04x}", at + wrong, buf[wrong], byte(at + wrong));
        }
        at += n;
    }
    assert_eq!(at, BYTES, "the stream ended after {at} bytes");
}

fn main() {
    let port: u16 = std::env::args().nth(1).expect("the harness's datagram port").parse().expect("a port");
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, LISTENING)).expect("a listener through netstack");
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a datagram socket through netstack");
    socket.send_to(b"grows ready", SocketAddr::from((HOST, port))).expect("the ready datagram");
    let (stream, _) = listener.accept().expect("the harness's connection");
    thread::scope(|scope| {
        scope.spawn(|| send(&stream));
        receive(&stream);
    });
    println!("  PASS: 4 MiB each way arrived in order through the send ring's growth");
    println!("stream_grows: a stream's send ring grows under load and keeps every byte in place");
}
