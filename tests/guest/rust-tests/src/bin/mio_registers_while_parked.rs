//! mio's ToyOS selector watches a registration another thread makes while a
//! poll is parked: a waker registered from a second thread once the first is
//! parked in a poll with no timeout, then woken, is the event that poll
//! returns.
//!
//! The poll is parked with nothing else registered: a `Poll` holds one waker,
//! and the waker is the one source this boot can make ready, having no
//! netstack. Red where the poll returns without the waker's event, or never
//! returns: the second thread then ends the process past [`CEILING`], by name.

#[path = "../roster.rs"]
mod roster;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use mio::{Events, Poll, Token, Waker};
use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;

const LATE: Token = Token(7);
/// A hang's ceiling: the waker is woken once the poll is parked, and the poll
/// is owed nothing else.
const CEILING: Duration = Duration::from_secs(10);

/// Set just before the main thread polls.
static POLLING: AtomicBool = AtomicBool::new(false);

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let mut poll = Poll::new().expect("a poll");
    let registry = poll.registry().try_clone().expect("a registry for the second thread");
    let (returned, on_return) = mpsc::channel::<()>();

    let late = std::thread::spawn(move || {
        // Past `POLLING` the main thread blocks only in the poll: parked, or
        // on the selector's lock with its copy of the registrations taken.
        roster::await_true(|| {
            POLLING.load(Ordering::SeqCst)
                && roster::my_threads(&cap).iter().any(|&(is_thread, state)| !is_thread && state == roster::BLOCKED)
        });
        let waker = Waker::new(&registry, LATE).expect("a waker registered while the poll is parked");
        waker.wake().expect("a wake");
        if on_return.recv_timeout(CEILING).is_err() {
            eprintln!("mio_registers_while_parked: a waker registered and woken while the poll was parked was not its event within {CEILING:?}");
            std::process::exit(1);
        }
        waker
    });

    let mut events = Events::with_capacity(8);
    POLLING.store(true, Ordering::SeqCst);
    poll.poll(&mut events, None).expect("a poll");
    returned.send(()).expect("the registering thread waits for the poll's return");
    let tokens: Vec<Token> = events.iter().map(|event| event.token()).collect();
    let _waker = late.join().expect("the registering thread");
    assert_eq!(tokens, [LATE], "a poll parked while another thread registered and woke a waker");
    println!("mio_registers_while_parked: a waker registered while the poll was parked was its event");
}
