//! Whether a listener's owner is owed a wake, and what its accept finds.
//!
//! **A listener is one smoltcp socket that becomes the connection it
//! accepts**, so the port listens only while that socket is in `Listen`: one
//! that left it is handed to its owner or listens again, or the port answers
//! every other peer with a reset for the rest of the boot.
//!
//! **An accept spends the owner's wake whatever it answers, a refusal
//! included, and a wake is owed only for a connection there is room to
//! take.** An owner refused holds no wake, so the connection it left is
//! announced again, and an owner refused for room is not woken until room
//! returns.

use smoltcp::socket::tcp;

/// A listener's port, and whether its owner holds a wake it has not spent on
/// an accept.
pub struct Listening {
    port: u16,
    woken: bool,
}

/// What an accept finds, handed the pipes `P` its request carried.
#[derive(Debug, PartialEq, Eq)]
pub enum Accept<P> {
    /// A connection, to hand over on the pipes.
    Take(P),
    /// A request that carried no pipes, whatever waits.
    NoPipes,
    /// A connection, and no room to take it.
    NoRoom,
    /// No connection.
    Nothing,
}

impl Listening {
    pub fn new(port: u16) -> Self {
        Self { port, woken: false }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The bytes to write the owner for `socket`: one wake if a connection
    /// waits, there is `room` to take it, and the owner holds no wake, and
    /// none otherwise. The wake is held from here on, so the caller ends the
    /// listener if the owner is not handed it.
    pub fn wake(&mut self, socket: &mut tcp::Socket, room: bool) -> &'static [u8] {
        let owed = settle(socket, self.port) && room && !self.woken;
        self.woken |= owed;
        if owed { &[1] } else { &[] }
    }

    /// An accept, with `room` for another connection or not, and the pipes
    /// its request carried.
    pub fn accept<P>(&mut self, socket: &mut tcp::Socket, room: bool, pipes: Option<P>) -> Accept<P> {
        self.woken = false;
        match (settle(socket, self.port), room, pipes) {
            (_, _, None) => Accept::NoPipes,
            (false, _, Some(_)) => Accept::Nothing,
            (true, false, Some(_)) => Accept::NoRoom,
            (true, true, Some(pipes)) => Accept::Take(pipes),
        }
    }
}

/// Puts `socket` back to listening on `port` if its peer reset it before its
/// owner took it, and says whether it holds a connection: a handshake
/// finished, whatever the peer did since. Its FIN included, which can land in
/// the same pass as the handshake's last ACK, so no pass ever sees the socket
/// `Established`.
fn settle(socket: &mut tcp::Socket, port: u16) -> bool {
    match socket.state() {
        tcp::State::Listen | tcp::State::SynReceived => false,
        tcp::State::Established | tcp::State::CloseWait => true,
        tcp::State::Closed => {
            socket
                .listen(port)
                .unwrap_or_else(|e| panic!("netd: a closed socket refused to listen on {port}: {e:?}"));
            false
        }
        other => panic!("netd: a listener's socket is {other:?}, which only netd closing or connecting it reaches"),
    }
}

#[cfg(test)]
mod tests;
