//! `TCP_NODELAY` as std answers it and as netstack's pipe ABI carries a
//! listener's.
//!
//! The first half is std's alone: a stream and its duplicate are one socket,
//! so a set on either is read on both, and a stream accepted from a listener
//! that holds no option has none.
//!
//! The second is ToyOS's: a listener made by `toyos::net`, whose option std
//! has no call for. A connection that waited when the option was set has not
//! got it, and the next one dialled has; the wake netstack writes the owner
//! is what says the first one waits.
//!
//! argv: the address and port of a host that accepts and holds each
//! connection, then the port std listens on and the port the pipe ABI's
//! listener does. That host dials each once at WAITING, and the second again
//! at AGAIN.

use std::net::{TcpListener, TcpStream};

const WAITING: &str = "nodelay_accepted: both listeners wait for a peer";
const AGAIN: &str = "nodelay_accepted: the listener waits for a second peer";

struct Said(u32);

impl Said {
    fn said<T: std::fmt::Debug + PartialEq>(&mut self, what: &str, got: T, want: T) {
        let wrong = got != want;
        println!("{what}: {got:?}{}", if wrong { "  <-- WRONG" } else { "" });
        self.0 += u32::from(wrong);
    }
}

/// The listener `toyos::net` binds on `port`: one connection that waits before
/// the option is set, and one dialled after.
fn the_pipe_abis_listener(port: u16, said: &mut Said) {
    use toyos::net::{self, NetError, OPT_NODELAY};

    let listener = net::tcp_bind([0; 4], port).unwrap_or_else(|e| panic!("a listener on {port}: {e:?}"));
    println!("{WAITING}");
    let mut wake = [0u8; 1];
    assert_eq!(listener.notify.read(&mut wake), Ok(1), "the wake for the first connection");
    said.said("set on a listener a connection waits at", net::tcp_listener_set_option(listener.socket_id, OPT_NODELAY, 1), Ok(()));
    let before = net::tcp_accept(listener.socket_id).unwrap_or_else(|e| panic!("the first accept: {e:?}"));
    said.said("the connection that waited before the set", before.options.nodelay(), false);
    said.said(
        "a listener's request that names a stream",
        net::tcp_listener_set_option(before.socket_id, OPT_NODELAY, 1),
        Err(NetError::NotConnected),
    );
    said.said(
        "a stream's request that names a listener",
        net::tcp_set_option(listener.socket_id, OPT_NODELAY, 1),
        Err(NetError::NotConnected),
    );
    said.said(
        "an option a listener has none of",
        net::tcp_listener_set_option(listener.socket_id, net::OPT_BROADCAST, 1),
        Err(NetError::InvalidInput),
    );
    println!("{AGAIN}");
    assert_eq!(listener.notify.read(&mut wake), Ok(1), "the wake for the second connection");
    let after = net::tcp_accept(listener.socket_id).unwrap_or_else(|e| panic!("the second accept: {e:?}"));
    said.said("the connection dialled after the set", after.options.nodelay(), true);
    for id in [before.socket_id, after.socket_id, listener.socket_id] {
        net::tcp_close(id).unwrap_or_else(|e| panic!("closing {id:?}: {e:?}"));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, host, holding, std_port, pipe_port] = &args[..] else {
        panic!("usage: nodelay_accepted <host> <holding port> <std's listener port> <the pipe ABI's listener port>")
    };
    let port = |text: &String| -> u16 { text.parse().unwrap_or_else(|e| panic!("the port {text}: {e}")) };
    let mut said = Said(0);

    let stream = TcpStream::connect((host.as_str(), port(holding))).expect("the holding server");
    let duplicate = stream.try_clone().expect("a duplicate");
    said.said("a fresh stream's nodelay", stream.nodelay().ok(), Some(false));
    said.said("set on the stream", stream.set_nodelay(true).is_ok(), true);
    said.said("read from its duplicate", duplicate.nodelay().ok(), Some(true));
    said.said("cleared on the duplicate", duplicate.set_nodelay(false).is_ok(), true);
    said.said("read from the stream", stream.nodelay().ok(), Some(false));

    let listener = TcpListener::bind(("0.0.0.0", port(std_port))).expect("std's listener");
    the_pipe_abis_listener(port(pipe_port), &mut said);
    let (accepted, _) = listener.accept().expect("std's accept");
    said.said("a stream accepted from a listener that holds none", accepted.nodelay().ok(), Some(false));

    if said.0 != 0 {
        println!("nodelay_accepted: {} wrong", said.0);
        std::process::exit(1);
    }
    println!("nodelay_accepted: ok");
}
