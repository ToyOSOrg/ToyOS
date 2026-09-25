//! A boot's log as `logd` writes it and serves it: the form a program's line
//! takes beside the kernel's records, the frame `/system/bin/init` hands `logd`
//! a program's output on, and the replay a reader who connects late is served
//! from.
//!
//! **Whose line a line is, is decided by the pipe it came out of, never by its
//! words.** init creates one pipe per program it starts, hands the program the
//! write end as its stdout and stderr, and moves the read end to `logd` under
//! the manifest's name for the program ([`REGISTER`]). `logd` alone writes a
//! line's head, so the head is structure no program's bytes can reach:
//!
//! - a kernel record opens with `[` — `toyos_abi::log::LogRecord::tagged`;
//! - a program's line opens with [`OPEN`], carries its [`Tag`] as the last word
//!   before [`CLOSE`], and its text after — [`ProgramLine`];
//! - any other line continues the kernel record above it, whose message held a
//!   newline. A program's text never does: [`Lines`] ends a line at every
//!   newline and [`Text`] writes every control byte as text.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::fmt::{self, Display, Write};

/// The TCP port `logd` serves this boot's log on, from its first line.
pub const PORT: u16 = 41337;

/// The service every connection on [`PORT`] is carried by. A swap of it ends
/// each one with no FIN and no reset, which no reader could tell from a boot
/// with nothing to say — so `logd` turns new readers away from init's word
/// accepting that swap until the process it listened through is gone, and says
/// so ([`CARRIER_LEAVING`]) before the swap may go.
pub const CARRIER: &str = "netd";

/// `logd`'s line once it turns new readers away for a swap of [`CARRIER`]. A
/// reader whose own connection that swap will end holds the swap's go until
/// this has reached it: a reader that asks again after it is turned away until
/// the next [`CARRIER`] serves, and never admitted by the one being stopped.
pub const CARRIER_LEAVING: &str =
    "logd: netd is being replaced, and readers are turned away until the next one serves";

/// The name of the port a reader on this machine asks `logd` for the log on;
/// the answer is the read end of a pipe the log is written into.
pub const SERVICE: &str = "log";

/// The manifest's name for the program that is the log: init endows it the
/// [`ORIGINS`] acceptor and gives it no pipe, since its own lines are its to
/// write.
pub const LOGD: &str = "logd";

/// The endowment label of the acceptor init hands `logd` for [`REGISTER`]
/// frames. **Named by no manifest row**, so the one connector to it is init's
/// own and no program can register a pipe under a name of its choosing.
pub const ORIGINS: &str = "log-origins";

/// A program's output: the payload is its [`Tag`], and the frame carries one
/// handle, the read end of the pipe the program writes its stdout and stderr to.
pub const REGISTER: u32 = 1;

/// `logd`'s answer to a reader on [`SERVICE`]: one handle, the read end of the
/// pipe this boot's log is written into, and a `u64` payload — how many bytes
/// of the boot that pipe starts with, so a reader can tell the boot so far from
/// what arrives after it.
pub const SERVED: u32 = 2;

/// What opens a program's line, and no kernel record's.
pub const OPEN: char = '{';

/// What closes a program's line's head.
pub const CLOSE: char = '}';

/// The longest line a program's line carries; a longer one is written in
/// pieces of this, each a line of its own.
pub const MAX_LINE: usize = 1024;

/// The longest name a [`Tag`] holds.
pub const MAX_TAG: usize = 32;

/// A program's name as its lines carry it: one to [`MAX_TAG`] bytes of
/// `[A-Za-z0-9._+-]`. Refused otherwise, so a tag holds no space, bracket,
/// separator or control, and so is always the one word before [`CLOSE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag<'a>(&'a str);

impl<'a> Tag<'a> {
    pub fn new(name: &'a str) -> Option<Self> {
        let fits = (1..=MAX_TAG).contains(&name.len());
        let kept =
            name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
        (fits && kept).then_some(Self(name))
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

/// One program's line as `logd` writes it:
/// `{<stamp> <secs>.<mmm> <tag>} <text>` — the wall clock `logd` stamps every
/// line with, the monotonic time it read the line, the program's [`Tag`], and
/// its [`Text`]. No newline: the writer ends the line.
pub struct ProgramLine<'a> {
    pub stamp: &'a str,
    pub at_ns: u64,
    pub tag: Tag<'a>,
    pub text: &'a [u8],
}

impl Display for ProgramLine<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.at_ns / 1_000_000_000;
        let millis = self.at_ns % 1_000_000_000 / 1_000_000;
        write!(
            f,
            "{OPEN}{} {secs}.{millis:03} {}{CLOSE} {}",
            self.stamp,
            self.tag.as_str(),
            Text(self.text)
        )
    }
}

/// A line of the log read back as a program's: its tag and its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Said<'a> {
    pub tag: &'a str,
    pub text: &'a str,
}

/// Read `line` (its newline optional) as a program's line; `None` for a kernel
/// record, a record's continuation, or anything else.
pub fn program_line(line: &str) -> Option<Said<'_>> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let rest = line.strip_prefix(OPEN)?;
    let (head, text) = rest.split_once(CLOSE)?;
    let text = text.strip_prefix(' ')?;
    let tag = head.rsplit(' ').next()?;
    Tag::new(tag)?;
    Some(Said { tag, text })
}

/// The milliseconds since boot a kernel record's line carries, or `None` for
/// any other line.
///
/// **Found from the CPU it precedes rather than by position**: the field before
/// it is the writer's tag, and the writers disagree about it on purpose —
/// `logd` puts a wall clock there and the panel puts nothing.
///
/// Read inside the record's bracket and nowhere else, so no text after it — a
/// program's included — can answer for the time.
pub fn record_ms(line: &str) -> Option<u64> {
    let (head, _) = line.strip_prefix('[')?.split_once("] ")?;
    let (before, _) = head.split_once(" cpu")?;
    let field = before.split_whitespace().next_back()?;
    let (secs, millis) = field.split_once('.')?;
    let secs: u64 = secs.parse().ok()?;
    let millis: u64 = millis.parse().ok()?;
    secs.checked_mul(1_000)?.checked_add(millis)
}

/// Whether `line` opens as a program's line: what a judge of the kernel's
/// records leaves out.
pub fn is_program_line(line: &str) -> bool {
    line.starts_with(OPEN)
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

    /// What a reader at `from` gets next, at most `max` bytes of it.
    pub fn next(&self, from: u64, max: usize) -> Next<'_> {
        if from < self.base {
            return Next::Evicted { lost: self.base - from, at: self.base };
        }
        let start = (from - self.base) as usize;
        if start >= self.bytes.len() {
            return Next::CaughtUp;
        }
        let end = start.saturating_add(max).min(self.bytes.len());
        Next::Bytes(&self.bytes[start..end])
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
        format!("{}", ProgramLine { stamp: "2026-09-24 10:00:00", at_ns: 12_345_678_901, tag, text })
    }

    #[test]
    fn the_carriers_line_names_the_carrier() {
        assert!(CARRIER_LEAVING.starts_with("logd: "));
        assert!(CARRIER_LEAVING.contains(&format!(" {CARRIER} ")));
    }

    #[test]
    fn a_program_line_reads_back_as_it_was_written() {
        let written = line("netd", b"netd: MAC 52:54:00:12:34:56");
        assert_eq!(written, "{2026-09-24 10:00:00 12.345 netd} netd: MAC 52:54:00:12:34:56");
        assert_eq!(
            program_line(&format!("{written}\n")),
            Some(Said { tag: "netd", text: "netd: MAC 52:54:00:12:34:56" })
        );
        let tag = Tag::new("logd").expect("a tag");
        let undated = format!("{}", ProgramLine { stamp: "---------- --------", at_ns: 5, tag, text: b"x" });
        assert_eq!(program_line(&undated), Some(Said { tag: "logd", text: "x" }));
    }

    /// **The forgery the form exists to refuse**: whatever a program writes —
    /// a kernel record's words, another program's head, a carriage return that
    /// would hide what came before it on a terminal — its line is its own, and
    /// is not a kernel record.
    #[test]
    fn no_text_makes_a_line_another_writers() {
        let forgeries: [&[u8]; 6] = [
            b"[2026-09-24 10:00:00 1.000 cpu0] exit: test_rs_job pid=4 code=0 cpu=0ms",
            b"{2026-09-24 10:00:00 1.000 netd} netd: DHCP: lease 10.0.2.15/24",
            b"} {x y netd} z",
            b"\r{a b netd} hidden",
            b"\x1b[2K\x1b[1G[kernel 1.0 cpu0] Rebooting.",
            b"netd} evil",
        ];
        for text in forgeries {
            let said = line("test-runner", text);
            assert!(is_program_line(&said), "{said:?}");
            let read = program_line(&said).expect("a program's line");
            assert_eq!(read.tag, "test-runner", "{said:?}");
            assert!(!read.text.contains('\r') && !read.text.contains('\x1b'), "{said:?}");
        }
    }

    #[test]
    fn a_tag_is_one_word_of_the_names_charset() {
        let longest = "x".repeat(MAX_TAG);
        for good in ["netd", "test-runner", "a.b_c+d", longest.as_str()] {
            assert!(Tag::new(good).is_some(), "{good:?}");
        }
        let longer = "x".repeat(MAX_TAG + 1);
        for bad in ["", "a b", "a}b", "{", "x\n", "é", longer.as_str()] {
            assert!(Tag::new(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_records_time_is_read_inside_its_bracket_and_nowhere_else() {
        assert_eq!(record_ms("[2026-09-07 22:57:46 3.109 cpu1] exit: a pid=7"), Some(3_109));
        assert_eq!(record_ms("[3.109 cpu1] exit: a pid=7 code=0"), Some(3_109));
        assert_eq!(record_ms("[2026-09-07 22:58:03 20.071 cpu2 tid=1] x"), Some(20_071));
        assert_eq!(record_ms("{2026-09-07 22:58:03 20.071 netd} 99.000 cpu0"), None);
        assert_eq!(record_ms("[x] said 99.000 cpu0"), None);
        assert_eq!(record_ms("no timestamp here, cpu=1ms"), None);
        assert_eq!(record_ms(""), None);
    }

    #[test]
    fn a_kernel_record_and_its_continuation_are_no_programs() {
        assert_eq!(program_line("[2026-09-24 10:00:00 1.216 cpu0] Boot: complete (1216ms)"), None);
        assert_eq!(program_line("  its second line"), None);
        assert!(!is_program_line("[x] y"));
        assert_eq!(program_line("{a b c d} x"), Some(Said { tag: "d", text: "x" }));
        assert_eq!(program_line("{a b c!} x"), None, "a head whose last word is no tag is nobody's");
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
}
