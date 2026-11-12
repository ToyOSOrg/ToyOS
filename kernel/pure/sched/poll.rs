//! A ring's polls: the one decision each makes, the handle it was made through,
//! and when one is answered.
//!
//! **A post is not an answer.** An object's post fires the polls on its watch,
//! and a fire only owes the poll a look. The answer is written by the ring's
//! submitter, in its `inbox_submit`, after it has looked at the object again
//! ([`deliver`]): an object ready at that look is answered with what it holds
//! then, and one that is not is armed again. So an answer says what the object
//! held when the wait that returned it looked.
//!
//! **A poll belongs to the handle it was made through** ([`Via`]). Its entries
//! stand on the object's watches only while that handle is in its table: they
//! are added in one hold of the table, after it read the handle there
//! ([`register`]), and the table ends them as the handle leaves it. A handle
//! that left first ends the poll instead. So closing or moving one handle ends
//! that handle's polls and no other's.
//!
//! **A handle has one poll that may answer.** A watch replaces the handle's
//! earlier poll whether a post has fired it or not ([`Polls::admit`]), so one
//! look answers a handle once, under the token of its newest watch.
//!
//! **A ring has one submitter, and only it writes the ring's completions**
//! ([`Submitter`]). The room an answer was read for is the room it is written
//! into, and an answer wakes nobody: the one thread that could wait for it is
//! the one writing it.
//!
//! **The submitter parks on the polls themselves** ([`awake`]): a fire takes
//! its poll and then posts the watch the submitter parks on, and the
//! submitter, registered there, reads its polls again before it parks. The
//! ring's teardown posts the same watch ([`torn_down`]), so a submitter parked
//! on a ring that goes reads it gone.

use alloc::vec::Vec;

use toyos_abi::handle::RawHandle;
use toyos_abi::syscall::SyscallError;

use crate::sched::sync::{Arc, AtomicU32, AtomicU8, Ordering};

const ARMED: u8 = 0;
const FIRED: u8 = 1;
const WITHDRAWN: u8 = 2;
const ENDED: u8 = 3;

/// A poll's answer, taken once: by a post or its registrant's recheck
/// (fired), by its handle leaving its table or its source ending (ended), or
/// by a newer poll on its handle or its ring's teardown (withdrawn).
pub struct Once(AtomicU8);

impl Once {
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self(AtomicU8::new(ARMED))
    }

    // Loom's atomics have no const constructor, so this arm alone drops `const`.
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self(AtomicU8::new(ARMED))
    }

    /// `true` for the one caller that takes the poll to be looked at.
    pub fn fire(&self) -> bool {
        self.take(FIRED)
    }

    /// `true` for the one caller that takes the poll to be answered as gone,
    /// with no look.
    pub fn end(&self) -> bool {
        self.take(ENDED)
    }

    /// `true` for the one caller that takes the poll back unanswered.
    pub fn withdraw(&self) -> bool {
        self.take(WITHDRAWN)
    }

    /// Whether nothing has taken the poll yet. `false` is permanent.
    pub fn armed(&self) -> bool {
        self.0.load(Ordering::Acquire) == ARMED
    }

    /// Whether it was taken to be answered as gone. Permanent once `true`.
    pub fn ended(&self) -> bool {
        self.0.load(Ordering::Acquire) == ENDED
    }

    // `poll-fire-load-store` is the negative control: the exchange split into
    // a load and a store lets two callers both take a poll.
    #[cfg(not(feature = "poll-fire-load-store"))]
    fn take(&self, to: u8) -> bool {
        self.0.compare_exchange(ARMED, to, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    #[cfg(feature = "poll-fire-load-store")]
    fn take(&self, to: u8) -> bool {
        if self.0.load(Ordering::Acquire) != ARMED {
            return false;
        }
        self.0.store(to, Ordering::Release);
        true
    }
}

impl Default for Once {
    fn default() -> Self {
        Self::new()
    }
}

/// The handle a poll was made through: the table it was looked up in, and its
/// number there. A watch's entries are found by it, and a ring's polls kept
/// by it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Via {
    pub table: u64,
    pub handle: RawHandle,
}

/// A ring, as a fire sees it.
pub trait Wake {
    /// Wake the ring's submitter if it is parked. Called from an interrupt
    /// handler too.
    fn wake(&self);
}

/// A ring its polls hold by reference count, as the kernel's do.
impl<T: Wake> Wake for Arc<T> {
    fn wake(&self) {
        T::wake(self);
    }
}

/// One watch a ring is waiting on.
pub struct Poll<W> {
    ring: W,
    pub user_data: u64,
    pub via: Via,
    /// The submission's interest, which the look asks about again.
    pub flags: u32,
    /// The directions whose watch posted it, for an object whose post is its
    /// readiness.
    posted: AtomicU32,
    state: Once,
}

impl<W: Wake> Poll<W> {
    pub fn new(ring: W, user_data: u64, via: Via, flags: u32) -> Self {
        Self { ring, user_data, via, flags, posted: AtomicU32::new(0), state: Once::new() }
    }

    /// The object may be ready: the watch of the directions in `posted` was
    /// posted, or with `0` its registrant saw it ready. Owes a look, once.
    pub fn fire(&self, posted: u32) {
        // Before the exchange, so the look the winning fire owes sees it.
        self.posted.fetch_or(posted, Ordering::Release);
        if self.state.fire() {
            self.ring.wake();
        }
    }

    /// Answered as gone, with no look: its handle left its table, or its
    /// source ended.
    pub fn end(&self) {
        if self.state.end() {
            self.ring.wake();
        }
    }

    /// Answer nothing: a newer poll on the same handle replaced it, or its
    /// ring went.
    pub fn withdraw(&self) {
        let _ = self.state.withdraw();
    }

    pub fn armed(&self) -> bool {
        self.state.armed()
    }

    pub fn ended(&self) -> bool {
        self.state.ended()
    }

    /// The directions whose watch posted this poll.
    pub fn posted(&self) -> u32 {
        self.posted.load(Ordering::Acquire)
    }
}

/// The table a poll's handle is in, as its registration sees it.
pub trait Table {
    /// Run `add` within one hold of the table, marking `handle` polled, if it
    /// is still there; `false`, with `add` not run, if it is not.
    fn while_held(&self, handle: RawHandle, add: impl FnOnce()) -> bool;
}

/// Put `poll`'s entries on the object's watches (`add`) while its handle is in
/// its table, or end it: the handle left after it was looked up, and nothing
/// would take the entries off again.
#[cfg(not(feature = "register-outside-table"))]
pub fn register<W: Wake>(poll: &Poll<W>, table: &impl Table, add: impl FnOnce()) {
    if !table.while_held(poll.via.handle, add) {
        poll.end();
    }
}

/// `register-outside-table` is the negative control: the handle is read in
/// one hold and the entries are added after it, so a removal between the two
/// finds nothing to end.
#[cfg(feature = "register-outside-table")]
pub fn register<W: Wake>(poll: &Poll<W>, table: &impl Table, add: impl FnOnce()) {
    if table.while_held(poll.via.handle, || {}) {
        add();
    } else {
        poll.end();
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
    /// Its place among every poll its ring ever kept.
    order: u64,
}

impl<W: Wake> Polls<W> {
    pub const fn new() -> Self {
        Self { polls: Vec::new(), kept: 0 }
    }

    /// The bytes one kept poll's place in the list takes.
    pub const fn place_bytes() -> usize {
        core::mem::size_of::<Kept<W>>()
    }

    fn keep(&mut self, poll: Arc<Poll<W>>) -> Kept<W> {
        self.kept += 1;
        Kept { poll, order: self.kept - 1 }
    }

    /// Keep `poll` as its handle's one poll, unless `cap` are kept already.
    /// Every earlier poll on the handle answers nothing from here on, whether
    /// a post has fired it or not.
    pub fn admit(&mut self, poll: Arc<Poll<W>>, cap: usize) -> bool {
        self.polls.retain(|kept| {
            let other = !replaces(&poll, &kept.poll);
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

    /// The oldest poll owed a look among those kept before `pass`.
    pub fn take_owed(&self, pass: u64) -> Option<Arc<Poll<W>>> {
        self.polls.iter().find(|kept| kept.order < pass && !kept.poll.armed()).map(|kept| kept.poll.clone())
    }

    /// Let go of `poll` once its look has an answer.
    pub fn settle(&mut self, poll: &Arc<Poll<W>>) {
        let at = self.looked_at(poll);
        self.polls.remove(at);
    }

    /// Keep `again` in `poll`'s place once its look found nothing.
    pub fn renew(&mut self, poll: &Arc<Poll<W>>, again: Arc<Poll<W>>) {
        let at = self.looked_at(poll);
        self.polls[at] = self.keep(again);
    }

    /// Where the poll a look took is. Only the submitter changes the list, so
    /// nothing can have taken it out during the look.
    fn looked_at(&self, poll: &Arc<Poll<W>>) -> usize {
        self.place(poll).expect("a poll left its ring's list while its submitter looked at it")
    }

    /// Whether a poll is owed a look.
    pub fn owed(&self) -> bool {
        self.polls.iter().any(|kept| !kept.poll.armed())
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

/// Whether `newer` takes `kept`'s place: a watch on the same handle.
// `admit-keeps-the-replaced` is the negative control: a newer watch leaves the
// older standing, so one handle is answered twice.
#[cfg(not(feature = "admit-keeps-the-replaced"))]
fn replaces<W>(newer: &Poll<W>, kept: &Poll<W>) -> bool {
    newer.via == kept.via
}

#[cfg(feature = "admit-keeps-the-replaced")]
fn replaces<W>(_newer: &Poll<W>, _kept: &Poll<W>) -> bool {
    false
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

/// A ring's one submitter, the only writer of its completions.
pub trait Submitter<W> {
    /// Whether the completion ring takes one more answer.
    fn room(&self) -> bool;
    /// Write one completion; the ring's teardown leaves nowhere to write it.
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
        let result = if poll.ended() {
            -(SyscallError::NotFound as i32)
        } else {
            match look(ring, &poll) {
                Look::Ready(flags) => flags as i32,
                Look::Refused(e) => -(e as i32),
                Look::Waits => continue,
            }
        };
        if ring.polls(|polls| polls.settle(&poll)).is_some() {
            ring.answer(poll.user_data, result);
        }
    }
}

/// The park predicate of a submitter waiting until `enough()`, read once it is
/// registered on the watch a fire posts: it does not park over a poll
/// [`deliver`] would answer, nor on a ring torn down.
pub fn awake<W: Wake>(ring: &impl Submitter<W>, enough: impl FnOnce() -> bool) -> bool {
    match ring.polls(|polls| polls.owed()) {
        None => true,
        Some(owed) => owed && ring.room() || enough(),
    }
}

/// The ring is going: none of its polls answers, and its submitter, parked or
/// about to park, is woken to read it gone.
pub fn torn_down<W: Wake>(mut polls: Polls<W>, ring: &impl Wake) {
    polls.withdraw_all();
    wake_the_submitter(ring);
}

#[cfg(not(feature = "teardown-posts-nothing"))]
fn wake_the_submitter(ring: &impl Wake) {
    ring.wake();
}

/// `teardown-posts-nothing` is the negative control: a submitter parked on a
/// ring that goes waits for its deadline, or for good.
#[cfg(feature = "teardown-posts-nothing")]
fn wake_the_submitter(_ring: &impl Wake) {}

/// `post-is-an-answer` is the negative control: a fired poll is answered with
/// its interest and nobody looks.
#[cfg(not(feature = "post-is-an-answer"))]
fn look<W: Wake>(ring: &impl Submitter<W>, poll: &Arc<Poll<W>>) -> Look {
    ring.look(poll)
}

#[cfg(feature = "post-is-an-answer")]
fn look<W: Wake>(_ring: &impl Submitter<W>, poll: &Arc<Poll<W>>) -> Look {
    Look::Ready(poll.flags)
}

#[cfg(all(test, not(feature = "loom")))]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};

    /// A ring that counts the wakes its fires owe.
    #[derive(Clone, Default)]
    struct Counted(Arc<AtomicU32>);

    impl Counted {
        fn wakes(&self) -> u32 {
            self.0.load(Ordering::Acquire)
        }
    }

    impl Wake for Counted {
        fn wake(&self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn via(handle: u32) -> Via {
        Via { table: 1, handle: RawHandle(handle) }
    }

    fn poll(ring: &Counted, token: u64, handle: u32) -> Arc<Poll<Counted>> {
        Arc::new(Poll::new(ring.clone(), token, via(handle), 1))
    }

    /// Each way a poll is taken excludes every other, and only a fire and an
    /// end owe the ring a wake.
    #[test]
    fn a_poll_is_taken_once_and_only_an_answer_wakes() {
        type Take = fn(&Poll<Counted>);
        let ways: [(&str, Take, bool); 3] =
            [("fire", |p| p.fire(0), true), ("end", Poll::end, true), ("withdraw", Poll::withdraw, false)];
        for (first, take, wakes) in ways {
            for (then, again, _) in ways {
                let ring = Counted::default();
                let p = poll(&ring, 7, 1);
                take(&p);
                again(&p);
                assert!(!p.armed(), "{first} left the poll armed");
                assert_eq!(p.ended(), first == "end", "{first} then {then}");
                assert_eq!(ring.wakes(), u32::from(wakes), "{first} then {then} woke the ring");
            }
        }
    }

    /// A watch on a handle replaces that handle's poll, fired or not, and no
    /// other handle's.
    #[test]
    fn a_watch_replaces_its_own_handles_poll_alone() {
        let ring = Counted::default();
        let mut polls = Polls::new();
        let (first, other) = (poll(&ring, 1, 1), poll(&ring, 2, 2));
        assert!(polls.admit(first.clone(), 8));
        assert!(polls.admit(other.clone(), 8));
        first.fire(1);
        let newer = poll(&ring, 3, 1);
        assert!(polls.admit(newer.clone(), 8));
        assert!(!first.armed() && !first.ended(), "the replaced poll was fired and stays so");
        assert!(polls.place(&first).is_none(), "the replaced poll is kept");
        assert!(other.armed() && newer.armed(), "another handle's poll was taken");
        assert_eq!(polls.polls.len(), 2);
    }

    /// The cap counts the polls kept after a replacement.
    #[test]
    fn a_replacement_is_admitted_at_the_cap() {
        let ring = Counted::default();
        let mut polls = Polls::new();
        assert!(polls.admit(poll(&ring, 1, 1), 1));
        assert!(!polls.admit(poll(&ring, 2, 2), 1), "a second handle past the cap");
        assert!(polls.admit(poll(&ring, 3, 1), 1), "the same handle's watch at the cap");
    }

    /// A submitter over a fake object: whether it is ready, the room left,
    /// and every answer written.
    struct Fake {
        polls: RefCell<Option<Polls<Counted>>>,
        ready: Cell<bool>,
        room: Cell<u32>,
        answers: RefCell<Vec<(u64, i32)>>,
    }

    impl Fake {
        fn new(room: u32) -> Self {
            Self { polls: RefCell::new(Some(Polls::new())), ready: Cell::new(false), room: Cell::new(room), answers: RefCell::new(Vec::new()) }
        }
    }

    impl Submitter<Counted> for Fake {
        fn room(&self) -> bool {
            self.room.get() > 0
        }

        fn answer(&self, user_data: u64, result: i32) {
            let room = self.room.get().checked_sub(1).expect("an answer written with no room read for it");
            self.room.set(room);
            self.answers.borrow_mut().push((user_data, result));
        }

        fn look(&self, poll: &Arc<Poll<Counted>>) -> Look {
            if self.ready.get() {
                return Look::Ready(poll.flags);
            }
            let again = Arc::new(Poll::new(poll.ring.clone(), poll.user_data, poll.via, poll.flags));
            self.polls(|polls| polls.renew(poll, again));
            Look::Waits
        }

        fn polls<R>(&self, f: impl FnOnce(&mut Polls<Counted>) -> R) -> Option<R> {
            self.polls.borrow_mut().as_mut().map(f)
        }
    }

    /// Answers stop where the room does, and the polls past it are answered
    /// by the next delivery, oldest first.
    #[test]
    fn the_room_read_is_the_room_written() {
        let ring = Counted::default();
        let fake = Fake::new(1);
        fake.ready.set(true);
        let polls: Vec<_> = (1..=3).map(|h| poll(&ring, u64::from(h), h)).collect();
        for p in &polls {
            fake.polls(|kept| kept.admit(p.clone(), 8));
            p.fire(1);
        }
        deliver(&fake);
        assert_eq!(*fake.answers.borrow(), [(1, 1)]);
        fake.room.set(2);
        deliver(&fake);
        assert_eq!(*fake.answers.borrow(), [(1, 1), (2, 1), (3, 1)]);
    }

    /// An ended poll is answered `NotFound` with no look, and one whose look
    /// finds nothing is armed again and answers nothing.
    #[test]
    fn an_ended_poll_is_gone_and_a_waiting_one_is_armed_again() {
        let ring = Counted::default();
        let fake = Fake::new(4);
        let (ended, waiting) = (poll(&ring, 1, 1), poll(&ring, 2, 2));
        fake.polls(|polls| polls.admit(ended.clone(), 8));
        fake.polls(|polls| polls.admit(waiting.clone(), 8));
        ended.end();
        waiting.fire(1);
        deliver(&fake);
        assert_eq!(*fake.answers.borrow(), [(1, -(SyscallError::NotFound as i32))]);
        assert!(fake.polls(|polls| polls.polls.len() == 1 && polls.polls[0].poll.armed()) == Some(true));
    }

    /// A submitter does not park over an owed poll it has room for, nor on a
    /// ring torn down.
    #[test]
    fn a_submitter_parks_on_nothing_owed_and_never_on_a_ring_gone() {
        let ring = Counted::default();
        let fake = Fake::new(1);
        let p = poll(&ring, 1, 1);
        fake.polls(|polls| polls.admit(p.clone(), 8));
        assert!(!awake(&fake, || false), "parked short of an armed poll");
        p.fire(1);
        assert!(awake(&fake, || false), "parked over an owed poll");
        fake.room.set(0);
        assert!(!awake(&fake, || false), "woken for an answer with no room to write it");
        let polls = fake.polls.borrow_mut().take().expect("the ring's polls");
        torn_down(polls, &ring);
        assert!(!p.armed() && !p.ended(), "a fired poll stays as it was");
        assert!(awake(&fake, || false), "parked on a ring torn down");
        assert_eq!(ring.wakes(), 2, "the teardown woke nobody");
    }

    /// A table that holds one handle or none.
    struct OneHandle(Cell<Option<bool>>);

    impl Table for OneHandle {
        fn while_held(&self, _handle: RawHandle, add: impl FnOnce()) -> bool {
            let Some(_) = self.0.get() else { return false };
            self.0.set(Some(true));
            add();
            true
        }
    }

    /// A poll whose handle is in its table is registered and its handle marked
    /// polled; one whose handle left is ended, with nothing added.
    #[test]
    fn a_poll_is_registered_only_through_a_handle_still_held() {
        let ring = Counted::default();
        let held = OneHandle(Cell::new(Some(false)));
        let p = poll(&ring, 1, 1);
        let mut added = false;
        register(&p, &held, || added = true);
        assert!(added && p.armed() && held.0.get() == Some(true));

        let gone = OneHandle(Cell::new(None));
        let q = poll(&ring, 2, 1);
        register(&q, &gone, || unreachable!("an entry added for a handle gone"));
        assert!(q.ended());
    }
}
