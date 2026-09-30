//! A client's number is bounded where the server keeps it: a stream asked to
//! start past the largest offset a file has is refused, and the server that
//! refused it goes on answering.
//!
//! A raw client of `fs:/home`, past std: std never asks for such a stream, and
//! any program holding `/home` can. Accepted, the offset would grow by what
//! the pipe carries until it overflowed, which panics the server that every
//! program's `/apps`, `/config`, `/home` and `/state` are on.

use toyos::fs::{
    window_put, Reply, Request, HELLO, MAX_FILE_BYTES, OPEN, O_CREATE, O_WRITE, REPLY, STREAM, WINDOW_BYTES,
};
use toyos::ipc::Connection;
use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos_abi::syscall::SyscallError;

/// Under `/home`, relative to it.
const NAME: &[u8] = b"fs_stream_offset";

fn answer(conn: &Connection) -> Reply {
    let header = conn.recv_header().expect("a reply: the server is still there");
    assert_eq!(header.msg_type, REPLY, "a reply frame");
    conn.recv_payload(&header).expect("a reply's words")
}

fn open(conn: &Connection, window: &SharedMemory) -> Reply {
    // SAFETY: the region is `WINDOW_BYTES` long and outlives this use.
    window_put(unsafe { Window::new(window.as_ptr(), WINDOW_BYTES) }, 0, NAME);
    let request = Request { len: NAME.len() as u64, flags: O_WRITE | O_CREATE, ..Request::new() };
    conn.send(OPEN, &request).expect("open");
    answer(conn)
}

fn main() {
    let names = toyos::endow::namespace().expect("this program was endowed a namespace");
    let conn = names.open("fs:/home").expect("this program holds fs:/home");
    let window = SharedMemory::create(WINDOW_BYTES).expect("a window");
    conn.send_with_handles(&[window.share().expect("the window, shared")], HELLO, &Request::new())
        .expect("hello");
    assert_eq!(answer(&conn).status, 0, "fs:/home answers its hello");

    let opened = open(&conn, &window);
    assert_eq!(opened.status, 0, "{} opened to write", core::str::from_utf8(NAME).unwrap_or(""));
    let fid = opened.value;

    for offset in [u64::MAX - 1, MAX_FILE_BYTES + 1] {
        conn.send(STREAM, &Request { fid, offset, ..Request::new() }).expect("stream");
        let reply = answer(&conn);
        if reply.status == 0 {
            // What the server's end would have been: the pipe's bytes drained
            // at an offset that overflows.
            if let Some([end]) = conn.recv_handles_exact::<1>() {
                let _ = toyos_abi::syscall::write(end, &[0x5A; 16]);
                toyos_abi::syscall::close(end);
            }
            panic!("a stream at offset {offset:#x} was accepted");
        }
        assert_eq!(
            reply.status,
            SyscallError::InvalidArgument.to_u64(),
            "a stream at offset {offset:#x} was answered status {}",
            reply.status
        );
        println!("fs_stream_offset: a stream at {offset:#x} refused InvalidArgument");
    }

    // The same connection, so an end of the server between the two would be
    // this read failing and not a reconnect hiding it.
    assert_eq!(open(&conn, &window).status, 0, "the server answers after the refusals");
    drop(conn);
    let _ = std::fs::remove_file("/home/fs_stream_offset");
    println!("fs_stream_offset: PASS");
}
