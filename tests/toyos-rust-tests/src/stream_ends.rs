//! Each way a stream ends, as std reports it to a client: the same code runs
//! in a guest against netstack and on the harness's host against the host
//! kernel's TCP, and the two reports are compared line for line.
//!
//! The peer is the harness's: a connection's first byte names what it does,
//! and `tests/netcase/stream_ends.c` dials it with the same bytes. Every end it
//! makes waits on something the client did, so no line depends on how long
//! anything took.
//!
//! Std alone: the harness compiles this file too.

use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, TcpStream};

/// What the peer sends before the reset of `reset_mid_stream`.
pub const AHEAD: usize = 1 << 16;

/// The peer sends its FIN before the client writes.
pub const PEER_FIN_FIRST: u8 = b'F';
/// The peer sends [`AHEAD`] bytes, reads the client's one, then resets.
pub const RESET_MID_STREAM: u8 = b'R';
/// The peer reads to the client's end, then resets.
pub const RESET_AFTER_HALF_CLOSE: u8 = b'D';
/// The peer sends three bytes and its FIN.
pub const BOTH_CLOSE: u8 = b'B';
/// The peer answers one byte; the client then shuts its sending half with
/// nothing pending, and the peer, at that FIN, sends four bytes and its own.
pub const SHUT_FIRST: u8 = b'S';

/// The report a host's TCP gives, and so the one netstack owes.
pub const EXPECTED: [&str; 6] = [
    "peer_fin_first: the read Ok(0), then a write Ok(5)",
    "reset_mid_stream: 65536 bytes came as sent, the answer Ok(1), then Err(ConnectionReset)",
    "reset_after_half_close: shut the sending half: Ok, then Err(ConnectionReset)",
    "both_close: 3 bytes came as sent, then Ok(0)",
    "both_close: shut the sending half: Ok, then Ok(0)",
    "shut_first: the answer Ok(1), shut the sending half: Ok, 4 bytes came as sent, then Ok(0)",
];

/// Byte `i` of what the peer sends ahead of a reset: a pattern no shift or
/// cut reproduces.
pub fn pattern(i: usize) -> u8 {
    (i % 251) as u8
}

fn kind<T>(result: std::io::Result<T>) -> String {
    match result {
        Ok(_) => "Ok".to_string(),
        Err(e) => format!("Err({:?})", e.kind()),
    }
}

fn count(result: std::io::Result<usize>) -> String {
    match result {
        Ok(n) => format!("Ok({n})"),
        Err(e) => format!("Err({:?})", e.kind()),
    }
}

/// What one more read answers: the end, or a byte that is not one.
fn the_end(stream: &mut TcpStream) -> String {
    count(stream.read(&mut [0u8; 1]))
}

/// Reads until `want` bytes, the end or a refusal: how many matched
/// `expect`, and what stopped it, if anything did before `want`.
fn take(stream: &mut TcpStream, want: usize, expect: impl Fn(usize) -> u8) -> (String, Option<String>) {
    let mut buf = vec![0u8; 65536];
    let mut got = 0;
    while got < want {
        let room = buf.len().min(want - got);
        match stream.read(&mut buf[..room]) {
            Ok(0) => return (format!("{got} bytes came"), Some("Ok(0)".to_string())),
            Ok(n) => {
                if let Some(at) = (0..n).find(|&i| buf[i] != expect(got + i)) {
                    return (format!("byte {} differs after {got} bytes", got + at), None);
                }
                got += n;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return (format!("{got} bytes came"), Some(format!("Err({:?})", e.kind()))),
        }
    }
    (format!("{got} bytes came"), None)
}

fn dial(host: &str, port: u16, what: u8) -> TcpStream {
    let mut stream = TcpStream::connect((host, port)).unwrap_or_else(|e| panic!("the peer at {host}:{port}: {e}"));
    stream.write_all(&[what]).unwrap_or_else(|e| panic!("naming the end to the peer: {e}"));
    stream
}

/// Every end, in order: one line each as [`EXPECTED`] spells them.
pub fn run(host: &str, port: u16, mut say: impl FnMut(String)) {
    let mut stream = dial(host, port, PEER_FIN_FIRST);
    let read = the_end(&mut stream);
    say(format!("peer_fin_first: the read {read}, then a write {}", count(stream.write(b"after"))));

    let mut stream = dial(host, port, RESET_MID_STREAM);
    let line = match take(&mut stream, AHEAD, pattern) {
        (came, Some(end)) => format!("{came}, then {end}"),
        (came, None) => {
            let answer = count(stream.write(b"k"));
            format!("{came} as sent, the answer {answer}, then {}", the_end(&mut stream))
        }
    };
    say(format!("reset_mid_stream: {line}"));

    let mut stream = dial(host, port, RESET_AFTER_HALF_CLOSE);
    let shut = kind(stream.shutdown(Shutdown::Write));
    say(format!("reset_after_half_close: shut the sending half: {shut}, then {}", the_end(&mut stream)));

    let mut stream = dial(host, port, BOTH_CLOSE);
    let (came, end) = take(&mut stream, 3, |i| b"bye"[i]);
    let end = end.unwrap_or_else(|| the_end(&mut stream));
    say(format!("both_close: {came} as sent, then {end}"));
    let shut = kind(stream.shutdown(Shutdown::Write));
    say(format!("both_close: shut the sending half: {shut}, then {}", the_end(&mut stream)));

    let mut stream = dial(host, port, SHUT_FIRST);
    let answer = the_end(&mut stream);
    let shut = kind(stream.shutdown(Shutdown::Write));
    let (came, end) = take(&mut stream, 4, |i| b"late"[i]);
    let end = end.unwrap_or_else(|| the_end(&mut stream));
    say(format!("shut_first: the answer {answer}, shut the sending half: {shut}, {came} as sent, then {end}"));
}
