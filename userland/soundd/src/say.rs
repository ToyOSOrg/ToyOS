//! soundd's output: one thread writes it, and no other thread can wait on it.
//!
//! soundd's stdout and stderr are a pipe to `logd`, and a write to a full pipe
//! parks the writer until `logd` reads. The mix thread runs in the RT band and
//! may never wait on logging, so **no thread but [`drain`]'s writes that pipe**.
//! Every other thread speaks as a [`Voice`]: `say!` pushes the line onto that
//! voice's own bounded ring ([`Spsc`]) and returns whether or not the ring had
//! room. A full ring drops the line and counts it, and the drain says the count
//! in its next write, after the lines it kept — later, never silently.
//!
//! **Why a ring and not `write_nonblock` on the pipe itself**: a nonblocking
//! pipe write takes what fits, so a line is cut where the pipe filled; and a
//! count of refused lines could be said only by the thread that refused them,
//! on its next line — which after the last `soundd: suspended` may be never.
//!
//! One ring per voice because a ring with one producer takes no lock and no
//! compare-and-swap, so the mix thread's push is a bounded number of its own
//! instructions whatever the other threads are doing. Each line takes a ticket
//! as it is said, and a voice is [`BUSY`] from before it takes one until its
//! push is done: the drain writes a line only once no line with an earlier
//! ticket can still be on its way, so a line said earlier and pushed later is
//! never overtaken, and the rings merge back into the order their lines were
//! said in.
//!
//! A panic's message is the one write another thread makes, and it is that
//! process's last.

use core::cell::Cell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::io::{ErrorKind, Write as _};
use std::sync::OnceLock;

use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

use crate::control::MAX_CONTROL_CLIENTS;
use crate::ring::Spsc;

/// The threads that speak. Each is the one producer on its own ring.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Voice {
    /// `main`, which becomes the mix thread and the null sink.
    Mix,
    Control,
}

const VOICES: usize = 2;

/// Lines one voice's ring holds: every client connecting and leaving in one
/// burst fits whole, with room to spare.
const RING_LINES: usize = 256;
const _: () = assert!(RING_LINES >= 2 * MAX_CONTROL_CLIENTS + 64);

/// A line and the ticket that orders it among every voice's.
type Line = (u64, String);

static RINGS: [Spsc<Line, RING_LINES>; VOICES] = [const { Spsc::new() }; VOICES];
static SPOKEN_FOR: [AtomicBool; VOICES] = [const { AtomicBool::new(false) }; VOICES];
static TICKET: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// Whether a voice is between taking a ticket and landing its line: a line
/// another voice pushed meanwhile may carry a later ticket than this one.
static BUSY: [AtomicBool; VOICES] = [const { AtomicBool::new(false) }; VOICES];
/// The write end of the pipe the drain parks on: one byte per line said.
static WAKE: OnceLock<RawHandle> = OnceLock::new();

thread_local! {
    static SPEAKER: Cell<Option<Voice>> = const { Cell::new(None) };
}

/// Start the one writer, and make the calling thread `voice`. First in `main`,
/// so every line soundd says has somewhere to go.
pub(crate) fn start(voice: Voice) {
    let wake = syscall::pipe().expect("soundd: no pipe to wake its line writer");
    if WAKE.set(wake.write).is_err() {
        unreachable!("soundd's line writer is started once");
    }
    std::thread::Builder::new()
        .name("soundd-say".into())
        .spawn(move || drain(wake.read))
        .expect("soundd: failed to spawn its line writer");
    speak_as(voice);
}

/// Make the calling thread `voice`, which no other thread may be.
pub(crate) fn speak_as(voice: Voice) {
    assert!(
        !SPOKEN_FOR[voice as usize].swap(true, Ordering::AcqRel),
        "soundd: a second thread speaks as {voice:?}"
    );
    SPEAKER.with(|s| s.set(Some(voice)));
}

/// `say!`'s one step, which never waits: a push, and a nonblocking byte to wake
/// the drain. The voice is [`BUSY`] from before its ticket is taken until the
/// push has landed or been dropped, and the byte is written after either.
pub(crate) fn said(line: String) {
    let voice = SPEAKER
        .with(Cell::get)
        .unwrap_or_else(|| panic!("soundd: a thread with no voice said {line:?}"));
    BUSY[voice as usize].store(true, Ordering::SeqCst);
    let ticket = TICKET.fetch_add(1, Ordering::SeqCst);
    if RINGS[voice as usize].try_push((ticket, line)).is_err() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    BUSY[voice as usize].store(false, Ordering::SeqCst);
    let wake = *WAKE.get().expect("a voice is given out only after the writer starts");
    match syscall::write_nonblock(wake, &[1]) {
        // A full wake pipe is a drain with a wake already owed.
        Ok(_) | Err(SyscallError::WouldBlock) => {}
        Err(e) => panic!("soundd: its line writer's wake pipe refused a byte: {e:?}"),
    }
}

/// The one writer: every line waiting, in the order it was said, then the count
/// of those that found their ring full, in one write: a burst costs one
/// syscall.
///
/// The count is read after every batch is gathered, so a line dropped while
/// this thread was parked on the pipe is said in the batch after that write.
fn drain(wake: RawHandle) -> ! {
    let mut heads: [Option<Line>; VOICES] = [const { None }; VOICES];
    let mut woken = [0u8; 512];
    let mut batch = Vec::new();
    loop {
        // At most what the rings hold at once, so a voice that never stops
        // cannot keep this from writing.
        let mut emptied = false;
        let mut behind = false;
        for _ in 0..VOICES * RING_LINES {
            // In this order: every ticket below `taken` was taken before any
            // voice was looked at, so one of them not yet pushed belongs to a
            // voice found busy below whose ring was empty.
            let taken = TICKET.load(Ordering::SeqCst);
            let busy: [bool; VOICES] = core::array::from_fn(|v| BUSY[v].load(Ordering::SeqCst));
            for (head, ring) in heads.iter_mut().zip(&RINGS) {
                if head.is_none() {
                    *head = ring.pop();
                }
            }
            let Some(next) = (0..VOICES)
                .filter(|&i| heads[i].is_some())
                .min_by_key(|&i| heads[i].as_ref().map(|(ticket, _)| *ticket))
            else {
                emptied = true;
                break;
            };
            let ticket = heads[next].as_ref().map_or(0, |(ticket, _)| *ticket);
            // A line said before this one may still be on its way: written
            // after it, once its voice has woken this thread again.
            if ticket >= taken || (0..VOICES).any(|v| v != next && busy[v] && heads[v].is_none()) {
                behind = true;
                break;
            }
            let (_, line) = heads[next].take().expect("chosen for holding a line");
            batch.extend_from_slice(line.as_bytes());
        }
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            batch.extend_from_slice(
                format!("soundd: {dropped} line(s) went unsaid: their queue to the log was full\n")
                    .as_bytes(),
            );
        }
        if !batch.is_empty() {
            match std::io::stderr().write_all(&batch) {
                Ok(()) => {}
                // `logd` gone, and there is nobody left to tell.
                Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
                // Anything else would lose this batch and every line after it
                // without a word, so soundd ends instead.
                Err(e) => {
                    eprintln!("soundd: its pipe to the log refused {} bytes ({e})", batch.len());
                    std::process::abort();
                }
            }
            batch.clear();
        }
        // Parked only on rings found empty or on a line still on its way: a
        // voice writes its wake byte after its push and after it is no longer
        // busy, so this read returns for either.
        if emptied || behind {
            match syscall::read(wake, &mut woken) {
                Ok(1..) => {}
                other => panic!("soundd: its line writer's wake pipe answered {other:?}"),
            }
        }
    }
}
