//! The kernel's log record, the cursor that reads it, and the head every
//! line of the log opens with ([`Head`]).
//!
//! One layout, two types over it. The kernel's slot is this struct with its
//! first word made atomic; [`LogRecord`] is what a reader gets, and by the time
//! it holds one that word is just a sequence number. One type with an
//! `AtomicU64` in it would make the copy-out a transmute of an atomic into a
//! value nobody synchronises on, and would give a userland reader a field named
//! `commit` that commits nothing.

/// Message bytes a record carries.
///
/// **Sized to the next power-of-two record that holds the longest measured
/// line**, 863 bytes. The record's other fields are 32 bytes fixed, so
/// [`RECORD_BYTES`] — a power of two by its own derivation — is 32 plus this
/// constant; 1024 is the smallest power of two past 32 + 863, which makes this
/// 992, at zero alignment padding. A longer message is cut at its tail and the
/// cut counted in [`LogRecord::elided`]; the unbounded case — a demangled
/// backtrace symbol — is elided head-and-tail before it is formatted
/// (`toyos-elide`).
pub const MAX_RECORD_MESSAGE: usize = 992;

/// One record on the wire, and one slot in a shard. A power of two so a reader
/// indexes by shift and the kernel never does length arithmetic.
pub const RECORD_BYTES: usize = 1024;

/// Shards a cursor can name, which is the machine's CPU count.
///
/// Not read from `sched::MAX_CPUS`: this is an ABI struct's width, so it is
/// fixed by the ABI and the kernel is what must agree with it. A machine with
/// more CPUs than this is a kernel that cannot answer
/// [`SYS_LOG_READ`](crate::syscall::SYS_LOG_READ) at all, which is a build-time
/// disagreement rather than a runtime one.
pub const MAX_LOG_SHARDS: usize = 8;

/// How much a record matters, in order: a reader may keep what is at or above
/// a floor, and one that treats a severity specially compares against it.
///
/// **Four because four have writers and readers.** The kernel writes `Info`
/// (`log!`) and `Alert` (`alert!`); a program's stdout is `Info` and its stderr
/// `Error`, and its own lines choose. Every rendered line names one above
/// `Info` by its [`word`](Self::word), a screen colours the line by it, and
/// `/system/bin/logkeeper` makes the volume durable at `Alert` rather than on
/// its interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Severity {
    Info = 0,
    Warn = 1,
    Error = 2,
    /// A refusal, a corruption, or a fault.
    Alert = 3,
}

impl Severity {
    /// A byte that crossed a trust boundary, or came out of a persisted
    /// region, is not a `Severity` until this says so.
    pub const fn from_u8(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Info),
            1 => Some(Self::Warn),
            2 => Some(Self::Error),
            3 => Some(Self::Alert),
            _ => None,
        }
    }

    /// The word a rendered line carries for it; `Info` carries none.
    pub const fn word(self) -> Option<&'static str> {
        match self {
            Self::Info => None,
            Self::Warn => Some("warn"),
            Self::Error => Some("error"),
            Self::Alert => Some("alert"),
        }
    }
}

/// Set when the record was written before the kernel knew a rate to read the
/// counter at, so its `at_ns` is no time and its line says so ([`UNTIMED`]).
pub const FLAG_UNTIMED: u8 = 1 << 0;

/// What a reader gets. Plain POD, `Copy`, no interior mutability.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
pub struct LogRecord {
    /// The record's identity. In the kernel's slot this same word is also the
    /// validity word; by the time a reader holds a copy it is just the sequence
    /// number, and it is what [`LogCursor::next`] counts in.
    pub seq: u64,
    /// Nanoseconds since the counter's zero: [`crate::clock::stamp_ns`]'s reading.
    pub at_ns: u64,
    pub pid: u32,
    pub tid: u32,
    pub cpu: u16,
    /// Message bytes present in `msg`, never above [`MAX_RECORD_MESSAGE`].
    pub len: u16,
    /// Bytes the message would have had past [`MAX_RECORD_MESSAGE`],
    /// saturating. **Never a silent truncation** — this is the difference
    /// between a bound and a lie.
    pub elided: u16,
    pub severity: u8,
    /// [`FLAG_UNTIMED`] and nothing else yet.
    pub flags: u8,
    pub msg: [u8; MAX_RECORD_MESSAGE],
}

const _: () = assert!(core::mem::size_of::<LogRecord>() == RECORD_BYTES);
const _: () = assert!(core::mem::align_of::<LogRecord>() == 64);
/// Every byte belongs to a field: this crosses the boundary through
/// [`LogRecord::as_bytes`], so a gap would publish whatever the kernel stack
/// held. Spelled as the sum of the field widths rather than as
/// [`RECORD_BYTES`], which is the *other* claim about this struct — a padded
/// layout that happened to reach 1024 bytes would satisfy that one.
const _: () = assert!(
    core::mem::size_of::<LogRecord>() == 8 + 8 + 4 + 4 + 2 + 2 + 2 + 1 + 1 + MAX_RECORD_MESSAGE
);
/// The kernel's slot is this layout with the first word made atomic, so the
/// body it copies is everything past that word and must start where it does.
const _: () = assert!(core::mem::offset_of!(LogRecord, at_ns) == core::mem::size_of::<u64>());

impl LogRecord {
    /// A record no shard ever wrote, for sizing a read buffer.
    ///
    /// Not `Default`, and the reason is the state this is indistinguishable
    /// from: an all-zero record is exactly a **zeroed slot** — a shard's `.bss`
    /// or `alloc_zeroed` storage that nothing has ever written. Sequence
    /// numbers start at *one* (`FIRST_SEQ`) precisely so that state can never be
    /// read as a record; a type whose `Default` produced it would hand that
    /// state back through the front door. The name says it is filler.
    pub const EMPTY: Self = Self {
        seq: 0,
        at_ns: 0,
        pid: 0,
        tid: 0,
        cpu: 0,
        len: 0,
        elided: 0,
        severity: Severity::Info as u8,
        flags: 0,
        msg: [0; MAX_RECORD_MESSAGE],
    };

    /// The message, as far as it is text.
    ///
    /// **A record crossed the syscall boundary, so its bytes are input.** `len`
    /// is clamped rather than trusted and a non-UTF-8 body answers with what
    /// decoded, because a diagnostic that refuses to render a corrupt record is
    /// a diagnostic that hides the corruption it was called to show.
    pub fn message(&self) -> &str {
        let len = (self.len as usize).min(MAX_RECORD_MESSAGE);
        let bytes = &self.msg[..len];
        match core::str::from_utf8(bytes) {
            Ok(text) => text,
            Err(e) => {
                // `from_utf8` guarantees the prefix before `valid_up_to` is
                // valid, so this cannot fail and is not an `expect` on input.
                core::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap_or("")
            }
        }
    }

    /// The record's own bytes, which is what goes on the wire.
    ///
    /// Here rather than at the kernel's copy-out, the shape every ABI struct in
    /// this crate has: the `unsafe` belongs beside the layout assertion that
    /// discharges it, not beside the caller that happens to need it.
    #[inline]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `self` is a valid `&Self` (non-null, aligned, readable for
        // `size_of::<Self>()` bytes), and the const assert above proves the
        // `repr(C)` layout has no padding, so every byte the slice exposes is
        // an initialized field, not a gap.
        unsafe {
            core::slice::from_raw_parts(self as *const Self as *const u8, core::mem::size_of::<Self>())
        }
    }

    pub fn severity(&self) -> Option<Severity> {
        Severity::from_u8(self.severity)
    }

    /// When the record was written, or `None` for a record written before the
    /// kernel knew a rate to read the counter at.
    pub fn at_ns(&self) -> Option<u64> {
        (self.flags & FLAG_UNTIMED == 0).then_some(self.at_ns)
    }

    /// The same line with the wall clock `wall` inside its head, as `/log`
    /// carries it: `[2026-10-04 09:30:00 11.665 cpu0 kernel] …`.
    pub fn dated<'a>(&'a self, wall: &'a str) -> Dated<'a> {
        Dated { record: self, wall }
    }

    fn fmt_dated(&self, f: &mut core::fmt::Formatter<'_>, wall: &str) -> core::fmt::Result {
        let head = Head {
            wall,
            at_ns: self.at_ns(),
            cpu: Some(u32::from(self.cpu)),
            who: KERNEL,
            severity: self.severity().unwrap_or(Severity::Info),
            tid: self.tid,
            pid: None,
        };
        write!(f, "{head} {}", self.message())?;
        if self.elided != 0 {
            write!(f, " …[{} bytes elided]", self.elided)?;
        }
        Ok(())
    }
}

/// What [`LogRecord::dated`] renders. `Display`, so a caller writes it
/// straight into its own line and nothing buffers a record to date it.
pub struct Dated<'a> {
    record: &'a LogRecord,
    wall: &'a str,
}

impl core::fmt::Display for Dated<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.record.fmt_dated(f, self.wall)
    }
}

/// A kernel record's line, so the kernel's console, the panel, the black box
/// and `logkeeper` produce byte-identical text: [`Head`]'s, with no wall clock.
impl core::fmt::Display for LogRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.fmt_dated(f, "")
    }
}

/// Who said a line, where that is not a program: the kernel's records and the
/// loader's lines. No program's name is either (`toyos_logstream::Tag`).
pub const KERNEL: &str = "kernel";
pub const LOADER: &str = "loader";

/// What a line's time reads when it was said before its sayer knew a rate to
/// read the counter at: the width of a time, and no number.
pub const UNTIMED: &str = "--.---";

/// The head every line of the log opens with, whoever said it — the loader,
/// the kernel or a program — and on every surface it reaches:
/// `[<wall> <secs>.<mmm> cpu<n> <who> <severity> tid=<n> pid=<n>]`, the time
/// first, then where it ran, then who said it.
///
/// - `<wall>` is the wall clock, which only `/log` carries;
/// - the time is seconds since the CPU's counter last started — power-on, or
///   the reset since — read at the rate the machine states or measured
///   (`bootloader/src/stamp.rs`, `kernel/src/clock.rs`), its seconds right
///   aligned in two columns; [`UNTIMED`] where no rate was known yet;
/// - `cpu<n>` where the sayer knows its CPU;
/// - the severity's [`word`](Severity::word) above `Info`, the thread where it
///   is not zero, and the process where the line's ring is not its own.
///
/// No newline, and no space after the bracket: the caller writes the text.
pub struct Head<'a> {
    /// Empty for none.
    pub wall: &'a str,
    pub at_ns: Option<u64>,
    pub cpu: Option<u32>,
    /// [`KERNEL`], [`LOADER`] or a program's name.
    pub who: &'a str,
    pub severity: Severity,
    /// Zero for none.
    pub tid: u32,
    pub pid: Option<u32>,
}

impl core::fmt::Display for Head<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("[")?;
        if !self.wall.is_empty() {
            write!(f, "{} ", self.wall)?;
        }
        match self.at_ns {
            Some(ns) => write!(f, "{:>2}.{:03}", ns / 1_000_000_000, ns % 1_000_000_000 / 1_000_000)?,
            None => f.write_str(UNTIMED)?,
        }
        if let Some(cpu) = self.cpu {
            write!(f, " cpu{cpu}")?;
        }
        write!(f, " {}", self.who)?;
        if let Some(word) = self.severity.word() {
            write!(f, " {word}")?;
        }
        if self.tid != 0 {
            write!(f, " tid={}", self.tid)?;
        }
        if let Some(pid) = self.pid {
            write!(f, " pid={pid}")?;
        }
        f.write_str("]")
    }
}

/// Per-reader state. **The kernel holds none.**
///
/// No object, no handle lifecycle, no cursor to leak or go stale, and a second
/// reader costs nothing. The stream is not consumed either: `logkeeper` and a
/// `log-follow` tool coexist with no coordination.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LogCursor {
    /// Out: how many shards the machine has. A caller passes a zeroed cursor
    /// the first time and reads it back.
    pub shards: u32,
    pub _pad: u32,
    /// Out: records this read skipped because they were overwritten. The
    /// kernel never reads it, so a reader's total is the reader's own sum.
    ///
    /// **Derived, never counted by a producer.** The kernel computes it from
    /// `head` and `next`, which both have to be right anyway, so no counter can
    /// drift from the ring. It lives here so a reader that ignores loss has to
    /// actively ignore a field it is already passing.
    pub lost: u64,
    /// In/out: the next sequence number wanted from each shard. A number past
    /// the one the shard issues next is refused, and a shard not yet published
    /// issues 1 next.
    pub next: [u64; MAX_LOG_SHARDS],
}

const _: () = assert!(core::mem::size_of::<LogCursor>() == 16 + 8 * MAX_LOG_SHARDS);

impl LogCursor {
    /// A cursor that has read nothing. The kernel fills `shards` on the first
    /// call.
    pub const fn new() -> Self {
        Self { shards: 0, _pad: 0, lost: 0, next: [0; MAX_LOG_SHARDS] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    /// The layout the kernel's slot is the same eight bytes of, and the one a
    /// persisted region is a byte-for-byte array of. A change here is a change
    /// to both and the `const` assertions above are what say so.
    #[test]
    fn the_record_is_the_size_and_shape_both_sides_assume() {
        assert_eq!(core::mem::size_of::<LogRecord>(), RECORD_BYTES);
        assert_eq!(core::mem::align_of::<LogRecord>(), 64);
        assert_eq!(core::mem::offset_of!(LogRecord, seq), 0);
        assert_eq!(core::mem::offset_of!(LogRecord, at_ns), 8);
        assert_eq!(core::mem::size_of::<LogCursor>(), 80);
    }

    /// **The encoder is the wire, so the test decodes the wire.**
    ///
    /// Not `as_bytes().len() == RECORD_BYTES`, which a padded struct passes:
    /// every field is read back out of the slice at the offset `#[repr(C)]`
    /// puts it at, and the tail is the message. A gap anywhere before `msg`
    /// shifts one of these and the assertion that catches it is the one whose
    /// field moved.
    #[test]
    fn as_bytes_is_the_fields_and_nothing_between_them() {
        let r = record("hello");
        let b = r.as_bytes();
        assert_eq!(b.len(), RECORD_BYTES);
        assert_eq!(u64::from_ne_bytes(b[0..8].try_into().unwrap()), 7);
        assert_eq!(u64::from_ne_bytes(b[8..16].try_into().unwrap()), 1_234_567_890);
        assert_eq!(u32::from_ne_bytes(b[16..20].try_into().unwrap()), 3);
        assert_eq!(u32::from_ne_bytes(b[20..24].try_into().unwrap()), 4);
        assert_eq!(u16::from_ne_bytes(b[24..26].try_into().unwrap()), 2);
        assert_eq!(u16::from_ne_bytes(b[26..28].try_into().unwrap()), 5);
        assert_eq!(u16::from_ne_bytes(b[28..30].try_into().unwrap()), 0);
        assert_eq!(b[30], Severity::Info as u8);
        assert_eq!(b[31], 0);
        assert_eq!(&b[32..37], b"hello");
        assert!(b[37..].iter().all(|&x| x == 0));
    }

    fn record(msg: &str) -> LogRecord {
        let mut r = LogRecord {
            seq: 7,
            at_ns: 1_234_567_890,
            pid: 3,
            tid: 4,
            cpu: 2,
            len: msg.len() as u16,
            elided: 0,
            severity: Severity::Info as u8,
            flags: 0,
            msg: [0; MAX_RECORD_MESSAGE],
        };
        r.msg[..msg.len()].copy_from_slice(msg.as_bytes());
        r
    }

    #[test]
    fn a_record_renders_the_same_line_for_every_consumer() {
        assert_eq!(record("hello").to_string(), "[ 1.234 cpu2 kernel tid=4] hello");
    }

    /// The wall clock lands *inside* the bracket, first, and an empty one leaves
    /// the line byte for byte what `Display` writes — which is what makes
    /// `/log`'s writer this formatter's caller rather than a second
    /// implementation of it.
    #[test]
    fn a_wall_clock_goes_through_the_bracket_and_an_empty_one_changes_nothing() {
        let r = record("hello");
        assert_eq!(r.dated("2026-10-04 09:30:00").to_string(), "[2026-10-04 09:30:00  1.234 cpu2 kernel tid=4] hello");
        assert_eq!(r.dated("").to_string(), r.to_string());
    }

    /// The decorations, each of which a consumer would otherwise invent: no
    /// time where none was known, the severity, and the cut.
    #[test]
    fn untimed_severity_and_elided_are_in_the_line_rather_than_in_a_convention() {
        let mut r = record("x");
        r.flags = FLAG_UNTIMED;
        r.tid = 0;
        assert_eq!(r.to_string(), "[--.--- cpu2 kernel] x");
        assert_eq!(r.at_ns(), None);

        let mut r = record("x");
        r.severity = Severity::Alert as u8;
        assert_eq!(r.to_string(), "[ 1.234 cpu2 kernel alert tid=4] x");

        let mut r = record("x");
        r.elided = 900;
        assert_eq!(r.to_string(), "[ 1.234 cpu2 kernel tid=4] x …[900 bytes elided]");
    }

    /// Time first, then where, then who, for every sayer: a program's line
    /// names no CPU, and seconds past two columns widen the head.
    #[test]
    fn every_sayer_gets_the_one_head() {
        let head = |at_ns, cpu, who, severity, tid, pid| {
            Head { wall: "", at_ns, cpu, who, severity, tid, pid }.to_string()
        };
        assert_eq!(head(Some(9_876_000_000), Some(0), LOADER, Severity::Info, 0, None), "[ 9.876 cpu0 loader]");
        assert_eq!(head(None, Some(0), LOADER, Severity::Info, 0, None), "[--.--- cpu0 loader]");
        assert_eq!(head(Some(13_064_999_999), None, "supervisor", Severity::Info, 0, None), "[13.064 supervisor]");
        assert_eq!(
            head(Some(123_000_000_000), None, "soundserver", Severity::Warn, 2, Some(9)),
            "[123.000 soundserver warn tid=2 pid=9]"
        );
    }

    /// **`len` came across the syscall boundary**, so a record claiming more
    /// message than a record can hold answers with what it has rather than
    /// panicking a reader. `logkeeper` is userland and this is its input too.
    #[test]
    fn a_length_past_the_bound_is_clamped_and_not_a_panic() {
        let mut r = record("abc");
        r.len = u16::MAX;
        assert_eq!(r.message().len(), MAX_RECORD_MESSAGE);
    }

    /// A corrupt tail must not hide the readable head: a diagnostic that
    /// refuses to render a broken record hides the breakage it exists to show.
    #[test]
    fn a_non_utf8_body_renders_what_decoded() {
        let mut r = record("ok");
        r.msg[2] = 0xff;
        r.len = 3;
        assert_eq!(r.message(), "ok");
    }

    /// A `u8` from the wire is not a `Severity` until [`Severity::from_u8`] says so —
    /// there is no `unsafe` transmute anywhere on this path.
    #[test]
    fn an_undeclared_severity_byte_decodes_to_nothing() {
        assert_eq!(Severity::from_u8(3), Some(Severity::Alert));
        assert_eq!(Severity::from_u8(4), None);
        assert_eq!(record("x").severity(), Some(Severity::Info));
    }

    /// The ladder is ordered, which is what a floor or a durability trigger
    /// compares against.
    #[test]
    fn severities_are_ordered_from_info_to_alert() {
        assert!(Severity::Info < Severity::Warn);
        assert!(Severity::Warn < Severity::Error);
        assert!(Severity::Error < Severity::Alert);
        assert_eq!(Severity::Info.word(), None);
        assert_eq!(Severity::Alert.word(), Some("alert"));
    }

    #[test]
    fn a_fresh_cursor_has_read_nothing() {
        assert_eq!(LogCursor::new(), LogCursor::default());
        assert_eq!(LogCursor::new().next, [0; MAX_LOG_SHARDS]);
    }
}
