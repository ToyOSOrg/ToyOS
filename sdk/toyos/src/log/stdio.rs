//! A program's stdout and stderr, and its own lines: records into its log ring
//! when the stream is one, bytes into its descriptor when it is anything else.
//!
//! **Where a stream goes is what its descriptor names** (`crate::fd`).
//! The supervisor gives a program its log ring as descriptors 1 and
//! 2, so a daemon's output is records; a program a terminal started holds that
//! terminal's pipes there, so its output is bytes on the terminal. The first
//! write to a stream asks the descriptor once ([`FileType::SharedMemory`] is a
//! ring) and every later one takes the answer, so a write to a ring makes no
//! syscall. A program a daemon spawns directly inherits the daemon's stdio and
//! so writes into the daemon's ring: its lines are the daemon's, which is the
//! daemon's decision to have spawned it that way.
//!
//! A write to a ring **never waits, allocates or makes a syscall**: it stamps
//! the record — time off the clock page, thread off the thread's control
//! block — and pushes it ([`super::ring::push`]); a full ring drops it and the
//! reader counts the drop. Lines are assembled per stream, so a line that
//! arrives in pieces (`eprintln!` writes each format fragment) is one record.
//!
//! **A process holds this state once**, in the copy of this crate that owns
//! its descriptor table (`crate::fd`): what each stream's descriptor was found
//! to name, the ring's mapping and the line each stream is assembling. Every
//! copy linked into the image reaches it through `toyos_stdio_*`, which that
//! copy defines, so std, libc and a program's own copy join one line and see
//! one answer. The table's owner forgets a stream's answer with every change
//! of what descriptor 1 or 2 names, whichever copy asked for the change, so
//! the write after a redirect asks again.
//!
//! [`FileType::SharedMemory`]: toyos_abi::syscall::FileType::SharedMemory

use core::fmt;

use toyos_abi::log::Severity;

use super::region::{Body, FLAG_CLOSES, FLAG_UNENDED, TEXT_BYTES};

/// A standard stream, by the descriptor it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Stream {
    Out = 1,
    Err = 2,
}

impl Stream {
    /// What a line on this stream is, when it becomes a record: every line on
    /// stderr is an error, so a routine line goes on stdout.
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
    /// The last record handed out was marked unended: a line is open.
    open: bool,
}

impl Composer {
    pub const fn new(severity: Severity) -> Self {
        let mut body = Body::EMPTY;
        body.severity = severity as u8;
        Self { body, open: false }
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
    /// writer ended its line there. A line ended with nothing held, whose
    /// earlier records went out unended, is ended by a record that says only
    /// that.
    pub fn finish(&mut self, ended: bool, emit: &mut impl FnMut(&mut Body)) {
        if self.body.len > 0 {
            self.close(if ended { 0 } else { FLAG_UNENDED }, emit);
        } else if ended && self.open {
            self.close(FLAG_CLOSES, emit);
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
        self.open = flags & FLAG_UNENDED != 0;
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
pub fn compose(severity: Severity, args: fmt::Arguments<'_>, emit: &mut impl FnMut(&mut Body)) {
    let mut composer = Composer::new(severity);
    let _ = fmt::write(&mut Formatter { composer: &mut composer, emit }, args);
    composer.finish(true, emit);
}

/// One stream's partial line, held by one write at a time.
///
/// **No write waits for it.** One that finds it held (a second thread's, or
/// the message of a panic raised under the hold, on the holder's own thread)
/// is answered `None` and writes what it has as records of its own.
#[cfg(any(test, feature = "fd-table"))]
pub(crate) struct Held {
    busy: core::sync::atomic::AtomicBool,
    composer: core::cell::UnsafeCell<Composer>,
}

// SAFETY: `composer` is reached only between a successful `busy` swap and its
// release, so one thread at a time.
#[cfg(any(test, feature = "fd-table"))]
unsafe impl Sync for Held {}

#[cfg(any(test, feature = "fd-table"))]
impl Held {
    pub(crate) const fn new(stream: Stream) -> Self {
        Self {
            busy: core::sync::atomic::AtomicBool::new(false),
            composer: core::cell::UnsafeCell::new(Composer::new(stream.severity())),
        }
    }

    /// `f` on the line, or `None` where another write holds it.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Composer) -> R) -> Option<R> {
        use core::sync::atomic::Ordering;
        /// Lets go as `f` returns or unwinds.
        struct Release<'a>(&'a core::sync::atomic::AtomicBool);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        if self.busy.swap(true, Ordering::Acquire) {
            return None;
        }
        let _release = Release(&self.busy);
        // SAFETY: the swap above made this thread the only holder.
        Some(f(unsafe { &mut *self.composer.get() }))
    }

    /// `bytes` onto the line, or as records of their own where the line is
    /// held: whole, in order, and marked unended where they end no line.
    pub(crate) fn feed(&self, severity: Severity, bytes: &[u8], emit: &mut impl FnMut(&mut Body)) {
        if self.with(|composer| composer.feed(bytes, emit)).is_none() {
            let mut own = Composer::new(severity);
            own.feed(bytes, emit);
            own.finish(false, emit);
        }
    }
}

#[cfg(all(target_os = "toyos", not(test)))]
pub use target::*;

/// The guest's half, in every copy: the owner's answers, and what a copy does
/// with them itself.
#[cfg(all(target_os = "toyos", not(test)))]
mod target {
    use core::ptr::NonNull;

    use toyos_abi::log::Severity;
    use toyos_abi::syscall;

    use super::{compose, Stream};
    use crate::log::region::{Body, Lane, Ring};

    // What every copy calls and the owner defines ([`owner`]).
    extern "C" {
        /// The base of the ring `stream` goes to, asking its descriptor if
        /// nothing has since the descriptor last changed: null for a stream
        /// that is written as its descriptor.
        fn toyos_stdio_ring(stream: Stream) -> *mut u8;
        /// `len` bytes at `buf` onto `stream`'s line in its ring: false, with
        /// nothing written, for a stream that is written as its descriptor.
        fn toyos_stdio_write(stream: Stream, buf: *const u8, len: usize) -> bool;
        /// What `stream` holds of a line into its ring, ending the line if
        /// `ended`. Asks no descriptor.
        fn toyos_stdio_finish(stream: Stream, ended: bool);
        /// This process's pid, once a stream was found to be a ring.
        fn toyos_stdio_pid() -> u32;
    }

    /// Where a stream's bytes go.
    pub enum Sink {
        Ring(Ring),
        /// The stream's descriptor (`crate::fd`), 1 or 2.
        Descriptor(i32),
    }

    /// The stream's sink, asking its descriptor the first time and the first
    /// time after each change of what the descriptor names.
    pub fn sink(stream: Stream) -> Sink {
        // SAFETY: the owner's definition, of plain values.
        match NonNull::new(unsafe { toyos_stdio_ring(stream) }) {
            // SAFETY: the owner answers only the base of a ring's mapping,
            // which it keeps for the process's life.
            Some(base) => Sink::Ring(unsafe { Ring::at(base) }),
            None => Sink::Descriptor(stream as i32),
        }
    }

    /// Ask both streams now, so no later write is the one that asks. A thread
    /// that may not make a syscall — soundserver's mix thread — relies on this
    /// having run before it writes.
    pub fn bind() {
        let _ = sink(Stream::Out);
        let _ = sink(Stream::Err);
    }

    fn stamp(body: &mut Body) {
        body.at_ns = toyos_abi::clock::stamp_ns();
        // SAFETY: the owner's definition, of no arguments.
        body.pid = unsafe { toyos_stdio_pid() };
        body.tid = crate::tls::current_tid().0;
    }

    fn push(ring: &Ring) -> impl FnMut(&mut Body) + '_ {
        move |body: &mut Body| {
            stamp(body);
            // A dropped record is counted where it would have been read.
            let _ = ring.push(body);
        }
    }

    /// `buf` onto a stream: whole lines to its ring, or every byte to its
    /// descriptor, and none anywhere for a descriptor that is not open, as
    /// std on unix drops a closed stdout's bytes.
    pub fn write(stream: Stream, buf: &[u8]) -> Result<usize, crate::files::Error> {
        // SAFETY: `buf` is live for the call, which keeps nothing of it.
        if unsafe { toyos_stdio_write(stream, buf.as_ptr(), buf.len()) } {
            return Ok(buf.len());
        }
        match crate::files::write(stream as i32, buf) {
            Err(crate::files::Error::Fd(crate::fd::Error::BadFd)) => Ok(buf.len()),
            written => written,
        }
    }

    /// Whatever part of a line a stream holds, as a record the line goes on
    /// from. Never asks the descriptor: a stream nothing was written to holds
    /// nothing.
    pub fn flush(stream: Stream) {
        // SAFETY: the owner's definition, of plain values.
        unsafe { toyos_stdio_finish(stream, false) }
    }

    /// Whatever part of a line a stream holds, as the line's end: the
    /// process is leaving, so the line is as long as it gets.
    pub fn end(stream: Stream) {
        // SAFETY: the owner's definition, of plain values.
        unsafe { toyos_stdio_finish(stream, true) }
    }

    /// Claim one of the stderr ring's lanes for the calling thread, which
    /// becomes its only writer: what a thread that may not retry a write
    /// writes into ([`say_lane`]). `None` where stderr is no ring, or every
    /// lane is claimed.
    pub fn claim_lane() -> Option<Lane> {
        match sink(Stream::Err) {
            // SAFETY: the owner's definition, of no arguments.
            Sink::Ring(ring) => ring.claim_lane(unsafe { toyos_stdio_pid() }, crate::tls::current_tid().0),
            Sink::Descriptor(_) => None,
        }
    }

    /// One line into a lane the calling thread claimed. Wait-free: a
    /// formatting pass on the stack and a lane push.
    pub fn say_lane(lane: &Lane, severity: Severity, args: core::fmt::Arguments<'_>) {
        compose(severity, args, &mut |body: &mut Body| {
            stamp(body);
            let _ = lane.push(body);
        })
    }

    /// Some of `bytes` written to the descriptor's `object`: a stream's as
    /// its bytes, without waiting, since a line may be said from inside a call
    /// waiting on that very stream; a handle's by a plain write. `None` for
    /// nothing, and for a stream another copy of this crate holds.
    fn write_line(object: crate::fd::Lent<'_>, bytes: &[u8]) -> Option<usize> {
        let wait = crate::net::stream::Wait::Nonblocking;
        match object.kernel_handle() {
            Some(handle) if object.tag() == crate::fd::STREAM => crate::net::stream::write(handle, bytes, wait).ok(),
            Some(handle) => syscall::write(handle.raw(), bytes).ok(),
            None => None,
        }
    }

    /// One line of this program's own, at `severity`, onto its stderr.
    pub fn say(severity: Severity, args: core::fmt::Arguments<'_>) {
        match sink(Stream::Err) {
            Sink::Ring(ring) => compose(severity, args, &mut push(&ring)),
            Sink::Descriptor(fd) => compose(severity, args, &mut |body: &mut Body| {
                let write = |bytes: &[u8]| crate::fd::with(fd, |o| write_line(o, bytes));
                let mut rest = body.text();
                while !rest.is_empty() {
                    match write(rest) {
                        Ok(Some(n @ 1..)) => rest = &rest[n..],
                        _ => return,
                    }
                }
                if !body.unended() {
                    let _ = write(b"\n");
                }
            }),
        }
    }
}

/// The stream state itself, in the copy that owns the descriptor table.
///
/// **A stream's word is `UNASKED`, `DESCRIPTOR` or the base of a mapping of
/// the ring its descriptor named when it was asked**, and [`changed`] makes it
/// `UNASKED` before a change of that descriptor returns to its caller. A
/// ring's mapping stays for the process's life, so a write that read the word
/// before a redirect finishes in the ring it found.
#[cfg(all(feature = "fd-table", not(test)))]
pub(crate) mod owner {
    use core::ptr::NonNull;
    use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    use toyos_abi::syscall::{self, FileType, SyscallError};

    use super::{Held, Stream};
    use crate::fd::Fd;
    use crate::log::region::{Body, Ring, RING_BYTES};
    use crate::sync::Lock;

    const UNASKED: usize = 0;
    const DESCRIPTOR: usize = 1;

    static SINKS: [AtomicUsize; 2] = [AtomicUsize::new(UNASKED), AtomicUsize::new(UNASKED)];
    static PENDING: [Held; 2] = [Held::new(Stream::Out), Held::new(Stream::Err)];
    /// Held by the one thread asking a descriptor or forgetting an answer,
    /// which is named the asker while it holds it ([`Asking`]).
    static ASKING: Lock<()> = Lock::new(());
    /// The thread holding `ASKING`, [`NO_ASKER`] for none: its own write
    /// meanwhile, the message of a panic raised under the lock, goes to the
    /// descriptor rather than waiting on the lock it holds.
    static ASKER: AtomicU32 = AtomicU32::new(NO_ASKER);
    /// The one tid the kernel never issues; a main thread's is 0.
    const NO_ASKER: u32 = toyos_abi::Tid::MAX.0;
    /// This process's pid, asked once with the first ring: a record carries
    /// it and a write makes no syscall.
    static PID: AtomicU32 = AtomicU32::new(0);

    fn index(stream: Stream) -> usize {
        stream as usize - 1
    }

    /// The ring a stream's word names, if it names one.
    fn ring_at(word: usize) -> Option<Ring> {
        // SAFETY: only `ask` stores an address in a stream's word, and it is
        // the base of a ring mapping this process keeps for its life.
        (word > DESCRIPTOR).then(|| unsafe { Ring::at(NonNull::new_unchecked(word as *mut u8)) })
    }

    /// `ASKING`, held by the calling thread, which is the asker until this
    /// drops, as its holder returns or unwinds.
    struct Asking {
        _held: crate::sync::Held<'static, ()>,
    }

    impl Asking {
        fn take(tid: u32) -> Self {
            let held = ASKING.lock();
            ASKER.store(tid, Ordering::Relaxed);
            Self { _held: held }
        }
    }

    impl Drop for Asking {
        /// Names no asker before the lock is let go.
        fn drop(&mut self) {
            ASKER.store(NO_ASKER, Ordering::Relaxed);
        }
    }

    /// The stream's word, asking its descriptor if it is unasked: never
    /// `UNASKED`, but `DESCRIPTOR` to the thread that is the asker.
    fn answer(stream: Stream) -> usize {
        let word = &SINKS[index(stream)];
        let tid = crate::tls::current_tid().0;
        loop {
            match word.load(Ordering::Acquire) {
                UNASKED if ASKER.load(Ordering::Relaxed) == tid => return DESCRIPTOR,
                UNASKED => {}
                answered => return answered,
            }
            let asking = Asking::take(tid);
            if word.load(Ordering::Acquire) != UNASKED {
                continue;
            }
            let answer = ask(stream);
            word.store(*answer.as_ref().unwrap_or(&DESCRIPTOR), Ordering::Release);
            drop(asking);
            // Said with the stream answered and the lock let go, so the
            // panic's own message has a descriptor to go to.
            if let Err(why) = answer {
                panic!("stdio: descriptor {} {why}", stream as u32);
            }
        }
    }

    /// Ask a stream's descriptor what it names. A ring is mapped, and a
    /// mapping outlives whatever later happens to the descriptor and its
    /// handle; a descriptor that is not open is written as one, and its bytes
    /// go nowhere. `Err` finishes the sentence the refusal panics with.
    fn ask(stream: Stream) -> Result<usize, why::Why> {
        use why::Why;
        // A descriptor that is not open, or names a file a server holds, is
        // written as a descriptor.
        let Ok(held) = crate::fd::hold(stream as Fd) else { return Ok(DESCRIPTOR) };
        let Some(handle) = held.object().kernel_handle() else { return Ok(DESCRIPTOR) };
        let stat = syscall::fstat(handle.raw()).map_err(Why::Fstat)?;
        if stat.file_type != FileType::SharedMemory {
            return Ok(DESCRIPTOR);
        }
        if (stat.size as usize) < RING_BYTES {
            return Err(Why::Small(stat.size));
        }
        // SAFETY: the descriptor's handle, held for this call; the mapping is
        // never taken away.
        let at = unsafe { syscall::vm_map(handle.raw(), 0, stat.size as usize, syscall::VmProt::ReadWrite, syscall::VmAt::ANYWHERE) }.map_err(Why::Map)?;
        let at = NonNull::new(at).ok_or(Why::Null)?;
        // SAFETY: a fresh mapping of a region at least `RING_BYTES` long.
        if !unsafe { Ring::at(at) }.is_laid_out() {
            return Err(Why::NotARing);
        }
        PID.store(syscall::getpid().0, Ordering::Relaxed);
        Ok(at.as_ptr() as usize)
    }

    /// Why a descriptor could not be taken as a stream, in words that need no
    /// allocation to say.
    mod why {
        use core::fmt;

        use super::SyscallError;

        pub enum Why {
            Fstat(SyscallError),
            Small(u64),
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
                    Self::Map(e) => write!(f, "holds a ring that will not map: {e:?}"),
                    Self::Null => write!(f, "holds a ring the kernel mapped at null"),
                    Self::NotARing => write!(f, "holds a region that is not a log ring"),
                }
            }
        }
    }

    /// Descriptor `fd` names something else now: if it is a stream's, the
    /// stream is asked again. The table's owner calls this before the change
    /// returns to whoever asked for it, and never under its writer's lock: an
    /// ask holds `ASKING` while `fd::hold` may take that lock.
    pub(crate) fn changed(fd: Fd) {
        let stream = match fd {
            1 => Stream::Out,
            2 => Stream::Err,
            _ => return,
        };
        // After any ask already under way, whose answer may be the old one.
        let _asking = Asking::take(crate::tls::current_tid().0);
        let old = SINKS[index(stream)].swap(UNASKED, Ordering::AcqRel);
        // What was written of a line before the change went where the
        // descriptor then led.
        if let Some(ring) = ring_at(old) {
            let _ = PENDING[index(stream)].with(|composer| composer.finish(false, &mut push(&ring)));
        }
    }

    fn push(ring: &Ring) -> impl FnMut(&mut Body) + '_ {
        move |body: &mut Body| {
            body.at_ns = toyos_abi::clock::stamp_ns();
            body.pid = PID.load(Ordering::Relaxed);
            body.tid = crate::tls::current_tid().0;
            // A dropped record is counted where it would have been read.
            let _ = ring.push(body);
        }
    }

    #[no_mangle]
    extern "C" fn toyos_stdio_ring(stream: Stream) -> *mut u8 {
        match answer(stream) {
            DESCRIPTOR => core::ptr::null_mut(),
            base => base as *mut u8,
        }
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_stdio_write(stream: Stream, buf: *const u8, len: usize) -> bool {
        let Some(ring) = ring_at(answer(stream)) else { return false };
        // SAFETY: the caller's live bytes.
        let buf = unsafe { core::slice::from_raw_parts(buf, len) };
        PENDING[index(stream)].feed(stream.severity(), buf, &mut push(&ring));
        true
    }

    #[no_mangle]
    extern "C" fn toyos_stdio_finish(stream: Stream, ended: bool) {
        if let Some(ring) = ring_at(SINKS[index(stream)].load(Ordering::Acquire)) {
            // A line another write holds is that write's to go on with.
            let _ = PENDING[index(stream)].with(|composer| composer.finish(ended, &mut push(&ring)));
        }
    }

    #[no_mangle]
    extern "C" fn toyos_stdio_pid() -> u32 {
        PID.load(Ordering::Relaxed)
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

    /// A flush mid-line sends what is held as a piece the line goes on
    /// from; the process ending it with nothing more to say sends a record
    /// that only ends it, and one with no line open sends nothing.
    #[test]
    fn a_line_a_flush_opened_is_closed_at_its_end() {
        let mut out = Vec::new();
        let mut emit = |body: &mut Body| out.push((body.text().to_vec(), body.unended(), body.closes()));
        let mut composer = Composer::new(Severity::Info);
        composer.feed(b"prompt> ", &mut emit);
        composer.finish(false, &mut emit);
        composer.finish(true, &mut emit);
        composer.finish(true, &mut emit);
        composer.feed(b"whole\n", &mut emit);
        composer.finish(true, &mut emit);
        assert_eq!(out, [
            (b"prompt> ".to_vec(), true, false),
            (Vec::new(), false, true),
            (b"whole".to_vec(), false, false),
        ]);
    }

    #[test]
    fn a_formatted_line_is_one_ended_record() {
        let mut out = Vec::new();
        compose(Severity::Warn, format_args!("x={} y={}", 1, "two"), &mut |body: &mut Body| {
            out.push((body.text().to_vec(), body.unended(), body.severity()))
        });
        assert_eq!(out, [(b"x=1 y=two".to_vec(), false, Some(Severity::Warn))]);
    }

    /// Two threads writing whole lines through one stream's hold, each
    /// taking its turn or writing records of its own: every record is one
    /// thread's whole line, and none is lost.
    #[test]
    fn a_stream_held_by_two_writers_keeps_each_line_whole() {
        use std::sync::{Arc, Mutex};
        let held = Arc::new(Held::new(Stream::Err));
        let got = Arc::new(Mutex::new(Vec::new()));
        let writers: Vec<_> = b"ab"
            .iter()
            .map(|&tag| {
                let (held, got) = (Arc::clone(&held), Arc::clone(&got));
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        let mut line = [tag; 32];
                        line[31] = b'\n';
                        held.feed(Severity::Error, &line, &mut |b: &mut Body| got.lock().unwrap().push(b.text().to_vec()));
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

    /// A write from inside the hold, which is what the message of a panic
    /// raised there is, returns: its bytes are records of their own, the
    /// piece that ends no line marked unended, and the held line is untouched.
    #[test]
    fn a_write_under_the_streams_own_hold_is_its_own_records_and_does_not_wait() {
        let held = Held::new(Stream::Err);
        let mut inner = Vec::new();
        let mut outer = Vec::new();
        held.feed(Severity::Error, b"held ", &mut |_: &mut Body| unreachable!("no line ended"));
        let entered = held.with(|composer| {
            assert!(held.with(|_| ()).is_none(), "the hold is one write's");
            held.feed(Severity::Error, b"panicked\nat x", &mut |b: &mut Body| inner.push((b.text().to_vec(), b.unended())));
            composer.feed(b"line\n", &mut |b: &mut Body| outer.push((b.text().to_vec(), b.unended())));
        });
        assert!(entered.is_some());
        assert_eq!(inner, [(b"panicked".to_vec(), false), (b"at x".to_vec(), true)]);
        assert_eq!(outer, [(b"held line".to_vec(), false)]);
    }

    /// A hold whose write unwound is free again.
    #[test]
    fn a_hold_a_panic_unwound_through_is_let_go() {
        let held = Held::new(Stream::Out);
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| held.with(|_| panic!("under the hold"))));
        assert!(unwound.is_err());
        assert!(held.with(|_| ()).is_some());
    }
}

/// A host's build is a guest program's unit tests: there are no descriptors and no
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

    pub fn say(severity: Severity, args: fmt::Arguments<'_>) {
        compose(severity, args, &mut |_: &mut Body| {});
    }

    pub fn say_lane(_lane: &Lane, severity: Severity, args: fmt::Arguments<'_>) {
        compose(severity, args, &mut |_: &mut Body| {});
    }
}
