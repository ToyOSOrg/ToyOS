//! A program's stdout and stderr, and its own lines: records into its log ring
//! when the stream is one, bytes into the handle when it is anything else.
//!
//! **Where a stream goes is what its slot holds.** `/system/bin/init` puts a
//! program's log ring in slots 1 and 2, so a daemon's output is records; a
//! program a terminal started holds that terminal's pipes there, so its output
//! is bytes on the terminal. The first write to a stream asks the slot once
//! ([`FileType::SharedMemory`] is a ring) and every later one takes the answer,
//! so a write to a ring makes no syscall. A program a daemon spawns directly
//! inherits the daemon's slots and so writes into the daemon's ring: its lines
//! are the daemon's, which is the daemon's decision to have spawned it that
//! way.
//!
//! A write to a ring **never waits, allocates or makes a syscall**: it stamps
//! the record — time off the clock page, thread off the thread's control
//! block — and pushes it ([`super::ring::push`]); a full ring drops it and the
//! reader counts the drop. Lines are assembled per stream, so a line that
//! arrives in pieces (`eprintln!` writes each format fragment) is one record.
//!
//! [`FileType::SharedMemory`]: toyos_abi::syscall::FileType::SharedMemory

use core::fmt;

use toyos_abi::log::Severity;

use super::region::{Body, FLAG_UNENDED, TEXT_BYTES};

/// A standard stream, by the slot it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Stream {
    Out = 1,
    Err = 2,
}

impl Stream {
    /// What a line on this stream is, when it becomes a record.
    pub const fn severity(self) -> Severity {
        match self {
            Self::Out => Severity::Info,
            Self::Err => Severity::Error,
        }
    }
}

/// Bytes into records: a record ends at a newline, which it does not keep,
/// or when its text is full, in which case it is marked unended and the line
/// goes on in the next one.
pub struct Composer {
    body: Body,
}

impl Composer {
    pub const fn new(severity: Severity) -> Self {
        let mut body = Body::EMPTY;
        body.severity = severity as u8;
        Self { body }
    }

    /// Take `bytes`, and hand every record they complete to `emit`.
    pub fn feed(&mut self, bytes: &[u8], emit: &mut impl FnMut(&mut Body)) {
        for &byte in bytes {
            if byte == b'\n' {
                self.close(0, emit);
                continue;
            }
            if self.body.len as usize == TEXT_BYTES {
                self.close(FLAG_UNENDED, emit);
            }
            self.body.text[self.body.len as usize] = byte;
            self.body.len += 1;
        }
    }

    /// Hand what is held to `emit`, if anything: `ended` says whether the
    /// writer ended its line there.
    pub fn finish(&mut self, ended: bool, emit: &mut impl FnMut(&mut Body)) {
        if self.body.len > 0 {
            self.close(if ended { 0 } else { FLAG_UNENDED }, emit);
        }
    }

    fn close(&mut self, flags: u8, emit: &mut impl FnMut(&mut Body)) {
        // A carriage return before the newline is the writer's line ending,
        // not its text.
        if flags == 0 && self.body.len > 0 && self.body.text[self.body.len as usize - 1] == b'\r' {
            self.body.len -= 1;
        }
        self.body.flags = flags;
        emit(&mut self.body);
        self.body.len = 0;
    }
}

/// `format_args!` into a [`Composer`].
struct Formatter<'a, E: FnMut(&mut Body)> {
    composer: &'a mut Composer,
    emit: &'a mut E,
}

impl<E: FnMut(&mut Body)> fmt::Write for Formatter<'_, E> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.composer.feed(s.as_bytes(), self.emit);
        Ok(())
    }
}

/// One whole line, formatted on the stack and handed to `emit` as records.
pub fn compose(severity: Severity, args: fmt::Arguments, emit: &mut impl FnMut(&mut Body)) {
    let mut composer = Composer::new(severity);
    let _ = fmt::write(&mut Formatter { composer: &mut composer, emit }, args);
    composer.finish(true, emit);
}

/// A lock around one stream's partial line, taken for the length of a copy.
///
/// A spin and not a sleep: std already serialises each stream's writers with
/// its own lock, so the only contender is a panic message racing an ordinary
/// write — and a write to a ring takes no lock at all past this one.
#[cfg(any(test, target_os = "toyos"))]
pub(crate) struct Held {
    busy: core::sync::atomic::AtomicBool,
    composer: core::cell::UnsafeCell<Composer>,
}

// SAFETY: `composer` is reached only between a successful `busy` swap and its
// release, so one thread at a time.
#[cfg(any(test, target_os = "toyos"))]
unsafe impl Sync for Held {}

#[cfg(any(test, target_os = "toyos"))]
impl Held {
    pub(crate) const fn new(stream: Stream) -> Self {
        Self {
            busy: core::sync::atomic::AtomicBool::new(false),
            composer: core::cell::UnsafeCell::new(Composer::new(stream.severity())),
        }
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Composer) -> R) -> R {
        use core::sync::atomic::Ordering;
        while self.busy.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }
        // SAFETY: the swap above made this thread the only holder.
        let out = f(unsafe { &mut *self.composer.get() });
        self.busy.store(false, Ordering::Release);
        out
    }
}

#[cfg(all(target_os = "toyos", not(test)))]
pub use target::*;

/// The guest's half: slots, rings and syscalls.
#[cfg(all(target_os = "toyos", not(test)))]
mod target {
    use core::ptr::NonNull;
    use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    use toyos_abi::log::Severity;
    use toyos_abi::syscall::{self, FileType, SyscallError};
    use toyos_abi::RawHandle;

    use super::{compose, Held, Stream};
    use crate::log::region::{Body, Lane, Ring, RING_BYTES};

    /// Unasked, being asked, a handle — and past those, a ring's address.
    const UNASKED: usize = 0;
    const ASKING: usize = 1;
    const HANDLE: usize = 2;

    static SINKS: [AtomicUsize; 2] = [AtomicUsize::new(UNASKED), AtomicUsize::new(UNASKED)];
    static PENDING: [Held; 2] = [Held::new(Stream::Out), Held::new(Stream::Err)];
    /// This process's pid, asked once with the first ring: a record carries
    /// it and a write makes no syscall.
    static PID: AtomicU32 = AtomicU32::new(0);

    /// Where a stream's bytes go.
    pub enum Sink {
        Ring(Ring),
        Handle(RawHandle),
    }

    fn index(stream: Stream) -> usize {
        stream as usize - 1
    }

    /// The stream's sink, asking its slot the first time.
    pub fn sink(stream: Stream) -> Sink {
        let word = &SINKS[index(stream)];
        loop {
            match word.load(Ordering::Acquire) {
                UNASKED => {
                    if word.compare_exchange(UNASKED, ASKING, Ordering::Acquire, Ordering::Acquire).is_ok() {
                        match ask(stream) {
                            Ok(answer) => word.store(answer, Ordering::Release),
                            // The handle first, so the panic's own message has a
                            // stream to go to rather than a probe to wait on.
                            Err(why) => {
                                word.store(HANDLE, Ordering::Release);
                                panic!("stdio: slot {} {why}", stream as u32);
                            }
                        }
                    }
                }
                ASKING => core::hint::spin_loop(),
                HANDLE => return Sink::Handle(RawHandle(stream as u32)),
                // SAFETY: only `ask` stores an address here, and it is the
                // base of a ring mapping this process keeps for its life.
                at => return Sink::Ring(unsafe { Ring::at(NonNull::new_unchecked(at as *mut u8)) }),
            }
        }
    }

    /// Ask a stream's slot what it holds, once. A ring is mapped through a
    /// handle of this process's own, so the mapping outlives whatever later
    /// happens to the slot. `Err` finishes the sentence the refusal panics with.
    fn ask(stream: Stream) -> Result<usize, why::Why> {
        use why::Why;
        let slot = RawHandle(stream as u32);
        let stat = syscall::fstat(slot).map_err(Why::Fstat)?;
        if stat.file_type != FileType::SharedMemory {
            return Ok(HANDLE);
        }
        if (stat.size as usize) < RING_BYTES {
            return Err(Why::Small(stat.size));
        }
        let own = syscall::dup(slot).map_err(Why::Dup)?;
        // SAFETY: `own` is a live shared-memory handle this process holds and
        // never closes, so the mapping stays for the process's life.
        let at = unsafe { syscall::shm_map(own) }.map_err(Why::Map)?;
        let at = NonNull::new(at).ok_or(Why::Null)?;
        // SAFETY: a fresh mapping of a region at least `RING_BYTES` long.
        if !unsafe { Ring::at(at) }.is_laid_out() {
            return Err(Why::NotARing);
        }
        PID.store(syscall::getpid().0, Ordering::Relaxed);
        Ok(at.as_ptr() as usize)
    }

    /// Why a slot could not be taken as a stream, in words that need no
    /// allocation to say.
    mod why {
        use core::fmt;
        use toyos_abi::syscall::SyscallError;

        pub enum Why {
            Fstat(SyscallError),
            Small(u64),
            Dup(SyscallError),
            Map(SyscallError),
            Null,
            NotARing,
        }

        impl fmt::Display for Why {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                match self {
                    Self::Fstat(e) => write!(f, "answers fstat with {e:?}"),
                    Self::Small(size) => write!(
                        f,
                        "is a {size}-byte region, and a log ring is {}",
                        crate::log::region::RING_BYTES
                    ),
                    Self::Dup(e) => write!(f, "holds a ring that will not duplicate: {e:?}"),
                    Self::Map(e) => write!(f, "holds a ring that will not map: {e:?}"),
                    Self::Null => write!(f, "holds a ring the kernel mapped at null"),
                    Self::NotARing => write!(f, "holds a region that is not a log ring"),
                }
            }
        }
    }

    /// Ask both streams now, so no later write is the one that asks. A thread
    /// that may not make a syscall — soundd's mix thread — relies on this
    /// having run before it writes.
    pub fn bind() {
        let _ = sink(Stream::Out);
        let _ = sink(Stream::Err);
    }

    /// Forget what a stream's slot held: the slot was just replaced.
    pub fn forget(stream: Stream) {
        SINKS[index(stream)].store(UNASKED, Ordering::Release);
    }

    fn stamp(body: &mut Body) {
        body.at_ns = toyos_abi::clock::nanos_since_boot();
        body.pid = PID.load(Ordering::Relaxed);
        body.tid = toyos_abi::current_tid().0;
    }

    fn push(ring: &Ring) -> impl FnMut(&mut Body) + '_ {
        move |body: &mut Body| {
            stamp(body);
            // A dropped record is counted where it would have been read.
            let _ = ring.push(body);
        }
    }

    /// `buf` onto a stream: whole lines to its ring, or every byte to its
    /// handle.
    pub fn write(stream: Stream, buf: &[u8]) -> Result<usize, SyscallError> {
        match sink(stream) {
            Sink::Handle(handle) => syscall::write(handle, buf),
            Sink::Ring(ring) => {
                PENDING[index(stream)].with(|composer| composer.feed(buf, &mut push(&ring)));
                Ok(buf.len())
            }
        }
    }

    /// Whatever part of a line a stream holds, as a record of its own.
    pub fn flush(stream: Stream) {
        if let Sink::Ring(ring) = sink(stream) {
            PENDING[index(stream)].with(|composer| composer.finish(false, &mut push(&ring)));
        }
    }

    /// Claim one of the stderr ring's lanes for the calling thread, which
    /// becomes its only writer: what a thread that may not retry a write
    /// writes into ([`say_lane`]). `None` where stderr is no ring, or every
    /// lane is claimed.
    pub fn claim_lane() -> Option<Lane> {
        match sink(Stream::Err) {
            Sink::Ring(ring) => ring.claim_lane(PID.load(Ordering::Relaxed), toyos_abi::current_tid().0),
            Sink::Handle(_) => None,
        }
    }

    /// One line into a lane the calling thread claimed. Wait-free: a
    /// formatting pass on the stack and a lane push.
    pub fn say_lane(lane: &Lane, severity: Severity, args: core::fmt::Arguments) {
        compose(severity, args, &mut |body: &mut Body| {
            stamp(body);
            let _ = lane.push(body);
        })
    }

    /// One line of this program's own, at `severity`, onto its stderr.
    pub fn say(severity: Severity, args: core::fmt::Arguments) {
        match sink(Stream::Err) {
            Sink::Ring(ring) => compose(severity, args, &mut push(&ring)),
            Sink::Handle(handle) => compose(severity, args, &mut |body: &mut Body| {
                let mut rest = body.text();
                while !rest.is_empty() {
                    match syscall::write(handle, rest) {
                        Ok(n @ 1..) => rest = &rest[n..],
                        _ => return,
                    }
                }
                if !body.unended() {
                    let _ = syscall::write(handle, b"\n");
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::String;
    use std::vec::Vec;

    fn lines(severity: Severity, pieces: &[&[u8]], ended: bool) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut composer = Composer::new(severity);
        let mut emit = |body: &mut Body| {
            assert_eq!(body.severity(), Some(severity));
            out.push((String::from_utf8(body.text().to_vec()).unwrap(), body.unended()))
        };
        for piece in pieces {
            composer.feed(piece, &mut emit);
        }
        composer.finish(ended, &mut emit);
        out
    }

    /// `eprintln!` writes each fragment as its own `write`; the record is the
    /// line, whatever the fragments were.
    #[test]
    fn a_line_in_pieces_is_one_record() {
        let got = lines(Severity::Error, &[b"a=", b"1", b" b=", b"2", b"\n"], true);
        assert_eq!(got, [("a=1 b=2".into(), false)]);
    }

    #[test]
    fn a_newline_ends_a_record_and_a_carriage_return_before_it_goes() {
        let got = lines(Severity::Info, &[b"one\r\ntwo\n\nthree"], false);
        assert_eq!(got, [("one".into(), false), ("two".into(), false), ("".into(), false), ("three".into(), true)]);
    }

    /// A line longer than a record is several, every one but the last marked
    /// unended, and not a byte lost.
    #[test]
    fn a_long_line_is_records_in_order_with_nothing_lost() {
        let long: Vec<u8> = (0..TEXT_BYTES * 2 + 5).map(|i| b'a' + (i % 26) as u8).collect();
        let mut piece = long.clone();
        piece.push(b'\n');
        let got = lines(Severity::Info, &[&piece], true);
        assert_eq!(got.iter().map(|(t, u)| (t.len(), *u)).collect::<Vec<_>>(), [
            (TEXT_BYTES, true),
            (TEXT_BYTES, true),
            (5, false)
        ]);
        let joined: Vec<u8> = got.into_iter().flat_map(|(t, _)| t.into_bytes()).collect();
        assert_eq!(joined, long);
    }

    #[test]
    fn a_formatted_line_is_one_ended_record() {
        let mut out = Vec::new();
        compose(Severity::Warn, format_args!("x={} y={}", 1, "two"), &mut |body: &mut Body| {
            out.push((body.text().to_vec(), body.unended(), body.severity()))
        });
        assert_eq!(out, [(b"x=1 y=two".to_vec(), false, Some(Severity::Warn))]);
    }

    /// Two threads writing lines in pieces through one stream's lock: every
    /// record is one thread's whole line.
    #[test]
    fn a_stream_held_by_two_writers_keeps_each_line_whole() {
        use std::sync::{Arc, Mutex};
        let held = Arc::new(Held::new(Stream::Err));
        let got = Arc::new(Mutex::new(Vec::new()));
        let writers: Vec<_> = [b'a', b'b']
            .into_iter()
            .map(|tag| {
                let (held, got) = (Arc::clone(&held), Arc::clone(&got));
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        let mut line = [tag; 32];
                        line[31] = b'\n';
                        held.with(|composer| {
                            composer.feed(&line[..10], &mut |b: &mut Body| got.lock().unwrap().push(b.text().to_vec()));
                            composer.feed(&line[10..], &mut |b: &mut Body| got.lock().unwrap().push(b.text().to_vec()));
                        });
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 400);
        for line in got.iter() {
            assert!(line.len() == 31 && line.iter().all(|&b| b == line[0]), "{line:?}");
        }
    }
}

/// A host's build is a guest program's unit tests: there are no slots and no
/// ring, and a line said there is composed and goes nowhere.
#[cfg(not(all(target_os = "toyos", not(test))))]
pub use host::*;

#[cfg(not(all(target_os = "toyos", not(test))))]
mod host {
    use core::fmt;

    use toyos_abi::log::Severity;

    use super::compose;
    use crate::log::region::{Body, Lane};

    pub fn bind() {}

    pub fn claim_lane() -> Option<Lane> {
        None
    }

    pub fn say(severity: Severity, args: fmt::Arguments) {
        compose(severity, args, &mut |_: &mut Body| {});
    }

    pub fn say_lane(_lane: &Lane, severity: Severity, args: fmt::Arguments) {
        compose(severity, args, &mut |_: &mut Body| {});
    }
}
