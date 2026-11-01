//! A wake from any thread or process to whoever holds the bell.
//!
//! An event object: a [`Waker`] raises it without waiting, and the [`Bell`] is
//! watched in a [`Poller`](crate::poller::Poller) beside the holder's other
//! handles, or waited on alone. Wakes coalesce, so any number raised before
//! the bell is taken are one. A readable watch takes nothing.
//!
//! Take the bell before reading whatever the wakers announce: a wake raised
//! after the take is then still pending at the next wait, and one raised
//! before it is never lost.
//!
//! Each side sees the other's last handle go: a bell whose every waker is gone
//! is readable for good and its take answers [`Hungup`], never a wake; a
//! waker whose bell is gone rings into [`Hungup`].

use toyos_abi::syscall::{self, SyscallError};

use crate::{AsHandle, BorrowedHandle, OwnedHandle};

/// The other side's last handle has gone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hungup;

/// A fresh bell and the waker that rings it.
pub fn pair() -> Result<(Waker, Bell), SyscallError> {
    let ends = syscall::event_create()?;
    Ok((Waker(OwnedHandle(ends.signal)), Bell(OwnedHandle(ends.wait))))
}

/// Rings a [`Bell`] from any thread, and never waits.
pub struct Waker(OwnedHandle);

impl Waker {
    /// Ring, whether or not anyone still holds the bell.
    pub fn wake(&self) {
        let _ = self.ring();
    }

    /// Ring; [`Hungup`] once nobody holds the bell.
    pub fn ring(&self) -> Result<(), Hungup> {
        match syscall::event_signal(self.0.raw()) {
            Ok(()) => Ok(()),
            Err(SyscallError::Gone) => Err(Hungup),
            Err(e) => panic!("Waker::ring: the event refused a signal: {e:?}"),
        }
    }

    /// Another waker of the same bell, to hand on.
    pub fn try_clone(&self) -> Result<Waker, SyscallError> {
        self.0.try_clone().map(Waker)
    }
}

/// The receiving end: readable while a wake is pending or every waker is gone.
pub struct Bell(OwnedHandle);

impl Bell {
    /// Take every wake raised so far, and say whether there was one.
    ///
    /// For a bell whose wakers are this process's own: one whose every waker
    /// is gone is readable for good, and a wait on it would spin, so that is
    /// refused here by name.
    pub fn take(&self) -> bool {
        self.try_take().expect("Bell::take: every waker is gone")
    }

    /// [`take`](Self::take), answering a bell whose every waker is gone.
    pub fn try_take(&self) -> Result<bool, Hungup> {
        match syscall::event_take(self.0.raw()) {
            Ok(()) => Ok(true),
            Err(SyscallError::WouldBlock) => Ok(false),
            Err(SyscallError::Gone) => Err(Hungup),
            Err(e) => panic!("Bell::take: the event refused a take: {e:?}"),
        }
    }

    /// Block until a wake is pending, and take it.
    pub fn wait(&self) {
        let woken = self.wait_for(u64::MAX).expect("Bell::wait: every waker is gone");
        assert!(woken, "Bell::wait: a wait with no timeout returned with no wake");
    }

    /// Wait at most `timeout_nanos` (`u64::MAX`: for good) for a wake and take
    /// it: `true` for one taken, `false` at the timeout.
    pub fn wait_for(&self, timeout_nanos: u64) -> Result<bool, Hungup> {
        let until = toyos_abi::clock::nanos_since_boot().saturating_add(timeout_nanos);
        loop {
            if self.try_take()? {
                return Ok(true);
            }
            let left = match timeout_nanos {
                u64::MAX => u64::MAX,
                _ => until.saturating_sub(toyos_abi::clock::nanos_since_boot()),
            };
            if left == 0 {
                return Ok(false);
            }
            match syscall::handle_wait(self.0.raw(), crate::poller::READABLE, left) {
                // Ready, or the timeout: the take above says which.
                Ok(_) => {}
                Err(e) => panic!("Bell::wait_for: the event refused a wait: {e:?}"),
            }
        }
    }
}

impl AsHandle for Waker {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl AsHandle for Bell {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

/// A side a peer sent: its type is the sender's word, as a connection's is.
impl From<OwnedHandle> for Waker {
    fn from(handle: OwnedHandle) -> Self {
        Self(handle)
    }
}

impl From<OwnedHandle> for Bell {
    fn from(handle: OwnedHandle) -> Self {
        Self(handle)
    }
}

impl From<Waker> for OwnedHandle {
    fn from(waker: Waker) -> Self {
        waker.0
    }
}

impl From<Bell> for OwnedHandle {
    fn from(bell: Bell) -> Self {
        bell.0
    }
}
