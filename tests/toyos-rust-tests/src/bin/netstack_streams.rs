//! Streams through netstack against a TCP nobody here wrote: the host
//! kernel's, behind QEMU's user network.
//!
//! argv[1] is the port of the harness's host server, which sends back every
//! byte it reads. The job writes [`BULK`] bytes of a sequence to it and shuts
//! its sending half down at once, while it reads them back, and compares each:
//! more than a pipe and both of the stack's buffers hold, so a byte lost,
//! repeated or moved where either side made the other wait shows as the first
//! that differs. It reads the bytes it sent and not the stream's end, which
//! std reads as a reset behind a shutdown
//! (`issues/a-netstack-client-cannot-tell-a-reset-from-the-peers-fin.md`).
//!
//! argv[2] is a port the job listens on. It says so and takes no connection
//! until its listener's pipe has given up two wakes: the harness dials twice,
//! so the second connection arrives while the first waits to be accepted, and
//! both are then accepted, answered with the byte each sent and closed, which
//! each peer reads as its stream's end.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpStream};
use std::time::{Duration, Instant};

use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::SyscallError;

/// The host, as QEMU's user network names it to a guest.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// Twice a kernel pipe, so the client's pipe fills behind the stack's window.
const BULK: usize = 4 * 1024 * 1024;

/// A hang ceiling on the two peers' wakes: the harness dials both as it reads
/// the line that says the listener waits.
const WOKEN: Duration = Duration::from_secs(60);

/// The sequence's byte at `at`: no period a buffer's or a segment's size
/// divides.
fn byte(at: usize) -> u8 {
    (at % 251) as u8 ^ (at / 251) as u8
}

fn bulk(port: u16) {
    let mut reading = TcpStream::connect((HOST, port)).unwrap_or_else(|e| panic!("the echoing server: {e}"));
    let mut writing = reading.try_clone().expect("a second handle on the stream");
    let writer = std::thread::spawn(move || {
        let mut sent = 0;
        let mut chunk = [0u8; 8192];
        while sent < BULK {
            let len = chunk.len().min(BULK - sent);
            for (i, b) in chunk[..len].iter_mut().enumerate() {
                *b = byte(sent + i);
            }
            writing.write_all(&chunk[..len]).unwrap_or_else(|e| panic!("writing byte {sent} of the bulk: {e}"));
            sent += len;
        }
        // At once: what the pipe still holds is the stack's to send first.
        writing.shutdown(Shutdown::Write).expect("shutting the sending half");
    });
    let mut read = 0;
    let mut chunk = [0u8; 8192];
    while read < BULK {
        let len = reading.read(&mut chunk).unwrap_or_else(|e| panic!("reading byte {read} of the bulk: {e}"));
        assert_ne!(len, 0, "the stream ended at byte {read} of the bulk");
        for (i, b) in chunk[..len].iter().enumerate() {
            assert_eq!(*b, byte(read + i), "byte {} came back as another", read + i);
        }
        read += len;
    }
    writer.join().expect("the writer");
    println!("netstack_streams: {BULK} bytes went out and came back as sent");
}

fn two_peers(port: u16) {
    let bound = toyos::net::tcp_bind([0, 0, 0, 0], port).unwrap_or_else(|e| panic!("listening on {port}: {e:?}"));
    println!("netstack_streams: the listener waits for two peers");
    let poller = Poller::new(1);
    let asked = Instant::now();
    let mut wakes = [0u8; 2];
    let mut woken = 0;
    while woken < wakes.len() {
        let Some(left) = WOKEN.checked_sub(asked.elapsed()) else {
            // Which half is missing: a connection that never finished its
            // handshake, or a wake for one that did.
            let waiting: Vec<_> = (0..wakes.len())
                .map(|_| toyos::net::tcp_accept(bound.socket_id).map(|accepted| accepted.remote_port))
                .collect();
            panic!("the listener was woken for {woken} of two peers in {WOKEN:?}, and two accepts then answered {waiting:?}");
        };
        poller.watch(&bound.notify, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
        match bound.notify.read_nonblock(&mut wakes[woken..]) {
            Ok(0) => panic!("netstack ended the listener after {woken} wake(s)"),
            Ok(n) => woken += n,
            Err(SyscallError::WouldBlock) => {}
            Err(e) => panic!("reading the listener's wakes: {e:?}"),
        }
    }
    let mut answered = Vec::new();
    for _ in 0..wakes.len() {
        let accepted = toyos::net::tcp_accept(bound.socket_id).unwrap_or_else(|e| panic!("an accept after its wake: {e:?}"));
        let mut said = [0u8; 1];
        assert_eq!(accepted.rx.read(&mut said), Ok(1), "a peer's one byte");
        assert_eq!(accepted.tx.write(&said), Ok(1), "its answer");
        answered.push(said[0]);
        toyos::net::tcp_close(accepted.socket_id).expect("closing an accepted stream");
    }
    toyos::net::tcp_close(bound.socket_id).expect("closing the listener");
    answered.sort_unstable();
    assert_eq!(answered, *b"12", "each peer was accepted once");
    println!("netstack_streams: two peers that arrived before any accept were both accepted");
}

fn main() {
    let port = |at: usize| -> u16 {
        std::env::args()
            .nth(at)
            .and_then(|p| p.parse().ok())
            .expect("usage: netstack_streams <echoing host port> <guest port to listen on>")
    };
    bulk(port(1));
    two_peers(port(2));
    println!("netstack_streams: ok");
}
