//! What a volume answers, whatever its format: every path here is the
//! volume's own — `/`-separated, no leading `/`, `""` its root — and already
//! resolved ([`crate::resolve`]), so a volume never follows a link.
//!
//! **An open file is a [`Node`]**, one per path while any client holds it
//! open, so two opens of one file write one set of pages and see one length. A
//! node whose file was unlinked or renamed over answers `Stale` from then on:
//! its blocks may be another file's.

use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::OnceLock;

use toyos_abi::clock::nanos_since_boot;
use toyos::fs::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
}

impl Kind {
    pub fn wire(self) -> u64 {
        match self {
            Kind::File => toyos::fs::KIND_FILE,
            Kind::Dir => toyos::fs::KIND_DIR,
            Kind::Symlink => toyos::fs::KIND_SYMLINK,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime: u64,
}

/// One entry [`Volume::list`] answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub name: String,
    pub meta: Meta,
    /// What [`Volume::identity`] answers for it.
    pub identity: u64,
}

/// How an open treats what is, or is not, at its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenHow {
    pub create: bool,
    pub create_new: bool,
    pub truncate: bool,
}

/// An open file, while any client holds it.
pub type Node = u64;

/// Where a read's bytes go, `len()` of them at most: the client's window, which
/// is shared with the client and so is written through a copy and never
/// through a reference, or a buffer of this process's own.
pub trait Out {
    fn len(&self) -> usize;
    /// `bytes` at `at`, inside `len()`.
    fn put(&mut self, at: usize, bytes: &[u8]);
    /// `len` zeros at `at`, inside `len()`: a hole's bytes.
    fn zero(&mut self, at: usize, len: usize);
}

/// A buffer of this process's own as a read's destination.
pub struct Buf<'a>(pub &'a mut [u8]);

impl Out for Buf<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        self.0[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn zero(&mut self, at: usize, len: usize) {
        self.0[at..at + len].fill(0);
    }
}

/// A client's window as a read's destination, written through copies only.
pub struct Window<'a>(pub toyos::shm::Bytes<'a>);

impl Out for Window<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn put(&mut self, at: usize, bytes: &[u8]) {
        self.0.write(at, bytes);
    }

    fn zero(&mut self, at: usize, len: usize) {
        self.0.sub(at, len).zero();
    }
}

pub trait Volume {
    /// Whether anything here may be changed.
    fn writable(&self) -> bool;

    /// What `path` is, without following it.
    fn lstat(&mut self, path: &str) -> Result<Meta, Error>;

    /// What the symlink at `path` holds.
    fn read_link(&mut self, path: &str) -> Result<String, Error>;

    /// The entries directly under `dir`.
    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error>;

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, Error>;

    /// One holder gone; the node goes with its last, once its entry is
    /// written. `Err` is that entry refused: the node stays, unheld, for the
    /// next sync to write, and the refusal is the last holder's answer.
    fn close(&mut self, node: Node) -> Result<(), Error>;

    fn node_meta(&mut self, node: Node) -> Result<Meta, Error>;

    /// The number of the file `path` names, without following it
    /// (`toyos_abi::syscall::Stat::node`): the same through every path that
    /// names it and across its renames, for as long as it exists, and never
    /// another existing file's.
    fn identity(&mut self, path: &str) -> Result<u64, Error>;

    /// Up to `out.len()` bytes at `offset`, from the start of `out`; 0 at or
    /// past the end. A file's bytes come from the disk, as last written back.
    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, Error>;

    /// Where in the volume's memory the open file `node`'s bytes lie as they
    /// read, nobody writing them: the byte they start at, a whole page in,
    /// and their length, verified. `None` for a file it holds otherwise, as a
    /// device's volume holds every file.
    fn in_place(&mut self, _node: Node) -> Result<Option<(u64, u64)>, Error> {
        Ok(None)
    }

    /// Make room for `len` bytes at `offset` and grow the size to their end,
    /// zeroing what lies between the old end and `offset` where the format
    /// holds no hole: whole or refused. The bytes are not written, and read
    /// as whatever the blocks held until [`Self::write_back`] writes them, so
    /// nothing may read them from here before then: a write refused after
    /// its reserve gives back its room through [`Self::unreserve`].
    fn reserve(&mut self, node: Node, offset: u64, len: u64) -> Result<Reserved, Error>;

    /// A write refused after its reserve: the size back to the reserve's old
    /// one, or to `landed` where the write's bytes reached past it, and every
    /// block the reserve took past both given back, since it holds what the
    /// file that last had it left there. `landed` is 0 where none landed.
    /// Called before any other call on the file, so nothing has recorded the
    /// reserve's blocks.
    fn unreserve(&mut self, node: Node, reserved: Reserved, landed: u64) -> Result<(), Error> {
        self.truncate(node, reserved.size.max(landed))
    }

    /// `data` at the whole-page `offset`, inside the size, into the room
    /// [`Self::reserve`] made; a last page in part is zeros past the size.
    fn write_back(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), Error>;

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), Error>;

    /// Stamp the open file `node` as last written at `mtime`; `Unsupported`
    /// where the format stamps its files itself.
    fn set_mtime(&mut self, node: Node, mtime: u64) -> Result<(), Error>;

    fn mkdir(&mut self, path: &str) -> Result<(), Error>;

    fn rmdir(&mut self, path: &str) -> Result<(), Error>;

    /// A file or a symlink; a directory is `rmdir`'s.
    fn unlink(&mut self, path: &str) -> Result<(), Error>;

    /// Replaces a file or symlink at `to`; never a directory.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Error>;

    fn symlink(&mut self, path: &str, target: &str) -> Result<(), Error>;

    /// Everything written, and every open file's length, durable, but for the
    /// files the answer names: each is one whose entry was refused, with why,
    /// and no refusal keeps any other file from the device. `Err` is the
    /// volume's own flush refused.
    fn sync(&mut self) -> Result<Vec<(Node, Error)>, Error>;

    /// Whether the volume's next commit carries `node`'s entry whatever it is
    /// asked to: one written since the last, or one naming blocks taken since,
    /// which the commit holds as taken. The file's pages reach the volume
    /// before that commit, so no entry it makes durable names a block that
    /// does not hold the file's bytes.
    fn carried(&self, _node: Node) -> bool {
        false
    }

    /// `node`'s entry durable, with every entry [`Self::carried`] names and
    /// no other open file's: an fsync's commit. `None` commits only those, as
    /// a directory's fsync does, whose names every change writes at once.
    /// `Err` is `node`'s entry or the commit refused; another's refused entry
    /// is said, and kept for the next commit.
    fn commit(&mut self, node: Option<Node>) -> Result<(), Error>;

    /// What more can be written to the volume: none, where nothing can be.
    fn room(&mut self) -> Result<toyos::fs::Room, Error>;

    /// One line of what the volume and its cache have done, for a log.
    fn describe(&self) -> String;
}

/// What a file held before a [`Volume::reserve`], for [`Volume::unreserve`].
#[derive(Clone, Copy, Debug)]
pub struct Reserved {
    pub size: u64,
    /// The pages it held blocks for. Past them a page below `size` is a hole,
    /// whose block, if the reserve took one, holds what another file left.
    pub held: u64,
}

impl Reserved {
    /// Where the file's old bytes on the volume end: a page in part reads
    /// what it held below this, and zeros from here.
    pub fn contents(&self) -> u64 {
        self.size.min(self.held.saturating_mul(toyos_blockdev::BLOCK as u64))
    }
}

/// A write straight to the volume, for a file with no paged object — on a
/// server with no pager, or one whose object went — and for a volume's
/// tests: the reserve, then each page it touches written back, what the page
/// held below the old contents' end read first.
pub trait Writes: Volume {
    fn write(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), Error> {
        const PAGE: u64 = toyos_blockdev::BLOCK as u64;
        let reserved = self.reserve(node, offset, data.len() as u64)?;
        let size = self.node_meta(node)?.size;
        let end = offset + data.len() as u64;
        let mut landed = 0;
        for start in (offset / PAGE * PAGE..end).step_by(PAGE as usize) {
            let mut page = vec![0u8; PAGE as usize];
            let below = reserved.contents().saturating_sub(start).min(PAGE) as usize;
            let (from, to) = (offset.max(start), end.min(start + PAGE));
            let written = self.read(node, start, &mut Buf(&mut page[..below])).and_then(|_| {
                page[(from - start) as usize..(to - start) as usize]
                    .copy_from_slice(&data[(from - offset) as usize..(to - offset) as usize]);
                self.write_back(node, start, &page[..(size - start).min(PAGE) as usize])
            });
            if let Err(e) = written {
                if let Err(again) = self.unreserve(node, reserved, landed) {
                    println!("fileserver: a refused write was not cut back ({again:?})");
                }
                return Err(e);
            }
            landed = to;
        }
        Ok(())
    }
}

impl<V: Volume + ?Sized> Writes for V {}

/// A volume's [`Volume::identity`] numbers: one per key the volume names a
/// file by, minted on first ask from a count that never goes back, so a number
/// is never another file's while both exist. The volume tells it every move
/// and removal; what it holds is bounded by the files that exist.
pub struct Identities<K> {
    of: BTreeMap<K, u64>,
    next: u64,
}

impl<K: Ord> Identities<K> {
    pub fn new() -> Self {
        // 0 is `Stat::node`'s "no file".
        Self { of: BTreeMap::new(), next: 1 }
    }

    pub fn of(&mut self, key: K) -> u64 {
        *self.of.entry(key).or_insert_with(|| {
            let number = self.next;
            self.next += 1;
            number
        })
    }

    /// The file at `from` is now at `to`, and whatever was at `to` is gone.
    pub fn moved(&mut self, from: &K, to: K) {
        let number = self.of.remove(from);
        self.of.remove(&to);
        if let Some(number) = number {
            self.of.insert(to, number);
        }
    }

    pub fn forget(&mut self, key: &K) {
        self.of.remove(key);
    }
}

impl<K: Ord> Default for Identities<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl Identities<String> {
    /// The directory at `from` is now at `to`, with everything beneath it.
    pub fn moved_tree(&mut self, from: &str, to: &str) {
        for key in self.tree(to) {
            self.of.remove(&key);
        }
        for key in self.tree(from) {
            let number = self.of.remove(&key).expect("listed by tree");
            self.of.insert(format!("{to}{}", &key[from.len()..]), number);
        }
    }

    /// `dir`'s key and every key beneath it.
    fn tree(&self, dir: &str) -> Vec<String> {
        self.of
            .range::<str, _>((Bound::Included(dir), Bound::Unbounded))
            .map(|(key, _)| key)
            .take_while(|key| key.starts_with(dir))
            .filter(|key| key.len() == dir.len() || key.as_bytes()[dir.len()] == b'/')
            .cloned()
            .collect()
    }
}

/// The parent of `path`, `""` for a name at the root.
pub fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `dir` and `name` as one path.
pub fn join(dir: &str, name: &str) -> String {
    match (dir.is_empty(), name.is_empty()) {
        (true, _) => name.to_string(),
        (false, true) => dir.to_string(),
        (false, false) => format!("{dir}/{name}"),
    }
}

pub const NANOS_PER_SEC: u64 = 1_000_000_000;

static ANCHOR: OnceLock<Option<u64>> = OnceLock::new();

/// Anchors [`now_nanos`]. fileserver does it once, at its start: a clock that will
/// not anchor is a panic, and there it holds no client's data.
pub fn take_anchor() {
    let taken = anchor(|| (nanos_since_boot(), toyos_abi::syscall::clock_epoch(), nanos_since_boot()));
    ANCHOR.set(taken).expect("the anchor is taken once");
}

/// What a file written now is stamped with (`toyos_abi::syscall::Stat::mtime`):
/// nanoseconds since the Unix epoch, UTC, as the kernel's `clock::mtime_now`
/// reckons them, and 0 — undated — on a machine whose RTC never answered.
pub fn now_nanos() -> u64 {
    let anchor = ANCHOR.get().expect("fileserver takes the anchor at its start");
    anchor.map_or(0, |secs| secs.saturating_mul(NANOS_PER_SEC).saturating_add(nanos_since_boot()))
}

/// The Unix second at the counter's nanosecond zero, the kernel's `BOOT_SECS`,
/// out of `read`'s counter reading, `SYS_CLOCK_EPOCH` and counter reading.
/// The epoch is the anchor plus the whole seconds of the counter at the call,
/// so readings on either side within one second name it exactly. `None` is the
/// clock refused.
fn anchor(mut read: impl FnMut() -> (u64, Option<u64>, u64)) -> Option<u64> {
    for _ in 0..3 {
        let (before, epoch, after) = read();
        let epoch = epoch?;
        if before / NANOS_PER_SEC == after / NANOS_PER_SEC {
            return Some(epoch - before / NANOS_PER_SEC);
        }
    }
    panic!("fileserver: three calls to the wall clock each straddled a second of the counter");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The kernel's arithmetic, `BOOT_SECS + ⌊n/10⁹⌋` at the counter reading
    /// `n` its call took: the anchor comes back exactly, and a call whose
    /// readings straddle a second is asked again rather than guessed at.
    #[test]
    fn the_anchor_is_the_kernels() {
        const BOOT_SECS: u64 = 2_000_000_000;
        let kernel = |n: u64| Some(BOOT_SECS + n / NANOS_PER_SEC);

        assert_eq!(anchor(|| (4_200_000_000, kernel(4_500_000_000), 4_700_000_000)), Some(BOOT_SECS));

        let mut calls = [(4_900_000_000, 5_000_000_100, 5_100_000_000), (5_200_000_000, 5_250_000_000, 5_300_000_000)]
            .into_iter();
        let straddled = anchor(|| {
            let (before, at, after) = calls.next().expect("asked at most twice");
            (before, kernel(at), after)
        });
        assert_eq!(straddled, Some(BOOT_SECS));
        assert_eq!(calls.next(), None, "the straddled call was asked again");

        assert_eq!(anchor(|| (1, None, 2)), None);
    }

    /// One number per key, kept across a move, never given to another key —
    /// not after a removal, not to what a move displaced.
    #[test]
    fn a_number_follows_its_file_and_is_never_another_files() {
        let mut ids = Identities::new();
        let a = ids.of(10u64);
        let b = ids.of(20);
        assert_ne!(a, 0);
        assert_ne!(a, b);
        assert_eq!(ids.of(10), a, "asked again");

        ids.moved(&10, 30);
        assert_eq!(ids.of(30), a, "the file moved, its number with it");
        assert_ne!(ids.of(10), a, "what is at the old key now is another file");

        ids.moved(&30, 20);
        assert_eq!(ids.of(20), a, "a move over another file takes the key");
        ids.forget(&20);
        let fresh = ids.of(20);
        assert!(fresh != a && fresh != b, "a removed file's number, {fresh}, is never handed out again");

        let unseen = ids.of(40);
        ids.moved(&50, 40);
        assert_ne!(ids.of(40), unseen, "a move from a key never asked displaces the target all the same");
    }

    /// A directory's move carries every key beneath it and nothing that only
    /// shares its prefix, and drops what was beneath its new name.
    #[test]
    fn a_directory_moves_with_everything_beneath_it() {
        let mut ids: Identities<String> = Identities::new();
        let of = |ids: &mut Identities<String>, k: &str| ids.of(k.to_string());
        let dir = of(&mut ids, "a");
        let inner = of(&mut ids, "a/x/y");
        let sibling = of(&mut ids, "a-b");
        let stale = of(&mut ids, "c/old");

        ids.moved_tree("a", "c");
        assert_eq!(of(&mut ids, "c"), dir);
        assert_eq!(of(&mut ids, "c/x/y"), inner);
        assert_eq!(of(&mut ids, "a-b"), sibling, "a name that only shares the prefix stays");
        assert!(![dir, inner, sibling, stale].contains(&of(&mut ids, "a/x/y")), "the old path is no file of before");
        assert!(![dir, inner, sibling, stale].contains(&of(&mut ids, "c/old")), "what was under the new name went");
    }

    #[test]
    #[should_panic(expected = "each straddled a second")]
    fn a_clock_that_always_straddles_is_refused_by_name() {
        anchor(|| (999_999_999, Some(1), 1_000_000_000));
    }
}
