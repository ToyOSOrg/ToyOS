//! A program's console lines, as records.
//!
//! Every unit a console object emits — a whole line, or the piece of one it had
//! to let go — is also one record in the ring, so the line reaches `/log`, the
//! log stream and the panel on a machine whose serial console reaches nothing.
//! The serial console keeps the raw line it always had; the drain skips the
//! record ([`Origin::Spoken`](super::Origin::Spoken)).
//!
//! **A program's record opens with the form only the kernel writes for one**
//! (`toyos_elide::spoken`'s `Head`): the sigil, the name its holder was spawned
//! under with every byte that could read as structure replaced, and `: `. The
//! kernel's own records never open with the sigil (`log::commit`).
//!
//! **A program's share of the ring is bounded** ([`PROGRAM_BURST`],
//! [`PROGRAM_PER_SEC`]), so no console holder can lap the kernel's own records
//! out of a shard before `logd` reads them. A line past the share reaches the
//! serial console and is counted; the count is recorded before the next line
//! that is, and when the console goes away.

use toyos_elide::spoken::{said, Head, Line, Share, Verdict};

use crate::process::THREAD_NAME_LEN;

use super::shard::SHARD_RECORDS;

/// At most a quarter of one CPU's shard: a program that spends its whole burst
/// at once leaves three quarters of the shard it runs on to the kernel's records.
const PROGRAM_BURST: u64 = 128;
const _: () = assert!(PROGRAM_BURST * 4 <= SHARD_RECORDS as u64);

/// Slow enough that one program needs ten seconds or more to write the rest of
/// a shard after its burst, orders of magnitude past how long `logd` waits
/// between reads when nothing wakes it (its `IDLE_NANOS`). Per console holder,
/// and each spawn mints a holder with a full burst.
const PROGRAM_PER_SEC: u64 = 16;
const _: () = assert!((SHARD_RECORDS as u64 - PROGRAM_BURST) / PROGRAM_PER_SEC >= 10);

/// One console holder: the name it speaks under and what is left of its share.
pub struct Speaker {
    name: [u8; THREAD_NAME_LEN],
    share: Share<PROGRAM_BURST, PROGRAM_PER_SEC>,
}

impl Speaker {
    /// `name` is the process table's name for the holder, the program's file name.
    pub fn new(name: [u8; THREAD_NAME_LEN]) -> Self {
        Self { name, share: Share::new(crate::clock::nanos_since_boot()) }
    }

    fn name(&self) -> &[u8] {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(THREAD_NAME_LEN);
        &self.name[..len]
    }

    /// One unit the console emitted, its newline included when it had one.
    pub fn say(&mut self, unit: &[u8]) {
        let line = unit.strip_suffix(b"\n").unwrap_or(unit);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Verdict::Record { withheld } = self.share.admit(crate::clock::nanos_since_boot())
        else {
            return;
        };
        if withheld > 0 {
            self.withheld(withheld);
        }
        let name = self.name();
        super::emit_spoken(format_args!("{}{}", Head(name), Line(said(name, line))));
    }

    /// The console is going away: what it withheld has no later line to ride on.
    pub fn finish(&mut self) {
        let withheld = self.share.take_withheld();
        if withheld > 0 {
            self.withheld(withheld);
        }
    }

    fn withheld(&self, count: u64) {
        super::emit_spoken(format_args!(
            "{}...[{count} line(s) past this program's share of the log were not recorded]",
            Head(self.name())
        ));
    }
}
