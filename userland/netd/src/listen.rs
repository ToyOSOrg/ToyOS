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

/// What an accept finds.
#[derive(Debug, PartialEq, Eq)]
pub enum Accept {
    /// A connection, to hand over.
    Take,
    /// No room for another connection, whether one waits or not.
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

    /// Whether the owner is owed a wake for `socket`: a connection waits,
    /// there is `room` to take it, and the owner holds no wake.
    pub fn owes_wake(&self, socket: &mut tcp::Socket, room: bool) -> bool {
        settle(socket, self.port) && room && !self.woken
    }

    pub fn woke(&mut self) {
        self.woken = true;
    }

    /// An accept, with `room` for another connection or not.
    pub fn accept(&mut self, socket: &mut tcp::Socket, room: bool) -> Accept {
        self.woken = false;
        match (settle(socket, self.port), room) {
            (_, false) => Accept::NoRoom,
            (true, true) => Accept::Take,
            (false, true) => Accept::Nothing,
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
