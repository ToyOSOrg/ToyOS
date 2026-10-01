//! A ring's polls, and when one of them is answered.
//!
//! **A post is not an answer.** An object's post fires the polls on its watch,
//! and a fire only owes the poll a look ([`Wake::owe`]). The answer is written
//! by the ring's own submitter, in its `inbox_submit`, after it has looked at
//! the object again ([`deliver`]): an object ready at that look is answered with
//! what it holds then, and one that is not is armed again. So an answer says
//! what the object held when the wait that returned it looked — a peer's empty
//! write, and a post that lands after its bytes were read, answer nothing.
//!
//! **A handle has one poll that may answer.** A watch replaces the handle's
//! earlier poll whether a post has fired it or not ([`Polls::admit`]), so one
//! look answers a handle once, under the token of its newest watch.
//!
//! Compiled a second time by `kernel-loom`, so it names nothing of the kernel's.

use alloc::sync::Arc;
use alloc::vec::Vec;

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::SyscallError;

use super::once::Once;

/// Who a fire tells.
pub trait Wake {
    /// A poll is owed a look: wake whoever waits in this ring's `inbox_submit`.
    fn owe(&self);
}

/// One `OP_WATCH` a ring is waiting on: one-shot across every watch it is
/// registered on and against its own registrant's recheck.
pub struct Poll<W> {
    wake: W,
    pub user_data: u64,
    /// The handle the poll was submitted against; the key a watch replaces by.
    pub handle: RawHandle,
    /// The submission's interest, which the look asks about again.
    pub flags: u32,
    /// The directions whose watch posted it, for an object whose post is its
    /// readiness.
    posted: AtomicU32,
    state: Once,
}

impl<W: Wake> Poll<W> {
    pub fn new(wake: W, user_data: u64, handle: RawHandle, flags: u32) -> Self {
        Self { wake, user_data, handle, flags, posted: AtomicU32::new(0), state: Once::new() }
    }

    /// The object may be ready: the watch of the directions in `posted` was
    /// posted, or with `0` its registrant saw it ready. Owes a look, once.
    pub fn fire(&self, posted: u32) {
        // Before the exchange, so the look that the winning fire owes sees it.
        self.posted.fetch_or(posted, Ordering::Release);
        if self.state.fire() {
            self.wake.owe();
        }
    }

    /// The object's source ended: the poll is answered as gone, with no look.
    pub fn end(&self) {
        if self.state.end() {
            self.wake.owe();
        }
    }

    /// Answer nothing: a newer poll on the same handle replaced it, or its
    /// ring went away.
    pub fn withdraw(&self) {
        let _ = self.state.withdraw();
    }

    pub fn armed(&self) -> bool {
        self.state.armed()
    }

    /// The directions whose watch posted this poll.
    pub fn posted(&self) -> u32 {
        self.posted.load(Ordering::Acquire)
    }
}

/// A ring's polls that may still answer: armed, or taken and not yet looked at.
pub struct Polls<W> {
    polls: Vec<Arc<Poll<W>>>,
}

impl<W: Wake> Polls<W> {
    pub const fn new() -> Self {
        Self { polls: Vec::new() }
    }

    /// Keep `poll` as its handle's one poll, unless `cap` are kept already.
    /// Every earlier poll on the handle answers nothing from here on, whether
    /// a post has fired it or not.
    pub fn admit(&mut self, poll: Arc<Poll<W>>, cap: usize) -> bool {
        self.polls.retain(|p| {
            let other = p.handle != poll.handle;
            if !other {
                p.withdraw();
            }
            other
        });
        if self.polls.len() >= cap {
            return false;
        }
        self.polls.push(poll);
        true
    }

    /// The oldest poll something has taken, given up for its look.
    pub fn take_owed(&mut self) -> Option<Arc<Poll<W>>> {
        let at = self.polls.iter().position(|p| !p.armed())?;
        Some(self.polls.remove(at))
    }

    /// The ring is going: no poll answers.
    pub fn withdraw_all(&mut self) {
        for poll in self.polls.drain(..) {
            poll.withdraw();
        }
    }
}

impl<W: Wake> Default for Polls<W> {
    fn default() -> Self {
        Self::new()
    }
}

/// What a look at a poll's object found.
pub enum Look {
    /// Ready in the directions this word names, which is the answer.
    Ready(u32),
    /// The handle names nothing that could answer any more; the refusal is
    /// the answer.
    Refused(SyscallError),
    /// Not ready: a new poll on the handle is armed, and this one answers
    /// nothing.
    Armed,
}

/// The submitter's side of [`deliver`].
pub trait Submitter<W> {
    /// Whether the completion ring takes one more answer.
    fn room(&self) -> bool;
    fn answer(&self, user_data: u64, result: i32);
    /// Look at the object `poll` watches, and arm a new poll on it if it is
    /// not ready.
    fn look(&self, poll: &Poll<W>) -> Look;
}

/// Answer every poll `take` gives up, oldest first, while the ring has room; a
/// poll left over is answered by a later look, never dropped.
pub fn deliver<W: Wake>(mut take: impl FnMut() -> Option<Arc<Poll<W>>>, ring: &impl Submitter<W>) {
    while ring.room() {
        let Some(poll) = take() else { return };
        let result = if poll.state.ended() {
            -(SyscallError::NotFound as i32)
        } else {
            match look(ring, &poll) {
                Look::Ready(flags) => flags as i32,
                Look::Refused(e) => -(e as i32),
                Look::Armed => continue,
            }
        };
        ring.answer(poll.user_data, result);
    }
}

/// `post-is-an-answer` is the negative control: a fired poll is answered with
/// its interest and nobody looks, which is what a ring did before this file,
/// and `kernel-loom`'s `inbox_answer` reds.
#[cfg(not(feature = "post-is-an-answer"))]
fn look<W: Wake>(ring: &impl Submitter<W>, poll: &Poll<W>) -> Look {
    ring.look(poll)
}

#[cfg(feature = "post-is-an-answer")]
fn look<W: Wake>(_ring: &impl Submitter<W>, poll: &Poll<W>) -> Look {
    Look::Ready(poll.flags)
}
