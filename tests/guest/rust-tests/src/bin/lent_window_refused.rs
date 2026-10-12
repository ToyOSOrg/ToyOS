//! A file server lets go a hello whose window the kernel says is not a region
//! `WINDOW_BYTES` long, whatever the client says: a pipe end, or a region
//! twice that; and the server that let it go answers the next client as the
//! same process.

use toyos::fs::WINDOW_BYTES;
use toyos_abi::syscall::SyscallError;

#[path = "../lent_hello.rs"]
mod lent_hello;
use lent_hello::{hello, region, window};

fn main() {
    let served = hello(window()).expect("lent_window_refused: a window of WINDOW_BYTES is served");
    let (read, _write) = toyos::pipe_pair().expect("lent_window_refused: a pipe");

    // Every hostile hello is made before any is judged, so a server that serves one is seen serving each.
    let answers = [
        ("a pipe end", hello(read.into())),
        ("a region twice WINDOW_BYTES long", hello(region(2 * WINDOW_BYTES))),
    ];
    let again = hello(window()).expect("lent_window_refused: a window after the refusals is served");

    for (what, answer) in &answers {
        println!("lent_window_refused: a hello lending {what} answered {answer:?}");
    }
    assert!(
        answers.iter().all(|(_, answer)| *answer == Err(SyscallError::Gone)),
        "lent_window_refused: a hostile hello was not let go unanswered"
    );
    assert_eq!(again, served, "lent_window_refused: the server that let go is not the one that answers");
}
