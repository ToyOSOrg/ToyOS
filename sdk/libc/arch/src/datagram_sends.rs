//! What a datagram socket asks netstack when it sends: a socket never bound is
//! bound once, at any address to a port the stack chooses, before its first
//! datagram, and a bind refused sends nothing.

use crate::dgramreq::{send_to, Datagrams};

/// What a socket asked of netstack, in order.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    Bind([u8; 4], u16),
    Send(Vec<u8>, [u8; 4], u16),
}

/// A socket over a netstack that answers every call, or refuses binds.
struct Socket {
    held: bool,
    binds: bool,
    asked: Vec<Asked>,
}

impl Socket {
    fn new(held: bool) -> Self {
        Socket { held, binds: true, asked: Vec::new() }
    }
}

impl Datagrams for Socket {
    type Error = &'static str;
    fn held(&self) -> bool {
        self.held
    }
    fn bind(&mut self, ip: [u8; 4], port: u16) -> Result<(), Self::Error> {
        self.asked.push(Asked::Bind(ip, port));
        match self.binds {
            true => {
                self.held = true;
                Ok(())
            }
            false => Err("no port free"),
        }
    }
    fn send(&mut self, data: &[u8], ip: [u8; 4], port: u16) -> Result<usize, Self::Error> {
        self.asked.push(Asked::Send(data.to_vec(), ip, port));
        Ok(data.len())
    }
}

const PEER: [u8; 4] = [10, 0, 2, 2];

#[test]
fn a_socket_never_bound_is_bound_to_a_port_the_stack_chooses_before_its_first_send() {
    let mut socket = Socket::new(false);
    assert_eq!(send_to(&mut socket, b"ping", PEER, 7), Ok(4));
    assert_eq!(send_to(&mut socket, b"again", PEER, 7), Ok(5));
    assert_eq!(
        socket.asked,
        [Asked::Bind([0; 4], 0), Asked::Send(b"ping".to_vec(), PEER, 7), Asked::Send(b"again".to_vec(), PEER, 7)],
        "one bind, before the first send alone",
    );
}

#[test]
fn a_bound_socket_sends_with_no_bind() {
    let mut socket = Socket::new(true);
    assert_eq!(send_to(&mut socket, b"ping", PEER, 7), Ok(4));
    assert_eq!(socket.asked, [Asked::Send(b"ping".to_vec(), PEER, 7)]);
}

#[test]
fn a_refused_bind_sends_nothing() {
    let mut socket = Socket { binds: false, ..Socket::new(false) };
    assert_eq!(send_to(&mut socket, b"ping", PEER, 7), Err("no port free"));
    assert_eq!(socket.asked, [Asked::Bind([0; 4], 0)]);
}
