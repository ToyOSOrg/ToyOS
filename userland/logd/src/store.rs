//! `/log` as this program's policy: one file per boot, continuations,
//! preallocation, and retention.
//!
//! Every byte goes through `SYS_WRITE` and `SYS_FSYNC` exactly as any other
//! program's would.
//!
//! **A part is preallocated to its whole length when it is made.** An append
//! extends the file: on FAT that is a cluster chain and a directory entry
//! rewritten on every flush, and the in-place window a reset can land in is
//! metadata as well as data. So a part is created at [`MAX_LOG_BYTES`] of
//! zeros and written from its start; the flush that reaches its end moves no
//! metadata, and a part is cut to what it holds when it is finished — at
//! rotation, and at init's flush before the machine stops. A part a crash
//! left is its lines and then zeros, which a reader of the volume stops at.
//!
//! **Every boot keeps its first part** ([`retire`]): a boot that floods the
//! volume deletes older parts of its own and never another boot's start.
//!
//! **`toyos_wallclock::classify` is the whole of what this program may delete.**
//! `/log` is userland-writable, `toybox` writes there and the bootloader writes
//! its own file there, and the only thing standing between somebody else's file
//! and `delete_file` is that the function does not recognise it.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use toyos_wallclock::{classify, Class, UNDATED_STEM};

/// Where the logs go.
///
/// The root of the log partition, so that plugging the stick into another
/// machine puts them at the top of the window that opens.
pub const DIR: &str = "/log";

/// How many of this program's files the volume keeps, including the one this
/// boot is writing.
///
/// `create_log_volume` makes the smallest volume there is a FAT32 for and
/// `fsck_msdos` reports 35,098,112 free bytes on a fresh one, so sixteen files
/// at [`MAX_LOG_BYTES`] is 16 MiB — under half, with the rest left for anything
/// a later diagnostic wants to drop beside them.
pub const MAX_LOG_FILES: usize = 16;

/// Earlier boots whose first part the volume keeps whatever the boot writing
/// now does: half the files, so the boot writing now has the other half.
pub const EARLIER_BOOTS: usize = MAX_LOG_FILES / 2;

/// How many continuation files one boot may produce before this gives up.
///
/// The part number is four digits wide and a fifth would sort *before* the
/// fourth, putting retention in the wrong order.
pub const MAX_LOG_PARTS: u32 = 9999;

/// How large one file may get before the next part starts, and what a part is
/// preallocated to.
///
/// One mebibyte: a boot that logs a hundred times more than any real one still
/// fits, and sixteen of them fit the volume with room to spare. It also bounds
/// what `/system/bin/console` reads off USB before it paints anything.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// The rotate-fast bound, and it is an argument now rather than a kernel
/// actuator.
///
/// It exists for the same reason `test-small-caches` does: filling megabytes by
/// logging would take a boot far longer than a test should wait, and the code
/// it drives is the shipped code — only the bound moves. 256 bytes, so one
/// boot's own log crosses it many times over and drives both the continuation
/// and the retention path.
pub const ROTATE_FAST_BYTES: u64 = 256;

/// The name of one file in this boot's sequence.
///
/// The first part carries the bare stem, because that is what nearly every boot
/// ever writes and a `_0001` on it would be noise on every stick. A
/// continuation takes `_` rather than any other separator for one reason: it is
/// the only legal character that sorts *after* `.`, so `<stem>.log` still comes
/// before `<stem>_0002.log` and retention deletes a boot's parts in the order
/// they were written.
pub fn path(stem: &str, part: u32) -> String {
    match part {
        1 => format!("{DIR}/{stem}.log"),
        n => format!("{DIR}/{stem}_{n:04}.log"),
    }
}

/// A path of this program's as its boot's stem and its part number.
fn stem_and_part(path: &str) -> Option<(&str, u32)> {
    let name = path.rsplit('/').next()?.strip_suffix(".log")?;
    match name.split_once('_') {
        Some((stem, part)) => Some((stem, part.parse().ok()?)),
        None => Some((name, 1)),
    }
}

/// Every file on `/log` that this program wrote, oldest first.
pub fn ours() -> Vec<String> {
    let Ok(entries) = fs::read_dir(DIR) else { return Vec::new() };
    let mut ours: Vec<(Class, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((classify(&name)?, name))
        })
        .collect();
    ours.sort();
    ours.into_iter().map(|(_, name)| format!("{DIR}/{name}")).collect()
}

/// Which of `existing` — this program's files, oldest first — to delete so the
/// volume holds at most [`MAX_LOG_FILES`] once `current`'s next part is made.
///
/// **A pure answer, and the retention floor is its order**: whole earlier
/// boots past the newest [`EARLIER_BOOTS`] first, oldest first; then earlier
/// boots' continuations; then this boot's own continuations, oldest first.
/// Never the first part of a boot the floor keeps, and never `current`'s
/// first part, so one boot's flood costs its own middle and nobody's start.
pub fn retire(existing: &[String], current: &str) -> Vec<String> {
    let over = (existing.len() + 1).saturating_sub(MAX_LOG_FILES);
    if over == 0 {
        return Vec::new();
    }
    let mut earlier: Vec<&str> = Vec::new();
    for (stem, _) in existing.iter().filter_map(|p| stem_and_part(p)) {
        if stem != current && earlier.last() != Some(&stem) {
            earlier.push(stem);
        }
    }
    let dropped_boots = earlier.len().saturating_sub(EARLIER_BOOTS);
    let whole_boots = &earlier[..dropped_boots];
    let rank = |path: &String| -> Option<u8> {
        let (stem, part) = stem_and_part(path)?;
        match (whole_boots.contains(&stem), stem == current, part) {
            (true, _, _) => Some(0),
            (false, false, 1) => None,
            (false, false, _) => Some(1),
            (false, true, 1) => None,
            (false, true, _) => Some(2),
        }
    };
    let mut candidates: Vec<(u8, usize)> = existing
        .iter()
        .enumerate()
        .filter_map(|(i, path)| Some((rank(path)?, i)))
        .collect();
    candidates.sort();
    candidates.into_iter().take(over).map(|(_, i)| existing[i].clone()).collect()
}

/// Delete what [`retire`] names, and answer what is left. Every deletion is
/// named on the log.
fn sweep(current: &str, mut say: impl FnMut(String)) -> Vec<String> {
    let existing = ours();
    let going = retire(&existing, current);
    let mut kept: Vec<String> = existing.into_iter().filter(|p| !going.contains(p)).collect();
    for path in going {
        // Named, because a file disappearing off the owner's stick with nothing
        // saying why is indistinguishable from a bug in this program.
        match fs::remove_file(&path) {
            Ok(()) => say(format!(
                "logd: {DIR} holds more than {MAX_LOG_FILES} logs, so {path} was deleted"
            )),
            Err(e) => {
                say(format!(
                    "logd: {path} is past the {MAX_LOG_FILES}-log bound and would not delete: {e}"
                ));
                kept.push(path);
            }
        }
    }
    kept
}

/// The lowest index no undated log on the volume is using.
pub fn undated_stem(kept: &[String]) -> Option<String> {
    (0..MAX_LOG_FILES)
        .map(|i| format!("{UNDATED_STEM}-{i:02}"))
        .find(|stem| !kept.iter().any(|path| path.starts_with(&format!("{DIR}/{stem}"))))
}

/// This boot's file, and everything about where it is in its own sequence.
pub struct Volume {
    file: File,
    stem: String,
    part: u32,
    /// Bytes in the current part so far — not the file's length, which is the
    /// preallocation. Kept here rather than read back from the filesystem so a
    /// disagreement shows up as a wrong length rather than being silently
    /// corrected.
    size: u64,
    rotate_at: u64,
}

impl Volume {
    /// Open this boot's first file, after making room for it.
    ///
    /// `stem` is a wall-clock stamp, or `None` for a boot that could not be
    /// placed in time — which takes the lowest free `unknown-NN`.
    pub fn open(stem: Option<String>, rotate_at: u64, mut say: impl FnMut(String)) -> Option<Self> {
        let stem = match stem {
            Some(stem) => stem,
            None => match undated_stem(&ours()) {
                Some(stem) => stem,
                None => {
                    say(format!(
                        "logd: no free {UNDATED_STEM} name on {DIR}; this boot's log has nowhere \
                         to go"
                    ));
                    return None;
                }
            },
        };
        let kept = sweep(&stem, &mut say);
        // The first part number this boot's name does not already have on the
        // volume. Two boots inside one second is a machine nobody has, but a
        // test that stages the wall clock has it every run, and a colliding
        // name would silently write over the older boot.
        //
        // **Exhaustion says so before it gives up**, so the machine's log never
        // goes to the console only with nothing saying why.
        let Some(part) = (1..=MAX_LOG_PARTS).find(|p| !kept.contains(&path(&stem, *p))) else {
            say(format!(
                "logd: every one of the {MAX_LOG_PARTS} part numbers under {stem} is taken on \
                 {DIR}; this boot's log has nowhere to go"
            ));
            return None;
        };
        let full = path(&stem, part);
        let file = match create(&full, rotate_at) {
            Ok(file) => file,
            Err(e) => {
                say(format!("logd: cannot create {full}: {e}"));
                return None;
            }
        };
        Some(Volume { file, stem, part, size: 0, rotate_at })
    }

    /// Where this boot's log is being written.
    pub fn path(&self) -> String {
        path(&self.stem, self.part)
    }

    /// Write already-rendered lines at the part's end. The caller batches;
    /// this does no buffering of its own beyond what the file cache is.
    pub fn write(&mut self, lines: &[u8]) -> std::io::Result<()> {
        self.file.write_all(lines)?;
        self.size += lines.len() as u64;
        Ok(())
    }

    /// Get everything written so far onto the device, cache flush included —
    /// which is what `SYS_FSYNC` means on this tree.
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file.sync_all()
    }

    /// Cut the part to what it holds, and make that durable: what a part is
    /// once nothing more will be written to it.
    pub fn finish(&mut self) -> std::io::Result<()> {
        self.file.set_len(self.size)?;
        self.file.sync_all()
    }

    /// Bytes in the current part so far.
    pub fn bytes(&self) -> u64 {
        self.size
    }

    /// Which part of this boot's sequence is being written, from 1.
    pub fn part(&self) -> u32 {
        self.part
    }

    /// Whether this part has reached its bound.
    pub fn full(&self) -> bool {
        self.size >= self.rotate_at
    }

    /// Finish this part and carry on in the next file of this boot's sequence.
    pub fn rotate(&mut self, mut say: impl FnMut(String)) -> std::io::Result<()> {
        let full = self.path();
        let bytes = self.size;
        if self.part >= MAX_LOG_PARTS {
            return Err(std::io::Error::other("this boot has no continuation left"));
        }
        self.finish()?;
        sweep(&self.stem, &mut say);
        self.part += 1;
        self.size = 0;
        let next = self.path();
        self.file = create(&next, self.rotate_at)?;
        say(format!("logd: {full} reached {bytes} bytes and this boot continues in {next}"));
        Ok(())
    }
}

/// Make a part, preallocated to `len`.
fn create(path: &str, len: u64) -> std::io::Result<File> {
    // Truncating rather than appending: the name is this boot's alone — the
    // part search above is what makes that true — so anything already under it
    // is a name collision and not a log to continue.
    let file = OpenOptions::new().write(true).create(true).truncate(true).open(PathBuf::from(path))?;
    file.set_len(len)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot(stem: &str, parts: u32) -> Vec<String> {
        (1..=parts).map(|p| path(stem, p)).collect()
    }

    /// A volume under the bound loses nothing.
    #[test]
    fn nothing_goes_while_there_is_room() {
        let existing: Vec<String> = (0..10).flat_map(|b| boot(&format!("2026-09-0{b}"), 1)).collect();
        assert!(retire(&existing, "2026-09-26").is_empty());
    }

    /// **The flood this floor exists for.** A boot that has written eight
    /// parts on a volume holding eight earlier boots deletes its own oldest
    /// continuation, and keeps its own start and every earlier boot's.
    #[test]
    fn a_flooding_boot_costs_its_own_middle_and_nobody_elses_start() {
        let mut existing: Vec<String> =
            (0..8).flat_map(|b| boot(&format!("2026-09-1{b}"), 1)).collect();
        existing.extend(boot("2026-09-26", 8));
        assert_eq!(existing.len(), 16);
        let going = retire(&existing, "2026-09-26");
        assert_eq!(going, [path("2026-09-26", 2)]);
        for earlier in 0..8 {
            assert!(!going.contains(&path(&format!("2026-09-1{earlier}"), 1)));
        }
    }

    /// Past the floor, the oldest boots go whole, before anybody's
    /// continuations; then earlier boots' continuations go before this boot's.
    #[test]
    fn whole_old_boots_go_first_then_earlier_continuations() {
        let mut existing: Vec<String> = Vec::new();
        for b in 0..10 {
            existing.extend(boot(&format!("2026-08-{:02}", 10 + b), 1));
        }
        existing.extend(boot("2026-09-01", 3));
        existing.extend(boot("2026-09-26", 3));
        assert_eq!(existing.len(), 16);
        // Eleven earlier boots, eight kept: the three oldest may go whole, and
        // the oldest alone makes the room one new part needs.
        assert_eq!(retire(&existing, "2026-09-26"), [path("2026-08-10", 1)]);

        let mut existing: Vec<String> =
            (0..8).flat_map(|b| boot(&format!("2026-08-{:02}", 10 + b), 1)).collect();
        existing.extend(boot("2026-09-01", 5));
        existing.extend(boot("2026-09-26", 3));
        assert_eq!(existing.len(), 16);
        assert_eq!(retire(&existing, "2026-09-26"), [path("2026-08-10", 1)]);

        let mut existing: Vec<String> =
            (0..7).flat_map(|b| boot(&format!("2026-08-{:02}", 10 + b), 1)).collect();
        existing.extend(boot("2026-09-01", 6));
        existing.extend(boot("2026-09-26", 3));
        assert_eq!(existing.len(), 16);
        assert_eq!(retire(&existing, "2026-09-26"), [path("2026-09-01", 2)]);
    }

    /// Names that are not this program's are none of its business; a name
    /// that parses carries its stem and part.
    #[test]
    fn a_part_is_its_stem_and_number() {
        assert_eq!(stem_and_part("/log/2026-09-26-10-00-00.log"), Some(("2026-09-26-10-00-00", 1)));
        assert_eq!(
            stem_and_part("/log/2026-09-26-10-00-00_0007.log"),
            Some(("2026-09-26-10-00-00", 7))
        );
        assert_eq!(stem_and_part("/log/loader.txt"), None);
    }
}
