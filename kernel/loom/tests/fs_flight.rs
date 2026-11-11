//! Loom: one file server connection's requests in flight, the SDK's
//! (`sdk/toyos/src/fs/flight.rs`).
//!
//! Callers share a connection whose replies whoever waits receives: a reply
//! filed in another caller's slot, the receive turn handed on, a connection
//! that ends under requests in flight, and a request too large for a slot
//! holding every slot. Each reply is read from a loom cell, so one read before
//! its filing is ordered is loom's own causality violation. Every wait blocks,
//! the futex's as the shim models it and the wire's, so a caller left waiting
//! with nobody receiving is loom's deadlock.

#![cfg(feature = "loom")]

use std::collections::VecDeque;

use kernel_loom::fs_flight::{Flight, Taken, SLOTS};
use loom::sync::atomic::{AtomicU32, Ordering};
use loom::sync::{Arc, Condvar, Mutex};
use loom::thread;

/// The connection's generation in these models.
const LINK: u64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    Gone,
    Malformed,
}

/// One connection: requests to the server, replies back, and its end, each
/// wait on it blocking, so a caller that nobody answers is loom's deadlock.
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

struct World {
    flight: Flight<u64, Why>,
    wire: Wire,
}

/// One request and its reply, as a caller of the SDK's transport makes it.
fn ask(world: &World, whole: bool) -> Result<u64, Why> {
    let taken = world.flight.take(whole);
    let tag = world.flight.sent(taken, LINK);
    world.wire.ask(tag);
    let answered = world.flight.answer(taken, || world.wire.receive(), Why::Malformed);
    world.flight.give(taken);
    if let Ok(reply) = answered {
        assert_eq!(reply, tag, "a caller was answered another caller's reply");
    }
    answered
}

fn model(f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(3);
    builder.check(f);
}

fn world() -> Arc<World> {
    Arc::new(World { flight: Flight::new(), wire: Wire::default() })
}

/// Two callers, the server answering the later request first: each is
/// answered its own reply, whichever of them receives.
#[test]
fn each_caller_is_answered_its_own_reply_whoever_receives() {
    model(|| {
        let world = world();
        let callers: Vec<_> = (0..2)
            .map(|_| {
                let world = Arc::clone(&world);
                thread::spawn(move || ask(&world, false))
            })
            .collect();
        for tag in world.wire.take(2).into_iter().rev() {
            world.wire.answer(tag);
        }
        for caller in callers {
            assert!(caller.join().unwrap().is_ok());
        }
    });
}

/// The connection ends with a request in flight and another on its way: both
/// fail, and neither waits on.
#[test]
fn a_connection_that_ends_fails_every_request_on_it() {
    model(|| {
        let world = world();
        let callers: Vec<_> = (0..2)
            .map(|_| {
                let world = Arc::clone(&world);
                thread::spawn(move || ask(&world, false))
            })
            .collect();
        world.wire.take(1);
        world.wire.end();
        for caller in callers {
            assert_eq!(caller.join().unwrap(), Err(Why::Gone));
        }
    });
}

/// A reply whose tag names no request in flight fails the connection's
/// requests rather than answering one.
#[test]
fn a_reply_naming_no_request_fails_the_connection() {
    model(|| {
        let world = world();
        let caller = {
            let world = Arc::clone(&world);
            thread::spawn(move || ask(&world, false))
        };
        let tag = world.wire.take(1)[0];
        world.wire.answer(tag + SLOTS as u64);
        assert_eq!(caller.join().unwrap(), Err(Why::Malformed));
    });
}

/// A caller holding every slot holds them alone.
#[test]
fn every_slot_is_held_by_one_caller_alone() {
    model(|| {
        let flight = Arc::new(Flight::<u64, Why>::new());
        let holding = Arc::new(AtomicU32::new(0));
        let hold = |flight: &Flight<u64, Why>, holding: &AtomicU32, whole: bool| {
            let taken: Taken = flight.take(whole);
            let mine = if whole { u32::MAX } else { 1 << taken.first() };
            let before = holding.fetch_or(mine, Ordering::AcqRel);
            assert_eq!(before & mine, 0, "two callers held one slot");
            assert!(!whole || before == 0, "every slot was taken beside a slot held");
            assert!(before != u32::MAX, "a slot was taken beside every slot held");
            holding.fetch_and(!mine, Ordering::AcqRel);
            flight.give(taken);
        };
        let other = {
            let (flight, holding) = (Arc::clone(&flight), Arc::clone(&holding));
            thread::spawn(move || hold(&flight, &holding, true))
        };
        hold(&flight, &holding, false);
        other.join().unwrap();
    });
}
