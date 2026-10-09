//! What the host these tests run on does with `TCP_NODELAY` set on a listening socket: the
//! reading `listeners`' rule for a listener's option rests on, asked of every host the suite
//! runs on. Nothing of the node runs here: the sockets are the host kernel's, over loopback.
//! `tests/listeners.rs` holds the node to the same two answers
//! (`a_stream_starts_with_the_options_its_connection_took_from_its_listener`).
//!
//! **The second answer waits on the listener, not on the connect.** A connect returns when the
//! SYN-ACK arrives; the listener's end is established by the ACK after it, and a host may copy
//! the option then. So the option is set only once the listener is readable, which is the host
//! saying a connection waits to be accepted.

use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpStream};
use std::time::{Duration, Instant};

use mio::net::TcpListener;
use mio::{Events, Interest, Poll, Token};
use socket2::SockRef;

/// How long a loopback handshake may take before the test fails.
const CEILING: Duration = Duration::from_secs(60);

/// Whether the socket accepted from a listener has `TCP_NODELAY`, the option set on the
/// listener `before` the connection began, or else once it waited to be accepted.
fn accepted_with_nodelay(before: bool) -> bool {
    let mut listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0).into()).expect("a loopback port");
    let mut poll = Poll::new().expect("the host's poller");
    poll.registry().register(&mut listener, Token(0), Interest::READABLE).expect("a listener is a source");
    assert!(!SockRef::from(&listener).tcp_nodelay().expect("a listener answers for the option"));
    if before {
        SockRef::from(&listener).set_tcp_nodelay(true).expect("a listener takes the option");
    }
    let _peer = TcpStream::connect(listener.local_addr().expect("bound")).expect("the listener's queue takes it");
    let (mut events, start) = (Events::with_capacity(1), Instant::now());
    while events.is_empty() {
        let left = CEILING.checked_sub(start.elapsed()).expect("the connection waits at its listener within the ceiling");
        match poll.poll(&mut events, Some(left)) {
            Err(interrupted) if interrupted.kind() == ErrorKind::Interrupted => {}
            polled => polled.expect("the host's poller"),
        }
    }
    if !before {
        SockRef::from(&listener).set_tcp_nodelay(true).expect("a listener takes the option");
    }
    let (accepted, _) = listener.accept().expect("the connection that waits");
    assert!(SockRef::from(&listener).tcp_nodelay().expect("a listener answers for the option"));
    accepted.nodelay().expect("an accepted socket answers for the option")
}

#[test]
fn a_host_gives_a_connection_the_nodelay_its_listener_had_when_it_began() {
    let answers = (accepted_with_nodelay(true), accepted_with_nodelay(false));
    assert_eq!(answers, (true, false), "{}: (set before the connection began, set once it waited to be accepted)", std::env::consts::OS);
}
