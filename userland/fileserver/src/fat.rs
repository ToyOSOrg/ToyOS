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
use diskserver::disk::{Disk, BLOCK};
use crate::volume::{parent, Kind, Meta, Node, OpenHow, Out, Volume, NANOS_PER_SEC};

/// The most entries one directory listing materialises.
const MAX_LIST: usize = 16_384;

/// The volume as `toyos-fat32` reads it: bytes, over the cache's blocks.
pub struct Bytes<D> {
    cache: Rc<Cache<D>>,
    /// The volume's own length, which is at most the partition's.
    len: u64,
}

// The disk's `ReadOnly` is `Device` here because it never arrives: the one
// read-only grant is the BOOT server's, whose volume is mounted unwritable,
// whose clients are refused every changing operation before it, and whose
// close and sync write nothing. `toyos-fat32` has the one device word since it
// reads every refused write as of unknown outcome.
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
    /// What this volume stamps its entries with, [`crate::volume::now_nanos`]'s
    /// unit. FAT specifies local time; this stamps UTC because the owner ruled
    /// the hardware clock is UTC.
    clock: fn() -> u64,
    /// Where a read lands before it goes out: `toyos-fat32` reads into a
    /// slice, and a client's window is never one. Kept, so a read allocates
    /// nothing.
    scratch: Vec<u8>,
}

fn word(e: Error) -> SyscallError {
    match e {
        Error::NotFound => SyscallError::NotFound,
        Error::AlreadyExists => SyscallError::AlreadyExists,
        Error::Io | Error::NotFat32 | Error::Truncated | Error::CorruptChain | Error::CorruptDirectory => {
            SyscallError::Io
        }
        Error::NotADirectory | Error::IsADirectory | Error::DirectoryNotEmpty | Error::InvalidName => {
            SyscallError::InvalidArgument
        }
        Error::NoSpace | Error::TooLarge | Error::LimitExceeded => SyscallError::ResourceExhausted,
    }
}

fn logged(what: &str, path: &str, e: Error) -> SyscallError {
    if e != Error::NotFound {
        println!("fileserver: {what} of '{path}': {e}");
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
            "fileserver: FAT32 mounted, {} bytes, {}-byte sectors, {}-byte clusters",
            geom.total_sectors as u64 * geom.bytes_per_sector as u64,
            geom.bytes_per_sector,
            geom.bytes_per_cluster()
        );
        Ok(Self {
            fs,
            cache,
            writable,
            open: BTreeMap::new(),
            by_path: BTreeMap::new(),
            next: 1,
            clock,
            scratch: Vec::new(),
        })
    }

    fn time(&self) -> FatTime {
        FatTime::from_unix_secs((self.clock)() / NANOS_PER_SEC)
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

    /// Forget a node nobody holds and whose entry is level.
    fn release(&mut self, node: Node) {
        let open = self.open.remove(&node).expect("an open node");
        if self.by_path.get(&open.path) == Some(&node) {
            self.by_path.remove(&open.path);
        }
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
            let mtime = self.fs.metadata(path).map_err(|e| logged("metadata", path, e))?.modified_unix;
            return Ok(Meta { kind: Kind::File, size: len, mtime: mtime * NANOS_PER_SEC });
        }
        let meta = self.fs.metadata(path).map_err(|e| match e {
            Error::NotADirectory => SyscallError::NotFound,
            e => logged("metadata", path, e),
        })?;
        let kind = if meta.is_dir { Kind::Dir } else { Kind::File };
        Ok(Meta { kind, size: if meta.is_dir { 0 } else { meta.len }, mtime: meta.modified_unix * NANOS_PER_SEC })
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
                (e.name, Meta { kind, size: if e.is_dir { 0 } else { e.len }, mtime: e.modified_unix * NANOS_PER_SEC })
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
                // A refused close is said, and its node kept for the next sync.
                let _ = self.close(node);
                return Err(e);
            }
        }
        Ok(node)
    }

    fn close(&mut self, node: Node) -> Result<(), SyscallError> {
        let Some(open) = self.open.get_mut(&node) else { return Ok(()) };
        open.holders -= 1;
        if open.holders > 0 {
            return Ok(());
        }
        if self.writable {
            if let Err(e) = self.level(node) {
                println!("fileserver: node {node} was not brought level at its last close ({e:?}); the next sync does it");
                return Err(e);
            }
        }
        self.release(node);
        Ok(())
    }

    fn hold(&mut self, node: Node) {
        if let Some(open) = self.open.get_mut(&node) {
            open.holders += 1;
        }
    }

    fn node_meta(&mut self, node: Node) -> Result<Meta, SyscallError> {
        let open = self.entry(node)?;
        let (path, size) = (open.path.clone(), open.file.len());
        let mtime = self.fs.metadata(&path).map_err(|e| logged("metadata", &path, e))?.modified_unix;
        Ok(Meta { kind: Kind::File, size, mtime: mtime * NANOS_PER_SEC })
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, SyscallError> {
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        if self.scratch.len() < out.len() {
            self.scratch.resize(out.len(), 0);
        }
        let buf = &mut self.scratch[..out.len()];
        let n = self.fs.read(&mut open.file, offset, buf).map_err(|e| logged("read", &open.path, e))?;
        out.put(0, &buf[..n]);
        Ok(n)
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
        // Asked of the volume first: a delete the device refused leaves every
        // holder of the file its file.
        self.fs.remove(path).map_err(|e| logged("unlink", path, e))?;
        self.orphan(path);
        Ok(())
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
                println!("fileserver: {to} could not be put back and is under {stranded}");
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

    fn sync(&mut self) -> Result<Vec<(Node, SyscallError)>, SyscallError> {
        let mut unlevel = Vec::new();
        if !self.writable {
            return Ok(unlevel);
        }
        let nodes: Vec<Node> = self.open.keys().copied().collect();
        for node in nodes {
            match self.level(node) {
                Err(e) => unlevel.push((node, e)),
                Ok(()) if self.open[&node].holders == 0 => self.release(node),
                Ok(()) => {}
            }
        }
        self.fs.sync().map_err(|e| logged("sync", "", e))?;
        Ok(unlevel)
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

/// A FAT32 volume built from fatgen103 by the checker's tests, and not by the
/// driver under test.
#[cfg(test)]
#[path = "../../../toyos-fat32/check/tests/common/mod.rs"]
mod spec_volume;

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::cache::CLEAN_LIMIT;
    use crate::ram::Ram;
    use diskserver::disk::DiskError;

    /// A disk that refuses every read of one block, after the fixture's own
    /// bytes are on it.
    struct Refusing {
        ram: Ram,
        refused: Rc<Cell<u64>>,
    }

    impl Disk for Refusing {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
            let count = (out.len() / BLOCK) as u64;
            if (first..first + count).contains(&self.refused.get()) {
                return Err(DiskError::Device);
            }
            self.ram.read(first, out)
        }
        fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
            self.ram.write(first, data)
        }
        fn flush(&mut self) -> Result<(), DiskError> {
            self.ram.flush()
        }
    }

    /// Four blocks holding `0x5A`, the second of which refuses every read.
    fn bytes() -> Bytes<Refusing> {
        let mut ram = Ram::new(4);
        ram.write(0, &[0x5A; 4 * BLOCK]).unwrap();
        let cache = Rc::new(Cache::new(Refusing { ram, refused: Rc::new(Cell::new(1)) }));
        Bytes { cache, len: 4 * BLOCK as u64 }
    }

    /// A read the device refused is the device's word to the caller, never a
    /// buffer of zeros: a caller that took zeros for data merges its bytes
    /// into them and writes the result over what the medium held.
    #[test]
    fn a_refused_read_is_an_error_and_never_zeros() {
        let mut b = bytes();
        let mut buf = [0xEEu8; 100];
        assert_eq!(b.read_at(BLOCK as u64 + 10, &mut buf), Err(IoError::Device));
        let mut fine = [0u8; 100];
        b.read_at(10, &mut fine).unwrap();
        assert!(fine.iter().all(|&x| x == 0x5A), "the readable block reads as written");
    }

    /// A write that covers part of a block reads the rest of it first; when
    /// that read is refused the write is refused whole, and nothing reaches
    /// the block — the rest of it is another file's, or the table's.
    #[test]
    fn a_partial_write_over_an_unreadable_block_writes_nothing() {
        let mut b = bytes();
        assert_eq!(b.write_at(BLOCK as u64 + 10, &[1, 2, 3]), Err(IoError::Device));
        b.flush().unwrap();
        let cache = Rc::try_unwrap(b.cache).ok().expect("the one holder");
        let mut disk = cache.into_disk();
        disk.refused.set(u64::MAX);
        let mut block = [0u8; BLOCK];
        disk.read(1, &mut block).unwrap();
        assert!(block.iter().all(|&x| x == 0x5A), "the unreadable block was written over");
    }

    /// The specification's fixture volume, writable, on a disk with room past
    /// it for [`evict`] to read, and the handle that moves its refused block.
    fn spec_fat() -> (FatVolume<Refusing>, Rc<Cell<u64>>, spec_volume::Volume) {
        let spec = spec_volume::fixture();
        let blocks = spec.bytes.len().div_ceil(BLOCK);
        let mut image = spec.bytes.clone();
        image.resize(blocks * BLOCK, 0);
        let mut ram = Ram::new((blocks + 2 * CLEAN_LIMIT) as u64);
        ram.write(0, &image).unwrap();
        let refused = Rc::new(Cell::new(u64::MAX));
        let v = FatVolume::mount(Refusing { ram, refused: Rc::clone(&refused) }, true, || 1_717_245_296 * NANOS_PER_SEC).unwrap();
        (v, refused, spec)
    }

    /// Every clean block out of the cache, so the next read of one asks the disk.
    fn evict(v: &FatVolume<Refusing>) {
        let mut block = [0u8; BLOCK];
        for b in v.cache.blocks() - 2 * CLEAN_LIMIT as u64..v.cache.blocks() {
            v.cache.read_block(b, &mut block).unwrap();
        }
    }

    /// One file whose entry will not read back leaves every other file's sync
    /// whole, its own close refused and its node kept, and the next sync that
    /// reaches it brings it level.
    #[test]
    fn a_file_that_will_not_level_costs_only_its_own_file() {
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        let (mut v, refused, spec) = spec_fat();
        let a = v.open("a.txt", CREATE).unwrap();
        let b = v.open("sub/b.txt", CREATE).unwrap();
        for n in [a, b] {
            v.write(n, 0, &[1; 3000]).unwrap();
        }
        assert_eq!(v.sync(), Ok(Vec::new()));
        for n in [a, b] {
            v.write(n, 3000, &[2; 3000]).unwrap();
        }
        let root = (spec_volume::cluster_offset(spec_volume::ROOT_CLUSTER) / BLOCK) as u64;
        let sub = (spec_volume::cluster_offset(spec.at("sub").first) / BLOCK) as u64;
        assert_ne!(root, sub, "`a.txt`'s entry and `sub/b.txt`'s are in different blocks");
        evict(&v);
        refused.set(root);

        assert_eq!(v.sync(), Ok(vec![(a, SyscallError::Io)]), "only the file that did not level is named");
        assert!(!v.open[&b].file.needs_reconcile(), "the other file was brought level");
        assert!(v.open[&a].file.needs_reconcile());
        assert_eq!(v.close(a), Err(SyscallError::Io), "a close that left the entry behind is refused");
        assert!(v.open.contains_key(&a), "and its node kept");
        assert_eq!(v.sync(), Ok(vec![(a, SyscallError::Io)]), "a sync the entry still refuses names it again");
        assert!(v.open.contains_key(&a), "and keeps its node");

        refused.set(u64::MAX);
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert!(!v.open.contains_key(&a), "levelled, the unheld node goes");
        assert_eq!(v.lstat("a.txt").unwrap().size, 6000);
        v.close(b).unwrap();
        assert_eq!(v.lstat("sub/b.txt").unwrap().size, 6000);
    }

    /// An open file's entry the device will not read is `Io` to a stat,
    /// never mtime 0, which is "undated".
    #[test]
    fn an_unreadable_entry_is_io_and_not_undated() {
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        let (mut v, refused, _) = spec_fat();
        let a = v.open("a.txt", CREATE).unwrap();
        v.write(a, 0, &[1; 3000]).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        evict(&v);
        refused.set((spec_volume::cluster_offset(spec_volume::ROOT_CLUSTER) / BLOCK) as u64);

        assert_eq!(v.lstat("a.txt"), Err(SyscallError::Io));
        assert_eq!(v.node_meta(a), Err(SyscallError::Io));
    }

    /// A disk whose grant does not write: every write and flush is refused.
    struct Granted(Ram);

    impl Disk for Granted {
        fn blocks(&self) -> u64 {
            self.0.blocks()
        }
        fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
            self.0.read(first, out)
        }
        fn write(&mut self, _: u64, _: &[u8]) -> Result<(), DiskError> {
            panic!("a read-only volume wrote to its disk")
        }
        fn flush(&mut self) -> Result<(), DiskError> {
            panic!("a read-only volume flushed its disk")
        }
    }

    /// A volume mounted unwritable never asks its disk to write or flush, so
    /// the read-only grant's refusal never reaches [`Bytes`].
    #[test]
    fn a_read_only_volume_never_writes_its_disk() {
        const OPEN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };
        let spec = spec_volume::fixture();
        let blocks = spec.bytes.len().div_ceil(BLOCK);
        let mut ram = Ram::new(blocks as u64);
        let mut image = spec.bytes.clone();
        image.resize(blocks * BLOCK, 0);
        ram.write(0, &image).unwrap();
        let mut v = FatVolume::mount(Granted(ram), false, || 1_717_245_296 * NANOS_PER_SEC).unwrap();

        assert_eq!(v.lstat("short.txt").unwrap().size, 100);
        v.list("sub").unwrap();
        let n = v.open("short.txt", OPEN).unwrap();
        let mut out = vec![0u8; 100];
        assert_eq!(v.read(n, 0, &mut crate::volume::Buf(&mut out)), Ok(100));
        assert!(out == spec.bytes[spec_volume::cluster_offset(spec.at("short.txt").first)..][..100], "the file reads as the fixture wrote it");
        v.close(n).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
    }

    /// What the driver says of a volume that stopped answering is `Io`, which
    /// no caller takes for a name that is not there.
    #[test]
    fn a_device_error_is_io_and_not_not_found() {
        assert_eq!(word(Error::Io), SyscallError::Io);
        assert_eq!(word(Error::NotFound), SyscallError::NotFound);
    }
}
