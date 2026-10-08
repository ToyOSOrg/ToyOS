//! What the host these tests run on does with `TCP_NODELAY` set on a listening socket: the
//! reading `listeners`' rule for a listener's option rests on, asked of every host the suite
//! runs on. Nothing of the node runs here: the sockets are the host kernel's, over loopback.
//! `tests/listeners.rs` holds the node to the same two answers
//! (`a_stream_starts_with_the_options_its_connection_took_from_its_listener`).

use std::net::{Ipv4Addr, TcpListener, TcpStream};

use socket2::SockRef;

/// Whether the socket accepted from a listener has `TCP_NODELAY`, the option set on the
/// listener `before` the connection began, or else once it was established and before the
/// accept. A connect that returned is a handshake that ended: the connection waits in the
/// listener's queue, and the accept takes it without waiting.
fn accepted_with_nodelay(before: bool) -> bool {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a loopback port");
    assert!(!SockRef::from(&listener).tcp_nodelay().expect("a listener answers for the option"));
    if before {
        SockRef::from(&listener).set_tcp_nodelay(true).expect("a listener takes the option");
    }
    let _peer = TcpStream::connect(listener.local_addr().expect("bound")).expect("the listener's queue takes it");
    if !before {
        SockRef::from(&listener).set_tcp_nodelay(true).expect("a listener takes the option");
    }
    let (accepted, _) = listener.accept().expect("the connection that waits");
    assert!(SockRef::from(&listener).tcp_nodelay().expect("a listener answers for the option"));
    accepted.nodelay().expect("an accepted socket answers for the option")
}

#[test]
fn a_host_gives_a_connection_the_nodelay_its_listener_had_when_it_began() {
    assert!(accepted_with_nodelay(true), "set before the connection began, the accepted socket has it");
    assert!(!accepted_with_nodelay(false), "set after the connection was established, it does not");
}
