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
//! loom's atomics in this crate; every model here is one thread.
//!
//! The negative case is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features post-is-an-answer \
//!   --test inbox_answer
//! ```
//!
//! answers a fired poll without looking, as a ring did before `polls.rs`, and
//! this file must red.

#![cfg(feature = "loom")]

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use kernel_loom::inbox_polls::{deliver, Look, Poll, Polls, Submitter, Wake};
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

/// What a fire tells: a count, standing in for the ring's waiter.
#[derive(Clone, Default)]
struct Owed(Arc<AtomicU32>);

impl Wake for Owed {
    fn owe(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// One object: whether it holds bytes, and the polls armed on its watch.
#[derive(Default)]
struct Object {
    bytes: Cell<bool>,
    /// Whether a post on it is its readiness, the kernel holding none to look at.
    posts_are_readiness: bool,
    armed: RefCell<Vec<Arc<Poll<Owed>>>>,
}

/// One ring's kernel over the objects `H`, `G` and `L`.
struct Kernel {
    owed: Owed,
    polls: RefCell<Polls<Owed>>,
    h: Object,
    g: Object,
    l: Object,
    /// The completion ring, and how many answers it holds.
    ring: RefCell<Vec<(u64, i32)>>,
    size: usize,
}

impl Kernel {
    fn new(size: usize) -> Self {
        Self {
            owed: Owed::default(),
            polls: RefCell::new(Polls::new()),
            h: Object::default(),
            g: Object::default(),
            l: Object { posts_are_readiness: true, ..Object::default() },
            ring: RefCell::new(Vec::new()),
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

    /// `OP_WATCH` for bytes on `handle`, answered under `token`.
    fn watch(&self, handle: RawHandle, token: u64) {
        self.arm(Poll::new(self.owed.clone(), token, handle, READABLE));
    }

    /// The handle's one poll: fired now if its object holds bytes, armed on
    /// its watch if not.
    fn arm(&self, poll: Poll<Owed>) {
        let poll = Arc::new(poll);
        assert!(self.polls.borrow_mut().admit(poll.clone(), 16));
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

    /// `inbox_submit`'s look at what the ring is owed.
    fn submit(&self) {
        deliver(|| self.polls.borrow_mut().take_owed(), self);
    }

    /// Every answer the reader's drain hands it.
    fn drain(&self) -> Vec<(u64, i32)> {
        self.ring.take()
    }
}

impl Submitter<Owed> for Kernel {
    fn room(&self) -> bool {
        self.ring.borrow().len() < self.size
    }

    fn answer(&self, user_data: u64, result: i32) {
        self.ring.borrow_mut().push((user_data, result));
    }

    fn look(&self, poll: &Poll<Owed>) -> Look {
        let object = self.object(poll.handle);
        if object.bytes.get() || (object.posts_are_readiness && poll.posted() & READABLE != 0) {
            return Look::Ready(READABLE);
        }
        self.arm(Poll::new(self.owed.clone(), poll.user_data, poll.handle, poll.flags));
        Look::Armed
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
