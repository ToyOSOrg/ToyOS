//! The record stream: where a boot's log goes besides the file, said once for
//! everyone who has to spell it, and the one decision a peer that will not take
//! it forces.
//!
//! `/system/bin/logd` owns every policy about where records go. The file is the
//! sink of record and this is the second sink: the same text line, in the same
//! order, over a TCP connection netd opens for it, the instant the file gets
//! it.
//!
//! Nothing here can lose a record from the file. [`Backlog::round`] offers a
//! line and never waits: a peer that stops taking bytes fills the queue, and
//! the lines that do not fit are refused, counted, and reported in one line
//! that goes into the file like any other record. A drop nobody can count is
//! the failure this type exists to make impossible, so the refusal and the
//! counter are one statement.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use toyos_abi::log::MAX_RECORD_MESSAGE;

/// The boot parameter carrying the listener's address, with the address after
/// it — `logstream=10.0.2.2:41337`.
///
/// A *valued* parameter, like `blackbox=`: the kernel matches it with
/// `starts_with` rather than whole, so it is not in `kernel/src/params.rs`'s
/// `PARAMS` table and is cleared by name in `src/build.rs`'s `VALUED_PARAMS`
/// instead.
pub const PARAM: &str = "logstream=";

/// Where the kernel puts [`PARAM`]'s value for userland to find.
///
/// The kernel command line reaches no process, and the kernel already builds
/// `/system/bin/init`'s environment. `init` passes its own environment on to
/// the daemons it starts at boot and clears it for anything the launcher
/// starts, so `logd` reads this and a program a user runs does not.
pub const ENV: &str = "TOYOS_LOG_STREAM";

/// What the kernel copies [`PARAM`]'s value into.
///
/// The parameter line lives in memory the allocator may hand out, so the value
/// is copied out of it before `mm::init` runs and there is no heap to copy it
/// into. The widest address this can carry is `255.255.255.255:65535`, which is
/// 21 bytes.
pub const MAX_VALUE_BYTES: usize = 64;

/// The value of [`PARAM`] on a boot parameter line, or `None` when the line
/// does not carry it.
///
/// The line is comma-separated, as `toyos_abi::boot::actuators` reads it.
pub fn value_in(cmdline: &str) -> Option<&str> {
    cmdline.split(',').find_map(|token| token.strip_prefix(PARAM))
}

/// Why an address the machine was handed is not one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Malformed {
    /// No `:` at all, so nothing says which port.
    NoPort,
    /// The part before the `:` is not four decimal octets.
    NotAnAddress,
    /// The part after the `:` is not a port, or is zero — which names no
    /// listener on any stack.
    NotAPort,
}

impl Malformed {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoPort => "it names no port",
            Self::NotAnAddress => "the part before the colon is not four decimal octets",
            Self::NotAPort => "the part after the colon is not a port between 1 and 65535",
        }
    }
}

/// `a.b.c.d:port`, refused by name.
///
/// A boot whose address is a typo has to say so rather than stream to whatever
/// the typo parsed as.
pub fn endpoint(value: &str) -> Result<([u8; 4], u16), Malformed> {
    let (host, port) = value.rsplit_once(':').ok_or(Malformed::NoPort)?;
    let mut octets = [0u8; 4];
    let mut seen = 0usize;
    for (slot, text) in host.split('.').enumerate() {
        let octet = octets.get_mut(slot).ok_or(Malformed::NotAnAddress)?;
        // Digits asked for before `parse` is: `u8::from_str` accepts a leading
        // `+`, so `10.0.2.+2` would otherwise be this machine's own address
        // spelled a way no writer of it meant.
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Malformed::NotAnAddress);
        }
        *octet = text.parse::<u8>().map_err(|_| Malformed::NotAnAddress)?;
        seen = slot + 1;
    }
    if seen != 4 {
        return Err(Malformed::NotAnAddress);
    }
    if !port.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Malformed::NotAPort);
    }
    let port: u16 = port.parse().map_err(|_| Malformed::NotAPort)?;
    if port == 0 {
        return Err(Malformed::NotAPort);
    }
    Ok((octets, port))
}

/// Records `logd` asks `SYS_LOG_READ` for at once, which is also the most it
/// can offer this queue between two rounds of its own loop.
///
/// Above `MAX_LOG_SHARDS`, which the call refuses below, and large enough that
/// an ordinary boot's burst is a handful of syscalls rather than one per line.
pub const BATCH: usize = 64;

/// What `toyos_abi::log::Tagged` renders around a record's message — the
/// brackets, the wall-clock stamp `logd` tags it with, the monotonic
/// `{secs}.{mmm} cpuN`, and the `boot`, `tid=` and elided-byte fields a record
/// may carry — plus the newline `logd` ends the line with.
///
/// Above every one of those at its widest, which is a claim about what `Tagged`
/// prints and is checked by rendering it: a number short here is a bound that
/// does not hold what it says it holds, and a listener losing lines on a round
/// nobody thought could overflow.
const AROUND_A_MESSAGE: usize = 128;

/// The widest line one record renders to.
const WIDEST_LINE: usize = MAX_RECORD_MESSAGE + AROUND_A_MESSAGE;

/// What the queue may hold before a line is refused rather than waited for:
/// one whole `SYS_LOG_READ` batch at its widest, so a listener that misses one
/// round of `logd`'s loop loses nothing.
///
/// It is deliberately the *small* buffer in the chain. Everything downstream —
/// the pipe netd reads, netd's own send buffer, the peer's receive window —
/// absorbs a stall before this is reached at all, so a line that reaches this
/// bound is a peer that is gone rather than one that is behind.
pub const MAX_BACKLOG_BYTES: usize = BATCH * WIDEST_LINE;

/// Whether this round may put a line in the log about what the queue refused.
///
/// `logd` holds the clock and decides how often a run of loss is worth a line;
/// what that line may not be is decided here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Due {
    Now,
    NotYet,
}

/// The lines waiting for a listener that is slower than the machine.
///
/// **Bounded, and its refusals are counted.** The alternative — waiting for the
/// socket — puts a listener on the far side of a cable between `logd` and the
/// file it owns, which is the one thing the stream may never cost.
///
/// Ordering is FIFO and drops are at the tail: nothing is ever reordered,
/// duplicated or evicted after it was taken, so what a listener received is
/// always the file's own lines in the file's own order. Evicting the head
/// instead would keep the end of a boot at the price of making the two
/// readings incomparable.
#[derive(Debug, Default)]
pub struct Backlog {
    lines: VecDeque<String>,
    bytes: usize,
    /// Lines this queue refused, for the life of the process.
    dropped: u64,
    /// How much of [`Self::dropped`] has been said out loud.
    reported: u64,
}

impl Backlog {
    pub const fn new() -> Self {
        Self { lines: VecDeque::new(), bytes: 0, dropped: 0, reported: 0 }
    }

    /// One round: the lines the file has just taken, offered in order, and the
    /// one line the file owes about what this queue refused.
    ///
    /// **The report is returned and never offered.** A report handed to a queue
    /// that is refusing is refused too, which is a drop, which owes another
    /// report — a run that never ends and a log that fills with lines about
    /// itself. This is the only function that both offers lines and produces
    /// the report, so it is the only place that mistake can be made.
    pub fn round<'a>(
        &mut self,
        wrote: impl IntoIterator<Item = &'a str>,
        due: Due,
    ) -> Option<String> {
        for line in wrote {
            self.admit(line);
        }
        match due {
            Due::Now => self.report(),
            Due::NotYet => None,
        }
    }

    /// Offer one line, answering whether the queue took it.
    ///
    /// `false` is a drop and is counted; there is no third answer, and no
    /// answer that waits.
    fn admit(&mut self, line: &str) -> bool {
        if self.bytes + line.len() > MAX_BACKLOG_BYTES {
            self.dropped += 1;
            return false;
        }
        self.bytes += line.len();
        self.lines.push_back(line.to_string());
        true
    }

    /// Everything waiting, oldest first, leaving the queue empty.
    ///
    /// The writer takes the whole queue in one step so it holds no lock while
    /// it writes: a `logd` blocked behind its own stream thread would be the
    /// defect this type exists to prevent, one level in.
    pub fn drain(&mut self) -> Vec<String> {
        self.bytes = 0;
        self.lines.drain(..).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The one line that says what the stream lost, or `None` when it has lost
    /// nothing since it last said so.
    fn report(&mut self) -> Option<String> {
        let unsaid = self.dropped - self.reported;
        if unsaid == 0 {
            return None;
        }
        self.reported = self.dropped;
        // **Two numbers, and they are checkable against each other**: every
        // line's first number is what this run of loss added, and the second is
        // the boot's running total, so a reader that adds the first numbers up
        // must arrive at the last line's second one.
        Some(alloc::format!(
            "logd: {unsaid} record(s) never reached the log stream, and {} in this boot; \
             /log has every one of them",
            self.dropped
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A round that offers the queue no line, for the reports.
    const NOTHING: [&str; 0] = [];

    #[test]
    fn the_value_is_read_off_a_line_that_carries_other_parameters() {
        assert_eq!(value_in("logstream=10.0.2.2:41337"), Some("10.0.2.2:41337"));
        assert_eq!(
            value_in("root=1234,blackbox=0x1000,logstream=10.0.2.2:1,watchdog"),
            Some("10.0.2.2:1")
        );
        assert_eq!(value_in("root=1234,watchdog"), None);
        assert_eq!(value_in(""), None);
        // The name whole, not a prefix of another token.
        assert_eq!(value_in("mylogstream=1.2.3.4:5"), None);
        // Named and empty is not an address; `endpoint` is what refuses it.
        assert_eq!(value_in("logstream="), Some(""));
    }

    #[test]
    fn an_address_is_four_octets_and_a_port() {
        assert_eq!(endpoint("10.0.2.2:41337"), Ok(([10, 0, 2, 2], 41337)));
        assert_eq!(endpoint("255.255.255.255:65535"), Ok(([255, 255, 255, 255], 65535)));
        assert_eq!(endpoint("192.168.1.10:22"), Ok(([192, 168, 1, 10], 22)));
    }

    /// Every way a typo reaches this function, refused by name rather than
    /// parsed into some other machine's address.
    #[test]
    fn a_typo_is_refused_and_says_which_kind_it_is() {
        assert_eq!(endpoint(""), Err(Malformed::NoPort));
        assert_eq!(endpoint("10.0.2.2"), Err(Malformed::NoPort));
        assert_eq!(endpoint("10.0.2:22"), Err(Malformed::NotAnAddress));
        assert_eq!(endpoint("10.0.2.2.2:22"), Err(Malformed::NotAnAddress));
        assert_eq!(endpoint("10.0.2.256:22"), Err(Malformed::NotAnAddress));
        assert_eq!(endpoint("10.0.2.+2:22"), Err(Malformed::NotAnAddress));
        assert_eq!(endpoint("10.0..2:22"), Err(Malformed::NotAnAddress));
        assert_eq!(endpoint("t14:22"), Err(Malformed::NotAnAddress));
        assert_eq!(endpoint("10.0.2.2:"), Err(Malformed::NotAPort));
        assert_eq!(endpoint("10.0.2.2:65536"), Err(Malformed::NotAPort));
        assert_eq!(endpoint("10.0.2.2:0"), Err(Malformed::NotAPort));
        assert_eq!(endpoint("10.0.2.2:http"), Err(Malformed::NotAPort));
        assert_eq!(endpoint("10.0.2.2:+22"), Err(Malformed::NotAPort));
        // Widest form, so the kernel's copy buffer is not the thing that refuses one.
        assert!("255.255.255.255:65535".len() < MAX_VALUE_BYTES);
        // Each kind says a different thing, so a boot's log names which typo it was.
        let words = [Malformed::NoPort, Malformed::NotAnAddress, Malformed::NotAPort]
            .map(Malformed::as_str);
        for (i, word) in words.iter().enumerate() {
            assert!(!word.is_empty());
            assert!(!words[..i].contains(word), "{word:?} is said by two kinds");
        }
    }

    #[test]
    fn a_queue_that_is_read_keeps_every_line_in_order() {
        let mut q = Backlog::new();
        for i in 0..1000 {
            let line = std::format!("line {i}\n");
            assert_eq!(q.round([line.as_str()], Due::Now), None);
            assert_eq!(q.drain(), std::vec![line]);
        }
        assert!(q.is_empty());
    }

    /// **The accounting, which is the whole of what a stalled peer costs.**
    /// A queue that stops taking lines and does not count them is the failure
    /// no boot can see: the file is whole, the stream is short, and nothing
    /// says by how much.
    #[test]
    fn a_starved_queue_drops_the_newest_and_counts_every_one() {
        let line = "x".repeat(1024);
        let mut q = Backlog::new();
        let mut admitted = 0u64;
        while q.admit(&line) {
            admitted += 1;
            assert!(admitted < 1_000, "the bound never refused a line");
        }
        for _ in 0..99 {
            assert!(!q.admit(&line));
        }

        let said = q.round(NOTHING, Due::Now).expect("a queue that dropped says so");
        assert!(said.contains("100 record(s) never reached"), "{said}");
        // One line per episode: nothing new to say until something else drops.
        assert_eq!(q.round(NOTHING, Due::Now), None);
        let again = q.round([line.as_str()], Due::Now).expect("a second episode says so too");
        assert!(again.contains("1 record(s) never reached"), "{again}");
        assert!(again.contains("and 101 in this boot"), "{again}");
        // And a round that is not due says nothing however much it refused.
        assert_eq!(q.round([line.as_str()], Due::NotYet), None);

        // What it did take is a prefix of what it was offered, in order.
        let kept = q.drain();
        assert_eq!(kept.len() as u64, admitted);
        assert!(kept.iter().all(|k| *k == line));
        assert!(q.is_empty());
    }

    /// **A report that is offered back is a run that never ends.** Written into
    /// the file *and* handed to a queue that is refusing, the report is itself
    /// refused; that drop owes another report, and the next round owes another,
    /// for the life of the boot. So a queue nothing new is offered goes quiet.
    #[test]
    fn a_drop_report_is_never_a_line_the_queue_is_offered() {
        let line = "y".repeat(1024);
        let mut q = Backlog::new();
        while q.admit(&line) {}
        let said = q.round(NOTHING, Due::Now).expect("a starved queue says so");
        assert!(said.contains("never reached the log stream"), "{said}");
        assert_eq!(
            q.round(NOTHING, Due::Now),
            None,
            "a queue offered nothing new still owes a report, so it reported its own report"
        );
    }

    /// A line wider than the whole queue is refused rather than admitted into a
    /// queue it does not fit — the bound is on the bytes, not on the count.
    #[test]
    fn one_impossible_line_does_not_evict_the_boot() {
        let mut q = Backlog::new();
        assert!(q.admit("first\n"));
        assert!(!q.admit(&"y".repeat(MAX_BACKLOG_BYTES + 1)));
        assert!(q.round(NOTHING, Due::Now).expect("a refusal is counted").contains("1 record(s)"));
        assert_eq!(q.drain(), std::vec!["first\n"]);
        assert!(q.is_empty());
        // And the bytes came back with it: the queue takes lines again.
        assert!(q.admit(&"z".repeat(MAX_BACKLOG_BYTES)));
    }

    #[test]
    fn draining_gives_the_bytes_back() {
        let mut q = Backlog::new();
        for _ in 0..64 {
            assert!(q.admit(&"a".repeat(1000)));
        }
        assert_eq!(q.drain().len(), 64);
        // The whole bound is available again, which a `bytes` that only ever
        // grew would refuse.
        assert!(q.admit(&"b".repeat(MAX_BACKLOG_BYTES)));
        assert_eq!(q.round(NOTHING, Due::Now), None);
    }

    /// The widest line `logd` can put in front of this queue: every field
    /// `toyos_abi::log::Tagged` renders at the maximum its type allows, tagged
    /// with the widest stamp, ended with the newline `logd` writes.
    fn widest_line() -> String {
        let mut record = toyos_abi::log::LogRecord::EMPTY;
        record.at_ns = u64::MAX;
        record.tid = u32::MAX;
        record.cpu = u16::MAX;
        record.elided = u16::MAX;
        record.len = MAX_RECORD_MESSAGE as u16;
        record.msg = [b'm'; MAX_RECORD_MESSAGE];
        record.flags = toyos_abi::log::FLAG_EARLY;
        // The stamp `logd` tags a record with: `Civil`'s `YYYY-MM-DD HH:MM:SS`,
        // or the same width in dashes on a boot with no clock.
        alloc::format!("{}\n", record.tagged("9999-12-31 23:59:59"))
    }

    /// **The bound holds one whole batch of the widest lines a record renders
    /// to**, which is what makes "a listener that misses one round loses
    /// nothing" arithmetic rather than a hope.
    ///
    /// The width is rendered rather than assumed: the claim
    /// [`AROUND_A_MESSAGE`] makes is about what `Tagged` prints, so a number
    /// short of it fails here and not on a boot.
    #[test]
    fn the_bound_holds_a_whole_batch_of_the_widest_lines_a_record_renders_to() {
        let line = widest_line();
        assert!(
            line.len() <= WIDEST_LINE,
            "a record renders to {} bytes and the bound allows {WIDEST_LINE}",
            line.len()
        );
        let mut q = Backlog::new();
        for _ in 0..BATCH {
            assert!(q.admit(&line), "the bound refused a line inside one batch");
        }
        assert_eq!(q.round(NOTHING, Due::Now), None, "a whole batch was refused a line");
    }

    /// The name and the environment entry are one statement about one machine:
    /// a parameter that is not `name=` cannot carry a value, and an environment
    /// key with an `=` in it splits in the wrong place.
    #[test]
    fn the_two_spellings_are_shaped_the_way_their_readers_read_them() {
        assert!(PARAM.ends_with('='));
        assert!(!ENV.contains('='));
        assert!(!ENV.contains('\0'));
    }
}
