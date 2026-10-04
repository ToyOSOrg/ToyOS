//! The calendar.
//!
//! Nothing here allocates, nothing here is `unsafe`, and nothing here reads a
//! device: it is arithmetic over numbers its callers hand it.

#![no_std]

use core::fmt;

/// Seconds in a day.
const DAY: u64 = 86_400;

/// A wall-clock instant in the fields a human reads.
///
/// The one calendar in the tree. The RTC decodes its registers into this, the
/// log's file names come out of it, `SYS_CLOCK_REALTIME` answers out of it and
/// FAT's directory stamps are built from it — so there is one conversion
/// between seconds and dates rather than one per caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    pub year: u64,
    pub month: u64,
    pub day: u64,
    pub hour: u64,
    pub min: u64,
    pub sec: u64,
}

impl fmt::Display for Civil {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02} {}", self.year, self.month, self.day, self.time_of_day())
    }
}

impl Civil {
    /// Whether these fields name a day that exists, at a time that exists.
    ///
    /// The Unix epoch is the floor because everything downstream counts
    /// unsigned seconds from it. A leap second lands on 60 and is rejected: the
    /// RTC does not report one, and a clock that does is not one this kernel
    /// understands.
    pub fn is_valid(&self) -> bool {
        (1970..=9999).contains(&self.year)
            && (1..=12).contains(&self.month)
            && (1..=days_in_month(self.year, self.month)).contains(&self.day)
            && self.hour < 24
            && self.min < 60
            && self.sec < 60
    }

    /// Seconds from the Unix epoch to this instant, reading it in the same zone
    /// the epoch is in.
    ///
    /// **Total, on every field combination, including ones no calendar has.**
    /// [`days_from_civil`] saturates rather than checking, so a month of 0 or
    /// 13..=15 and a day of 0 — which is what a hostile or never-initialised
    /// FAT directory entry decodes to — read as the day before the first of the
    /// following month instead of refusing. That is the property `toyos-fat32`
    /// needs: a timestamp is not load-bearing enough to fail a volume read
    /// over, and its `every_bit_pattern_decodes` asserts it over all 65,536
    /// date encodings. [`Self::is_valid`] is the *other* caller's answer — the
    /// RTC's — and refusing there is what keeps an impossible instant out of
    /// the wall clock.
    pub fn to_unix_secs(&self) -> u64 {
        days_from_civil(self.year, self.month, self.day) * DAY
            + self.hour * 3_600
            + self.min * 60
            + self.sec
    }

    pub fn from_unix_secs(secs: u64) -> Civil {
        let (year, month, day) = civil_from_days(secs / DAY);
        let rem = secs % DAY;
        Civil { year, month, day, hour: rem / 3_600, min: rem % 3_600 / 60, sec: rem % 60 }
    }

    /// `YYYY-MM-DD-HHMMSS`, the stem one boot's log files are named for.
    ///
    /// A `Display` adapter and not a `String`, so this crate allocates nothing
    /// and the kernel can use it from a context that must not.
    pub fn stem(&self) -> Stem {
        Stem(*self)
    }

    /// `HH:MM:SS`, the stamp a line the build system prints opens with.
    pub fn time_of_day(&self) -> TimeOfDay {
        TimeOfDay(*self)
    }
}

/// [`Civil::stem`]'s rendering. Sortable by name, which is what makes `/log`
/// sort into the order the boots happened in.
pub struct Stem(Civil);

impl fmt::Display for Stem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.0;
        write!(
            f,
            "{:04}-{:02}-{:02}-{:02}{:02}{:02}",
            t.year, t.month, t.day, t.hour, t.min, t.sec
        )
    }
}

/// [`Civil::time_of_day`]'s rendering.
pub struct TimeOfDay(Civil);

impl fmt::Display for TimeOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let t = self.0;
        write!(f, "{:02}:{:02}:{:02}", t.hour, t.min, t.sec)
    }
}

/// The shape [`Stem`] renders, as the character classes a name must match to be
/// one of the log's own files: `d` is a digit and every other byte is itself.
///
/// One declaration, so the writer and the sweeper cannot disagree about what a
/// dated name looks like.
pub const STEM_SHAPE: &[u8] = b"dddd-dd-dd-dddddd";

/// The shape [`TimeOfDay`] renders, in [`STEM_SHAPE`]'s classes.
const TIME_OF_DAY_SHAPE: &[u8] = b"dd:dd:dd";

/// Whether `text` is exactly what a rendering of `shape` looks like.
fn is_shaped(text: &str, shape: &[u8]) -> bool {
    text.len() == shape.len()
        && text.bytes().zip(shape).all(|(b, want)| match want {
            b'd' => b.is_ascii_digit(),
            c => b == *c,
        })
}

/// Whether `stem` is exactly what [`Stem`] would have rendered.
pub fn is_stem(stem: &str) -> bool {
    is_shaped(stem, STEM_SHAPE)
}

/// What follows the [`TimeOfDay`] `line` opens with, or `None` where it opens
/// with none.
pub fn after_time_of_day(line: &str) -> Option<&str> {
    let (head, rest) = line.split_at_checked(TIME_OF_DAY_SHAPE.len())?;
    is_shaped(head, TIME_OF_DAY_SHAPE).then_some(rest)
}

/// The name a boot gets when the machine would not say what time it is.
///
/// A word and not a zero date: `0000-00-00-000000.log` sorts correctly and
/// reads as a real timestamp that happens to be absurd, and the difference
/// matters to whoever finds it on a stick six months from now.
pub const UNDATED_STEM: &str = "unknown";

/// Where a file sits in the order retention deletes in: lower goes first.
///
/// Undated boots go before dated ones because they cannot be ordered against
/// them — there is no clock to compare — and of the two kinds, the one that can
/// be placed in time is the one worth keeping. Within a kind the name is the
/// order, which is what the timestamp format is for.
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Class {
    Undated,
    Dated,
}

/// Whether `name` on the log volume is one of `logkeeper`'s files, and which kind.
///
/// An allow-list, and the strictness is the safety property in both
/// directions: `logkeeper` deletes nothing this does not recognise, and a host
/// reading the volume for a boot's log reads nothing else — the bootloader's
/// own `loader.log` is not one of these.
pub fn classify(name: &str) -> Option<Class> {
    let stem = name.strip_suffix(".log")?;
    let stem = match stem.split_once('_') {
        Some((head, part)) => {
            if part.len() != 4 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            head
        }
        None => stem,
    };

    if let Some(index) = stem.strip_prefix(UNDATED_STEM).and_then(|s| s.strip_prefix('-')) {
        let ours = index.len() == 2 && index.bytes().all(|b| b.is_ascii_digit());
        return ours.then_some(Class::Undated);
    }
    is_stem(stem).then_some(Class::Dated)
}

fn is_leap(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u64, month: u64) -> u64 {
    const LENGTHS: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    match month {
        2 if is_leap(year) => 29,
        1..=12 => LENGTHS[month as usize - 1],
        _ => 0,
    }
}

/// Days from 1970-01-01 to this date. Hinnant's algorithm, restricted to the
/// non-negative half — the epoch is [`Civil::is_valid`]'s floor.
fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let y = if month <= 2 { year.saturating_sub(1) } else { year };
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day.saturating_sub(1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe).saturating_sub(719_468)
}

/// The inverse.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    extern crate alloc;
    use super::*;
    use alloc::format;

    /// The round trip, over the dates a calendar gets wrong: a leap day, a
    /// century that is not a leap year, and the epoch itself.
    #[test]
    fn the_calendar_round_trips_through_the_dates_that_break_one() {
        for secs in [
            0,                 // 1970-01-01
            951_782_400,       // 2000-02-29, the leap day of a leap century
            4_107_542_400,     // 2100-03-01, the day after a February that had 28
            1_786_795_200,     // 2026-08-15 12:00:00
        ] {
            assert_eq!(Civil::from_unix_secs(secs).to_unix_secs(), secs);
        }
        assert_eq!(format!("{}", Civil::from_unix_secs(951_782_400)), "2000-02-29 00:00:00");
        assert_eq!(format!("{}", Civil::from_unix_secs(4_107_542_400).stem()), "2100-03-01-000000");
    }

    /// A name the log writes is a name the log recognises. The two used to be a
    /// format string in one file and a byte-shape in another.
    #[test]
    fn every_stem_this_renders_is_one_it_reads_back() {
        for secs in [0, 951_782_400, 1_786_795_200, 4_107_542_400] {
            let stem = format!("{}", Civil::from_unix_secs(secs).stem());
            assert!(is_stem(&stem), "`{stem}` is not the shape it was rendered as");
        }
        for no in ["2026-08-15-12000", "2026-8-15-120000", "unknown-01", "2026-08-15-12000x"] {
            assert!(!is_stem(no), "`{no}` was accepted as a dated stem");
        }
    }

    /// A time of day this renders is one a reader of the line it opens finds,
    /// and what the reader is handed is the rest of that line, whole.
    #[test]
    fn every_time_of_day_this_renders_is_one_it_reads_back() {
        // Midnight, an hour, minute and second that differ, and the last
        // second of a day.
        for (secs, rendered) in [(0, "00:00:00"), (1_786_806_245, "15:04:05"), (1_786_838_399, "23:59:59")] {
            let at = format!("{}", Civil::from_unix_secs(secs).time_of_day());
            assert_eq!(at, rendered);
            assert_eq!(after_time_of_day(&at), Some(""));
            assert_eq!(after_time_of_day(&format!("{at}   PASS  a  (3s)")), Some("   PASS  a  (3s)"));
        }
        // Short of one, not one, one that is not where the line opens, and one
        // whose eighth byte is the first of a wider character.
        for no in ["", "12:00:0", "12:00:0x y", "1200:00:00 y", "[12:00:00] y", "12:00:0é"] {
            assert_eq!(after_time_of_day(no), None, "`{no}` was read as opening with a time of day");
        }
    }

    /// The allow-list, from both sides: `logkeeper` deletes only what this names,
    /// and a host reading the volume for a boot's log reads only what it names.
    #[test]
    fn only_logkeepers_own_names_are_logkeepers() {
        assert_eq!(classify("2026-09-06-084003.log"), Some(Class::Dated));
        assert_eq!(classify("2026-09-06-084003_0002.log"), Some(Class::Dated));
        assert_eq!(classify("unknown-00.log"), Some(Class::Undated));
        assert_eq!(classify("unknown-07_9999.log"), Some(Class::Undated));
        // An undated boot goes before a dated one, which is the delete order.
        assert!(Class::Undated < Class::Dated);

        // The bootloader's own file, and anything else on a volume a person
        // and `toybox` can both write to.
        for no in ["loader.log", "LOADER.LOG", "boot.log", "notes.txt", ".log", "log"] {
            assert_eq!(classify(no), None, "`{no}` was taken for one of logkeeper's");
        }
        // A part number that is not four digits, and an index that is not two.
        for no in [
            "2026-09-06-084003_2.log",
            "2026-09-06-084003_00002.log",
            "2026-09-06-084003_abcd.log",
            "unknown-0.log",
            "unknown-000.log",
            "unknown.log",
        ] {
            assert_eq!(classify(no), None, "`{no}` was taken for one of logkeeper's");
        }
    }

    #[test]
    fn an_impossible_instant_is_refused_and_a_leap_day_is_not() {
        assert!(Civil { year: 2000, month: 2, day: 29, hour: 0, min: 0, sec: 0 }.is_valid());
        assert!(!Civil { year: 2100, month: 2, day: 29, hour: 0, min: 0, sec: 0 }.is_valid());
        assert!(!Civil { year: 1969, month: 1, day: 1, hour: 0, min: 0, sec: 0 }.is_valid());
        assert!(!Civil { year: 2026, month: 1, day: 1, hour: 0, min: 0, sec: 60 }.is_valid());
    }
}
