//! A watch is answered for what its object holds, never for a post.
//!
//! One poller watches two pipes, both empty and both armed. A write of no
//! bytes into the first posts its readers and leaves nothing to read; one byte
//! then goes into the second. The wait that follows answers the second pipe's
//! token alone: a reader handed the first pipe's would read it blocking and
//! park for good.

use toyos::poller::{Poller, READABLE};

const EMPTY: u64 = 1;
const FILLED: u64 = 2;

fn main() {
    let empty = toyos::pipe_pair().expect("the pipe that stays empty");
    let filled = toyos::pipe_pair().expect("the pipe that takes a byte");

    let poller = Poller::new(2);
    poller.watch(&empty.0, READABLE, EMPTY);
    poller.watch(&filled.0, READABLE, FILLED);
    // A non-blocking enter, so both polls are armed before either write: a
    // write that found none would post nothing.
    poller.wait(0, 0, |token| panic!("nothing is ready yet, got token {token}"));

    let byte = [0x5A];
    // A slice of a real buffer: the kernel is handed an address it can read.
    assert_eq!(empty.1.write(&byte[..0]), Ok(0), "a write of no bytes");
    assert_eq!(filled.1.write(&byte), Ok(1), "a write of one byte");

    let mut tokens = Vec::new();
    poller.wait(1, u64::MAX, |token| tokens.push(token));
    assert_eq!(tokens, [FILLED], "the wait answered a pipe with nothing to read");
    println!("inbox_empty_write: a write of no bytes answered no watch");
}
