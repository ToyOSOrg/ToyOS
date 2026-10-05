//! A boot's log as `logkeeper` writes it and serves it: the form a program's line
//! takes beside the kernel's records, the frame `/system/bin/supervisor` hands `logkeeper`
//! a program's output on, the replay a reader who connects late is served
//! from, and the one reader of a line's head ([`parse`]).
//!
//! **Whose line a line is, is decided by the ring it came out of, never by its
//! words.** The supervisor creates one log ring per program it starts, hands the program
//! the ring as its stdout and stderr, and sends `logkeeper` the ring under the
//! manifest's name for the program and its pid ([`REGISTER`]). `logkeeper` alone
//! writes a line's head, so the head is structure no program's bytes can
//! reach. Every line opens with the one head, `toyos_abi::log::Head`:
//!
//! - a kernel record's names `kernel` — `toyos_abi::log::LogRecord`;
//! - a program's names its [`Tag`], which is never `kernel` or `loader` —
//!   [`ProgramLine`];
//! - the loader's names `loader`, on the console and in `loader.log` only;
//! - any other line continues the kernel record above it, whose message held a
//!   newline. A program's text never does: a record ends at a newline, and
//!   [`Text`] writes every control byte as text.
//!
//! The same form goes to the console, where `logkeeper` is the one writer of
//! program lines, without the wall-clock stamp. A terminal shows either in
//! the console's form and in colour ([`Shown`]); no line of the log carries a
//! colour.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::fmt::{self, Display, Write};

pub use toyos_abi::log::Severity;
use toyos_abi::log::{Head, KERNEL, LOADER, UNTIMED};

/// The TCP port `logkeeper` serves this boot's log on, from its first line, on a
/// boot whose manifest gives `logkeeper` a `netstack` connector. **No shipping image
/// does**: the port answers whoever connects, with no authentication, so it is
/// the test estates' and the metal bench's alone.
pub const PORT: u16 = 41337;

/// The service every connection on [`PORT`] is carried by. A swap of it ends
/// each one with no FIN and no reset, which no reader could tell from a boot
/// with nothing to say — so `logkeeper` turns new readers away from the supervisor's
/// [`SWAP`] frame accepting that swap until the process it listened through is
/// gone, and says so ([`CARRIER_LEAVING`]) before the swap may go.
pub const CARRIER: &str = "netstack";

/// `logkeeper`'s line once it turns new readers away for a swap of [`CARRIER`]. A
/// reader whose own connection that swap will end holds the swap's go until
/// this has reached it: a reader that asks again after it is turned away until
/// the next [`CARRIER`] serves, and never admitted by the one being stopped.
pub const CARRIER_LEAVING: &str =
    "logkeeper: netstack is being replaced, and readers are turned away until the next one serves";

/// The name of the port a reader on this machine asks `logkeeper` for the log on;
/// the answer is the read end of a pipe the log is written into. The same port
/// answers `inspect`, so a reader says which it wants ([`READ`]).
pub const SERVICE: &str = "log";

/// A reader's request on [`SERVICE`]: a bare frame, answered by [`SERVED`].
pub const READ: u32 = 3;

/// The manifest's name for the program that is the log: the supervisor endows it the
/// [`ORIGINS`] acceptor and a console it may write ([`CONSOLE`]), and gives
/// it a ring like every other program's, which it reads like every other.
pub const LOGKEEPER: &str = "logkeeper";

/// The endowment label of the acceptor the supervisor hands `logkeeper` for its frames.
/// **Named by no manifest row**, so the one connector to it is the supervisor's own and
/// no program can register a ring under a name of its choosing.
pub const ORIGINS: &str = "log-origins";

/// The endowment label of the one console the supervisor gives a writable handle to:
/// `logkeeper`'s, where it puts every program's line with its head.
pub const CONSOLE: &str = "log-console";

/// The endowment label of a program's end of the pipe [`REGISTER`] hands
/// `logkeeper` the other end of: nothing is written to it, and the program's end
/// closing — which only its exit does — is how `logkeeper` knows the ring has no
/// writer left to wait for. A label and not a slot, so no child inherits it.
pub const ALIVE: &str = "log-alive";

/// The supervisor → `logkeeper`: a program's log ring. The payload is [`Registration`]; the
/// frame carries two handles — the ring, and the read end of a pipe whose
/// only writer is the program, so its end is the ring's.
pub const REGISTER: u32 = 1;

/// `logkeeper`'s answer to a reader on [`SERVICE`]: one handle, the read end of the
/// pipe this boot's log is written into, and a `u64` payload — how many bytes
/// of the boot that pipe starts with, so a reader can tell the boot so far from
/// what arrives after it.
pub const SERVED: u32 = 2;

/// The supervisor → `logkeeper`: a word on a swap of [`CARRIER`], one byte of payload —
/// [`SWAP_LEAVING`] once the supervisor has accepted it, [`SWAP_BACK`] once the process
/// it replaced is gone and another serves or none will.
pub const SWAP: u32 = 4;
pub const SWAP_LEAVING: u8 = b'L';
pub const SWAP_BACK: u8 = b'B';

/// The supervisor → `logkeeper`: the machine is about to be stopped. `logkeeper` reads every ring
/// and the kernel's records, writes them, makes the volume durable, and
/// answers [`FLUSHED`]; the supervisor asks for the stop once it has, or once its bound
/// is spent.
pub const FLUSH: u32 = 5;
pub const FLUSHED: u32 = 6;

/// The supervisor → `logkeeper`: the stop a [`FLUSH`] was for was refused, and the machine
/// runs on. `logkeeper` writes the file again, from the first line it held back.
pub const RESUME: u32 = 7;

/// The supervisor's line before it asks `logkeeper` to flush for a stop: the last line of a
/// boot `/log` is owed, since `logkeeper` answers only once it is durable.
pub const STOPPING: &str = "supervisor: power: the machine stops, and logkeeper makes the log whole first";

/// What a [`REGISTER`] frame says: the pid the supervisor started, and its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Registration<'a> {
    pub pid: u32,
    pub tag: Tag<'a>,
}

impl<'a> Registration<'a> {
    /// The frame's payload, into `out`; answers its length.
    pub fn encode(&self, out: &mut [u8; 4 + MAX_TAG]) -> usize {
        out[..4].copy_from_slice(&self.pid.to_le_bytes());
        let tag = self.tag.as_str().as_bytes();
        out[4..4 + tag.len()].copy_from_slice(tag);
        4 + tag.len()
    }

    /// `None` for a payload no [`Registration::encode`] made.
    pub fn decode(payload: &'a [u8]) -> Option<Self> {
        let pid = u32::from_le_bytes(payload.get(..4)?.try_into().ok()?);
        let tag = Tag::new(core::str::from_utf8(payload.get(4..)?).ok()?)?;
        Some(Self { pid, tag })
    }
}

/// The longest line a byte stream's line carries; a longer one is let go in
/// pieces of this, each a line of its own ([`Lines`]).
pub const MAX_LINE: usize = 1024;

/// The longest name a [`Tag`] holds.
pub const MAX_TAG: usize = 32;

/// A program's name as its lines carry it: one to [`MAX_TAG`] bytes of
/// `[A-Za-z0-9._+-]`, and none of the words a head names another sayer or a
/// CPU by — `kernel`, `loader`, `cpu<n>`. Refused otherwise, so a tag holds no
/// space, bracket, separator or control, and a head that names it is a
/// program's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag<'a>(&'a str);

impl<'a> Tag<'a> {
    pub fn new(name: &'a str) -> Option<Self> {
        let fits = (1..=MAX_TAG).contains(&name.len());
        let kept =
            name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        let anothers = name == KERNEL || name == LOADER || cpu(name).is_some();
        (fits && kept && !anothers).then_some(Self(name))
    }

    pub fn as_str(&self) -> &'a str {
        self.0
    }
}

/// Bytes a program wrote, as a line's text: a control character — C0, DEL or
/// C1 — is written `\xNN` or `\u{NN}`, and each run that is not UTF-8 is one
/// U+FFFD, so no reader of the log, a terminal included, is handed a byte that
/// acts rather than reads. A tab reads, and stays.
pub struct Text<'a>(pub &'a [u8]);

impl Display for Text<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.0.utf8_chunks() {
            for ch in chunk.valid().chars() {
                match ch {
                    '\t' => f.write_char(ch)?,
                    '\0'..='\x1f' | '\x7f' => write!(f, "\\x{:02x}", ch as u32)?,
                    '\u{80}'..='\u{9f}' => write!(f, "\\u{{{:x}}}", ch as u32)?,
                    _ => f.write_char(ch)?,
                }
            }
            if !chunk.invalid().is_empty() {
                f.write_char('\u{FFFD}')?;
            }
        }
        Ok(())
    }
}

/// One program's line as `logkeeper` writes it: the one head
/// (`toyos_abi::log::Head`) — the wall clock `logkeeper` stamps every line of
/// `/log` with (the console's lines carry none), the time the program wrote
/// it, its [`Tag`], its severity above `Info`, the thread that wrote it where
/// that is not the first, and the process where it is not the one the
/// supervisor registered the ring for — then its [`Text`]. No newline: the
/// writer ends the line.
///
/// **The head is `logkeeper`'s words about the record; only the text is the
/// program's.** The time, thread and process are what the program stamped,
/// and the tag is what the supervisor named the ring.
pub struct ProgramLine<'a> {
    /// Empty for none.
    pub stamp: &'a str,
    pub at_ns: u64,
    pub severity: Severity,
    pub tid: u32,
    /// The writing process, where it is not the ring's own.
    pub pid: Option<u32>,
    pub tag: Tag<'a>,
    pub text: &'a [u8],
}

impl Display for ProgramLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let head = Head {
            wall: self.stamp,
            at_ns: Some(self.at_ns),
            cpu: None,
            who: self.tag.as_str(),
            severity: self.severity,
            tid: self.tid,
            pid: self.pid,
        };
        write!(f, "{head} {}", Text(self.text))
    }
}

/// Whose a line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    Kernel,
    Loader,
    /// A program, by its [`Tag`].
    Program(&'a str),
}

/// A line read back through its head: every reader of a line — `logkeeper`'s
/// readers, `/system/bin/console`, the host's harness and metal judges — reads
/// it here, and nowhere else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parsed<'a> {
    /// Milliseconds since the counter's zero; `None` for a line said before
    /// its sayer knew a rate (`toyos_abi::log::UNTIMED`).
    pub ms: Option<u64>,
    pub cpu: Option<u32>,
    pub source: Source<'a>,
    pub severity: Severity,
    /// The head's words before its sayer's, from the time on: no wall clock.
    pub lead: &'a str,
    /// The head's words after its sayer's: `alert tid=3`, or empty.
    pub rest: &'a str,
    pub text: &'a str,
}

/// Read `line`, its newline optional, through its head: `None` for a line no
/// sayer's head opens — a record's continuation, firmware's, or anything else.
///
/// **Read inside the head's bracket and nowhere else**, so no text after it —
/// a program's included — can answer for the time, the CPU or the sayer; and
/// every word of it must be the head's, so a line that only looks like one
/// is none.
pub fn parse(line: &str) -> Option<Parsed<'_>> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let (head, text) = line.strip_prefix('[')?.split_once(']')?;
    let text = match text {
        "" => text,
        _ => text.strip_prefix(' ')?,
    };
    let mut words = Words { head, at: 0 };
    let (mut word, _) = words.next()?;
    // The lead keeps the time's right alignment: only the wall clock and its one space go.
    let mut from = 0;
    if wall_date(word) {
        let (time, at) = words.next().filter(|(time, _)| wall_time(time))?;
        from = at + time.len() + 1;
        (word, _) = words.next()?;
    }
    let ms = match word {
        UNTIMED => None,
        time => Some(millis(time)?),
    };
    let (mut who, mut at) = words.next()?;
    let on = cpu(who);
    if on.is_some() {
        (who, at) = words.next()?;
    }
    let lead = head[from..at].trim_end();
    let source = match who {
        KERNEL => Source::Kernel,
        LOADER => Source::Loader,
        tag => Source::Program(Tag::new(tag)?.as_str()),
    };
    let rest = head[at + who.len()..].trim_start();
    let mut severity = Severity::Info;
    for (word, _) in (Words { head: rest, at: 0 }) {
        if let Some(said) = [Severity::Warn, Severity::Error, Severity::Alert].into_iter().find(|s| s.word() == Some(word)) {
            severity = said;
        } else if !["tid=", "pid="].iter().any(|key| word.strip_prefix(key).is_some_and(digits)) {
            return None;
        }
    }
    Some(Parsed { ms, cpu: on, source, severity, lead, rest, text })
}

/// A head's words, each with where it starts; the seconds' right alignment
/// leaves more than one space between two.
struct Words<'a> {
    head: &'a str,
    at: usize,
}

impl<'a> Iterator for Words<'a> {
    type Item = (&'a str, usize);

    fn next(&mut self) -> Option<Self::Item> {
        let rest = &self.head[self.at..];
        let start = self.at + (rest.len() - rest.trim_start_matches(' ').len());
        let len = self.head[start..].find(' ').unwrap_or(self.head.len() - start);
        self.at = start + len;
        (len > 0).then(|| (&self.head[start..start + len], start))
    }
}

fn digits(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit())
}

/// `cpu<n>` as `n`.
fn cpu(word: &str) -> Option<u32> {
    word.strip_prefix("cpu").filter(|n| digits(n))?.parse().ok()
}

/// `/log`'s wall clock, `YYYY-MM-DD HH:MM:SS`, or the dashes of a machine
/// that could not say: two words no other head word looks like.
fn wall_date(word: &str) -> bool {
    word.len() == 10 && word.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

fn wall_time(word: &str) -> bool {
    word.len() == 8 && word.bytes().all(|b| b.is_ascii_digit() || b == b':' || b == b'-')
}

/// `<secs>.<mmm>` as milliseconds.
fn millis(field: &str) -> Option<u64> {
    let (secs, millis) = field.split_once('.')?;
    if !digits(secs) || millis.len() != 3 || !digits(millis) {
        return None;
    }
    secs.parse::<u64>().ok()?.checked_mul(1_000)?.checked_add(millis.parse().ok()?)
}

/// A line of the log read back as a program's: its tag, severity and text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Said<'a> {
    pub tag: &'a str,
    pub severity: Severity,
    pub text: &'a str,
}

/// Read `line` (its newline optional) as a program's line; `None` for a kernel
/// record, a record's continuation, or anything else.
pub fn program_line(line: &str) -> Option<Said<'_>> {
    let parsed = parse(line)?;
    let Source::Program(tag) = parsed.source else { return None };
    Some(Said { tag, severity: parsed.severity, text: parsed.text })
}

/// Whether `line` is a program's: what a judge of the kernel's records leaves
/// out.
pub fn is_program_line(line: &str) -> bool {
    program_line(line).is_some()
}

/// Whether `line` is a kernel record's, whichever writer spelled it: the
/// console's, the panel's, the black box's or `/log`'s.
pub fn is_kernel_line(line: &str) -> bool {
    parse(line).is_some_and(|parsed| parsed.source == Source::Kernel)
}

/// The milliseconds since the counter's zero a kernel record's line carries,
/// or `None` for any other line and for a record said before the kernel knew
/// a rate.
pub fn record_ms(line: &str) -> Option<u64> {
    parse(line).filter(|parsed| parsed.source == Source::Kernel)?.ms
}

/// The longest head a kernel record's console line opens with: a time of
/// twenty digits, a CPU of ten, and every word after its sayer.
pub const MAX_HEAD: usize = 96;

/// Where the kernel's first record opens in `bytes`: [`Opening::At`] its
/// bracket; [`Opening::From`] the earliest byte that could still open one once
/// more arrive; [`Opening::Nowhere`] for neither.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    At(usize),
    From(usize),
    Nowhere,
}

/// Find the kernel's first record in a console's bytes, whatever came before
/// it: firmware's text, the loader's lines, the last of them cut off where
/// the kernel began writing.
pub fn kernel_opening(bytes: &[u8]) -> Opening {
    let mut from = None;
    for (at, _) in bytes.iter().enumerate().filter(|(_, &b)| b == b'[') {
        let window = &bytes[at..bytes.len().min(at + MAX_HEAD)];
        let Some(close) = window.iter().position(|&b| b == b']') else {
            if window.len() < MAX_HEAD {
                from.get_or_insert(at);
            }
            continue;
        };
        let head = &window[..=close];
        let Ok(head) = core::str::from_utf8(head) else { continue };
        if !is_kernel_line(head) {
            continue;
        }
        match bytes.get(at + close + 1) {
            Some(b' ') => return Opening::At(at),
            Some(_) => continue,
            None => {
                from.get_or_insert(at);
            }
        }
    }
    from.map_or(Opening::Nowhere, Opening::From)
}

/// What opens a line on a screen: whose it is, and its head's words from the
/// time on — never the wall clock, which only `/log` keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeadShown<'a> {
    pub source: Source<'a>,
    /// `11.665 cpu0`, or `13.064` for a program's.
    pub lead: &'a str,
    /// `alert tid=3`, or empty.
    pub rest: &'a str,
}

/// A line of the log as a terminal shows it: the console's form, with no wall
/// clock, coloured by the line's severity and whose it is, and its text with
/// no byte that acts ([`Text`]). The colour is this rendering's and never in
/// the line it was read from.
///
/// `head` is `None` for a kernel record's continuation, which [`Showing`]
/// gives the severity of the record above it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown<'a> {
    pub head: Option<HeadShown<'a>>,
    pub severity: Severity,
    pub text: &'a str,
}

/// The log's lines, in the order they came, as a screen shows each: a line
/// that opens with no head continues the last kernel record and wears its
/// severity, whatever program lines came between.
#[derive(Clone, Copy, Debug)]
pub struct Showing {
    /// The last kernel record's severity.
    severity: Severity,
}

impl Default for Showing {
    fn default() -> Self {
        Self { severity: Severity::Info }
    }
}

impl Showing {
    /// `line`, its newline optional, as a screen shows it.
    pub fn line<'a>(&mut self, line: &'a str) -> Shown<'a> {
        let line = line.strip_suffix('\n').unwrap_or(line);
        let Some(parsed) = parse(line) else {
            return Shown { head: None, severity: self.severity, text: line };
        };
        if parsed.source == Source::Kernel {
            self.severity = parsed.severity;
        }
        let head = HeadShown { source: parsed.source, lead: parsed.lead, rest: parsed.rest };
        Shown { head: Some(head), severity: parsed.severity, text: parsed.text }
    }
}

/// The SGR words a [`Shown`] is drawn in. The stamp is a grey and not SGR 2,
/// which `/system/bin/terminal` does not draw; the rest are the sixteen colours
/// a host terminal's theme keeps legible on its own ground. The loader's
/// sayer wears the kernel's colour: it is the machine's own, as the kernel is.
const STAMP: &str = "\x1b[38;5;245m";
const KERNEL_INK: &str = "\x1b[94m";
const PROGRAM: &str = "\x1b[36m";
const WARN: &str = "\x1b[33m";
const ERROR: &str = "\x1b[91m";
const RESET: &str = "\x1b[0m";

impl Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(HeadShown { source, lead, rest }) = self.head {
            let (ink, who) = match source {
                Source::Kernel => (KERNEL_INK, KERNEL),
                Source::Loader => (KERNEL_INK, LOADER),
                Source::Program(tag) => (PROGRAM, tag),
            };
            write!(f, "{STAMP}[{lead} {ink}{who}{STAMP}")?;
            if !rest.is_empty() {
                write!(f, " {rest}")?;
            }
            write!(f, "]{RESET} ")?;
        }
        let ink = match self.severity {
            Severity::Info => "",
            Severity::Warn => WARN,
            Severity::Error | Severity::Alert => ERROR,
        };
        write!(f, "{ink}{}{RESET}", Text(self.text.as_bytes()))
    }
}

/// One program's output, assembled into lines as it arrives in chunks.
///
/// A line ends at `\n`, which it does not keep, and a `\r` before it goes too;
/// one that reaches [`MAX_LINE`] bytes is let go as it stands, a piece
/// [`Ended::No`] says the program had not ended. What is left when the writer
/// is gone is a piece of its own ([`Lines::finish`]): a dying program's last
/// words are said, not dropped.
#[derive(Default)]
pub struct Lines {
    held: Vec<u8>,
}

/// Whether a piece is where its writer ended a line — the difference between
/// a line of the log and what the program wrote, for a reader that keeps the
/// program's own bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Yes,
    No,
}

impl Lines {
    pub const fn new() -> Self {
        Self { held: Vec::new() }
    }

    /// Take `bytes`, and hand every piece they complete to `line`, in order.
    pub fn push(&mut self, bytes: &[u8], mut line: impl FnMut(&[u8], Ended)) {
        for &byte in bytes {
            if byte == b'\n' {
                let whole = self.held.strip_suffix(b"\r").unwrap_or(&self.held);
                line(whole, Ended::Yes);
                self.held.clear();
                continue;
            }
            self.held.push(byte);
            if self.held.len() == MAX_LINE {
                line(&self.held, Ended::No);
                self.held.clear();
            }
        }
    }

    /// The writer is gone: what it left unfinished, if anything.
    pub fn finish(&mut self, line: impl FnOnce(&[u8], Ended)) {
        if !self.held.is_empty() {
            line(&self.held, Ended::No);
            self.held.clear();
        }
    }
}

/// This boot's log as it was written, for a reader that connects late: every
/// byte since the boot's first line, less whole lines from the front once more
/// than `cap` bytes are held — and a reader told how many bytes that cost it,
/// never handed a line cut in half.
///
/// Positions are offsets into the boot's whole log, so a reader's place
/// survives what is let go ahead of it.
pub struct Replay {
    bytes: Vec<u8>,
    /// The boot's offset of `bytes[0]`.
    base: u64,
    cap: usize,
}

/// What a reader at some offset is owed next.
#[derive(Debug, PartialEq, Eq)]
pub enum Next<'a> {
    /// The bytes after its offset, up to the most it asked for.
    Bytes(&'a [u8]),
    /// Its offset was let go: `lost` bytes are gone, and it resumes at `at`.
    Evicted { lost: u64, at: u64 },
    /// Nothing past its offset yet.
    CaughtUp,
}

impl Replay {
    pub const fn new(cap: usize) -> Self {
        Self { bytes: Vec::new(), base: 0, cap }
    }

    /// The boot's offset after the last byte held: where a reader who has read
    /// everything stands.
    pub fn end(&self) -> u64 {
        self.base + self.bytes.len() as u64
    }

    /// Append whole lines. What falls past `cap` goes from the front, a
    /// quarter of the cap at a time and at a line boundary, so an append does
    /// not move the whole buffer every time.
    pub fn append(&mut self, lines: &[u8]) {
        self.bytes.extend_from_slice(lines);
        if self.bytes.len() <= self.cap {
            return;
        }
        let over = (self.bytes.len() - self.cap + self.cap / 4).min(self.bytes.len());
        let cut = match self.bytes[over..].iter().position(|&b| b == b'\n') {
            Some(at) => over + at + 1,
            None => self.bytes.len(),
        };
        self.bytes.drain(..cut);
        self.base += cut as u64;
    }

    /// What a reader at `from` gets next: the whole lines that fit in `max`
    /// bytes, or the one line after `from` where it alone is longer. So a
    /// reader that started on a line always stands on one, and an eviction's
    /// notice never lands inside a line it was handed half of.
    pub fn next(&self, from: u64, max: usize) -> Next<'_> {
        if from < self.base {
            return Next::Evicted { lost: self.base - from, at: self.base };
        }
        let start = (from - self.base) as usize;
        if start >= self.bytes.len() {
            return Next::CaughtUp;
        }
        let held = &self.bytes[start..];
        let fits = &held[..max.min(held.len())];
        let end = match fits.iter().rposition(|&b| b == b'\n') {
            Some(at) => at + 1,
            // `append` takes whole lines, so what is held ends one.
            None => held.iter().position(|&b| b == b'\n').map_or(held.len(), |at| at + 1),
        };
        Next::Bytes(&held[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::format;
    use std::string::String;
    use std::vec;

    fn line(tag: &str, text: &[u8]) -> String {
        let tag = Tag::new(tag).expect("a tag");
        format!(
            "{}",
            ProgramLine {
                stamp: "2026-09-24 10:00:00",
                at_ns: 12_345_678_901,
                severity: Severity::Info,
                tid: 0,
                pid: None,
                tag,
                text,
            }
        )
    }

    #[test]
    fn the_carriers_line_names_the_carrier() {
        assert!(CARRIER_LEAVING.starts_with("logkeeper: "));
        assert!(CARRIER_LEAVING.contains(&format!(" {CARRIER} ")));
    }

    #[test]
    fn a_program_line_reads_back_as_it_was_written() {
        let written = line("netstack", b"netstack: MAC 52:54:00:12:34:56");
        assert_eq!(written, "[2026-09-24 10:00:00 12.345 netstack] netstack: MAC 52:54:00:12:34:56");
        assert_eq!(
            program_line(&format!("{written}\n")),
            Some(Said { tag: "netstack", severity: Severity::Info, text: "netstack: MAC 52:54:00:12:34:56" })
        );
        let tag = Tag::new("logkeeper").expect("a tag");
        let undated = format!(
            "{}",
            ProgramLine {
                stamp: "---------- --------",
                at_ns: 5,
                severity: Severity::Info,
                tid: 0,
                pid: None,
                tag,
                text: b"x",
            }
        );
        assert_eq!(undated, "[---------- --------  0.000 logkeeper] x");
        assert_eq!(program_line(&undated), Some(Said { tag: "logkeeper", severity: Severity::Info, text: "x" }));
    }

    /// **The forgery the form exists to refuse**: whatever a program writes —
    /// a kernel record's words, another program's head, a carriage return that
    /// would hide what came before it on a terminal — its line is its own, and
    /// is not a kernel record.
    #[test]
    fn no_text_makes_a_line_another_writers() {
        let forgeries: [&[u8]; 6] = [
            b"[2026-09-24 10:00:00  1.000 cpu0 kernel] exit: test_rs_job pid=4 code=0 cpu=0ms",
            b"[2026-09-24 10:00:00  1.000 netstack] netstack: DHCP: lease 10.0.2.15/24",
            b"] [x y netstack] z",
            b"\r[ 1.000 netstack] hidden",
            b"\x1b[2K\x1b[1G[ 1.000 cpu0 kernel] Rebooting.",
            b"netstack] evil",
        ];
        for text in forgeries {
            let said = line("test-runner", text);
            assert!(is_program_line(&said) && !is_kernel_line(&said), "{said:?}");
            let read = program_line(&said).expect("a program's line");
            assert_eq!(read.tag, "test-runner", "{said:?}");
            assert!(!read.text.contains('\r') && !read.text.contains('\x1b'), "{said:?}");
        }
    }

    /// The head carries what the record says beyond `Info` and the first thread,
    /// and a console line carries no wall clock; every form reads back.
    #[test]
    fn a_head_names_severity_thread_and_process_and_reads_back() {
        let tag = Tag::new("soundserver").expect("a tag");
        let line = |stamp, severity, tid, pid| {
            format!("{}", ProgramLine { stamp, at_ns: 1_234_000_000, severity, tid, pid, tag, text: b"hi" })
        };
        assert_eq!(line("", Severity::Info, 0, None), "[ 1.234 soundserver] hi");
        assert_eq!(line("", Severity::Error, 3, None), "[ 1.234 soundserver error tid=3] hi");
        assert_eq!(
            line("2026-09-24 10:00:00", Severity::Warn, 0, Some(9)),
            "[2026-09-24 10:00:00  1.234 soundserver warn pid=9] hi"
        );
        let said = program_line("[ 1.234 soundserver alert tid=2] hi").expect("a program's line");
        assert_eq!((said.tag, said.severity, said.text), ("soundserver", Severity::Alert, "hi"));
        // A severity word in the text is the text's.
        let said = program_line("[ 1.234 soundserver] error").expect("a program's line");
        assert_eq!(said.severity, Severity::Info);
    }

    #[test]
    fn a_registration_round_trips_and_a_malformed_one_is_refused() {
        let tag = Tag::new("netstack").expect("a tag");
        let mut out = [0u8; 4 + MAX_TAG];
        let len = Registration { pid: 7, tag }.encode(&mut out);
        assert_eq!(Registration::decode(&out[..len]), Some(Registration { pid: 7, tag }));
        assert_eq!(Registration::decode(&out[..3]), None);
        assert_eq!(Registration::decode(b"\x07\0\0\0a b"), None);
        assert_eq!(Registration::decode(b"\x07\0\0\0"), None);
        assert_eq!(Registration::decode(b"\x07\0\0\0kernel"), None);
    }

    /// A tag is one word of the names' charset, and never a word a head names
    /// another sayer or a CPU by: a program named `kernel` would write the
    /// kernel's records.
    #[test]
    fn a_tag_is_one_word_of_the_names_charset_and_no_other_sayers() {
        let longest = "x".repeat(MAX_TAG);
        for good in ["netstack", "test-runner", "a.b_c+d", "cpu", "cpux", "kernels", longest.as_str()] {
            assert!(Tag::new(good).is_some(), "{good:?}");
        }
        let longer = "x".repeat(MAX_TAG + 1);
        for bad in ["", "a b", "a]b", "[", "x\n", "é", "kernel", "loader", "cpu0", "cpu17", longer.as_str()] {
            assert!(Tag::new(bad).is_none(), "{bad:?}");
        }
    }

    /// Every sayer's line reads back through the one head, in every form it
    /// takes; a line that only looks like one is none.
    #[test]
    fn every_sayers_head_reads_back_and_nothing_else_does() {
        let read = |line| parse(line).map(|p| (p.ms, p.cpu, p.source, p.severity, p.text));
        assert_eq!(read("[ 9.876 cpu0 loader] Loader clock"), Some((Some(9_876), Some(0), Source::Loader, Severity::Info, "Loader clock")));
        assert_eq!(read("[--.--- cpu0 loader] x"), Some((None, Some(0), Source::Loader, Severity::Info, "x")));
        assert_eq!(read("[11.665 cpu3 kernel alert tid=7] fpu: x"), Some((Some(11_665), Some(3), Source::Kernel, Severity::Alert, "fpu: x")));
        assert_eq!(read("[2026-10-04 09:30:00 11.665 cpu0 kernel] y\n"), Some((Some(11_665), Some(0), Source::Kernel, Severity::Info, "y")));
        assert_eq!(read("[---------- -------- --.--- cpu0 kernel] y"), Some((None, Some(0), Source::Kernel, Severity::Info, "y")));
        assert_eq!(read("[123.456 supervisor warn tid=2 pid=9] z"), Some((Some(123_456), None, Source::Program("supervisor"), Severity::Warn, "z")));
        assert_eq!(read("[ 1.000 cpu0 kernel]"), Some((Some(1_000), Some(0), Source::Kernel, Severity::Info, "")));
        for none in [
            "",
            "  0: kernel::panic",
            "BdsDxe: loading Boot0001",
            "\x1b[2J\x1b[01;01H[ 1.000 cpu0 loader] x",
            "[x] said 99.000 cpu0",
            "[ 1.00 cpu0 kernel] three digits of milliseconds",
            "[ 1.000 cpu0] nobody said it",
            "[ 1.000 cpu0 kernel boot] a word no head has",
            "[ 1.000 cpu0 kernel tid=x] a thread no number names",
            "[ 1.000 cpu0 kernel]no space",
            "[2026-10-04 1.000 cpu0 kernel] half a wall clock",
            "[ 1.000 cpu0 cpu1 kernel] two CPUs",
            "[ 1.000 a b] two sayers",
        ] {
            assert_eq!(parse(none), None, "{none:?}");
        }
    }

    #[test]
    fn a_records_time_is_read_inside_its_bracket_and_nowhere_else() {
        assert_eq!(record_ms("[2026-09-07 22:57:46  3.109 cpu1 kernel] exit: a pid=7"), Some(3_109));
        assert_eq!(record_ms("[ 3.109 cpu1 kernel] exit: a pid=7 code=0"), Some(3_109));
        assert_eq!(record_ms("[2026-09-07 22:58:03 20.071 cpu2 kernel tid=1] x"), Some(20_071));
        assert_eq!(record_ms("[2026-09-07 22:58:03 20.071 netstack] [99.000 cpu0 kernel] x"), None);
        assert_eq!(record_ms("[ 9.000 cpu0 loader] a loader's line"), None);
        assert_eq!(record_ms("[--.--- cpu0 kernel] before the clock"), None);
        assert_eq!(record_ms("no timestamp here, cpu=1ms"), None);
    }

    /// The kernel's first record is found on a console whatever came before
    /// it — firmware's escapes, the loader's lines, the last of them cut by
    /// the kernel — and bytes that could still open it are held, no more.
    #[test]
    fn the_kernels_first_record_is_found_after_whatever_came_before() {
        let before = "\x1b[2J\x1b[01;01H[ 2.300 cpu0 loader] ROOT: read [at ";
        let console = format!("{before}[--.--- cpu0 kernel] boot: memory map\n");
        assert_eq!(kernel_opening(console.as_bytes()), Opening::At(before.len()));
        for cut in 0..console.len() {
            let held = kernel_opening(&console.as_bytes()[..cut]);
            assert!(
                matches!(held, Opening::Nowhere | Opening::From(_)) || held == Opening::At(before.len()),
                "{cut}: {held:?}"
            );
            if let Opening::From(at) = held {
                assert!(cut - at < MAX_HEAD, "{cut}: held from {at}");
            }
            if cut > before.len() + "[--.--- cpu0 kernel]".len() {
                assert_eq!(held, Opening::At(before.len()), "{cut}");
            }
        }
        assert_eq!(kernel_opening(b"[ 2.300 cpu0 loader] only the loader\n"), Opening::Nowhere);
    }

    /// Every head [`ProgramLine`] writes reads back to the time it carries, and
    /// no text after the head answers for it.
    #[test]
    fn a_program_lines_time_is_read_inside_its_head_and_nowhere_else() {
        let tag = Tag::new("test-runner").expect("a tag");
        for stamp in ["", "2026-09-24 10:00:00", "---------- --------"] {
            for severity in [Severity::Info, Severity::Warn, Severity::Error, Severity::Alert] {
                for (tid, pid) in [(0, None), (3, None), (0, Some(9)), (3, Some(9))] {
                    let line =
                        format!("{}", ProgramLine { stamp, at_ns: 1_500_999_999, severity, tid, pid, tag, text: b"9.000" });
                    let read = parse(&line).map(|p| (p.source, p.ms));
                    assert_eq!(read, Some((Source::Program("test-runner"), Some(1_500))), "{line:?}");
                }
            }
        }
    }

    /// What a terminal shows of a line, with its colours taken out.
    fn plain(shown: Shown<'_>) -> String {
        let painted = format!("{shown}");
        let mut out = String::new();
        let mut rest = painted.as_str();
        while let Some((before, after)) = rest.split_once('\x1b') {
            out.push_str(before);
            rest = &after[after.find('m').expect("an SGR ends in m") + 1..];
        }
        out.push_str(rest);
        out
    }

    fn kernel_record(severity: Severity, flags: u8, msg: &str) -> toyos_abi::log::LogRecord {
        let mut r = toyos_abi::log::LogRecord {
            seq: 1,
            at_ns: 1_193_000_000,
            cpu: 3,
            tid: 7,
            severity: severity as u8,
            flags,
            len: msg.len() as u16,
            ..toyos_abi::log::LogRecord::EMPTY
        };
        r.msg[..msg.len()].copy_from_slice(msg.as_bytes());
        r
    }

    /// **A screen shows the console's line, whichever form it read**: `/log`'s
    /// line and the console's, each made by its own writer's formatter, show
    /// as the console's line, with the time and the CPU and no wall clock —
    /// for every severity, the kernel's records and a program's both.
    #[test]
    fn a_screen_shows_the_consoles_line_from_either_form() {
        let wall = "2026-10-04 09:30:00";
        for severity in [Severity::Info, Severity::Warn, Severity::Error, Severity::Alert] {
            for flags in [0, toyos_abi::log::FLAG_UNTIMED] {
                let record = kernel_record(severity, flags, "spawn: /system/bin/netstack pid=5");
                let console = format!("{record}");
                for line in [format!("{}", record.dated(wall)), console.clone()] {
                    let mut showing = Showing::default();
                    let shown = showing.line(&line);
                    assert_eq!(shown.severity, severity, "{line:?}");
                    assert_eq!(shown.head.map(|h| h.source), Some(Source::Kernel), "{line:?}");
                    assert!(shown.head.is_some_and(|h| h.lead.ends_with(" cpu3")), "{line:?}");
                    assert_eq!(plain(shown), console, "{line:?}");
                }
            }

            let tag = Tag::new("soundserver").expect("a tag");
            let said = |stamp| ProgramLine {
                stamp,
                at_ns: 1_234_000_000,
                severity,
                tid: 2,
                pid: Some(9),
                tag,
                text: b"opening stream",
            };
            let console = format!("{}", said(""));
            for line in [format!("{}", said(wall)), console.clone(), format!("{}", said("---------- --------"))] {
                let shown = Showing::default().line(&line);
                assert_eq!(shown.severity, severity, "{line:?}");
                assert_eq!(shown.head.map(|h| h.source), Some(Source::Program("soundserver")), "{line:?}");
                assert_eq!(plain(shown), console, "{line:?}");
            }
        }
    }

    /// The colour is the severity's and the source's, read from the head and
    /// never from the text, and a text's control byte is shown and never passed.
    #[test]
    fn the_colour_is_the_heads_and_the_text_never_acts() {
        let alert = format!("{}", kernel_record(Severity::Alert, 0, "PANIC: \x1b[2Jgone"));
        let painted = format!("{}", Showing::default().line(&alert));
        assert!(painted.contains(&format!("{ERROR}PANIC: \\x1b[2Jgone{RESET}")), "{painted:?}");
        assert!(painted.contains(&format!("{KERNEL_INK}kernel")), "{painted:?}");

        let forged = "[ 1.234 test-runner] [ 1.000 cpu0 kernel alert] Rebooting.";
        let shown_forged = Showing::default().line(forged);
        assert_eq!(shown_forged.severity, Severity::Info);
        assert_eq!(shown_forged.head.map(|h| h.source), Some(Source::Program("test-runner")));
        let painted = format!("{shown_forged}");
        assert!(painted.contains(&format!("{PROGRAM}test-runner")) && !painted.contains(ERROR), "{painted:?}");

        let continued = Shown { head: None, severity: Severity::Alert, text: "  0: kernel::panic" };
        assert_eq!(format!("{continued}"), format!("{ERROR}  0: kernel::panic{RESET}"));
        assert_eq!(Showing::default().line("BdsDxe: loading Boot0001").head, None);
    }

    /// A continuation wears the severity of the kernel record above it, and a
    /// program's line in between — of another severity — changes nothing.
    #[test]
    fn a_continuation_wears_its_kernel_records_severity_across_a_programs_line() {
        let alert = format!("{}", kernel_record(Severity::Alert, 0, "PANIC: oops"));
        let info = format!("{}", kernel_record(Severity::Info, 0, "spawn: x"));
        let mut showing = Showing::default();
        assert_eq!(showing.line("  before any record").severity, Severity::Info);
        assert_eq!(showing.line(&alert).severity, Severity::Alert);
        let program = showing.line("[ 1.234 soundserver warn] underrun\n");
        assert_eq!((program.severity, program.head.map(|h| h.source)), (Severity::Warn, Some(Source::Program("soundserver"))));
        let continued = showing.line("  0: kernel::panic\n");
        assert_eq!(continued, Shown { head: None, severity: Severity::Alert, text: "  0: kernel::panic" });
        assert_eq!(showing.line("[ 1.300 soundserver] resumed").severity, Severity::Info);
        assert_eq!(showing.line("  1: kernel::main").severity, Severity::Alert);
        showing.line(&info);
        assert_eq!(showing.line("  more").severity, Severity::Info);
    }

    #[test]
    fn a_kernel_record_and_its_continuation_are_no_programs() {
        assert_eq!(program_line("[2026-09-24 10:00:00  1.216 cpu0 kernel] Boot: complete (1216ms)"), None);
        assert_eq!(program_line("  its second line"), None);
        assert!(!is_program_line("[x] y"));
        assert!(is_kernel_line("[ 1.216 cpu0 kernel] Boot: complete (1216ms)"));
        assert!(!is_kernel_line("[ 1.216 cpu0 loader] Starting kernel..."));
    }

    /// OSC, a lone ESC, a carriage return and a backspace inside a line each
    /// reach the log as text, and so do C1 and DEL; a tab stays a tab.
    #[test]
    fn a_control_byte_is_written_and_never_passed() {
        let text = "a\x1b]0;title\x07b\rc\x08d\x1be\x7ff\u{9b}g\th".as_bytes();
        assert_eq!(
            format!("{}", Text(text)),
            "a\\x1b]0;title\\x07b\\x0dc\\x08d\\x1be\\x7ff\\u{9b}g\th"
        );
        assert_eq!(format!("{}", Text(b"ok\xffok")), "ok\u{FFFD}ok");
    }

    fn assemble(chunks: &[&[u8]]) -> vec::Vec<(vec::Vec<u8>, Ended)> {
        let mut lines = Lines::new();
        let mut out = vec::Vec::new();
        for chunk in chunks {
            lines.push(chunk, |l, ended| out.push((l.to_vec(), ended)));
        }
        lines.finish(|l, ended| out.push((l.to_vec(), ended)));
        out
    }

    #[test]
    fn lines_are_whole_however_the_writes_split_them() {
        let got = assemble(&[b"one\ntw", b"o\r\n", b"", b"thr", b"ee\n\nlast"]);
        let want: [(&[u8], Ended); 5] = [
            (b"one", Ended::Yes),
            (b"two", Ended::Yes),
            (b"three", Ended::Yes),
            (b"", Ended::Yes),
            (b"last", Ended::No),
        ];
        assert_eq!(got, want.map(|(l, e)| (l.to_vec(), e)));
    }

    /// A line past the bound is let go in pieces, and only the last is one its
    /// program ended — so a reader that keeps the program's own bytes puts
    /// them back together.
    #[test]
    fn a_line_past_the_bound_is_let_go_in_pieces_and_nothing_is_lost() {
        let long = vec![b'x'; MAX_LINE * 2 + 7];
        let mut chunk = long.clone();
        chunk.push(b'\n');
        let got = assemble(&[&chunk]);
        let shape: vec::Vec<(usize, Ended)> = got.iter().map(|(l, e)| (l.len(), *e)).collect();
        assert_eq!(shape, vec![(MAX_LINE, Ended::No), (MAX_LINE, Ended::No), (7, Ended::Yes)]);
        assert_eq!(got.into_iter().flat_map(|(l, _)| l).collect::<vec::Vec<_>>(), long);
    }

    #[test]
    fn a_reader_at_any_offset_gets_the_bytes_after_it_in_order() {
        let mut replay = Replay::new(1 << 20);
        let mut want = vec::Vec::new();
        for i in 0..1000 {
            let one = format!("line {i}\n");
            replay.append(one.as_bytes());
            want.extend_from_slice(one.as_bytes());
        }
        let mut got = vec::Vec::new();
        let mut at = 0u64;
        loop {
            match replay.next(at, 97) {
                Next::Bytes(bytes) => {
                    got.extend_from_slice(bytes);
                    at += bytes.len() as u64;
                }
                Next::CaughtUp => break,
                Next::Evicted { .. } => panic!("nothing was let go"),
            }
        }
        assert_eq!(got, want);
        assert_eq!(at, replay.end());
    }

    /// Past the cap, whole lines go from the front, and a reader that stood
    /// among them is told exactly how many bytes it lost and resumes on a line.
    #[test]
    fn what_is_let_go_is_whole_lines_and_is_counted() {
        let mut replay = Replay::new(100);
        let mut total = 0u64;
        for i in 0..50 {
            let one = format!("l{i:02}\n");
            total += one.len() as u64;
            replay.append(one.as_bytes());
        }
        assert_eq!(replay.end(), total);
        let Next::Evicted { lost, at } = replay.next(0, 1000) else { panic!("the front was let go") };
        assert_eq!(lost, at);
        let Next::Bytes(rest) = replay.next(at, 1000) else { panic!("bytes after the cut") };
        assert!(rest.starts_with(b"l"), "{rest:?}");
        assert!(rest.len() <= 100);
        assert_eq!(at + rest.len() as u64, total);
        assert_eq!(replay.next(total, 10), Next::CaughtUp);
    }

    /// **A reader is never handed a line cut in half, eviction included.** A
    /// reader asking for fewer bytes than its next line holds is handed the
    /// whole line; appends then let that line go; every piece it is handed
    /// after ends a line, so the notice its eviction is owed begins one.
    #[test]
    fn a_reader_is_never_left_inside_a_line_even_by_an_eviction() {
        let mut replay = Replay::new(100);
        replay.append(b"a line of twenty-six bytes\n");
        let Next::Bytes(first) = replay.next(0, 7) else { panic!("a line is held") };
        assert_eq!(first, b"a line of twenty-six bytes\n", "a line handed in part");
        let mut at = first.len() as u64;
        for i in 0..40 {
            replay.append(format!("l{i:02}\n").as_bytes());
        }
        let mut evicted = false;
        let mut handed = vec::Vec::new();
        loop {
            match replay.next(at, 7) {
                Next::Bytes(bytes) => {
                    assert!(bytes.ends_with(b"\n"), "a piece that ends inside a line: {bytes:?}");
                    handed.extend_from_slice(bytes);
                    at += bytes.len() as u64;
                }
                Next::Evicted { at: resume, .. } => {
                    evicted = true;
                    at = resume;
                }
                Next::CaughtUp => break,
            }
        }
        assert!(evicted, "the appends let the reader's place go");
        assert!(handed.starts_with(b"l"), "{handed:?}");
        assert!(handed.ends_with(b"l39\n"));
    }
}
