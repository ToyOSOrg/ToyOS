//! Piped TCP connections that end with no close request from their client, as
//! a client that died leaves them: once netstack has let each connection go,
//! its stream count is what it was before the first, and so is its count of
//! sockets no table entry names.
//!
//! argv[1] is the port of the harness's host server, which ends every
//! connection it accepts at once. Each round reads the peer's end of stream,
//! drops both pipe ends, and waits for netstack's own count of live piped
//! connections to return.
//!
//! argv[2] is the port of a second, which holds every connection it accepts
//! and reads nothing. A client that shut its sending half down and left is one
//! whose send pipe netstack never reads again, so nothing but the kernel's
//! word on that pipe's other end can tell netstack it is gone: each such
//! connection is counted ownerless, with the pipe empty and with bytes in it.

use std::time::{Duration, Instant};

use toyos_inspect::{Value, NET};

/// The host, as QEMU's user network names it to a guest.
const HOST: [u8; 4] = [10, 0, 2, 2];

const ROUNDS: usize = 4;

/// A hang ceiling on netstack letting one ended connection go, or counting
/// one ownerless: it does the first on the pass after the peer acknowledges
/// its FIN, and the second on the pass the client's leaving wakes.
const LET_GO: Duration = Duration::from_secs(20);

fn count(key: &str) -> u64 {
    let snapshot = inspect::ask(NET).unwrap_or_else(|why| panic!("netstack's inspect answer: {why}"));
    match snapshot.get(key) {
        Some(Value::U64(n)) => *n,
        other => panic!("netstack's snapshot carries {key} as {other:?}"),
    }
}

/// A connection the peer holds open, whose client shuts its sending half down,
/// leaves `unread` in a send pipe netstack no longer reads, and is gone.
fn left_after_a_shutdown(port: u16, unread: &[u8]) {
    let ownerless = count("net.piped.ownerless");
    let conn = toyos::net::tcp_connect(HOST, port, 0).unwrap_or_else(|e| panic!("the holding server: {e:?}"));
    toyos::net::tcp_shutdown(conn.socket_id, 1).unwrap_or_else(|e| panic!("shutting the sending half: {e:?}"));
    if !unread.is_empty() {
        assert_eq!(conn.tx.write_nonblock(unread), Ok(unread.len()), "bytes into a send pipe nobody reads");
    }
    drop(conn);
    let asked = Instant::now();
    while count("net.piped.ownerless") != ownerless + 1 {
        assert!(
            asked.elapsed() < LET_GO,
            "netstack was not told a client that shut its sending half down, left {} byte(s) in its send pipe \
             and dropped both ends is gone",
            unread.len()
        );
    }
}

fn main() {
    let port = |at: usize| -> u16 {
        std::env::args()
            .nth(at)
            .and_then(|p| p.parse().ok())
            .expect("usage: netstack_socket_churn <ending host port> <holding host port>")
    };
    let (port, holding) = (port(1), port(2));
    let (streams, live) = (count("net.sockets.tcp"), count("net.piped.live"));
    let untabled = count("net.sockets.untabled");
    for round in 1..=ROUNDS {
        let conn = toyos::net::tcp_connect(HOST, port, 0)
            .unwrap_or_else(|e| panic!("connection {round} to the host server: {e:?}"));
        let mut byte = [0u8; 1];
        assert_eq!(conn.rx.read(&mut byte), Ok(0), "connection {round}: the host ends it without a byte");
        // Both pipe ends, and no close request.
        drop(conn);
        let asked = Instant::now();
        // Each question is a round trip netstack answers on a pass of its own.
        while count("net.piped.live") != live {
            assert!(
                asked.elapsed() < LET_GO,
                "netstack still counts connection {round} live {LET_GO:?} after both its ends closed"
            );
        }
    }
    let left = count("net.sockets.tcp");
    println!(
        "netstack_socket_churn: {ROUNDS} connections ended and let go; netstack held {streams} stream(s) before \
         them and holds {left} after"
    );
    assert_eq!(left, streams, "netstack keeps a stream for a connection it let go");
    assert_eq!(
        count("net.sockets.untabled"),
        untabled,
        "netstack keeps the socket of a connection whose table entry it let go"
    );
    left_after_a_shutdown(holding, &[]);
    left_after_a_shutdown(holding, b"never read");
    println!("netstack_socket_churn: two clients that shut down and left are ownerless");
    println!("netstack_socket_churn: ok");
}
