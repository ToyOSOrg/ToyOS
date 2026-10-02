//! **A post is not an answer**: a poll ring's answer is what the object held
//! when the ring's submitter looked, never what a post once announced.
//!
//! `kernel/src/inbox/polls.rs` against fake objects, standing in for
//! `inbox::submit`'s: a watch is the handle's one poll, a post fires every poll
//! armed on its object, and a submit looks at every fired poll's object again.
//! A reader that takes an answer for bytes and then reads blocking parks for
//! good on one written for nothing, which is why each case here is a reader's
//! sequence and its assertion the answers it is handed.
//!
//! In `loom::model`, because the one-shot under each poll is built from
//! loom's atomics in this crate; every model here is one thread, and a watch
//! another thread submits during a look is staged inside the look. A fire
//! racing the submitter's park is `toyos-sched-loom`'s
//! `a_fire_racing_a_submitters_park_is_never_lost`.
//!
//! The negative case is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features post-is-an-answer \
//!   --test inbox_answer
//! ```
//!
//! answers a fired poll without looking, and this file must red.

#![cfg(feature = "loom")]

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use kernel_loom::inbox_polls::{awake, deliver, Look, Poll, Polls, Submitter, Wake};
use toyos_abi::handle::RawHandle;
use toyos_abi::inbox::READABLE;
use toyos_abi::syscall::SyscallError;

const H: RawHandle = RawHandle(5);
const G: RawHandle = RawHandle(6);
/// The log: what it holds for a reader is the reader's cursor's to say.
const L: RawHandle = RawHandle(7);

/// The answer for bytes.
const BYTES: i32 = READABLE as i32;

const NOTHING: [(u64, i32); 0] = [];

/// The ring a fire tells, which has no waiter: every model is one thread.
#[derive(Clone)]
struct Ring;

impl Wake for Ring {
    fn wake(&self) {}
}

/// One object: whether it holds bytes, and the polls armed on its watch.
#[derive(Default)]
struct Object {
    bytes: Cell<bool>,
    /// Whether a post on it is its readiness, the kernel holding none to look at.
    posts_are_readiness: bool,
    armed: RefCell<Vec<Arc<Poll<Ring>>>>,
}

/// One ring's kernel over the objects `H`, `G` and `L`.
struct Kernel {
    ring: Ring,
    polls: RefCell<Polls<Ring>>,
    /// How many polls the ring keeps.
    cap: usize,
    h: Object,
    g: Object,
    l: Object,
    /// The handle the process has closed since it watched it.
    closed: Cell<Option<RawHandle>>,
    /// A watch another thread submits while the next look runs.
    lands_during_look: Cell<Option<(RawHandle, u64)>>,
    /// How many more renewals a peer's post follows at once.
    posts_after_renewal: Cell<u32>,
    /// How many looks the submitter has made.
    looks: Cell<u32>,
    /// The completion ring, and how many answers it holds.
    answers: RefCell<Vec<(u64, i32)>>,
    size: usize,
}

impl Kernel {
    fn new(size: usize) -> Self {
        Self {
            ring: Ring,
            polls: RefCell::new(Polls::new()),
            cap: 16,
            h: Object::default(),
            g: Object::default(),
            l: Object { posts_are_readiness: true, ..Object::default() },
            closed: Cell::new(None),
            lands_during_look: Cell::new(None),
            posts_after_renewal: Cell::new(0),
            looks: Cell::new(0),
            answers: RefCell::new(Vec::new()),
            size,
        }
    }

    fn object(&self, handle: RawHandle) -> &Object {
        match handle {
            H => &self.h,
            G => &self.g,
            L => &self.l,
            other => panic!("no object behind {other:?}"),
        }
    }

    /// `OP_WATCH` for bytes on `handle`, answered under `token`: the handle's
    /// one poll.
    fn watch(&self, handle: RawHandle, token: u64) {
        assert!(self.admit(handle, token), "the ring keeps no more polls");
    }

    /// The same, answering whether the ring kept the poll.
    fn admit(&self, handle: RawHandle, token: u64) -> bool {
        let poll = Arc::new(Poll::new(self.ring.clone(), token, handle, READABLE));
        let kept = self.polls.borrow_mut().admit(poll.clone(), self.cap);
        if kept {
            self.arm(poll);
        }
        kept
    }

    /// Fired now if its object holds bytes, armed on its watch if not.
    fn arm(&self, poll: Arc<Poll<Ring>>) {
        let object = self.object(poll.handle);
        if object.bytes.get() {
            poll.fire(0);
        } else {
            object.armed.borrow_mut().push(poll);
        }
    }

    /// Bytes reach the object; its post has not run.
    fn fill(&self, handle: RawHandle) {
        self.object(handle).bytes.set(true);
    }

    /// The object's post: every poll armed on it fires, whatever it holds.
    fn post(&self, handle: RawHandle) {
        for poll in self.object(handle).armed.take() {
            poll.fire(READABLE);
        }
    }

    /// The reader takes every byte the object holds.
    fn take(&self, handle: RawHandle) {
        self.object(handle).bytes.set(false);
    }

    /// The object's source ends.
    fn end(&self, handle: RawHandle) {
        for poll in self.object(handle).armed.take() {
            poll.end();
        }
    }

    /// The process closes `handle`, whose watch other handles share: no close
    /// ends its polls.
    fn close(&self, handle: RawHandle) {
        self.closed.set(Some(handle));
    }

    /// How many polls on `handle`'s watch a post would still fire.
    fn live(&self, handle: RawHandle) -> usize {
        self.object(handle).armed.borrow().iter().filter(|poll| poll.armed()).count()
    }

    /// `inbox_submit`'s look at what the ring is owed.
    fn submit(&self) {
        deliver(self);
    }

    /// Whether a submitter waiting for one more answer than the ring holds
    /// would go round again instead of parking.
    fn awake(&self) -> bool {
        awake(self, || false)
    }

    /// Every answer the reader's drain hands it.
    fn drain(&self) -> Vec<(u64, i32)> {
        self.answers.take()
    }
}

impl Submitter<Ring> for Kernel {
    fn room(&self) -> bool {
        self.answers.borrow().len() < self.size
    }

    fn answer(&self, user_data: u64, result: i32) {
        self.answers.borrow_mut().push((user_data, result));
    }

    fn polls<R>(&self, f: impl FnOnce(&mut Polls<Ring>) -> R) -> Option<R> {
        Some(f(&mut self.polls.borrow_mut()))
    }

    fn look(&self, poll: &Arc<Poll<Ring>>) -> Look {
        self.looks.set(self.looks.get() + 1);
        if let Some((handle, token)) = self.lands_during_look.take() {
            self.watch(handle, token);
        }
        if self.closed.get() == Some(poll.handle) {
            return Look::Refused(SyscallError::NotFound);
        }
        let object = self.object(poll.handle);
        if object.bytes.get() || (object.posts_are_readiness && poll.posted() & READABLE != 0) {
            return Look::Ready(READABLE);
        }
        let again = Arc::new(Poll::new(self.ring.clone(), poll.user_data, poll.handle, poll.flags));
        if self.polls.borrow_mut().renew(poll, again.clone()) {
            self.arm(again);
        }
        if let Some(left) = self.posts_after_renewal.get().checked_sub(1) {
            self.posts_after_renewal.set(left);
            self.post(poll.handle);
        }
        Look::Waits
    }
}

/// A peer's write of no bytes posts the pipe all the same
/// (`pipe::try_write` answers `Wrote(0)` and `sys_write` wakes the readers).
/// Nothing is there to read, so nothing is answered, and the poll armed again
/// answers the bytes that do come.
#[test]
fn a_post_with_nothing_to_read_answers_nothing() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 7);
        kernel.submit();
        assert_eq!(kernel.drain(), NOTHING);

        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), NOTHING, "a post with nothing behind it was answered");

        kernel.fill(H);
        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), [(7, BYTES)]);
    });
}

/// A frame is two writes, and a write posts after it has published its
/// bytes. The header's post wakes the reader, which reads header and payload
/// and watches again before the payload's post has run; that post lands on
/// the new watch with nothing left to read.
#[test]
fn a_post_that_lands_after_its_bytes_were_read_answers_nothing() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 7);
        kernel.submit();

        kernel.fill(H);
        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), [(7, BYTES)]);

        kernel.take(H);
        kernel.watch(H, 7);
        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), NOTHING, "the payload's late post was answered");
    });
}

/// A post fires a poll its reader then replaces before its next wait: one
/// arrival, answered once, under the newest watch's token.
#[test]
fn a_watch_replaces_a_poll_a_post_already_fired() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 1);
        kernel.submit();

        kernel.fill(H);
        kernel.post(H);
        kernel.watch(H, 2);
        kernel.submit();
        assert_eq!(kernel.drain(), [(2, BYTES)]);
    });
}

/// A look that finds nothing arms the poll again and goes on to the next one.
#[test]
fn a_poll_armed_again_does_not_end_the_look() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 3);
        kernel.watch(G, 4);
        kernel.submit();

        kernel.post(H);
        kernel.fill(G);
        kernel.post(G);
        kernel.submit();
        assert_eq!(kernel.drain(), [(4, BYTES)]);
    });
}

/// A watch replaces only its own handle's poll: one the reader did not renew
/// still answers when its object fills.
#[test]
fn a_poll_left_standing_still_answers() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 3);
        kernel.watch(G, 4);
        kernel.submit();
        kernel.watch(G, 4);
        kernel.submit();

        kernel.fill(H);
        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), [(3, BYTES)]);
    });
}

/// A source that ended is answered as gone: there is nothing to look at.
#[test]
fn an_ended_source_is_answered_gone_without_a_look() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 7);
        kernel.submit();

        kernel.end(H);
        kernel.submit();
        assert_eq!(kernel.drain(), [(7, -(SyscallError::NotFound as i32))]);
    });
}

/// A full ring stops the look, and the poll it did not reach is answered by
/// the next wait's rather than dropped.
#[test]
fn a_full_ring_leaves_a_fired_poll_for_the_next_look() {
    loom::model(|| {
        let kernel = Kernel::new(1);
        kernel.watch(H, 3);
        kernel.watch(G, 4);
        kernel.submit();

        for handle in [H, G] {
            kernel.fill(handle);
            kernel.post(handle);
        }
        kernel.submit();
        assert_eq!(kernel.drain(), [(3, BYTES)]);
        kernel.submit();
        assert_eq!(kernel.drain(), [(4, BYTES)]);
    });
}

/// The log's unread records are a property of the reader's cursor, which the
/// kernel does not hold: there is nothing to look at, and its post is the
/// answer.
#[test]
fn a_post_answers_for_an_object_with_nothing_to_look_at() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(L, 7);
        kernel.submit();
        assert_eq!(kernel.drain(), NOTHING);

        kernel.post(L);
        kernel.submit();
        assert_eq!(kernel.drain(), [(7, BYTES)]);
    });
}

/// A watch that replaces an armed poll takes it off its object's watch: a
/// poll left live there is one more entry for every re-watch of an idle handle.
#[test]
fn a_replaced_poll_is_withdrawn_from_its_watch() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 1);
        kernel.submit();
        kernel.watch(H, 2);
        kernel.submit();
        assert_eq!(kernel.live(H), 1, "the replaced poll is still live on its object's watch");
    });
}

/// A ring torn down leaves no poll live on any watch.
#[test]
fn a_ring_torn_down_withdraws_every_poll() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 1);
        kernel.watch(G, 2);
        kernel.submit();

        kernel.polls.borrow_mut().withdraw_all();
        assert_eq!((kernel.live(H), kernel.live(G)), (0, 0));
    });
}

/// A handle closed since it was watched names nothing to look at: the look's
/// refusal is the answer.
#[test]
fn a_handle_closed_since_its_watch_is_answered_with_the_refusal() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 7);
        kernel.submit();

        kernel.close(H);
        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), [(7, -(SyscallError::NotFound as i32))]);
    });
}

/// A ring keeps its cap of polls and no more, and a watch that replaces one
/// of them is inside it.
#[test]
fn a_ring_keeps_its_cap_of_polls_and_no_more() {
    loom::model(|| {
        let kernel = Kernel { cap: 2, ..Kernel::new(4) };
        assert!(kernel.admit(H, 1));
        assert!(kernel.admit(G, 2), "a ring under its cap refused a poll");
        assert!(!kernel.admit(L, 3), "a ring at its cap kept one more poll");
        assert!(kernel.admit(H, 4), "a ring at its cap refused a watch that replaces a poll");
    });
}

/// A second thread's watch lands while the look at the handle's fired poll
/// finds nothing: the newer watch holds the handle's place, and the look arms
/// nothing beside it.
#[test]
fn a_watch_during_a_look_that_finds_nothing_is_the_handles_one_poll() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 1);
        kernel.submit();

        kernel.post(H);
        kernel.lands_during_look.set(Some((H, 2)));
        kernel.submit();
        assert_eq!(kernel.drain(), NOTHING);
        assert_eq!(kernel.live(H), 1, "the look armed the older watch again beside the newer");

        kernel.fill(H);
        kernel.post(H);
        kernel.submit();
        assert_eq!(kernel.drain(), [(2, BYTES)]);
    });
}

/// The same while the look finds bytes: one arrival, answered once, under the
/// newer watch's token, by the pass after the one it landed in.
#[test]
fn a_watch_during_a_look_that_finds_bytes_answers_alone() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 1);
        kernel.submit();

        kernel.fill(H);
        kernel.post(H);
        kernel.lands_during_look.set(Some((H, 2)));
        kernel.submit();
        assert_eq!(kernel.drain(), NOTHING, "the look answered under the watch it was replaced by");
        assert!(kernel.awake(), "the newer watch's fire is owed the next look");
        kernel.submit();
        assert_eq!(kernel.drain(), [(2, BYTES)]);
    });
}

/// A submitter does not park over a poll it could answer, and does over a
/// poll a full ring leaves it no room to answer: it would go round for good.
#[test]
fn a_submitter_parks_only_with_nothing_it_could_answer() {
    loom::model(|| {
        let kernel = Kernel::new(1);
        kernel.watch(H, 3);
        kernel.watch(G, 4);
        kernel.submit();
        assert!(!kernel.awake(), "nothing is owed a look");

        for handle in [H, G] {
            kernel.fill(handle);
            kernel.post(handle);
        }
        assert!(kernel.awake(), "a fired poll is owed a look and the ring has room");
        kernel.submit();
        assert!(!kernel.awake(), "the ring is full: the poll left over waits for room");
        assert_eq!(kernel.drain(), [(3, BYTES)]);
        assert!(kernel.awake(), "room came back with a poll still owed its look");
    });
}

/// A peer that posts an empty object as fast as the submitter looks at it does
/// not hold the look: the poll renewed in this pass waits for the next, and
/// the handle behind it is answered.
#[test]
fn a_poll_fired_as_it_is_renewed_waits_for_the_next_look() {
    loom::model(|| {
        let kernel = Kernel::new(4);
        kernel.watch(H, 3);
        kernel.watch(G, 4);
        kernel.submit();

        kernel.post(H);
        kernel.fill(G);
        kernel.post(G);
        kernel.posts_after_renewal.set(3);
        kernel.submit();
        assert_eq!(kernel.looks.get(), 2, "the look went round on a poll it had renewed");
        assert_eq!(kernel.drain(), [(4, BYTES)]);
        assert!(kernel.awake(), "the renewed poll's fire is owed the next look");
    });
}
