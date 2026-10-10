//! The kernel's pipe ends as the node's [`ToClient`], [`FromClient`] and
//! [`Wake`]: one non-blocking call each, and the kernel's refusal in the
//! node's word for it.
//!
//! **The node owns an end and netstack only watches it.** [`hold`] splits a
//! pipe end into the [`Held`] the node is handed and the [`Watched`] netstack
//! keeps: the handle closes when the node drops its half, which is how a
//! client reads the end of its stream or finds its writes refused, and from
//! then the watched half names nothing.
//!
//! **Untrusted input.** An end is whatever handle its client moved, and
//! nothing checks its kind at intake: one that is no pipe end, or the wrong
//! end, answers a refusal here that is neither a full pipe nor a vanished
//! reader, and the node ends that client's stream or listener for it.

use std::rc::{Rc, Weak};

use toyos::Pipe;
use toyos_abi::syscall::SyscallError;
use toyos_net_node::{FromClient, ReadRefusal, ToClient, Wake, WriteRefusal};

/// The half the node holds: the handle lives as long as this does.
pub struct Held(Rc<Pipe>);

/// The half netstack watches by.
pub struct Watched(Weak<Pipe>);

pub fn hold(pipe: Pipe) -> (Held, Watched) {
    let pipe = Rc::new(pipe);
    let watched = Watched(Rc::downgrade(&pipe));
    (Held(pipe), watched)
}

impl Watched {
    /// The end, while the node holds it.
    pub fn held(&self) -> Option<Rc<Pipe>> {
        self.0.upgrade()
    }
}

fn write_refused(why: SyscallError) -> WriteRefusal {
    match why {
        SyscallError::WouldBlock => WriteRefusal::Full,
        SyscallError::Gone => WriteRefusal::Gone,
        _ => WriteRefusal::Broken,
    }
}

impl ToClient for Held {
    fn write(&mut self, bytes: &[u8]) -> Result<usize, WriteRefusal> {
        crate::prof::add(4, 1);
        let r = self.0.write_nonblock(bytes).map_err(write_refused);
        match r {
            Ok(n) => crate::prof::add(5, n as u64),
            Err(_) => crate::prof::add(6, 1),
        }
        r
    }
}

impl FromClient for Held {
    fn read(&mut self, out: &mut [u8]) -> Result<usize, ReadRefusal> {
        crate::prof::add(7, 1);
        self.0.read_nonblock(out).map_err(|why| match why {
            SyscallError::WouldBlock => {
                crate::prof::add(8, 1);
                ReadRefusal::Empty
            }
            _ => ReadRefusal::Broken,
        })
    }
}

impl Wake for Held {
    fn wake(&mut self) -> Result<(), WriteRefusal> {
        match self.0.write_nonblock(&[1]) {
            Ok(1) => Ok(()),
            Ok(_) => Err(WriteRefusal::Full),
            Err(why) => Err(write_refused(why)),
        }
    }
}
