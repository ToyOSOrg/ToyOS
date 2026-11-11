//! `/log` as this program's policy: one file per boot, continuations, and
//! retention.
//!
//! Writes and syncs use the same served file API as other programs.
//!
//! **A part is not preallocated.** Its whole length in zeros is a burst of
//! writes to the stick at every part's start, and a burst that size starves a
//! tone playing beside it; an append's cluster chain and directory entry are
//! the price instead.
//!
//! **Every boot keeps its first part** ([`retire`]): a boot that floods the
//! volume deletes older parts of its own and never another boot's start.
//!
//! **`toyos_wallclock::classify` and the stop mark are the whole of what this
//! program may delete.** Only this program's view and the supervisor's write
//! `/log`, but the bootloader writes its own files there, and the only thing
//! standing between those and `delete_file` is that neither names them.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

use toyos_logstream::LOG_CONTINUES;
use toyos_wallclock::{classify, Class, Part, UNDATED_STEM};

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
/// The part number is four digits wide (`toyos_wallclock::Part`).
pub const MAX_LOG_PARTS: u32 = 9999;

/// How large one file may get before the next part starts.
///
/// One mebibyte: a boot that logs a hundred times more than any real one still
/// fits, and sixteen of them fit the volume with room to spare. It also bounds
/// what `/system/bin/console` reads off USB before it paints anything.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Where one file of this boot's sequence is: `toyos_wallclock::Part` is its
/// name.
pub fn path(dir: &str, stem: &str, part: u32) -> String {
    format!("{dir}/{}", Part { stem, part })
}

/// A path of this program's as its boot's stem and its part number.
fn stem_and_part(path: &str) -> Option<(&str, u32)> {
    let (_, Part { stem, part }) = Part::parse(path.rsplit('/').next()?)?;
    Some((stem, part))
}

/// Every file in `dir` that this program wrote, oldest first.
pub fn ours(dir: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut ours: Vec<(Class, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((classify(&name)?, name))
        })
        .collect();
    ours.sort();
    ours.into_iter().map(|(_, name)| format!("{dir}/{name}")).collect()
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
fn sweep(dir: &str, current: &str, mut say: impl FnMut(String)) -> Vec<String> {
    let existing = ours(dir);
    let going = retire(&existing, current);
    let mut kept: Vec<String> = existing.into_iter().filter(|p| !going.contains(p)).collect();
    for path in going {
        // Named, because a file disappearing off the owner's stick with nothing
        // saying why is indistinguishable from a bug in this program.
        match fs::remove_file(&path) {
            Ok(()) => say(format!(
                "logkeeper: {dir} holds more than {MAX_LOG_FILES} logs, so {path} was deleted"
            )),
            Err(e) => {
                say(format!(
                    "logkeeper: {path} is past the {MAX_LOG_FILES}-log bound and would not delete: {e}"
                ));
                kept.push(path);
            }
        }
    }
    kept
}

/// The lowest index no undated log on the volume is using.
pub fn undated_stem(dir: &str, kept: &[String]) -> Option<String> {
    (0..MAX_LOG_FILES)
        .map(|i| format!("{UNDATED_STEM}-{i:02}"))
        .find(|stem| !kept.iter().any(|path| path.starts_with(&format!("{dir}/{stem}"))))
}

/// This boot's file, and everything about where it is in its own sequence.
///
/// **The file is the replay's text** (`crate::serve::Hub`), part after part,
/// with a note where the replay had let go of what the file was owed. Where
/// each part stands is kept twice, in its file's bytes and in the replay's
/// offsets, because a note is in the one and not the other.
///
/// **A part the volume refused is opened again** ([`Volume::catch_up`]): cut
/// to what its last sync made durable — or rebuilt whole, where its file is
/// shorter than that — and given the rest from the replay through the same
/// part-sized writes and continuations as any other.
pub struct Volume {
    dir: String,
    /// `None` while the part must be opened again.
    file: Option<File>,
    stem: String,
    part: u32,
    /// The replay's offset of this part's first byte of the boot.
    from: u64,
    /// Bytes in the part's file, and the replay's offset its text reaches.
    size: u64,
    through: u64,
    /// The same, as the part's last sync left them.
    durable: u64,
    durable_through: u64,
    rotate_at: u64,
}

/// What the replay gives a file next ([`Volume::catch_up`]).
pub enum Owed {
    /// The boot's text from the asked offset, whole lines.
    Text(Vec<u8>),
    /// The replay let go of `lost` bytes from the asked offset, and holds the
    /// boot again from `at`.
    Gap { lost: u64, at: u64 },
    /// The file has every byte the replay has.
    Nothing,
}

impl Volume {
    /// Open this boot's first file in `dir`, after making room for it. A
    /// create the volume refuses leaves the part to be opened again; `None`
    /// is a boot with no name to write under.
    ///
    /// `stem` is a wall-clock stamp, or `None` for a boot that could not be
    /// placed in time — which takes the lowest free `unknown-NN`.
    pub fn open(dir: &str, stem: Option<String>, rotate_at: u64, mut say: impl FnMut(String)) -> Option<Self> {
        let stem = match stem {
            Some(stem) => stem,
            None => match undated_stem(dir, &ours(dir)) {
                Some(stem) => stem,
                None => {
                    say(format!("logkeeper: no free {UNDATED_STEM} name on {dir}; this boot's log has nowhere to go"));
                    return None;
                }
            },
        };
        let kept = sweep(dir, &stem, &mut say);
        // The first part number this boot's name does not already have on the
        // volume. Two boots inside one second is a machine nobody has, but a
        // test that stages the wall clock has it every run, and a colliding
        // name would silently write over the older boot.
        //
        // **Exhaustion says so before it gives up**, so the machine's log never
        // goes to the console only with nothing saying why.
        let Some(part) = (1..=MAX_LOG_PARTS).find(|p| !kept.contains(&path(dir, &stem, *p))) else {
            say(format!(
                "logkeeper: every one of the {MAX_LOG_PARTS} part numbers under {stem} is taken on \
                 {dir}; this boot's log has nowhere to go"
            ));
            return None;
        };
        let full = path(dir, &stem, part);
        let file = create(&full)
            .inspect_err(|e| say(format!("logkeeper: cannot create {full}: {e}; it is asked again")))
            .ok();
        Some(Volume {
            dir: dir.to_string(),
            file,
            stem,
            part,
            from: 0,
            size: 0,
            through: 0,
            durable: 0,
            durable_through: 0,
            rotate_at,
        })
    }

    /// Where this boot's log is being written.
    pub fn path(&self) -> String {
        path(&self.dir, &self.stem, self.part)
    }

    /// Say on this volume that the boot stopped cleanly
    /// ([`toyos_logstream::STOPPED`]), durably.
    pub fn mark_stopped(&self) -> std::io::Result<()> {
        File::create(self.stopped_path())?.sync_all()
    }

    /// Take back [`Self::mark_stopped`]: the stop it was written for was
    /// refused. A mark that is not there is taken back already.
    pub fn unmark_stopped(&self) -> std::io::Result<()> {
        match fs::remove_file(self.stopped_path()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            done => done,
        }
    }

    fn stopped_path(&self) -> String {
        format!("{}/{}", self.dir, toyos_logstream::STOPPED)
    }

    /// Whether the part is open: false once the volume refused it, until
    /// [`Self::catch_up`] opens it again.
    pub fn is_open(&self) -> bool {
        self.file.is_some()
    }

    /// The volume refused the part: it is opened again before the next
    /// write, and holds no more than its last sync made durable.
    pub fn lost(&mut self) {
        self.file = None;
        (self.size, self.through) = (self.durable, self.durable_through);
    }

    /// The replay's offset the file's text reaches: anything past it is owed.
    pub fn through(&self) -> u64 {
        self.through
    }

    /// Write a round of the boot's text, the replay's next bytes, at the
    /// part's end. The caller batches; this does no buffering of its own
    /// beyond what the file cache is.
    pub fn write(&mut self, lines: &[u8]) -> std::io::Result<()> {
        self.put(lines)?;
        self.through += lines.len() as u64;
        Ok(())
    }

    /// Bytes into the file that are none of the replay's: a note.
    fn put(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.open_file()?.write_all(bytes)?;
        self.size += bytes.len() as u64;
        Ok(())
    }

    /// Get everything written so far onto the device, cache flush included.
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.open_file()?.sync_all()?;
        (self.durable, self.durable_through) = (self.size, self.through);
        Ok(())
    }

    fn open_file(&mut self) -> std::io::Result<&mut File> {
        self.file.as_mut().ok_or_else(|| std::io::Error::other("the part is not open"))
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

    /// Open the part again and write it what it is owed, `owed(offset, max)`
    /// answering the replay from an offset, at most `max` bytes. At most
    /// `budget` bytes of the boot this call, through part-sized writes and
    /// their continuations, each part synced as it fills; `note(lost)` is the
    /// line in place of a gap. Whether the file has every byte the replay
    /// has; an `Err` is the volume refusing again.
    pub fn catch_up(
        &mut self,
        mut owed: impl FnMut(u64, usize) -> Owed,
        note: impl Fn(u64) -> Vec<u8>,
        budget: u64,
        mut say: impl FnMut(String),
    ) -> std::io::Result<bool> {
        if self.file.is_none() {
            self.reopen(&mut say)?;
        }
        let mut written = 0u64;
        loop {
            if self.full() {
                self.rotate(&mut say)?;
                if !self.is_open() {
                    return Ok(false);
                }
            }
            if written >= budget {
                return Ok(false);
            }
            let room = (self.rotate_at - self.size).min(budget - written);
            match owed(self.through, room as usize) {
                Owed::Text(text) => {
                    written += text.len() as u64;
                    self.write(&text)?;
                }
                Owed::Gap { lost, at } => {
                    self.put(&note(lost))?;
                    self.through = at;
                }
                Owed::Nothing => {
                    self.sync()?;
                    return Ok(true);
                }
            }
        }
    }

    /// The stop: catch up while `more()` says there is time, `budget` at a
    /// time, then make durable what was written, with `shortfall(bytes)` in
    /// the file where the replay holds bytes up to `end` it does not. The
    /// bytes it ends short by; an `Err` is the volume refusing again, the part
    /// [`Self::lost`] and the file as its last sync left it.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &mut self,
        mut owed: impl FnMut(u64, usize) -> Owed,
        note: impl Fn(u64) -> Vec<u8>,
        end: u64,
        budget: u64,
        mut more: impl FnMut() -> bool,
        shortfall: impl Fn(u64) -> Vec<u8>,
        mut say: impl FnMut(String),
    ) -> std::io::Result<u64> {
        let mut stop = || -> std::io::Result<u64> {
            while !self.catch_up(&mut owed, &note, budget, &mut say)? && self.is_open() && more() {}
            let short = end - self.through;
            if self.is_open() {
                if short > 0 {
                    self.put(&shortfall(short))?;
                }
                self.sync()?;
            }
            Ok(short)
        };
        stop().inspect_err(|_| self.lost())
    }

    /// Open the part again at what its last sync made durable, or empty and
    /// from its start where its file is shorter than that: missing, cut, or
    /// never as long as the sync was told.
    fn reopen(&mut self, say: &mut impl FnMut(String)) -> std::io::Result<()> {
        let path = self.path();
        let mut file = OpenOptions::new().write(true).create(true).truncate(false).open(PathBuf::from(&path))?;
        if file.metadata()?.len() < self.durable {
            say(format!("logkeeper: {path} is shorter than its last sync left it; it is written again whole"));
            (self.durable, self.durable_through) = (0, self.from);
        }
        file.set_len(self.durable)?;
        file.seek(SeekFrom::End(0))?;
        (self.size, self.through) = (self.durable, self.durable_through);
        self.file = Some(file);
        Ok(())
    }

    /// Finish this part and carry on in the next file of this boot's
    /// sequence: `Err` where this part did not finish, and a refused create
    /// leaves the next part to be opened again.
    pub fn rotate(&mut self, mut say: impl FnMut(String)) -> std::io::Result<()> {
        let full = self.path();
        let bytes = self.size;
        if self.part >= MAX_LOG_PARTS {
            return Err(std::io::Error::other("this boot has no continuation left"));
        }
        // Durable before it is let go: nothing syncs a part once the next is
        // the one being written.
        self.sync()?;
        sweep(&self.dir, &self.stem, &mut say);
        self.part += 1;
        self.from = self.through;
        (self.size, self.durable, self.durable_through) = (0, 0, self.through);
        let next = self.path();
        say(format!("logkeeper: {full} reached {bytes}{LOG_CONTINUES}{next}"));
        self.file = create(&next)
            .inspect_err(|e| say(format!("logkeeper: cannot create {next}: {e}; it is asked again")))
            .ok();
        Ok(())
    }
}

/// Make a part.
fn create(path: &str) -> std::io::Result<File> {
    // Truncating rather than appending: the name is this boot's alone — the
    // part search above is what makes that true — so anything already under it
    // is a name collision and not a log to continue.
    OpenOptions::new().write(true).create(true).truncate(true).open(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::Hub;

    const STEM: &str = "2026-11-05-120000";

    fn note(lost: u64) -> Vec<u8> {
        format!("[{lost} bytes let go]\n").into_bytes()
    }

    /// A batch: the file given what it is owed, then the text written and
    /// synced where it is caught up, then the replay's.
    fn round(v: &mut Volume, hub: &Hub, text: &[u8]) {
        let caught = (v.is_open() && v.through() == hub.end())
            || v.catch_up(|at, max| hub.owed(at, max), note, MAX_LOG_BYTES, |_| {}).unwrap_or(false);
        if caught {
            v.write(text).unwrap();
            v.sync().unwrap();
            if v.full() {
                v.rotate(|_| {}).unwrap();
            }
        }
        hub.append(text, None);
    }

    /// Every part of the boot, in order, as the volume holds them; none past
    /// its bound by more than the line that filled it.
    fn on_disk(dir: &str, v: &Volume) -> Vec<u8> {
        let parts: Vec<Vec<u8>> = (1..=v.part()).map(|p| fs::read(path(dir, STEM, p)).unwrap_or_default()).collect();
        for (p, part) in parts.iter().enumerate() {
            assert!(part.len() < 64 + 24, "part {} is {} bytes, past its 64", p + 1, part.len());
        }
        parts.concat()
    }

    fn line(n: usize) -> Vec<u8> {
        format!("line {n:04}\n").into_bytes()
    }

    /// The record's own case: the first part's create refused at open. The
    /// part is made at the first catch-up, given the boot from its start.
    #[test]
    fn a_first_part_refused_at_open_is_made_and_given_the_boot() {
        let tmp = toyos_tmpdir::TempDir::new("logkeeper-store");
        // A log directory not there yet refuses the create, whoever asks.
        let log = tmp.path().join("log");
        let dir = log.to_str().unwrap();
        let hub = Hub::start(1 << 16, None);
        let mut v = Volume::open(dir, Some(STEM.into()), 1 << 16, |_| {}).unwrap();
        assert!(!v.is_open(), "a create the volume refused left the part open");
        let mut boot = line(0);
        round(&mut v, &hub, &boot);
        fs::create_dir(&log).unwrap();
        round(&mut v, &hub, &line(1));
        boot.extend(line(1));
        assert_eq!(on_disk(dir, &v), boot);
    }

    /// **The stop says what it could not write.** A file owed more than the
    /// stop has time for is synced as far as it got and ends with the
    /// shortfall; one the stop has time for is whole.
    #[test]
    fn a_stop_with_more_owed_than_its_time_syncs_and_says_the_shortfall() {
        let shortfall = |short: u64| format!("[{short} bytes short]\n").into_bytes();
        for time in [false, true] {
            let tmp = toyos_tmpdir::TempDir::new("logkeeper-store");
            let dir = tmp.path().to_str().unwrap();
            let hub = Hub::start(1 << 16, None);
            let mut v = Volume::open(dir, Some(STEM.into()), 1 << 16, |_| {}).unwrap();
            let mut boot = Vec::new();
            for n in 0..2 {
                round(&mut v, &hub, &line(n));
                boot.extend(line(n));
            }
            v.lost();
            for n in 2..40 {
                hub.append(&line(n), None);
                boot.extend(line(n));
            }
            let short = v
                .finish(|at, max| hub.owed(at, max), note, hub.end(), 64, || time, shortfall, |_| {})
                .unwrap();
            let mut want = boot[..boot.len() - short as usize].to_vec();
            if short > 0 {
                want.extend(shortfall(short));
            }
            assert_eq!(short == 0, time, "the stop with time is whole, the one without is short");
            assert_eq!(fs::read(v.path()).unwrap(), want);
            assert_eq!(v.durable, v.size, "what was written is synced");
        }
    }

    /// A stop whose reopen is refused after an unsynced write: the file is
    /// what its last sync left, and the bytes it lacks are counted from
    /// there, the unsynced write's included.
    #[test]
    fn a_stop_refused_after_an_unsynced_write_counts_from_the_last_sync() {
        let tmp = toyos_tmpdir::TempDir::new("logkeeper-store");
        let dir = tmp.path().to_str().unwrap();
        let hub = Hub::start(1 << 16, None);
        let mut v = Volume::open(dir, Some(STEM.into()), 1 << 16, |_| {}).unwrap();
        round(&mut v, &hub, &line(0));
        let synced = hub.end();
        v.write(&line(1)).unwrap();
        hub.append(&line(1), None);
        v.lost();
        hub.append(&line(2), None);
        // A directory where the part was refuses its reopen, whoever asks.
        fs::remove_file(v.path()).unwrap();
        fs::create_dir(v.path()).unwrap();
        let stopped = v.finish(|at, max| hub.owed(at, max), note, hub.end(), 64, || true, |_| Vec::new(), |_| {});
        assert!(stopped.is_err(), "a reopen onto a directory was answered");
        assert_eq!(hub.end() - v.through(), hub.end() - synced, "the shortfall counts from the last sync");
    }

    /// **The file is the replay's text, whatever it was refused.** Through
    /// an unsynced write the volume holds past its sync, a gap the replay let
    /// go of, a continuation whose create is refused, and a part cut short
    /// behind the volume's back, the parts on disk are the boot with one
    /// note where the gap was; the file's offsets and the replay's stay
    /// apart across the note.
    #[test]
    fn a_refused_file_is_given_back_the_boot_the_replay_holds() {
        let tmp = toyos_tmpdir::TempDir::new("logkeeper-store");
        let dir = tmp.path().to_str().unwrap();
        let hub = Hub::start(256, None);
        let mut v = Volume::open(dir, Some(STEM.into()), 64, |_| {}).unwrap();
        let mut boot = Vec::new();
        let n = std::cell::Cell::new(0);
        let rounds = |v: &mut Volume, count: usize, boot: &mut Vec<u8>| {
            for _ in 0..count {
                let text = line(n.replace(n.get() + 1));
                round(v, &hub, &text);
                boot.extend(text);
            }
        };
        rounds(&mut v, 4, &mut boot);
        assert_eq!(on_disk(dir, &v), boot);

        // A write that reached the file and no sync, more bytes after it, and
        // the volume refusing: the reopen cuts both away.
        let text = line(900);
        v.write(&text).unwrap();
        hub.append(&text, None);
        boot.extend(&text);
        fs::OpenOptions::new().append(true).open(v.path()).unwrap().write_all(b"torn").unwrap();
        v.lost();
        rounds(&mut v, 3, &mut boot);
        assert_eq!(on_disk(dir, &v), boot, "after an unsynced write");

        // Refused long enough that the replay lets go of what the file is owed.
        v.lost();
        let gap_from = v.through() as usize;
        for _ in 0..40 {
            let text = line(n.replace(n.get() + 1));
            hub.append(&text, None);
            boot.extend(text);
        }
        let Owed::Gap { lost, at } = hub.owed(v.through(), 0) else { panic!("the replay let nothing go") };
        assert_eq!(at, gap_from as u64 + lost);
        rounds(&mut v, 2, &mut boot);
        let mut want = boot[..gap_from].to_vec();
        want.extend(note(lost));
        want.extend(&boot[at as usize..]);
        assert_eq!(String::from_utf8(on_disk(dir, &v)).unwrap(), String::from_utf8(want.clone()).unwrap(), "across the gap");

        // A continuation whose create is refused: its name is a directory.
        let next = path(dir, STEM, v.part() + 1);
        fs::create_dir(&next).unwrap();
        while v.is_open() {
            let text = line(n.replace(n.get() + 1));
            round(&mut v, &hub, &text);
            boot.extend(&text);
            want.extend(text);
        }
        fs::remove_dir(&next).unwrap();
        rounds(&mut v, 2, &mut boot);
        want.extend(&boot[boot.len() - line(0).len() * 2..]);
        assert_eq!(on_disk(dir, &v), want, "after a refused continuation");

        // The part cut to nothing behind the volume's back: rebuilt whole.
        fs::write(v.path(), b"").unwrap();
        v.lost();
        rounds(&mut v, 2, &mut boot);
        want.extend(&boot[boot.len() - line(0).len() * 2..]);
        assert_eq!(on_disk(dir, &v), want, "after the part was cut");
        assert!(v.part() > 2, "the parts rotated: {}", v.part());
    }

    fn boot(stem: &str, parts: u32) -> Vec<String> {
        (1..=parts).map(|p| path(DIR, stem, p)).collect()
    }

    /// A volume under the bound loses nothing.
    #[test]
    fn nothing_goes_while_there_is_room() {
        let existing: Vec<String> = (0..10).flat_map(|b| boot(&format!("2026-09-0{b}-000000"), 1)).collect();
        assert!(retire(&existing, "2026-09-26-000000").is_empty());
    }

    /// **The flood this floor exists for.** A boot that has written eight
    /// parts on a volume holding eight earlier boots deletes its own oldest
    /// continuation, and keeps its own start and every earlier boot's.
    #[test]
    fn a_flooding_boot_costs_its_own_middle_and_nobody_elses_start() {
        let mut existing: Vec<String> =
            (0..8).flat_map(|b| boot(&format!("2026-09-1{b}-000000"), 1)).collect();
        existing.extend(boot("2026-09-26-000000", 8));
        assert_eq!(existing.len(), 16);
        let going = retire(&existing, "2026-09-26-000000");
        assert_eq!(going, [path(DIR, "2026-09-26-000000", 2)]);
        for earlier in 0..8 {
            assert!(!going.contains(&path(DIR, &format!("2026-09-1{earlier}-000000"), 1)));
        }
    }

    /// Past the floor, the oldest boots go whole, before anybody's
    /// continuations; then earlier boots' continuations go before this boot's.
    #[test]
    fn whole_old_boots_go_first_then_earlier_continuations() {
        let mut existing: Vec<String> = Vec::new();
        for b in 0..10 {
            existing.extend(boot(&format!("2026-08-{:02}-000000", 10 + b), 1));
        }
        existing.extend(boot("2026-09-01-000000", 3));
        existing.extend(boot("2026-09-26-000000", 3));
        assert_eq!(existing.len(), 16);
        // Eleven earlier boots, eight kept: the three oldest may go whole, and
        // the oldest alone makes the room one new part needs.
        assert_eq!(retire(&existing, "2026-09-26-000000"), [path(DIR, "2026-08-10-000000", 1)]);

        let mut existing: Vec<String> =
            (0..8).flat_map(|b| boot(&format!("2026-08-{:02}-000000", 10 + b), 1)).collect();
        existing.extend(boot("2026-09-01-000000", 5));
        existing.extend(boot("2026-09-26-000000", 3));
        assert_eq!(existing.len(), 16);
        assert_eq!(retire(&existing, "2026-09-26-000000"), [path(DIR, "2026-08-10-000000", 1)]);

        let mut existing: Vec<String> =
            (0..7).flat_map(|b| boot(&format!("2026-08-{:02}-000000", 10 + b), 1)).collect();
        existing.extend(boot("2026-09-01-000000", 6));
        existing.extend(boot("2026-09-26-000000", 3));
        assert_eq!(existing.len(), 16);
        assert_eq!(retire(&existing, "2026-09-26-000000"), [path(DIR, "2026-09-01-000000", 2)]);
    }

}
