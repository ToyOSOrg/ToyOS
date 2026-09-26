//! A wake from any thread to the one thread that waits for it.
//!
//! A pipe whose bytes say nothing but "woken": a [`Waker`] writes one without
//! waiting, and the [`Bell`] is watched in a [`Poller`](crate::poller::Poller)
//! beside the thread's other handles, or waited on alone. Wakes coalesce, so
//! any number raised before the bell is taken are one.
//!
//! Take the bell before reading whatever the wakers announce: a wake raised
//! after the take is then still pending at the next wait, and one raised
//! before it is never lost.

use toyos_abi::syscall::SyscallError;
use toyos_abi::RawHandle;

use crate::{pipe_pair, AsHandle, Pipe};

/// A fresh bell and the waker that rings it.
pub fn pair() -> Result<(Waker, Bell), SyscallError> {
    let (read, write) = pipe_pair()?;
    Ok((Waker(write), Bell(read)))
}

/// Rings a [`Bell`] from any thread, and never waits.
pub struct Waker(Pipe);

impl Waker {
    pub fn wake(&self) {
        match self.0.write_nonblock(&[1]) {
            // A full pipe is a wake already pending, and a bell nobody holds
            // any more has nobody left to wake.
            Ok(_) | Err(SyscallError::WouldBlock) | Err(SyscallError::Gone) => {}
            Err(e) => panic!("Waker::wake: the wake pipe refused a write: {e:?}"),
        }
    }
}

/// The receiving end: readable while a wake is pending.
pub struct Bell(Pipe);

impl Bell {
    /// Take every wake raised so far, and say whether there was one.
    ///
    /// A bell whose every [`Waker`] is gone reads as ended, readable for good,
    /// and a wait on it would spin: that is refused here by name.
    pub fn take(&self) -> bool {
        let mut buf = [0u8; 64];
        let mut woken = false;
        loop {
            match self.0.read_nonblock(&mut buf) {
                Ok(0) => panic!("Bell::take: every waker is gone"),
                Ok(_) => woken = true,
                Err(SyscallError::WouldBlock) => return woken,
                Err(e) => panic!("Bell::take: the wake pipe refused a read: {e:?}"),
            }
        }
    }

    /// Block until a wake is pending, and take it with every other one.
    pub fn wait(&self) {
        let mut buf = [0u8; 64];
        match self.0.read(&mut buf) {
            Ok(0) => panic!("Bell::wait: every waker is gone"),
            Ok(_) => {}
            Err(e) => panic!("Bell::wait: the wake pipe refused a read: {e:?}"),
        }
        self.take();
    }
}

impl AsHandle for Bell {
    fn as_handle(&self) -> RawHandle {
        self.0.as_handle()
    }
}
