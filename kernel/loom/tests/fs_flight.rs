//! Loom: one file server connection's requests in flight, the SDK's
//! (`sdk/toyos/src/fs/flight.rs`).
//!
//! Callers share connections whose replies whoever waits receives: a reply
//! filed in another caller's slot, the receive turn handed on, a connection
//! that ends under requests in flight, a send that fails beside its own
//! connection's callers and beside a fresh connection, two connections each
//! receiving for itself, a slot used again,
//! and a request too large for a slot holding every slot. The futex the shim
//! gives wakes only the word woken, and every wire here blocks, so a caller
//! left waiting with nobody receiving or nobody waking it is loom's deadlock.

#![cfg(feature = "loom")]

use std::collections::VecDeque;

use kernel_loom::fs_flight::{Flight, Taken, SLOTS};
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    Gone,
    Malformed,
}

/// One connection: requests to the server, replies back, and its end, each
/// wait on it blocking.
#[derive(Default)]
struct Wire {
    state: Mutex<Queues>,
    moved: Condvar,
}

#[derive(Default)]
struct Queues {
    asked: VecDeque<u64>,
    answered: VecDeque<u64>,
    ended: bool,
}

impl Wire {
    fn ask(&self, tag: u64) {
        self.state.lock().unwrap().asked.push_back(tag);
        self.moved.notify_all();
    }

    /// The next reply, its tag answered as its value, or the end.
    fn receive(&self) -> Result<(u64, u64), Why> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(tag) = state.answered.pop_front() {
                return Ok((tag, tag));
            }
            if state.ended {
                return Err(Why::Gone);
            }
            state = self.moved.wait(state).unwrap();
        }
    }

    /// The next `n` requests, as the server takes them.
    fn take(&self, n: usize) -> Vec<u64> {
        let mut state = self.state.lock().unwrap();
        while state.asked.len() < n {
            state = self.moved.wait(state).unwrap();
        }
        state.asked.drain(..n).collect()
    }

    fn answer(&self, tag: u64) {
        self.state.lock().unwrap().answered.push_back(tag);
        self.moved.notify_all();
    }

    fn end(&self) {
        self.state.lock().unwrap().ended = true;
        self.moved.notify_all();
    }
}

/// The requests in flight, two connections by generation (1 and 2), and the
/// slots callers hold.
struct World {
    flight: Flight<u64, Why>,
    wires: [Wire; 2],
    holding: AtomicU32,
}

impl World {
    fn new() -> Arc<Self> {
        Arc::new(Self { flight: Flight::new(), wires: Default::default(), holding: AtomicU32::new(0) })
    }

    fn wire(&self, link: u64) -> &Wire {
        &self.wires[link as usize - 1]
    }

    /// Slots for one request, as `Dir`'s ticket takes them: held alone.
    fn take(&self, whole: bool) -> Taken {
        let taken = self.flight.take(whole);
        let mine = if whole { u32::MAX } else { 1 << taken.first() };
        let before = self.holding.fetch_or(mine, Ordering::AcqRel);
        assert_eq!(before & mine, 0, "two callers held one slot");
        assert!(!whole || before == 0, "every slot was taken beside a slot held");
        assert!(before != u32::MAX, "a slot was taken beside every slot held");
        taken
    }

    fn give(&self, taken: Taken) {
        let mine = if taken.count() == SLOTS { u32::MAX } else { 1 << taken.first() };
        self.holding.fetch_and(!mine, Ordering::AcqRel);
        self.flight.give(taken);
    }

    /// One request on connection `link` and its reply, as `Dir` makes it.
    fn ask(&self, link: u64, whole: bool) -> Result<u64, Why> {
        let taken = self.take(whole);
        let tag = self.flight.sent(taken, link);
        self.wire(link).ask(tag);
        let answered = self.flight.answer(taken, || self.wire(link).receive(), Why::Malformed);
        self.give(taken);
        if let Ok(reply) = answered {
            assert_eq!(reply, tag, "a caller was answered another caller's reply");
        }
        answered
    }

    /// A request on connection `link` whose send fails: its slot goes back
    /// unanswered.
    fn unsent(&self, link: u64) {
        let taken = self.take(false);
        self.flight.sent(taken, link);
        self.give(taken);
    }
}

/// Every interleaving of `f` with at most `preemptions` preemptions: 3 for a
/// model of three threads, 2 for one whose third thread asks twice or whose
/// fourth makes it run past minutes.
fn model(preemptions: usize, f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(preemptions);
    builder.check(f);
}

fn spawn(world: &Arc<World>, f: impl FnOnce(&World) -> Result<u64, Why> + Send + 'static) -> thread::JoinHandle<Result<u64, Why>> {
    let world = Arc::clone(world);
    thread::spawn(move || f(&world))
}

/// Two callers, the server answering the later request first: each is
/// answered its own reply, whichever of them receives.
#[test]
fn each_caller_is_answered_its_own_reply_whoever_receives() {
    model(3, || {
        let world = World::new();
        let callers: Vec<_> = (0..2).map(|_| spawn(&world, |w| w.ask(1, false))).collect();
        for tag in world.wire(1).take(2).into_iter().rev() {
            world.wire(1).answer(tag);
        }
        for caller in callers {
            assert!(caller.join().unwrap().is_ok());
        }
    });
}

/// One caller asks twice, its second request in the slot it gave back,
/// beside a caller asking once: each request is answered its own reply.
#[test]
fn a_slot_used_again_is_answered_its_own_reply() {
    model(2, || {
        let world = World::new();
        let twice = spawn(&world, |w| w.ask(1, false).and_then(|_| w.ask(1, false)));
        let once = spawn(&world, |w| w.ask(1, false));
        for tag in world.wire(1).take(2).into_iter().rev() {
            world.wire(1).answer(tag);
        }
        let last = world.wire(1).take(1)[0];
        world.wire(1).answer(last);
        assert!(twice.join().unwrap().is_ok());
        assert!(once.join().unwrap().is_ok());
    });
}

/// The connection ends with a request in flight and another on its way: both
/// fail, and neither waits on.
#[test]
fn a_connection_that_ends_fails_every_request_on_it() {
    model(3, || {
        let world = World::new();
        let callers: Vec<_> = (0..2).map(|_| spawn(&world, |w| w.ask(1, false))).collect();
        world.wire(1).take(1);
        world.wire(1).end();
        for caller in callers {
            assert_eq!(caller.join().unwrap(), Err(Why::Gone));
        }
    });
}

/// A caller on a connection that ends, a caller whose send on it fails, and a
/// caller on a fresh connection, which may take the slot the failed send gave
/// back: the failure of the first connection is filed in no request of the
/// second, which is answered.
#[test]
fn a_connection_that_ends_fails_no_request_on_a_fresh_one() {
    model(2, || {
        let world = World::new();
        let ended = spawn(&world, |w| w.ask(1, false));
        let unsent = spawn(&world, |w| {
            w.unsent(1);
            Ok(0)
        });
        let fresh = spawn(&world, |w| w.ask(2, false));
        world.wire(1).take(1);
        world.wire(1).end();
        let tag = world.wire(2).take(1)[0];
        world.wire(2).answer(tag);
        assert_eq!(ended.join().unwrap(), Err(Why::Gone));
        unsent.join().unwrap().unwrap();
        assert!(fresh.join().unwrap().is_ok(), "a fresh connection's request failed with the old one");
    });
}

/// Two callers and a send that fails, all on one connection: a turn handed to
/// the failed send goes on to the caller still waiting, which is answered.
#[test]
fn a_failed_send_hands_its_turn_on() {
    model(2, || {
        let world = World::new();
        let unsent = spawn(&world, |w| {
            w.unsent(1);
            Ok(0)
        });
        let callers: Vec<_> = (0..2).map(|_| spawn(&world, |w| w.ask(1, false))).collect();
        for _ in 0..2 {
            let tag = world.wire(1).take(1)[0];
            world.wire(1).answer(tag);
        }
        unsent.join().unwrap().unwrap();
        for caller in callers {
            assert!(caller.join().unwrap().is_ok());
        }
    });
}

/// A caller on one connection is answered while a caller on another holds
/// that connection's turn, whose server answers only once the first caller
/// has returned: each connection receives for itself, so neither waits on
/// the other.
#[test]
fn each_connection_receives_for_itself() {
    model(3, || {
        let world = World::new();
        let second = spawn(&world, |w| w.ask(2, false));
        let first = spawn(&world, |w| w.ask(1, false));
        let tag = world.wire(1).take(1)[0];
        world.wire(1).answer(tag);
        assert!(first.join().unwrap().is_ok());
        let tag = world.wire(2).take(1)[0];
        world.wire(2).answer(tag);
        assert!(second.join().unwrap().is_ok());
    });
}

/// A reply whose tag names no request in flight fails the connection's
/// requests rather than answering one.
#[test]
fn a_reply_naming_no_request_fails_the_connection() {
    model(3, || {
        let world = World::new();
        let caller = spawn(&world, |w| w.ask(1, false));
        let tag = world.wire(1).take(1)[0];
        world.wire(1).answer(tag + SLOTS as u64);
        assert_eq!(caller.join().unwrap(), Err(Why::Malformed));
    });
}

/// A request holding every slot and one holding a slot, each answered as it
/// comes: neither is held beside the other, and each is answered its own.
#[test]
fn every_slot_is_held_by_one_caller_alone() {
    model(3, || {
        let world = World::new();
        let whole = spawn(&world, |w| w.ask(1, true));
        let one = spawn(&world, |w| w.ask(1, false));
        for _ in 0..2 {
            let tag = world.wire(1).take(1)[0];
            world.wire(1).answer(tag);
        }
        assert!(whole.join().unwrap().is_ok());
        assert!(one.join().unwrap().is_ok());
    });
}
