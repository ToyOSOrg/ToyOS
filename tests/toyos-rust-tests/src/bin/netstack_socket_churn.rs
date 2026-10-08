//! Piped TCP connections that end with no close request from their client, as
//! a client that died leaves them: once netstack has let each connection go,
//! its stream count is what it was before the first.
//!
//! argv[1] is the port of the harness's host server, which ends every
//! connection it accepts at once. Each round reads the peer's end of stream,
//! drops both pipe ends, and waits for netstack's own count of live piped
//! connections to return.

use std::time::{Duration, Instant};

use toyos_inspect::{Value, NET};

/// The host, as QEMU's user network names it to a guest.
const HOST: [u8; 4] = [10, 0, 2, 2];

const ROUNDS: usize = 4;

/// A hang ceiling on netstack letting one ended connection go: it does so on
/// the pass after the peer acknowledges its FIN.
const LET_GO: Duration = Duration::from_secs(20);

fn count(key: &str) -> u64 {
    let snapshot = inspect::ask(NET).unwrap_or_else(|why| panic!("netstack's inspect answer: {why}"));
    match snapshot.get(key) {
        Some(Value::U64(n)) => *n,
        other => panic!("netstack's snapshot carries {key} as {other:?}"),
    }
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netstack_socket_churn <host port>");
    let (streams, live) = (count("net.sockets.tcp"), count("net.piped.live"));
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
    println!("netstack_socket_churn: ok");
}
