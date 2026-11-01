//! The LOG and BOOT roles' volumes: FAT32, over `toyos-fat32`, through the
//! server's cache.
//!
//! **A growing write is two writes: the chain, then the directory entry.**
//! Between them the volume holds clusters its entry does not reach, which
//! fatgen103 refuses, so an open file changed since it was last level is
//! brought level — its entry written and stamped with its last write, its
//! chain reconciled — at its last close, at an fsync and at a sync. **The two are a cache flush
//! apart**: the cache writes lowest block first and the disk keeps no order
//! until a flush, so an entry put into the cache beside its chain and data
//! would reach the disk before the clusters it names.
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

use toyos::fs;
use toyos_blockdev::BlockDev;
use toyos_fat32::{Error, Fat32, FatTime};

use crate::cache::{Cache, Disk};
use crate::volume::{parent, Identities, Kind, Listed, Meta, Node, OpenHow, Out, Reserved, Volume, NANOS_PER_SEC};

/// The most entries one directory listing materialises.
const MAX_LIST: usize = 16_384;

struct Open {
    path: String,
    file: toyos_fat32::File,
    holders: u32,
    gone: bool,
    /// Written, resized or opened truncating since its entry was last
    /// brought level.
    dirty: bool,
    /// When it was last written or resized, which its level stamps: its
    /// mtime meanwhile.
    written: Option<FatTime>,
}

/// The disk's `ReadOnly` reaches `toyos-fat32` as a refused write, which it
/// takes as of unknown outcome: it never arrives here, since the one read-only
/// grant is the BOOT server's, whose volume is mounted unwritable, whose
/// clients are refused every changing operation before it, and whose close and
/// sync write nothing.
pub struct FatVolume<D: Disk> {
    fs: Fat32<Rc<Cache<D>>>,
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
    /// By where the entry lies (`Fat32::entry_offset`), which every spelling
    /// of a name shares and a rename moves.
    ids: Identities<u64>,
}

fn word(e: Error) -> fs::Error {
    match e {
        Error::NotFound => fs::Error::NotFound,
        Error::AlreadyExists => fs::Error::AlreadyExists,
        Error::Io | Error::NotFat32 | Error::Truncated | Error::CorruptChain | Error::CorruptDirectory => {
            fs::Error::Io
        }
        Error::NotADirectory => fs::Error::NotADirectory,
        Error::IsADirectory => fs::Error::IsADirectory,
        Error::DirectoryNotEmpty => fs::Error::NotEmpty,
        Error::InvalidName => fs::Error::InvalidName,
        Error::NoSpace => fs::Error::NoSpace,
        Error::TooLarge => fs::Error::FileTooLarge,
        Error::LimitExceeded => fs::Error::LimitExceeded,
    }
}

fn logged(what: &str, path: &str, e: Error) -> fs::Error {
    if e != Error::NotFound {
        println!("fileserver: {what} of '{path}': {e}");
    }
    word(e)
}

impl<D: Disk> FatVolume<D> {
    /// Mount the FAT32 volume on `disk`, or say why not; nothing is written
    /// before the volume is known to be FAT32.
    pub fn mount(disk: D, clean_limit: usize, writable: bool, clock: fn() -> u64) -> Result<Self, String> {
        let cache = Rc::new(Cache::new(disk, clean_limit));
        let fs = cache.block_on(Fat32::mount(Rc::clone(&cache))).map_err(|e| format!("no FAT32 here: {e}"))?;
        let geom = fs.geometry();
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
            ids: Identities::new(),
        })
    }

    fn time(&self) -> FatTime {
        FatTime::from_unix_secs((self.clock)() / NANOS_PER_SEC)
    }

    /// Now as an entry's write time keeps it: the even second at or before.
    fn write_time(&self) -> FatTime {
        let secs = (self.clock)() / NANOS_PER_SEC;
        FatTime::from_unix_secs(secs - secs % 2)
    }

    fn entry(&mut self, node: Node) -> Result<&mut Open, fs::Error> {
        let open = self.open.get_mut(&node).ok_or(fs::Error::Stale)?;
        if open.gone {
            return Err(fs::Error::Stale);
        }
        Ok(open)
    }

    /// Bring one open file's entry level with its chain, stamped with its
    /// last write (or now, for a chain alone), when it changed since the
    /// entry was last written.
    fn level(&mut self, node: Node) -> Result<(), fs::Error> {
        match self.open.get(&node) {
            Some(open) if !open.gone && (open.dirty || open.file.needs_reconcile()) => {}
            _ => return Ok(()),
        }
        let now = self.time();
        let open = self.open.get_mut(&node).expect("just found");
        let time = open.written.unwrap_or(now);
        // The chain and the data on the disk before the entry that reaches
        // them is in the cache.
        if open.file.needs_reconcile() {
            self.cache.block_on(self.cache.flush()).map_err(|_| logged("the chain", &open.path, Error::Io))?;
        }
        self.cache.block_on(self.fs.flush_meta(&mut open.file, time)).map_err(|e| logged("the entry", &open.path, e))?;
        if open.file.needs_reconcile() {
            self.cache.block_on(self.fs.reconcile(&mut open.file, time)).map_err(|e| logged("a reconcile", &open.path, e))?;
        }
        open.dirty = false;
        open.written = None;
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

    fn entry_offset(&mut self, path: &str) -> Result<u64, fs::Error> {
        self.cache.block_on(self.fs.entry_offset(path)).map_err(|e| match e {
            Error::NotADirectory => fs::Error::NotFound,
            e => logged("entry_offset", path, e),
        })
    }
}

impl<D: Disk> Volume for FatVolume<D> {
    fn writable(&self) -> bool {
        self.writable
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, fs::Error> {
        if path.is_empty() {
            return Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 });
        }
        if let Some(&node) = self.by_path.get(path) {
            if self.open.contains_key(&node) {
                return self.node_meta(node);
            }
        }
        let meta = self.cache.block_on(self.fs.metadata(path)).map_err(|e| match e {
            Error::NotADirectory => fs::Error::NotFound,
            e => logged("metadata", path, e),
        })?;
        let kind = if meta.is_dir { Kind::Dir } else { Kind::File };
        Ok(Meta { kind, size: if meta.is_dir { 0 } else { meta.len }, mtime: meta.modified_unix * NANOS_PER_SEC })
    }

    fn read_link(&mut self, path: &str) -> Result<String, fs::Error> {
        self.lstat(path)?;
        Err(fs::Error::NotALink)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, fs::Error> {
        let entries = self.cache.block_on(self.fs.read_dir(dir, MAX_LIST)).map_err(|e| logged("list", dir, e))?;
        Ok(entries
            .into_iter()
            .map(|e| {
                let kind = if e.is_dir { Kind::Dir } else { Kind::File };
                let meta = Meta { kind, size: if e.is_dir { 0 } else { e.len }, mtime: e.modified_unix * NANOS_PER_SEC };
                Listed { name: e.name, meta, identity: self.ids.of(e.entry_offset) }
            })
            .collect())
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, fs::Error> {
        if path.is_empty() {
            return Err(fs::Error::IsADirectory);
        }
        let node = match self.by_path.get(path).copied() {
            Some(_) if how.create_new => return Err(fs::Error::AlreadyExists),
            Some(node) => node,
            None => {
                let existing = match self.cache.block_on(self.fs.metadata(path)) {
                    Ok(meta) if meta.is_dir => return Err(fs::Error::IsADirectory),
                    Ok(_) => true,
                    Err(Error::NotFound | Error::NotADirectory) => false,
                    Err(e) => return Err(logged("metadata", path, e)),
                };
                let file = match existing {
                    true if how.create_new => return Err(fs::Error::AlreadyExists),
                    true => self.cache.block_on(self.fs.open(path)).map_err(|e| logged("open", path, e))?,
                    false if !(how.create || how.create_new) => return Err(fs::Error::NotFound),
                    false if !self.writable => return Err(fs::Error::ReadOnly),
                    false => {
                        let time = self.time();
                        let dir = parent(path);
                        if !dir.is_empty() {
                            self.cache.block_on(self.fs.create_dir_all(dir, time)).map_err(|e| logged("mkdir -p", dir, e))?;
                        }
                        self.cache.block_on(self.fs.create(path, time)).map_err(|e| logged("create", path, e))?
                    }
                };
                let node = self.next;
                self.next += 1;
                self.open.insert(node, Open { path: path.to_string(), file, holders: 0, gone: false, dirty: false, written: None });
                self.by_path.insert(path.to_string(), node);
                node
            }
        };
        self.open.get_mut(&node).expect("just found or made").holders += 1;
        if how.truncate {
            // POSIX's `open` stamps a truncated file even when it was empty.
            let now = self.write_time();
            let open = self.open.get_mut(&node).expect("just found or made");
            open.dirty = true;
            open.written = Some(now);
            if let Err(e) = self.truncate(node, 0) {
                // A refused close is said, and its node kept for the next sync.
                let _ = self.close(node);
                return Err(e);
            }
        }
        Ok(node)
    }

    fn close(&mut self, node: Node) -> Result<(), fs::Error> {
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

    fn node_meta(&mut self, node: Node) -> Result<Meta, fs::Error> {
        let open = self.entry(node)?;
        if let Some(written) = open.written {
            return Ok(Meta { kind: Kind::File, size: open.file.len(), mtime: written.to_unix_secs() * NANOS_PER_SEC });
        }
        let (path, size) = (open.path.clone(), open.file.len());
        let mtime = self.cache.block_on(self.fs.metadata(&path)).map_err(|e| logged("metadata", &path, e))?.modified_unix;
        Ok(Meta { kind: Kind::File, size, mtime: mtime * NANOS_PER_SEC })
    }

    fn identity(&mut self, path: &str) -> Result<u64, fs::Error> {
        let key = self.entry_offset(path)?;
        Ok(self.ids.of(key))
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, fs::Error> {
        let open = self.open.get_mut(&node).ok_or(fs::Error::Stale)?;
        if open.gone {
            return Err(fs::Error::Stale);
        }
        if self.scratch.len() < out.len() {
            self.scratch.resize(out.len(), 0);
        }
        let buf = &mut self.scratch[..out.len()];
        let n = self.cache.block_on(self.fs.read(&mut open.file, offset, buf)).map_err(|e| logged("read", &open.path, e))?;
        out.put(0, &buf[..n]);
        Ok(n)
    }

    fn reserve(&mut self, node: Node, offset: u64, len: u64) -> Result<Reserved, fs::Error> {
        let now = self.write_time();
        let open = self.open.get_mut(&node).ok_or(fs::Error::Stale)?;
        if open.gone {
            return Err(fs::Error::Stale);
        }
        open.dirty = true;
        open.written = Some(now);
        // A chain has no holes: it holds a block for every page of its size.
        let reserved = Reserved { size: open.file.len(), held: open.file.len().div_ceil(toyos_blockdev::BLOCK as u64) };
        self.cache.block_on(self.fs.reserve(&mut open.file, offset, len)).map_err(|e| logged("reserve", &open.path, e))?;
        Ok(reserved)
    }

    fn write_back(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), fs::Error> {
        let open = self.open.get_mut(&node).ok_or(fs::Error::Stale)?;
        if open.gone {
            return Err(fs::Error::Stale);
        }
        open.dirty = true;
        self.cache.block_on(self.fs.write_back(&mut open.file, offset, data)).map_err(|e| logged("write-back", &open.path, e))
    }

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), fs::Error> {
        let now = self.write_time();
        let open = self.open.get_mut(&node).ok_or(fs::Error::Stale)?;
        if open.gone {
            return Err(fs::Error::Stale);
        }
        if open.file.len() == size {
            return Ok(());
        }
        open.dirty = true;
        open.written = Some(now);
        self.cache.block_on(self.fs.set_len(&mut open.file, size)).map_err(|e| logged("set_len", &open.path, e))
    }

    /// FAT stamps a file at the flush that writes it, at its own resolution.
    fn set_mtime(&mut self, _node: Node, _mtime: u64) -> Result<(), fs::Error> {
        Err(fs::Error::Unsupported)
    }

    fn mkdir(&mut self, path: &str) -> Result<(), fs::Error> {
        let time = self.time();
        self.cache.block_on(self.fs.create_dir(path, time)).map_err(|e| logged("mkdir", path, e))
    }

    fn rmdir(&mut self, path: &str) -> Result<(), fs::Error> {
        if path.is_empty() {
            return Err(fs::Error::Root);
        }
        let key = self.entry_offset(path)?;
        self.cache.block_on(self.fs.remove_dir(path)).map_err(|e| logged("rmdir", path, e))?;
        self.ids.forget(&key);
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), fs::Error> {
        if self.cache.block_on(self.fs.metadata(path)).map_err(|e| logged("metadata", path, e))?.is_dir {
            return Err(fs::Error::IsADirectory);
        }
        let key = self.entry_offset(path)?;
        // Asked of the volume first: a delete the device refused leaves every
        // holder of the file its file.
        self.cache.block_on(self.fs.remove(path)).map_err(|e| logged("unlink", path, e))?;
        self.orphan(path);
        self.ids.forget(&key);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), fs::Error> {
        // The source is judged before the destination is disturbed, and a
        // rename onto the entry it already is — FAT names one entry by strings
        // that differ in case — is POSIX's no-op and never a delete of it.
        self.lstat(from)?;
        if from == to || self.cache.block_on(self.fs.same_entry(from, to)).map_err(|e| logged("same_entry", from, e))? {
            return Ok(());
        }
        if let Some(node) = self.by_path.get(from).copied() {
            self.level(node)?;
        }
        let from_key = self.entry_offset(from)?;
        let displaced = match self.entry_offset(to) {
            Ok(key) => Some(key),
            Err(fs::Error::NotFound) => None,
            Err(e) => return Err(e),
        };
        let replaced = self.cache.block_on(self.fs.replace_rename(from, to)).map_err(|e| {
            if let Some(stranded) = &e.stranded {
                println!("fileserver: {to} could not be put back and is under {stranded}");
            }
            logged("rename", from, e.cause)
        })?;
        self.orphan(to);
        let released = self.cache.block_on(self.fs.release_replaced(replaced)).map_err(|e| logged("release", to, e));
        if let Some(node) = self.by_path.remove(from) {
            self.open.get_mut(&node).expect("indexed").path = to.to_string();
            self.by_path.insert(to.to_string(), node);
        }
        if let Some(key) = displaced {
            self.ids.forget(&key);
        }
        // An entry the device will not show again is a file whose number goes
        // with it: renumbered, never another file's.
        match self.entry_offset(to) {
            Ok(to_key) => self.ids.moved(&from_key, to_key),
            Err(_) => self.ids.forget(&from_key),
        }
        released
    }

    fn symlink(&mut self, _path: &str, _target: &str) -> Result<(), fs::Error> {
        Err(fs::Error::Unsupported)
    }

    fn sync(&mut self) -> Result<Vec<(Node, fs::Error)>, fs::Error> {
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
        self.cache.block_on(self.fs.sync()).map_err(|e| logged("sync", "", e))?;
        Ok(unlevel)
    }

    /// A flush writes every chain the cache holds, whichever file asked: an
    /// open file changed since its entry was level is carried.
    fn carried(&self, node: Node) -> bool {
        self.open.get(&node).is_some_and(|open| !open.gone && (open.dirty || open.file.needs_reconcile()))
    }

    fn commit(&mut self, node: Option<Node>) -> Result<(), fs::Error> {
        if !self.writable {
            return Ok(());
        }
        if let Some(node) = node {
            self.level(node)?;
        }
        let carried: Vec<Node> = self.open.keys().copied().filter(|&n| self.carried(n)).collect();
        for n in carried {
            match self.level(n) {
                Err(e) => println!("fileserver: node {n} was not brought level at a commit ({e:?}); the next one does it"),
                Ok(()) if self.open[&n].holders == 0 => self.release(n),
                Ok(()) => {}
            }
        }
        self.cache.block_on(self.fs.sync()).map_err(|e| logged("sync", "", e))
    }

    fn room(&mut self) -> Result<fs::Room, fs::Error> {
        let unit = u64::from(self.fs.geometry().bytes_per_cluster());
        if !self.writable {
            return Ok(fs::Room { free: 0, unit });
        }
        let free = self.cache.block_on(self.fs.free_bytes()).map_err(|e| logged("free space", "", e))?;
        Ok(fs::Room { free, unit })
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
#[path = "../../../lib/fat32/check/tests/common/mod.rs"]
mod spec_volume;

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::volume::Writes;
    use crate::ram::Ram;
    use toyos_blockdev::{block_on_ready, Block, Bytes, BLOCK};

    impl Disk for Bytes<Vec<u8>> {}

    /// The clean blocks every volume's cache here keeps.
    const CLEAN_LIMIT: usize = 4096;

    /// A disk that refuses every read of one block, after the fixture's own
    /// bytes are on it.
    struct Refusing {
        ram: Ram,
        refused: Rc<Cell<u64>>,
    }

    impl BlockDev for Refusing {
        fn blocks(&self) -> u64 {
            self.ram.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), toyos_blockdev::Error> {
            if (lba..lba + out.len() as u64).contains(&self.refused.get()) {
                return Err(toyos_blockdev::Error::Device);
            }
            self.ram.read(lba, out).await
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), toyos_blockdev::Error> {
            self.ram.write(lba, data).await
        }
        async fn flush(&self) -> Result<(), toyos_blockdev::Error> {
            self.ram.flush().await
        }
    }

    impl Disk for Refusing {}

    /// `image`, padded to whole blocks, on a disk of `blocks` blocks.
    fn ram_with(image: &[u8], blocks: usize) -> Ram {
        let ram = Ram::new(blocks as u64);
        let mut padded = image.to_vec();
        padded.resize(image.len().div_ceil(BLOCK) * BLOCK, 0);
        block_on_ready(ram.write(0, padded.as_chunks::<BLOCK>().0)).unwrap();
        ram
    }

    /// The specification's fixture volume, writable, on a disk with room past
    /// it for [`evict`] to read, and the handle that moves its refused block.
    fn spec_fat() -> (FatVolume<Refusing>, Rc<Cell<u64>>, spec_volume::Volume) {
        spec_fat_on(|| 1_717_245_296 * NANOS_PER_SEC)
    }

    /// [`spec_fat`], stamping by `clock`.
    fn spec_fat_on(clock: fn() -> u64) -> (FatVolume<Refusing>, Rc<Cell<u64>>, spec_volume::Volume) {
        let spec = spec_volume::fixture();
        let ram = ram_with(&spec.bytes, spec.bytes.len().div_ceil(BLOCK) + 2 * CLEAN_LIMIT);
        let refused = Rc::new(Cell::new(u64::MAX));
        let v = FatVolume::mount(Refusing { ram, refused: Rc::clone(&refused) }, CLEAN_LIMIT, true, clock).unwrap();
        (v, refused, spec)
    }

    /// What [`a_files_mtime_is_its_last_write_s_through_its_handle_and_after_a_reopen`]'s
    /// volume reads as the time, which only that test sets.
    static NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// A FAT file's mtime is its last write's, kept to the even second at or
    /// before it (`FatTime`'s two-second resolution): through its handle
    /// before its entry is level, and read back off the disk after a sync
    /// later and a reopen, alike.
    #[test]
    fn a_files_mtime_is_its_last_write_s_through_its_handle_and_after_a_reopen() {
        use std::sync::atomic::Ordering::Relaxed;
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        let (mut v, _, _) = spec_fat_on(|| NOW.load(Relaxed));
        NOW.store(1_717_245_101 * NANOS_PER_SEC, Relaxed);
        let a = v.open("a.txt", CREATE).unwrap();
        NOW.store(1_717_245_297 * NANOS_PER_SEC, Relaxed);
        v.write(a, 0, &[1; 3000]).unwrap();
        let written = 1_717_245_296 * NANOS_PER_SEC;
        assert_eq!(v.node_meta(a).unwrap().mtime, written, "through the handle");
        NOW.store(1_717_245_301 * NANOS_PER_SEC + 500_000_000, Relaxed);
        assert_eq!(v.sync(), Ok(Vec::new()));
        v.close(a).unwrap();
        evict(&v);
        assert_eq!(v.lstat("a.txt").unwrap().mtime, written, "off the disk, a sync later");
        let again = v.open("a.txt", CREATE).unwrap();
        assert_eq!(v.node_meta(again).unwrap().mtime, written, "through a handle after a reopen");
    }

    /// What [`a_sync_of_files_nothing_changed_asks_the_disk_nothing`]'s volume
    /// reads as the time, which only that test sets.
    static LATER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Once a sync has made a file durable, a sync, an fsync or a close that
    /// finds it and every other file unchanged asks the disk for nothing and
    /// restamps no entry; the next write is stamped and written at its sync.
    #[test]
    fn a_sync_of_files_nothing_changed_asks_the_disk_nothing() {
        use std::sync::atomic::Ordering::Relaxed;
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        const OPEN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };
        let spec = spec_volume::fixture();
        let mut image = spec.bytes.clone();
        image.resize(spec.bytes.len().div_ceil(BLOCK) * BLOCK, 0);
        let epochs: Epochs = Rc::new(std::cell::RefCell::new(vec![Vec::new()]));
        let disk = Traced { image: Bytes::new(image), epochs: Rc::clone(&epochs) };
        LATER.store(1_717_245_296 * NANOS_PER_SEC, Relaxed);
        let mut v = FatVolume::mount(disk, CLEAN_LIMIT, true, || LATER.load(Relaxed)).unwrap();
        let a = v.open("a.txt", CREATE).unwrap();
        v.write(a, 0, &[1; 3000]).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let durable = epochs.borrow().clone();
        assert!(durable.last().is_some_and(Vec::is_empty), "the sync ended with a flush");

        LATER.store(1_717_245_310 * NANOS_PER_SEC, Relaxed);
        let stamped = v.lstat("short.txt").unwrap().mtime;
        let short = v.open("short.txt", OPEN).unwrap();
        let mut out = vec![0u8; 100];
        assert_eq!(v.read(short, 0, &mut crate::volume::Buf(&mut out)), Ok(100));
        v.close(short).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert!(*epochs.borrow() == durable, "a sync with nothing changed wrote or flushed the disk");
        assert_eq!(v.lstat("a.txt").unwrap().mtime, 1_717_245_296 * NANOS_PER_SEC);
        assert_eq!(v.lstat("short.txt").unwrap().mtime, stamped, "a file only read is not restamped");

        v.write(a, 0, &[2; 10]).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert_eq!(epochs.borrow().len(), durable.len() + 1, "the write's sync is one flush");
        v.close(a).unwrap();
        assert_eq!(epochs.borrow().len(), durable.len() + 1, "a close after the sync writes nothing");
        assert_eq!(v.lstat("a.txt").unwrap().mtime, 1_717_245_310 * NANOS_PER_SEC);
    }

    /// Every clean block out of the cache, so the next read of one asks the disk.
    fn evict(v: &FatVolume<Refusing>) {
        let blocks = v.cache.blocks();
        let mut out = vec![[0; BLOCK]; 2 * CLEAN_LIMIT];
        block_on_ready(v.cache.read(blocks - out.len() as u64, &mut out)).unwrap();
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

        assert_eq!(v.sync(), Ok(vec![(a, fs::Error::Io)]), "only the file that did not level is named");
        assert!(!v.open[&b].file.needs_reconcile(), "the other file was brought level");
        assert!(v.open[&a].file.needs_reconcile());
        assert_eq!(v.close(a), Err(fs::Error::Io), "a close that left the entry behind is refused");
        assert!(v.open.contains_key(&a), "and its node kept");
        assert_eq!(v.sync(), Ok(vec![(a, fs::Error::Io)]), "a sync the entry still refuses names it again");
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

        assert_eq!(v.lstat("a.txt"), Err(fs::Error::Io));
        assert_eq!(v.node_meta(a), Err(fs::Error::Io));
    }

    /// A file's number is one through every spelling of its name and across a
    /// rename, another file's is another, and a name removed or renamed over
    /// comes back as a new file.
    #[test]
    fn a_file_is_one_number_through_spellings_and_renames() {
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        let (mut v, _, _) = spec_fat();
        for path in ["a.txt", "sub/b.txt", "c.txt"] {
            let n = v.open(path, CREATE).unwrap();
            v.close(n).unwrap();
        }
        let a = v.identity("a.txt").unwrap();
        let b = v.identity("sub/b.txt").unwrap();
        let c = v.identity("c.txt").unwrap();
        let sub = v.identity("sub").unwrap();
        let root = v.identity("").unwrap();
        let all = [a, b, c, sub, root];
        assert!(all.iter().all(|&n| n != 0 && all.iter().filter(|&&m| m == n).count() == 1), "{all:?}");
        assert_eq!(v.identity("A.TXT"), Ok(a), "FAT names one entry by either case");
        assert_eq!(v.identity("SUB/b.TXT"), Ok(b));
        assert_eq!(v.identity("missing.txt"), Err(fs::Error::NotFound));

        v.rename("a.txt", "moved.txt").unwrap();
        assert_eq!(v.identity("moved.txt"), Ok(a), "a rename keeps the file's number");
        v.rename("sub/b.txt", "c.txt").unwrap();
        assert_eq!(v.identity("c.txt"), Ok(b), "a rename over another file takes its name, not its number");
        assert_eq!(v.identity("sub"), Ok(sub), "a directory is one file whatever moves out of it");

        v.unlink("moved.txt").unwrap();
        let n = v.open("moved.txt", CREATE).unwrap();
        v.close(n).unwrap();
        let again = v.identity("moved.txt").unwrap();
        assert!(!all.contains(&again), "a file made where one was removed is a new one: {again} of {all:?}");
    }

    /// A listing names each entry by the identity a stat of its path
    /// answers, asked before or after the stat.
    #[test]
    fn a_listing_names_each_entry_by_its_identity() {
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        let (mut v, _, _) = spec_fat();
        for path in ["a.txt", "sub/b.txt"] {
            let n = v.open(path, CREATE).unwrap();
            v.close(n).unwrap();
        }
        let b = v.identity("sub/b.txt").unwrap();
        for dir in ["", "sub"] {
            for e in v.list(dir).unwrap() {
                let path = if dir.is_empty() { e.name.clone() } else { format!("{dir}/{}", e.name) };
                assert_eq!(Ok(e.identity), v.identity(&path), "{path}");
            }
        }
        assert!(v.list("sub").unwrap().iter().any(|e| e.identity == b));
    }

    /// A disk whose grant does not write: every write and flush is refused.
    struct Granted(Ram);

    impl BlockDev for Granted {
        fn blocks(&self) -> u64 {
            self.0.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), toyos_blockdev::Error> {
            self.0.read(lba, out).await
        }
        async fn write(&self, _: u64, _: &[Block]) -> Result<(), toyos_blockdev::Error> {
            panic!("a read-only volume wrote to its disk")
        }
        async fn flush(&self) -> Result<(), toyos_blockdev::Error> {
            panic!("a read-only volume flushed its disk")
        }
    }

    impl Disk for Granted {}

    /// A volume mounted unwritable never asks its disk to write or flush, so
    /// the read-only grant's refusal never reaches `toyos-fat32`.
    #[test]
    fn a_read_only_volume_never_writes_its_disk() {
        const OPEN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };
        let spec = spec_volume::fixture();
        let ram = ram_with(&spec.bytes, spec.bytes.len().div_ceil(BLOCK));
        let mut v = FatVolume::mount(Granted(ram), CLEAN_LIMIT, false, || 1_717_245_296 * NANOS_PER_SEC).unwrap();

        assert_eq!(v.lstat("short.txt").unwrap().size, 100);
        v.list("sub").unwrap();
        let n = v.open("short.txt", OPEN).unwrap();
        let mut out = vec![0u8; 100];
        assert_eq!(v.read(n, 0, &mut crate::volume::Buf(&mut out)), Ok(100));
        assert!(out == spec.bytes[spec_volume::cluster_offset(spec.at("short.txt").first)..][..100], "the file reads as the fixture wrote it");
        v.close(n).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
    }

    /// The blocks a disk was written, in order, one epoch to each flush: the
    /// disk keeps no order among one epoch's writes, and has all of an epoch
    /// before any of the next.
    type Epochs = Rc<std::cell::RefCell<Vec<Vec<(u64, Vec<u8>)>>>>;

    /// A disk that keeps every block it is written in its flush epoch, so a
    /// test can stop it with any of one epoch's writes on it.
    struct Traced {
        image: Bytes<Vec<u8>>,
        epochs: Epochs,
    }

    impl BlockDev for Traced {
        fn blocks(&self) -> u64 {
            self.image.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), toyos_blockdev::Error> {
            self.image.read(lba, out).await
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), toyos_blockdev::Error> {
            {
                let mut epochs = self.epochs.borrow_mut();
                let epoch = epochs.last_mut().expect("an epoch is always open");
                for (block, data) in (lba..).zip(data) {
                    epoch.push((block, data.to_vec()));
                }
            }
            self.image.write(lba, data).await
        }
        async fn flush(&self) -> Result<(), toyos_blockdev::Error> {
            self.epochs.borrow_mut().push(Vec::new());
            self.image.flush().await
        }
    }

    impl Disk for Traced {}

    fn written<'a>(image: &mut [u8], writes: impl IntoIterator<Item = &'a (u64, Vec<u8>)>) {
        for (block, data) in writes {
            let at = *block as usize * BLOCK;
            image[at..at + BLOCK].copy_from_slice(data);
        }
    }

    /// An append and its sync, stopped with every flush epoch before one on
    /// the disk and any of that one's writes, leave a volume fatgen103's
    /// checker refuses only for clusters no file's size reaches, and that mounts and
    /// holds the file as the last whole sync left it or as the append made
    /// it: never an entry naming bytes its chain does not reach or its
    /// clusters do not hold yet. With every epoch on the disk the checker
    /// refuses nothing and the file is the append's.
    #[test]
    fn an_append_stopped_with_any_writes_of_a_flush_on_the_disk_reads_back_the_last_sync() {
        use toyos_fat32_check::{check, describe, Complaint};
        const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
        const OPEN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };
        let clock = || 1_717_245_296 * NANOS_PER_SEC;
        let spec = spec_volume::fixture();
        let mut image = spec.bytes.clone();
        image.resize(spec.bytes.len().div_ceil(BLOCK) * BLOCK, 0);
        let epochs: Epochs = Rc::new(std::cell::RefCell::new(vec![Vec::new()]));
        let mut v = FatVolume::mount(Traced { image: Bytes::new(image.clone()), epochs: Rc::clone(&epochs) }, CLEAN_LIMIT, true, clock)
            .unwrap();
        let held: Vec<u8> = (0..3000u32).map(|i| (i % 251 + 1) as u8).collect();
        let appended: Vec<u8> = (0..3 * BLOCK as u32).map(|i| (i % 241 + 2) as u8).collect();
        let a = v.open("a.txt", CREATE).unwrap();
        v.write(a, 0, &held).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let synced = epochs.borrow().len() - 1;
        v.write(a, held.len() as u64, &appended).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let epochs = epochs.borrow().clone();
        let whole = [&held[..], &appended[..]].concat();
        written(&mut image, epochs[..synced].iter().flatten());

        // Whether a stop between the chain's growth and the size's write was
        // reached, the state a reset mid-append leaves on the stick.
        let mut grown_before_its_size = false;
        for (e, epoch) in epochs.iter().enumerate().skip(synced) {
            for kept in 0..1u32 << epoch.len() {
                let stop = format!("stopped in flush epoch {e} of {} with writes {kept:#b} of its {}", epochs.len(), epoch.len());
                let mut bytes = image.clone();
                written(&mut bytes, epoch.iter().enumerate().filter(|(i, _)| kept >> i & 1 == 1).map(|(_, w)| w));
                let synced_whole = kept == (1 << epoch.len()) - 1 && epochs[e + 1..].iter().all(Vec::is_empty);
                // Clusters no entry reaches yet, and the last sync's entry over
                // the append's chain: the window between the chain and its
                // entry, closed once the sync is whole on the disk.
                let complaints = check(&bytes);
                grown_before_its_size |= complaints.iter().any(|c| matches!(c, Complaint::ChainTooLong { .. }));
                let refused: Vec<_> = complaints
                    .into_iter()
                    .filter(|c| match c {
                        _ if synced_whole => true,
                        Complaint::LostChain { .. } | Complaint::FsInfoFreeCount { .. } | Complaint::FatMirror { .. } => false,
                        Complaint::ChainTooLong { path, size, .. } => !(path == "/a.txt" && *size == held.len() as u64),
                        _ => true,
                    })
                    .collect();
                assert!(refused.is_empty(), "{stop}: {}", describe(&refused));
                let mut after = FatVolume::mount(Bytes::new(bytes), CLEAN_LIMIT, false, clock).unwrap_or_else(|e| panic!("{stop}: {e}"));
                let n = after.open("a.txt", OPEN).unwrap();
                let size = after.lstat("a.txt").unwrap().size as usize;
                let mut out = vec![0u8; size];
                let read = after.read(n, 0, &mut crate::volume::Buf(&mut out));
                assert_eq!(read, Ok(size), "{stop}: a {size}-byte file");
                assert!(
                    out == whole || out == held && !synced_whole,
                    "{stop}: the file is {size} bytes and neither the {} the last sync held nor the {} the append made",
                    held.len(),
                    whole.len()
                );
            }
            written(&mut image, epoch);
        }
        assert!(grown_before_its_size, "no stop fell between the chain's growth and the size's write");
    }

    /// What the driver says of a volume that stopped answering is `Io`, which
    /// no caller takes for a name that is not there.
    #[test]
    fn a_device_error_is_io_and_not_not_found() {
        assert_eq!(word(Error::Io), fs::Error::Io);
        assert_eq!(word(Error::NotFound), fs::Error::NotFound);
    }
}
