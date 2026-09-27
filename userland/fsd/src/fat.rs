//! The LOG and BOOT roles' volumes: FAT32, over `toyos-fat32`, through the
//! server's cache.
//!
//! **A growing write is two writes: the chain, then the directory entry.**
//! Between them the volume holds clusters its entry does not reach, which
//! fatgen103 refuses, so every open file is brought level — its entry written,
//! its chain reconciled — at its last close, at an fsync and at a sync, as
//! the kernel's adapter did.
//!
//! **A create makes the directories on its way**, as the kernel's did: a log
//! file's path is its own, and FAT has the directories to carry it. A
//! directory made by itself is `mkdir`'s and needs its parent.
//!
//! FAT32 has no symlink representation, so a symlink is refused rather than
//! written as a file its reader would take for one.
//!
//! A volume that is not FAT32 is left untouched: `toyos-fat32` has no format
//! path and its probe writes nothing.

use std::collections::BTreeMap;
use std::rc::Rc;

use toyos_abi::syscall::SyscallError;
use toyos_fat32::{BlockAccess, Error, Fat32, FatTime, IoError};

use crate::cache::Cache;
use crate::disk::{Disk, BLOCK};
use crate::volume::{parent, Kind, Meta, Node, OpenHow, Volume};

/// The most entries one directory listing materialises.
const MAX_LIST: usize = 16_384;

/// The volume as `toyos-fat32` reads it: bytes, over the cache's blocks.
pub struct Bytes<D> {
    cache: Rc<Cache<D>>,
    /// The volume's own length, which is at most the partition's.
    len: u64,
}

impl<D: Disk> BlockAccess for Bytes<D> {
    fn capacity(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        let end = offset.checked_add(buf.len() as u64).ok_or(IoError::Device)?;
        if end > self.len {
            return Err(IoError::Device);
        }
        let first = offset / BLOCK as u64;
        let last = end.div_ceil(BLOCK as u64);
        let mut span = vec![0u8; ((last - first) as usize) * BLOCK];
        self.cache.read(first, &mut span).map_err(|_| IoError::Device)?;
        let at = (offset % BLOCK as u64) as usize;
        buf.copy_from_slice(&span[at..at + buf.len()]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        let end = offset.checked_add(buf.len() as u64).ok_or(IoError::Device)?;
        if end > self.len {
            return Err(IoError::Device);
        }
        let first = offset / BLOCK as u64;
        let last = end.div_ceil(BLOCK as u64);
        let mut span = vec![0u8; ((last - first) as usize) * BLOCK];
        let at = (offset % BLOCK as u64) as usize;
        // What the write does not cover is another file's, or the table's:
        // read before it is written back.
        if at != 0 || buf.len() % BLOCK != 0 {
            self.cache.read(first, &mut span).map_err(|_| IoError::Device)?;
        }
        span[at..at + buf.len()].copy_from_slice(buf);
        self.cache.write(first, &span).map_err(|_| IoError::Device)
    }

    fn flush(&mut self) -> Result<(), IoError> {
        self.cache.flush().map_err(|_| IoError::Device)
    }
}

struct Open {
    path: String,
    file: toyos_fat32::File,
    holders: u32,
    gone: bool,
}

pub struct FatVolume<D: Disk> {
    fs: Fat32<Bytes<D>>,
    cache: Rc<Cache<D>>,
    writable: bool,
    open: BTreeMap<Node, Open>,
    by_path: BTreeMap<String, Node>,
    next: Node,
    /// Local seconds since the epoch, which is the zone FAT stamps in.
    clock: fn() -> u64,
}

fn word(e: Error) -> SyscallError {
    match e {
        Error::NotFound => SyscallError::NotFound,
        Error::AlreadyExists => SyscallError::AlreadyExists,
        Error::Io | Error::NotFat32 | Error::Truncated | Error::CorruptChain | Error::CorruptDirectory => {
            SyscallError::Io
        }
        // Nothing here refuses on a clock, so a repair pending is a volume
        // that will not settle: the device's word.
        Error::BudgetExpired | Error::RepairPending => SyscallError::Io,
        Error::NotADirectory | Error::IsADirectory | Error::DirectoryNotEmpty | Error::InvalidName => {
            SyscallError::InvalidArgument
        }
        Error::NoSpace | Error::TooLarge | Error::LimitExceeded => SyscallError::ResourceExhausted,
    }
}

fn logged(what: &str, path: &str, e: Error) -> SyscallError {
    if e != Error::NotFound {
        println!("fsd: {what} of '{path}': {e}");
    }
    word(e)
}

impl<D: Disk> FatVolume<D> {
    /// Mount the FAT32 volume on `disk`, or say why not; nothing is written
    /// before the volume is known to be FAT32.
    pub fn mount(disk: D, writable: bool, clock: fn() -> u64) -> Result<Self, String> {
        let cache = Rc::new(Cache::new(disk));
        let len = cache.blocks() * BLOCK as u64;
        let mut bytes = Bytes { cache: Rc::clone(&cache), len };
        let geom = Fat32::probe(&mut bytes).map_err(|e| format!("no FAT32 here: {e}"))?;
        // Tightened from the partition to the volume before anything writes.
        bytes.len = geom.total_sectors as u64 * geom.bytes_per_sector as u64;
        let fs = Fat32::mount(bytes).map_err(|e| format!("no FAT32 here: {e}"))?;
        println!(
            "fsd: FAT32 mounted, {} bytes, {}-byte sectors, {}-byte clusters",
            geom.total_sectors as u64 * geom.bytes_per_sector as u64,
            geom.bytes_per_sector,
            geom.bytes_per_cluster()
        );
        Ok(Self { fs, cache, writable, open: BTreeMap::new(), by_path: BTreeMap::new(), next: 1, clock })
    }

    fn time(&self) -> FatTime {
        FatTime::from_unix_secs((self.clock)())
    }

    fn entry(&mut self, node: Node) -> Result<&mut Open, SyscallError> {
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        Ok(open)
    }

    /// Bring one open file's entry level with its chain.
    fn level(&mut self, node: Node) -> Result<(), SyscallError> {
        let time = self.time();
        let Some(open) = self.open.get_mut(&node) else { return Ok(()) };
        if open.gone {
            return Ok(());
        }
        self.fs.flush_meta(&mut open.file, time).map_err(|e| logged("the entry", &open.path, e))?;
        if open.file.needs_reconcile() {
            self.fs.reconcile(&mut open.file, time).map_err(|e| logged("a reconcile", &open.path, e))?;
        }
        Ok(())
    }

    fn orphan(&mut self, path: &str) {
        if let Some(node) = self.by_path.remove(path) {
            if let Some(open) = self.open.get_mut(&node) {
                open.gone = true;
            }
        }
    }
}

impl<D: Disk> Volume for FatVolume<D> {
    fn writable(&self) -> bool {
        self.writable
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, SyscallError> {
        if path.is_empty() {
            return Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 });
        }
        if let Some(open) = self.by_path.get(path).and_then(|n| self.open.get(n)) {
            let len = open.file.len();
            let mtime = self.fs.metadata(path).map(|m| m.modified_unix).unwrap_or(0);
            return Ok(Meta { kind: Kind::File, size: len, mtime: mtime * 1_000_000_000 });
        }
        let meta = self.fs.metadata(path).map_err(|e| match e {
            Error::NotADirectory => SyscallError::NotFound,
            e => logged("metadata", path, e),
        })?;
        let kind = if meta.is_dir { Kind::Dir } else { Kind::File };
        Ok(Meta { kind, size: if meta.is_dir { 0 } else { meta.len }, mtime: meta.modified_unix * 1_000_000_000 })
    }

    fn read_link(&mut self, path: &str) -> Result<String, SyscallError> {
        self.lstat(path)?;
        Err(SyscallError::InvalidArgument)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<(String, Meta)>, SyscallError> {
        let entries = self.fs.read_dir(dir, MAX_LIST).map_err(|e| logged("list", dir, e))?;
        Ok(entries
            .into_iter()
            .map(|e| {
                let kind = if e.is_dir { Kind::Dir } else { Kind::File };
                (e.name, Meta { kind, size: if e.is_dir { 0 } else { e.len }, mtime: e.modified_unix * 1_000_000_000 })
            })
            .collect())
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, SyscallError> {
        if path.is_empty() {
            return Err(SyscallError::InvalidArgument);
        }
        let node = match self.by_path.get(path).copied() {
            Some(_) if how.create_new => return Err(SyscallError::AlreadyExists),
            Some(node) => node,
            None => {
                let existing = match self.fs.metadata(path) {
                    Ok(meta) if meta.is_dir => return Err(SyscallError::InvalidArgument),
                    Ok(_) => true,
                    Err(Error::NotFound | Error::NotADirectory) => false,
                    Err(e) => return Err(logged("metadata", path, e)),
                };
                let file = match existing {
                    true if how.create_new => return Err(SyscallError::AlreadyExists),
                    true => self.fs.open(path).map_err(|e| logged("open", path, e))?,
                    false if !(how.create || how.create_new) => return Err(SyscallError::NotFound),
                    false if !self.writable => return Err(SyscallError::PermissionDenied),
                    false => {
                        let time = self.time();
                        let dir = parent(path);
                        if !dir.is_empty() {
                            self.fs.create_dir_all(dir, time).map_err(|e| logged("mkdir -p", dir, e))?;
                        }
                        self.fs.create(path, time).map_err(|e| logged("create", path, e))?
                    }
                };
                let node = self.next;
                self.next += 1;
                self.open.insert(node, Open { path: path.to_string(), file, holders: 0, gone: false });
                self.by_path.insert(path.to_string(), node);
                node
            }
        };
        self.open.get_mut(&node).expect("just found or made").holders += 1;
        if how.truncate {
            if let Err(e) = self.truncate(node, 0) {
                self.close(node);
                return Err(e);
            }
        }
        Ok(node)
    }

    fn close(&mut self, node: Node) {
        let Some(open) = self.open.get_mut(&node) else { return };
        open.holders -= 1;
        if open.holders > 0 {
            return;
        }
        if self.writable {
            if let Err(e) = self.level(node) {
                println!("fsd: node {node} was not brought level at its last close: {e:?}");
            }
        }
        let open = self.open.remove(&node).expect("present above");
        if self.by_path.get(&open.path) == Some(&node) {
            self.by_path.remove(&open.path);
        }
    }

    fn hold(&mut self, node: Node) {
        if let Some(open) = self.open.get_mut(&node) {
            open.holders += 1;
        }
    }

    fn node_meta(&mut self, node: Node) -> Result<Meta, SyscallError> {
        let open = self.entry(node)?;
        let (path, size) = (open.path.clone(), open.file.len());
        let mtime = self.fs.metadata(&path).map(|m| m.modified_unix).unwrap_or(0);
        Ok(Meta { kind: Kind::File, size, mtime: mtime * 1_000_000_000 })
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut [u8]) -> Result<usize, SyscallError> {
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        self.fs.read(&mut open.file, offset, out).map_err(|e| logged("read", &open.path, e))
    }

    fn write(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), SyscallError> {
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        self.fs.write(&mut open.file, offset, data).map_err(|e| logged("write", &open.path, e))
    }

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), SyscallError> {
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        if open.file.len() == size {
            return Ok(());
        }
        self.fs.set_len(&mut open.file, size).map_err(|e| logged("set_len", &open.path, e))
    }

    fn mkdir(&mut self, path: &str) -> Result<(), SyscallError> {
        let time = self.time();
        self.fs.create_dir(path, time).map_err(|e| logged("mkdir", path, e))
    }

    fn rmdir(&mut self, path: &str) -> Result<(), SyscallError> {
        if path.is_empty() {
            return Err(SyscallError::PermissionDenied);
        }
        self.fs.remove_dir(path).map_err(|e| logged("rmdir", path, e))
    }

    fn unlink(&mut self, path: &str) -> Result<(), SyscallError> {
        if self.fs.metadata(path).map_err(|e| logged("metadata", path, e))?.is_dir {
            return Err(SyscallError::InvalidArgument);
        }
        self.orphan(path);
        self.fs.remove(path).map_err(|e| logged("unlink", path, e))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), SyscallError> {
        // The source is judged before the destination is disturbed, and a
        // rename onto the entry it already is — FAT names one entry by strings
        // that differ in case — is POSIX's no-op and never a delete of it.
        self.lstat(from)?;
        if from == to || self.fs.same_entry(from, to).map_err(|e| logged("same_entry", from, e))? {
            return Ok(());
        }
        if let Some(node) = self.by_path.get(from).copied() {
            self.level(node)?;
        }
        let replaced = self.fs.replace_rename(from, to).map_err(|e| {
            if let Some(stranded) = &e.stranded {
                println!("fsd: {to} could not be put back and is under {stranded}");
            }
            logged("rename", from, e.cause)
        })?;
        self.orphan(to);
        let released = self.fs.release_replaced(replaced).map_err(|e| logged("release", to, e));
        if let Some(node) = self.by_path.remove(from) {
            self.open.get_mut(&node).expect("indexed").path = to.to_string();
            self.by_path.insert(to.to_string(), node);
        }
        released
    }

    fn symlink(&mut self, _path: &str, _target: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        if !self.writable {
            return Ok(());
        }
        let nodes: Vec<Node> = self.open.keys().copied().collect();
        for node in nodes {
            self.level(node)?;
        }
        self.fs.sync().map_err(|e| logged("sync", "", e))
    }

    fn describe(&self) -> String {
        let c = self.cache.counts();
        format!(
            "FAT32{}, {} files open; cache {} blocks ({} dirty), {} of {} reads hit",
            if self.writable { "" } else { " read-only" },
            self.open.len(),
            c.cached,
            c.dirty,
            c.hits,
            c.reads
        )
    }
}
