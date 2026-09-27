//! What a volume answers, whatever its format: every path here is the
//! volume's own — `/`-separated, no leading `/`, `""` its root — and already
//! resolved ([`crate::resolve`]), so a volume never follows a link.
//!
//! **An open file is a [`Node`]**, one per path while any client holds it
//! open, so two opens of one file write one set of pages and see one length. A
//! node whose file was unlinked or renamed over answers `Gone` from then on:
//! its blocks may be another file's.

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

/// Nanoseconds since the Unix epoch, now, to the second the clock reads.
pub fn now_nanos() -> u64 {
    toyos_abi::syscall::clock_epoch().map_or(0, |secs| secs.saturating_mul(1_000_000_000))
}
