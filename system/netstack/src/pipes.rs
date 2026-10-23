//! A holder's bell of a listener or a datagram socket, as the node's [`Bell`]: netstack's write
//! end of a pipe it made, whose read end only that holder is handed.
//!
//! **A ring is one non-blocking write that is never retried.** A full pipe is rung already, and
//! one whose reader is gone has a holder whose connection's hang-up reports its leaving.

use toyos::Pipe;
use toyos_abi::syscall::SyscallError;
use toyos_net_node::Bell;

pub struct Rung(Pipe);

impl Bell for Rung {
    fn ring(&mut self) {
        let _ = self.0.write_nonblock(&[1]);
    }
}

/// A new bell: the read end to hand its holder, and the end the node rings.
pub fn bell() -> Result<(Pipe, Rung), SyscallError> {
    let (read, write) = toyos::pipe_pair()?;
    Ok((read, Rung(write)))
}
