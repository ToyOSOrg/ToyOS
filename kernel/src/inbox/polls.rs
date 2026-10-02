//! A ring's polls, and when one of them is answered.
//!
//! **A post is not an answer.** An object's post fires the polls on its watch,
//! and a fire only owes the poll a look. The answer is written by the ring's
//! own submitter, in its `inbox_submit`, after it has looked at the object
//! again ([`deliver`]): an object ready at that look is answered with what it
//! holds then, and one that is not is armed again. So an answer says what the
//! object held when the wait that returned it looked — a peer's empty write,
//! and a post that lands after its bytes were read, answer nothing.
//!
//! **A handle has one poll that may answer.** A watch replaces the handle's
//! earlier poll whether a post has fired it or not, and whether or not a
//! submitter is looking at it ([`Polls::admit`]), so one look answers a handle
//! once, under the token of its newest watch.
//!
//! **A submitter parks on the polls themselves** ([`awake`]): a fire takes its
//! poll and then posts the watch the submitter parks on, and the submitter,
//! registered there, reads its polls again before it parks. The lost-wake
//! argument is that watch's, and nothing here records a fire beside the poll.
//!
//! Compiled a second time by `kernel-loom` and by `toyos-sched-loom`, so it
//! names nothing of the kernel's.

use alloc::sync::Arc;
use alloc::vec::Vec;

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicU32, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::SyscallError;

use super::once::Once;

/// A ring, as a fire sees it.
pub trait Wake {
    /// Wake whoever waits in this ring's `inbox_submit`. Called from an
    /// interrupt handler too.
    fn wake(&self);
}

/// One `OP_WATCH` a ring is waiting on: one-shot across every watch it is
/// registered on and against its own registrant's recheck.
pub struct Poll<W> {
    ring: W,
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
    pub fn new(ring: W, user_data: u64, handle: RawHandle, flags: u32) -> Self {
        Self { ring, user_data, handle, flags, posted: AtomicU32::new(0), state: Once::new() }
    }

    /// The object may be ready: the watch of the directions in `posted` was
    /// posted, or with `0` its registrant saw it ready. Owes a look, once.
    pub fn fire(&self, posted: u32) {
        // Before the exchange, so the look that the winning fire owes sees it.
        self.posted.fetch_or(posted, Ordering::Release);
        if self.state.fire() {
            self.ring.wake();
        }
    }

    /// The object's source ended: the poll is answered as gone, with no look.
    pub fn end(&self) {
        if self.state.end() {
            self.ring.wake();
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

/// A ring's polls that may still answer: armed, or taken and not yet answered.
pub struct Polls<W> {
    polls: Vec<Kept<W>>,
    /// How many polls this ring ever kept: the next one's `order`.
    kept: u64,
}

struct Kept<W> {
    poll: Arc<Poll<W>>,
    /// A submitter took it for its look, and no other takes it.
    looking: bool,
    /// Its place among every poll its ring ever kept.
    order: u64,
}

impl<W: Wake> Kept<W> {
    /// Something has taken the poll, and no submitter is looking at it.
    fn owed(&self) -> bool {
        !self.looking && !self.poll.armed()
    }
}

impl<W: Wake> Polls<W> {
    pub const fn new() -> Self {
        Self { polls: Vec::new(), kept: 0 }
    }

    fn keep(&mut self, poll: Arc<Poll<W>>) -> Kept<W> {
        self.kept += 1;
        Kept { poll, looking: false, order: self.kept - 1 }
    }

    /// Keep `poll` as its handle's one poll, unless `cap` are kept already.
    /// Every earlier poll on the handle answers nothing from here on, whether
    /// a post has fired it or not.
    pub fn admit(&mut self, poll: Arc<Poll<W>>, cap: usize) -> bool {
        self.polls.retain(|kept| {
            let other = kept.poll.handle != poll.handle;
            if !other {
                kept.poll.withdraw();
            }
            other
        });
        if self.polls.len() >= cap {
            return false;
        }
        let kept = self.keep(poll);
        self.polls.push(kept);
        true
    }

    /// The bound of one pass of looks: a poll kept from here on waits for the
    /// next.
    pub fn pass(&self) -> u64 {
        self.kept
    }

    /// The oldest poll owed a look among those kept before `pass`, kept while
    /// it is looked at so a watch on its handle still replaces it.
    pub fn take_owed(&mut self, pass: u64) -> Option<Arc<Poll<W>>> {
        let kept = self.polls.iter_mut().find(|kept| kept.order < pass && kept.owed())?;
        kept.looking = true;
        Some(kept.poll.clone())
    }

    /// Let go of `poll` once its look has an answer. `false` if a newer watch
    /// replaced it during the look or the ring went: it answers nothing.
    pub fn settle(&mut self, poll: &Arc<Poll<W>>) -> bool {
        let Some(at) = self.place(poll) else { return false };
        self.polls.remove(at);
        true
    }

    /// Keep `again` in `poll`'s place once its look found nothing; `false` as
    /// [`Self::settle`] says it, and `again` is not kept.
    pub fn renew(&mut self, poll: &Arc<Poll<W>>, again: Arc<Poll<W>>) -> bool {
        let Some(at) = self.place(poll) else { return false };
        self.polls[at] = self.keep(again);
        true
    }

    /// Whether a poll is owed a look.
    pub fn owed(&self) -> bool {
        self.polls.iter().any(Kept::owed)
    }

    fn place(&self, poll: &Arc<Poll<W>>) -> Option<usize> {
        self.polls.iter().position(|kept| Arc::ptr_eq(&kept.poll, poll))
    }

    /// The ring is going: no poll answers.
    pub fn withdraw_all(&mut self) {
        for kept in self.polls.drain(..) {
            kept.poll.withdraw();
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
    /// Not ready: the poll that holds the handle's place now answers later,
    /// and this one answers nothing.
    Waits,
}

/// The submitter's side of [`deliver`].
pub trait Submitter<W> {
    /// Whether the completion ring takes one more answer.
    fn room(&self) -> bool;
    fn answer(&self, user_data: u64, result: i32);
    /// Look at the object `poll` watches, and [`Polls::renew`] the poll if it
    /// is not ready.
    fn look(&self, poll: &Arc<Poll<W>>) -> Look;
    /// Run `f` on the ring's polls under their lock; `None` once the ring is
    /// gone.
    fn polls<R>(&self, f: impl FnOnce(&mut Polls<W>) -> R) -> Option<R>;
}

/// Answer every poll owed a look, oldest first, while the ring has room; a
/// poll left over is answered by a later call, never dropped. One pass: a
/// poll a look renews waits for the next call, so a peer that posts an empty
/// object as fast as it is looked at holds nobody.
pub fn deliver<W: Wake>(ring: &impl Submitter<W>) {
    let Some(pass) = ring.polls(|polls| polls.pass()) else { return };
    while ring.room() {
        let Some(poll) = ring.polls(|polls| polls.take_owed(pass)).flatten() else { return };
        let result = if poll.state.ended() {
            -(SyscallError::NotFound as i32)
        } else {
            match look(ring, &poll) {
                Look::Ready(flags) => flags as i32,
                Look::Refused(e) => -(e as i32),
                Look::Waits => continue,
            }
        };
        if ring.polls(|polls| polls.settle(&poll)) == Some(true) {
            ring.answer(poll.user_data, result);
        }
    }
}

/// The park predicate of a submitter waiting until `enough()`, read once it is
/// registered on the watch a fire posts: it does not park over a poll
/// [`deliver`] would answer.
pub fn awake<W: Wake>(ring: &impl Submitter<W>, enough: impl FnOnce() -> bool) -> bool {
    ring.room() && ring.polls(|polls| polls.owed()) == Some(true) || enough()
}

/// `post-is-an-answer` is the negative control: a fired poll is answered with
/// its interest and nobody looks, and `kernel-loom`'s `inbox_answer` reds.
#[cfg(not(feature = "post-is-an-answer"))]
fn look<W: Wake>(ring: &impl Submitter<W>, poll: &Arc<Poll<W>>) -> Look {
    ring.look(poll)
}

#[cfg(feature = "post-is-an-answer")]
fn look<W: Wake>(_ring: &impl Submitter<W>, poll: &Arc<Poll<W>>) -> Look {
    Look::Ready(poll.flags)
}
