//! What a volume answers, whatever its format: every path here is the
//! volume's own — `/`-separated, no leading `/`, `""` its root — and already
//! resolved ([`crate::resolve`]), so a volume never follows a link.
//!
//! **An open file is a [`Node`]**, one per path while any client holds it
//! open, so two opens of one file write one set of pages and see one length. A
//! node whose file was unlinked or renamed over answers `Gone` from then on:
//! its blocks may be another file's.

use std::sync::OnceLock;

use toyos_abi::clock::nanos_since_boot;
use toyos_abi::syscall::SyscallError;

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

pub trait Volume {
    /// Whether anything here may be changed.
    fn writable(&self) -> bool;

    /// What `path` is, without following it.
    fn lstat(&mut self, path: &str) -> Result<Meta, SyscallError>;

    /// What the symlink at `path` holds.
    fn read_link(&mut self, path: &str) -> Result<String, SyscallError>;

    /// The entries directly under `dir`: name and what it is.
    fn list(&mut self, dir: &str) -> Result<Vec<(String, Meta)>, SyscallError>;

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, SyscallError>;

    /// One holder gone; the node goes with its last, once its entry is
    /// written. `Err` is that entry refused: the node stays, unheld, for the
    /// next sync to write, and the refusal is the last holder's answer.
    fn close(&mut self, node: Node) -> Result<(), SyscallError>;

    /// Hold the node once more, for a second file id or a stream.
    fn hold(&mut self, node: Node);

    fn node_meta(&mut self, node: Node) -> Result<Meta, SyscallError>;

    /// Up to `out.len()` bytes at `offset`, from the start of `out`; 0 at or
    /// past the end.
    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, SyscallError>;

    fn write(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), SyscallError>;

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), SyscallError>;

    fn mkdir(&mut self, path: &str) -> Result<(), SyscallError>;

    fn rmdir(&mut self, path: &str) -> Result<(), SyscallError>;

    /// A file or a symlink; a directory is `rmdir`'s.
    fn unlink(&mut self, path: &str) -> Result<(), SyscallError>;

    /// Replaces a file or symlink at `to`; never a directory.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), SyscallError>;

    fn symlink(&mut self, path: &str, target: &str) -> Result<(), SyscallError>;

    /// Everything written, and every open file's length, durable, but for the
    /// files the answer names: each is one whose entry was refused, with why,
    /// and no refusal keeps any other file from the device. `Err` is the
    /// volume's own flush refused.
    fn sync(&mut self) -> Result<Vec<(Node, SyscallError)>, SyscallError>;

    /// One line of what the volume and its cache have done, for a log.
    fn describe(&self) -> String;
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

/// Anchors [`now_nanos`]. fsd does it once, at its start: a clock that will
/// not anchor is a panic, and there it holds no client's data.
pub fn take_anchor() {
    let taken = anchor(|| (nanos_since_boot(), toyos_abi::syscall::clock_epoch(), nanos_since_boot()));
    ANCHOR.set(taken).expect("the anchor is taken once");
}

/// What a file written now is stamped with (`toyos_abi::syscall::Stat::mtime`):
/// nanoseconds since the Unix epoch, UTC, as the kernel's `clock::mtime_now`
/// reckons them, and 0 — undated — on a machine whose RTC never answered.
pub fn now_nanos() -> u64 {
    let anchor = ANCHOR.get().expect("fsd takes the anchor at its start");
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
    panic!("fsd: three calls to the wall clock each straddled a second of the counter");
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

    #[test]
    #[should_panic(expected = "each straddled a second")]
    fn a_clock_that_always_straddles_is_refused_by_name() {
        anchor(|| (999_999_999, Some(1), 1_000_000_000));
    }
}
