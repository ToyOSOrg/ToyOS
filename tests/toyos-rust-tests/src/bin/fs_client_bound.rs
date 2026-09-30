//! A file server serves a bounded number of clients, and a client past the
//! bound is answered `ResourceExhausted` at its hello — refused by name, never
//! left waiting in the port's queue.
//!
//! Raw connections to `fs:/home`, each lending the same window, which costs
//! one region however many there are. Every other program on this boot holds
//! connections of its own, so the refusal comes at or before this process's
//! `MAX_SERVED + 1`th.

use toyos::fs::{Reply, Request, HELLO, REPLY, WINDOW_BYTES};
use toyos::ipc::Connection;
use toyos::shm::SharedMemory;
use toyos_abi::syscall::SyscallError;

/// Mirrored from `userland/fsd/src/main.rs`.
const MAX_SERVED: usize = 128;

fn answer(conn: &Connection) -> Reply {
    let header = conn.recv_header().expect("a reply to the hello");
    assert_eq!(header.msg_type, REPLY, "a reply frame");
    conn.recv_payload(&header).expect("a reply's words")
}

fn main() {
    let names = toyos::endow::namespace().expect("this program was endowed a namespace");
    let window = SharedMemory::create(WINDOW_BYTES).expect("a window");
    let mut held = Vec::new();
    for n in 1..=MAX_SERVED + 1 {
        let conn = names.open("fs:/home").expect("this program holds fs:/home");
        conn.send_with_handles(&[window.share().expect("the window, shared")], HELLO, &Request::new())
            .expect("hello");
        let reply = answer(&conn);
        if reply.status == 0 {
            held.push(conn);
            continue;
        }
        assert_eq!(
            reply.status,
            SyscallError::ResourceExhausted.to_u64(),
            "connection {n} was refused status {}, not ResourceExhausted",
            reply.status
        );
        println!("fs_client_bound: connection {n} of this process refused ResourceExhausted at its hello");
        println!("fs_client_bound: PASS");
        return;
    }
    panic!("{} connections of this process were all served, past the bound of {MAX_SERVED}", MAX_SERVED + 1);
}
