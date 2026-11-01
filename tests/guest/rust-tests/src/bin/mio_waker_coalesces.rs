//! mio's ToyOS waker, from the `ToyOSOrg/mio` fork, which no program in the
//! image builds: any number of wakes before a poll are one event, the poll
//! that reports it takes it, and a wake after that is reported again.
//!
//! Red where the selector leaves the waker's readiness standing: a poll with
//! nothing new to say then reports the waker's token again, for good.

use std::time::Duration;

use mio::{Events, Poll, Token, Waker};

const WAKE: Token = Token(7);
const WAKES: usize = 10_000;
/// A hang's ceiling: the waker is rung before the poll that waits on it.
const CEILING: Duration = Duration::from_secs(10);

fn main() {
    let mut poll = Poll::new().expect("a poll");
    let waker = Waker::new(poll.registry(), WAKE).expect("a waker");
    let mut events = Events::with_capacity(8);

    for _ in 0..WAKES {
        waker.wake().expect("a wake");
    }
    poll.poll(&mut events, Some(CEILING)).expect("a poll");
    let tokens: Vec<Token> = events.iter().map(|event| event.token()).collect();
    assert_eq!(tokens, [WAKE], "{WAKES} wakes before a poll");

    poll.poll(&mut events, Some(Duration::ZERO)).expect("a poll");
    let tokens: Vec<Token> = events.iter().map(|event| event.token()).collect();
    assert!(tokens.is_empty(), "a poll after the wake was reported reported {tokens:?}");

    waker.wake().expect("a wake");
    poll.poll(&mut events, Some(CEILING)).expect("a poll");
    let tokens: Vec<Token> = events.iter().map(|event| event.token()).collect();
    assert_eq!(tokens, [WAKE], "a wake after the last was reported");
    println!("mio_waker_coalesces: {WAKES} wakes were one event, the poll that reported it took it, and the next wake was reported");
}
