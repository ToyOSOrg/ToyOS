//! A holder's bell of a listener or a datagram socket, as the node's [`Bell`]: netstack's
//! waker of an event it made, whose bell only that holder is handed.
//!
//! **A ring never waits and is never retried.** A holder whose bell is gone has a connection
//! whose hang-up reports its leaving.

use toyos::wake::{self, Bell as Handed, Waker};
use toyos_abi::syscall::SyscallError;
use toyos_net_node::Bell;

pub struct Rung(Waker);

impl Bell for Rung {
    fn ring(&mut self) {
        self.0.wake();
    }
}

/// A new bell: the side to hand its holder, and the side the node rings.
pub fn bell() -> Result<(Handed, Rung), SyscallError> {
    let (waker, bell) = wake::pair()?;
    Ok((bell, Rung(waker)))
}
