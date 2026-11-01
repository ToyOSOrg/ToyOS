//! A loopback stream carries 4 MiB each way, every byte where it was
//! written, while its rings grow: each send ring at the first write its room
//! cannot take, here the third, and each receive ring as [tcp] asks for more
//! buffer, every page committed by the end that touches it before it does. A
//! missing commit ends the process or netstack; a byte laid out by the wrong
//! size arrives out of place.
//!
//! It needs the guest: the commits are the kernel's, and the receive ring's
//! growth is the real netstack's. `netstack_holders_exported_to_children`'s
//! boot runs it, on tests/images/netcase.toml.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::thread;

const BYTES: usize = 4 << 20;
/// Write sizes in turn: a byte, a part of a ring, one ring and more than one,
/// so a write meets a full ring, a partial room and a growth pending.
const WRITES: [usize; 4] = [1, 1000, 64 << 10, 300 << 10];

/// The byte at stream position `at`, which repeats at no power of two a ring
/// could be.
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
    stream.shutdown(Shutdown::Write).expect("the sender's FIN");
}

fn receive(mut stream: &TcpStream, way: &str) {
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
            panic!("{way}: the byte at {} is {:#04x}, written {:#04x}", at + wrong, buf[wrong], byte(at + wrong));
        }
        at += n;
    }
    assert_eq!(at, BYTES, "{way}: the stream ended after {at} bytes");
}

fn main() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback listener");
    let addr = listener.local_addr().expect("the listener's address");
    let connected = TcpStream::connect(addr).expect("a loopback connect");
    let (accepted, _) = listener.accept().expect("the connect's accept");
    thread::scope(|scope| {
        for (way, from, to) in [("connected to accepted", &connected, &accepted), ("accepted to connected", &accepted, &connected)] {
            scope.spawn(move || send(from));
            scope.spawn(move || receive(to, way));
        }
    });
    println!("  PASS: 4 MiB each way arrived in order through its rings' growth");
    println!("stream_grows: a stream's rings grow under load and keep every byte in place");
}
