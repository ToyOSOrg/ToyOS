//! Two processes, two `write`s per line, and not one line that belongs to
//! both — nor one a program put on the console itself.
//!
//! **The defect this is aimed at is a `write` being the unit of
//! interleaving.** `println!` is a `LineWriter`: it issues `flush_buf()` and
//! then `inner.write(rest)`, so a line leaves the program in two pieces with an
//! arbitrary gap between them, and anything else written in that gap could
//! land inside the line. Each process assembles its own line before it is a
//! record (`toyos::log::stdio`), so two *processes* is the shape that tests it
//! and two threads would not.
//!
//! The two writes are made by hand rather than through `println!` because the
//! split has to be the subject rather than an implementation detail of `std`:
//! the line is a fixed width, the gap is exactly in the middle, and the newline
//! is on the second write, which is the piece the line waits for.
//!
//! **And the console is not a program's to write.** A program's console is
//! read-only: its lines reach the console only through `logd`, under its name,
//! so each writer's first act is a line on its console that must be refused.
//!
//! The verdict is the host's — a line is mixed or it is not, and only the
//! console capture can say. What this binary owes the host is that both writers
//! ran, said how much, and agreed about it.

use std::io::Write;
use std::process::{exit, Command};

use toyos::log::stdio::{self, Stream};
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

const SELF_PATH: &str = "/system/bin/test_rs_console_line_atomicity";

/// Lines each writer emits.
///
/// **A count and not a duration**, so the gate's verdict does not move with the
/// host: the assertion is zero mixed lines out of `2 * LINES`, and a run that
/// produced fewer lines than that has failed the non-vacuity check rather than
/// passed a weaker version of the same test. Both writers' lines fit the
/// runner's log ring unread, so a line missing is one lost, never one the ring
/// had no room for.
const LINES: usize = 400;

/// Bytes in one line, newline included.
///
/// Two hundred, which is comfortably inside `MAX_CONSOLE_LINE`'s 1024 — the
/// claim under test is that a whole line is one unit, and a line past that
/// bound is deliberately emitted in pieces of it, which is a different
/// sentence.
const WIDTH: usize = 200;

/// Bytes the third writer says, in two `write`s, never ending them with a
/// newline.
///
/// **The other half of the same line.** A line leaves on the `\n` that ends
/// it; the one moment a partial line stops being "not finished yet" and becomes
/// "all there will ever be" is the process exiting, which ends it. Without that
/// these bytes are dropped on the floor — a dying process's last words lost —
/// and a hundred of them arriving whole is also proof the line accumulated
/// across two `write`s to get there.
const MIDLINE: usize = 100;

/// The byte the third writer repeats. Not `A` or `B`, so the whole-line count
/// cannot see it, and a hundred of them in a row is nothing an ordinary console
/// line contains.
const MIDLINE_BYTE: u8 = b'C';

/// Digits of the per-writer sequence number each line carries after its
/// leading tag byte, zero-padded so every line stays exactly [`WIDTH`] bytes.
const SEQ_DIGITS: usize = 6;

/// The console, at stdin's slot: the runner starts this job holding its own
/// there (`CONSOLE_JOBS`), and each writer inherits it, read-only.
const CONSOLE: RawHandle = RawHandle(0);

/// A line in the kernel's own shape, which a program on the console could
/// pass off as a record.
const FORGED: &[u8] = b"[kernel 0.000 cpu0] console-atomicity: a record no kernel wrote\n";

fn main() {
    let mut args = std::env::args();
    let _ = args.next();
    match args.next().as_deref() {
        Some("C") => exit_mid_line(),
        Some(tag) => {
            let byte = tag.as_bytes().first().copied().unwrap_or(b'?');
            write_lines(byte);
        }
        None => parent(),
    }
}

/// Say half of something, say the other half, and exit without ever ending it.
///
/// No newline anywhere, so nothing in the write path makes these bytes a
/// line: what does is this process exiting. Through `std`, whose exit is the
/// one that ends it, with a flush between the halves, so the first leaves as
/// a piece the line goes on from.
fn exit_mid_line() {
    let partial = [MIDLINE_BYTE; MIDLINE];
    let (head, tail) = partial.split_at(MIDLINE / 2);
    let mut out = std::io::stdout();
    let wrote = out.write_all(head).and_then(|()| out.flush()).and_then(|()| out.write_all(tail));
    if let Err(e) = wrote {
        eprintln!("console-atomicity: the mid-line writer's write failed: {e}");
        exit(1);
    }
    exit(0);
}

/// A piece taken whole, or this process ends saying what it got instead.
fn written(got: Result<usize, SyscallError>, len: usize, who: &str) {
    if got != Ok(len) {
        eprintln!("console-atomicity: {who} wrote {got:?} of {len}");
        exit(1);
    }
}

/// Spawn the two writers and wait for both.
///
/// Two processes and not two threads: the buffer is per console object and a
/// process gets its own, so two threads of one process share one buffer and
/// would prove nothing about the property the object exists to have.
fn parent() {
    let mut children = Vec::new();
    for tag in ["A", "B"] {
        match Command::new(SELF_PATH).arg(tag).spawn() {
            Ok(child) => children.push((tag, child)),
            Err(e) => {
                eprintln!("console-atomicity: writer {tag} would not start: {e}");
                exit(1);
            }
        }
    }
    for (tag, mut child) in children {
        match child.wait() {
            Ok(status) if status.code() == Some(0) => {}
            Ok(status) => {
                eprintln!("console-atomicity: writer {tag} exited {:?}", status.code());
                exit(1);
            }
            Err(e) => {
                eprintln!("console-atomicity: writer {tag} would not be waited for: {e}");
                exit(1);
            }
        }
    }
    // **The mid-line writer runs after the other two are gone**, so the bytes
    // its exit flushes cannot land inside a line the count above is reading —
    // they are on the wire on their own, which is what lets the host look for
    // them as a run rather than as a line.
    match Command::new(SELF_PATH).arg("C").spawn().and_then(|mut c| c.wait()) {
        Ok(status) if status.code() == Some(0) => {}
        Ok(status) => {
            eprintln!("console-atomicity: the mid-line writer exited {:?}", status.code());
            exit(1);
        }
        Err(e) => {
            eprintln!("console-atomicity: the mid-line writer would not run: {e}");
            exit(1);
        }
    }
    // After all three, so the count the host checks against is a claim about a
    // run that finished rather than one still going. It follows the mid-line
    // writer's unterminated bytes on the wire, which is why the host finds this
    // declaration with a substring search rather than a whole-line one.
    println!(
        "console-atomicity: writers=2 lines={LINES} width={WIDTH} midline={MIDLINE} \
         seq={SEQ_DIGITS}"
    );
}

/// One writer: `LINES` numbered lines of one repeated byte, each in two
/// `write`s.
///
/// The sequence number after the leading tag byte is what lets the host tell
/// a gap in a writer's own run from a capture that ends early — a count alone
/// reads both as the same missing-lines number.
fn write_lines(tag: u8) {
    match syscall::write(CONSOLE, FORGED) {
        Err(SyscallError::PermissionDenied) => {}
        other => {
            eprintln!("console-atomicity: writer {} put a line on its console: {other:?}", tag as char);
            exit(1);
        }
    }
    let mut line = [tag; WIDTH];
    line[WIDTH - 1] = b'\n';
    for seq in 0..LINES {
        let mut digits = [0u8; SEQ_DIGITS];
        let mut rest = seq;
        for d in digits.iter_mut().rev() {
            *d = b'0' + (rest % 10) as u8;
            rest /= 10;
        }
        line[1..1 + SEQ_DIGITS].copy_from_slice(&digits);
        let (head, tail) = line.split_at(WIDTH / 2);
        // Refused rather than retried: a short write here is the sink taking
        // half a line, which is the defect and not an error to paper over.
        for piece in [head, tail] {
            written(stdio::write(Stream::Out, piece), piece.len(), &format!("writer {}", tag as char));
        }
    }
}
