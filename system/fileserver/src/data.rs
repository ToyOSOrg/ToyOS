//! Writable bcachefs DATA. Descriptions hold inode identities; buffered
//! sizes and capacity reservations stay here until their pages are written.

use std::collections::BTreeMap;
use std::rc::Rc;
use bcachefs::engine::{self, DirentType, Mount, MountOptions, New, Owner, Reservation};
use toyos::fs::Error;
use toyos_blockdev::{BlockDev, BLOCK};
use crate::cache::{Cache, Disk};
use crate::volume::{parent, Kind, Listed, Meta, Node, OpenHow, Out, Reserved, Volume};

const NODES: usize = 512;
const READ_CHUNK: usize = 256 << 10;
const OWNER: Owner = Owner { perm: 0o644, uid: 0, gid: 0 };
struct OsRandom;
impl toyos_crypto::Random for OsRandom {
    fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        getrandom::fill(out).map_err(|_| toyos_crypto::Error::Entropy)
    }
}
struct Pending { ticket: u64, reservation: Reservation }
struct Open {
    holders: usize,
    size: u64,
    mtime: u64,
    changed: bool,
    pages: BTreeMap<u64, Pending>,
}
pub struct DataVolume<D: Disk> {
    fs: Mount<Rc<Cache<D>>>,
    cache: Rc<Cache<D>>,
    roots: Vec<String>,
    open: BTreeMap<Node, Open>,
    next_ticket: u64,
    clock: fn() -> u64,
}
pub enum Probed<D: Disk> { Mounted(DataVolume<D>), Unmountable(String), Foreign }
#[derive(Debug, PartialEq, Eq)]
pub enum Located { Nowhere, Served([u8; 16]), Refused(String) }
pub fn find(served: Result<&[[u8; 16]], String>) -> Located {
    match served {
        Err(why) => Located::Refused(why),
        Ok([]) => Located::Nowhere,
        Ok([one]) => Located::Served(*one),
        Ok(served) => Located::Refused(format!("this machine has {} DATA partitions; none was selected", served.len())),
    }
}
fn word(e: engine::Error) -> Error {
    match e {
        engine::Error::NotFound => Error::NotFound,
        engine::Error::NotDirectory => Error::NotADirectory,
        engine::Error::Exists => Error::AlreadyExists,
        engine::Error::IsDirectory => Error::IsADirectory,
        engine::Error::NotEmpty => Error::NotEmpty,
        engine::Error::Stale => Error::Stale,
        engine::Error::NoSpace(_) => Error::NoSpace,
        engine::Error::ReadOnly(_) | engine::Error::Device(toyos_blockdev::Error::ReadOnly) => Error::ReadOnly,
        engine::Error::Invalid(_) => Error::InvalidName,
        engine::Error::Unsupported(_) => Error::Unsupported,
        _ => Error::Io,
    }
}
fn mapped<T>(operation: &str, result: engine::Result<T>) -> Result<T, Error> {
    result.map_err(|error| { println!("fileserver: DATA {operation}: {error:?}"); word(error) })
}
fn kind(inode: &engine::Inode) -> Result<Kind, Error> {
    match inode.kind() {
        Some(DirentType::Regular) => Ok(Kind::File),
        Some(DirentType::Directory) => Ok(Kind::Dir),
        Some(DirentType::Symlink) => Ok(Kind::Symlink),
        _ => Err(Error::Unsupported),
    }
}
fn time(value: u64) -> i64 { value.min(i64::MAX as u64) as i64 }
impl<D: Disk> DataVolume<D> {
    pub fn probe(disk: D, clean_limit: usize, roots: &[&str], clock: fn() -> u64) -> Probed<D> {
        let cache = Rc::new(Cache::new(disk, clean_limit));
        let first = cache.block_on(Mount::open(Rc::clone(&cache), MountOptions { cache_nodes: NODES }, Box::new(OsRandom)));
        let fs = match first {
            Ok(fs) => fs,
            Err(error) => {
                let mut block = [[0; BLOCK]; 2];
                if let Err(read) = cache.block_on(cache.read(0, &mut block)) {
                    return Probed::Unmountable(format!("mount {error:?}; designation read {read:?}"));
                }
                if bcachefs::designated(&block[0]) != Some(cache.blocks()) {
                    return if bcachefs::carries_volume(block.as_flattened()) {
                        Probed::Unmountable(format!("{error:?}; an existing volume cannot be reformatted"))
                    } else { Probed::Foreign };
                }
                println!("fileserver: formatting the explicitly designated DATA partition as bcachefs");
                let options = engine::Options { label: b"ToyOS DATA", time: clock(), encryption: None };
                if let Err(error) = cache.block_on(engine::mkfs(&cache, &options, &engine::Tree::new(), &mut OsRandom)) {
                    return Probed::Unmountable(format!("format: {error:?}"));
                }
                match cache.block_on(Mount::open(Rc::clone(&cache), MountOptions { cache_nodes: NODES }, Box::new(OsRandom))) {
                    Ok(fs) => fs,
                    Err(error) => return Probed::Unmountable(format!("new volume: {error:?}")),
                }
            }
        };
        Self::over(fs, cache, roots, clock).map_or_else(Probed::Unmountable, Probed::Mounted)
    }
    fn over(fs: Mount<Rc<Cache<D>>>, cache: Rc<Cache<D>>, roots: &[&str], clock: fn() -> u64) -> Result<Self, String> {
        // Mount recovery precedes every live description.
        cache.block_on(fs.release_orphans()).map_err(|error| format!("orphan recovery: {error:?}"))?;
        let mut volume = Self { fs, cache, roots: roots.iter().map(|root| root.to_string()).collect(), open: BTreeMap::new(), next_ticket: 1, clock };
        for root in roots {
            let mut current = String::new();
            for part in root.split('/').filter(|part| !part.is_empty()) {
                if !current.is_empty() { current.push('/'); }
                current.push_str(part);
                match volume.lstat(&current) {
                    Ok(meta) if meta.kind == Kind::Dir => {}
                    Err(Error::NotFound) => volume.mkdir(&current).map_err(|error| format!("root {current}: {error:?}"))?,
                    other => return Err(format!("root {current} is not a directory: {other:?}")),
                }
            }
        }
        volume.cache.block_on(volume.fs.commit()).map_err(|error| format!("mount commit: {error:?}"))?;
        Ok(volume)
    }
    fn lookup(&self, path: &str) -> Result<engine::Inode, Error> {
        self.cache.block_on(async {
            let mut ino = self.fs.root();
            for name in path.split('/').filter(|name| !name.is_empty()) {
                ino = self.fs.lookup(ino, name.as_bytes()).await?.ok_or(engine::Error::NotFound)?.ino;
            }
            self.fs.inode(ino).await
        }).map_err(word)
    }
    fn destination<'a>(&self, path: &'a str) -> Result<(u64, &'a [u8]), Error> {
        let name = path.rsplit('/').next().filter(|name| !name.is_empty()).ok_or(Error::Root)?;
        let dir = self.lookup(parent(path))?;
        if !dir.is_dir() { return Err(Error::NotADirectory); }
        Ok((dir.ino, name.as_bytes()))
    }
    fn meta(&self, inode: &engine::Inode) -> Result<Meta, Error> {
        let kind = kind(inode)?;
        let (size, mtime) = self.open.get(&inode.ino).map(|open| (open.size, open.mtime)).unwrap_or((inode.size, inode.mtime.max(0) as u64));
        Ok(Meta { kind, size: if kind == Kind::Dir { 0 } else { size }, mtime })
    }
    fn protected(&self, path: &str) -> bool { path.is_empty() || self.roots.iter().any(|root| root == path) }
    fn released(&self, removed: Option<engine::Removed>) -> Result<(), Error> {
        if let Some(removed) = removed.filter(|removed| removed.last_link && !self.open.contains_key(&removed.node)) {
            match self.cache.block_on(self.fs.inode(removed.node)) {
                Ok(inode) if !inode.is_dir() => mapped("release", self.cache.block_on(self.fs.release(removed.node)))?,
                Ok(_) | Err(engine::Error::NotFound) => {}
                Err(error) => return Err(word(error)),
            }
        }
        Ok(())
    }
    fn persist(&mut self, node: Node) -> Result<(), Error> {
        let open = self.open.get(&node).ok_or(Error::Stale)?;
        if !open.pages.is_empty() { return Err(Error::Io); }
        if !open.changed { return Ok(()); }
        let (size, mtime) = (open.size, open.mtime);
        mapped("length", self.cache.block_on(self.fs.truncate(node, size, time(mtime))))?;
        mapped("mtime", self.cache.block_on(self.fs.set_mtime(node, time(mtime))))?;
        self.open.get_mut(&node).expect("open inode").changed = false;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn format(disk: D, clean_limit: usize, roots: &[&str], clock: fn() -> u64) -> Result<Self, String> {
        let cache = Rc::new(Cache::new(disk, clean_limit));
        let options = engine::Options { label: b"test DATA", time: clock(), encryption: None };
        cache.block_on(engine::mkfs(&cache, &options, &engine::Tree::new(), &mut OsRandom)).map_err(|error| format!("{error:?}"))?;
        let fs = cache.block_on(Mount::open(Rc::clone(&cache), MountOptions { cache_nodes: NODES }, Box::new(OsRandom))).map_err(|error| format!("{error:?}"))?;
        Self::over(fs, cache, roots, clock)
    }
    #[cfg(test)]
    pub(crate) fn stopped(self) -> D {
        let Self { fs, cache, .. } = self;
        drop(fs);
        Rc::try_unwrap(cache).ok().expect("mount dropped its device").into_disk()
    }
}
impl<D: Disk> Volume for DataVolume<D> {
    fn writable(&self) -> bool { true }
    fn lstat(&mut self, path: &str) -> Result<Meta, Error> { self.meta(&self.lookup(path)?) }
    fn read_link(&mut self, path: &str) -> Result<String, Error> {
        let inode = self.lookup(path)?;
        if kind(&inode)? != Kind::Symlink { return Err(Error::NotALink); }
        let bytes = mapped("readlink", self.cache.block_on(self.fs.readlink(inode.ino)))?;
        String::from_utf8(bytes).map_err(|_| Error::Io)
    }
    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error> {
        let inode = self.lookup(dir)?;
        if !inode.is_dir() { return Err(Error::NotADirectory); }
        let mut cursor = 0;
        let mut listed = Vec::new();
        loop {
            let (entries, more) = mapped("readdir", self.cache.block_on(self.fs.readdir(inode.ino, cursor, 256)))?;
            if entries.is_empty() && more { return Err(Error::Io); }
            for entry in entries {
                cursor = entry.next;
                let inode = mapped("directory entry", self.cache.block_on(self.fs.inode(entry.ino)))?;
                listed.push(Listed { name: String::from_utf8(entry.name).map_err(|_| Error::Io)?, meta: self.meta(&inode)?, identity: entry.ino });
            }
            if !more { break; }
        }
        listed.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(listed)
    }
    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, Error> {
        let inode = match self.lookup(path) {
            Ok(_) if how.create_new => return Err(Error::AlreadyExists),
            Ok(inode) => inode,
            Err(Error::NotFound) if how.create || how.create_new => {
                let (dir, name) = self.destination(path)?;
                mapped("create", self.cache.block_on(self.fs.create(dir, name, New::File, OWNER, time((self.clock)()))))?
            }
            Err(error) => return Err(error),
        };
        match kind(&inode)? { Kind::Dir => return Err(Error::IsADirectory), Kind::Symlink => return Err(Error::NotFound), Kind::File => {} }
        let node = inode.ino;
        self.open.entry(node).or_insert(Open { holders: 0, size: inode.size, mtime: inode.mtime.max(0) as u64, changed: false, pages: BTreeMap::new() }).holders += 1;
        if how.truncate {
            if let Err(error) = self.truncate(node, 0) { let _ = self.close(node); return Err(error); }
        }
        Ok(node)
    }
    fn close(&mut self, node: Node) -> Result<(), Error> {
        let Some(open) = self.open.get_mut(&node) else { return Ok(()) };
        open.holders = open.holders.checked_sub(1).expect("one close per open");
        if open.holders != 0 { return Ok(()); }
        let inode = mapped("close", self.cache.block_on(self.fs.inode(node)))?;
        if inode.nlink == 0 {
            self.open.get_mut(&node).unwrap().pages.clear();
            mapped("orphan close", self.cache.block_on(self.fs.release(node)))?;
        } else { self.persist(node)?; }
        self.open.remove(&node);
        Ok(())
    }
    fn node_meta(&mut self, node: Node) -> Result<Meta, Error> {
        let open = self.open.get(&node).ok_or(Error::Stale)?;
        Ok(Meta { kind: Kind::File, size: open.size, mtime: open.mtime })
    }
    fn linked(&mut self, node: Node) -> Result<bool, Error> { Ok(mapped("link count", self.cache.block_on(self.fs.inode(node)))?.nlink != 0) }
    fn identity(&mut self, path: &str) -> Result<u64, Error> { Ok(self.lookup(path)?.ino) }
    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, Error> {
        let size = self.open.get(&node).ok_or(Error::Stale)?.size;
        let len = out.len().min(size.saturating_sub(offset).min(usize::MAX as u64) as usize);
        let mut bytes = vec![0; len.min(READ_CHUNK)];
        let mut done = 0;
        while done < len {
            let take = bytes.len().min(len - done);
            bytes[..take].fill(0);
            mapped("read", self.cache.block_on(self.fs.read(node, offset + done as u64, &mut bytes[..take])))?;
            out.put(done, &bytes[..take]);
            done += take;
        }
        Ok(len)
    }
    fn reserve(&mut self, node: Node, offset: u64, len: u64) -> Result<Reserved, Error> {
        let end = offset.checked_add(len).ok_or(Error::FileTooLarge)?;
        let open = self.open.get(&node).ok_or(Error::Stale)?;
        let ticket = self.next_ticket;
        self.next_ticket = ticket.checked_add(1).expect("reservation generation exhausted");
        let reserved = Reserved { size: open.size, contents: open.size, ticket };
        if len == 0 { return Ok(reserved); }
        for page in offset / BLOCK as u64..end.div_ceil(BLOCK as u64) {
            if self.open[&node].pages.contains_key(&page) { continue; }
            match self.cache.block_on(self.fs.reserve(node, page * BLOCK as u64, BLOCK)) {
                Ok(reservation) => { self.open.get_mut(&node).unwrap().pages.insert(page, Pending { ticket, reservation }); }
                Err(error) => { self.unreserve(node, reserved, 0)?; return Err(word(error)); }
            }
        }
        let open = self.open.get_mut(&node).unwrap();
        open.size = open.size.max(end);
        open.mtime = (self.clock)();
        open.changed = true;
        Ok(reserved)
    }
    fn unreserve(&mut self, node: Node, reserved: Reserved, landed: u64) -> Result<(), Error> {
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        let cancelled: Vec<_> = open.pages.iter().filter_map(|(&page, pending)| (pending.ticket == reserved.ticket && page * BLOCK as u64 >= landed).then_some(page)).collect();
        for page in cancelled { open.pages.remove(&page); }
        open.size = reserved.size.max(landed);
        Ok(())
    }
    fn write_back(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), Error> {
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        if !offset.is_multiple_of(BLOCK as u64) || offset.checked_add(data.len() as u64).is_none_or(|end| end > open.size) { return Err(Error::InvalidRequest); }
        for (i, bytes) in data.chunks(BLOCK).enumerate() {
            let page = offset / BLOCK as u64 + i as u64;
            let pending = open.pages.get_mut(&page).ok_or(Error::Io)?;
            let written = mapped("writeback", self.cache.block_on(self.fs.write_reserved(&mut pending.reservation, page * BLOCK as u64, bytes, time(open.mtime))))?;
            if written != bytes.len() { return Err(Error::Io); }
            open.pages.remove(&page);
        }
        Ok(())
    }
    fn truncate(&mut self, node: Node, size: u64) -> Result<(), Error> {
        let open = self.open.get(&node).ok_or(Error::Stale)?;
        if open.size == size { return Ok(()); }
        let now = (self.clock)();
        mapped("truncate", self.cache.block_on(self.fs.truncate(node, size, time(now))))?;
        let open = self.open.get_mut(&node).unwrap();
        open.pages.split_off(&size.div_ceil(BLOCK as u64));
        open.size = size;
        open.mtime = now;
        open.changed = true;
        Ok(())
    }
    fn set_mtime(&mut self, node: Node, mtime: u64) -> Result<(), Error> {
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        open.mtime = mtime;
        open.changed = true;
        Ok(())
    }
    fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        let (dir, name) = self.destination(path)?;
        mapped("mkdir", self.cache.block_on(self.fs.create(dir, name, New::Dir, Owner { perm: 0o755, ..OWNER }, time((self.clock)())))).map(drop)
    }
    fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        if self.protected(path) { return Err(Error::Root); }
        if !self.lookup(path)?.is_dir() { return Err(Error::NotADirectory); }
        let (dir, name) = self.destination(path)?;
        mapped("rmdir", self.cache.block_on(self.fs.unlink(dir, name, true, time((self.clock)())))).map(drop)
    }
    fn unlink(&mut self, path: &str) -> Result<(), Error> {
        if self.protected(path) { return Err(Error::Root); }
        let (dir, name) = self.destination(path)?;
        let removed = mapped("unlink", self.cache.block_on(self.fs.unlink(dir, name, false, time((self.clock)()))))?;
        self.released(Some(removed))
    }
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        if self.protected(from) || self.protected(to) { return Err(Error::Root); }
        let source = self.destination(from)?;
        let target = self.destination(to)?;
        let removed = mapped("rename", self.cache.block_on(self.fs.rename(source, target, engine::Rename::Replace, time((self.clock)()))))?;
        self.released(removed)
    }
    fn symlink(&mut self, path: &str, target: &str) -> Result<(), Error> {
        let (dir, name) = self.destination(path)?;
        mapped("symlink", self.cache.block_on(self.fs.create(dir, name, New::Symlink(target.as_bytes().to_vec()), OWNER, time((self.clock)())))).map(drop)
    }
    fn sync(&mut self) -> Result<Vec<(Node, Error)>, Error> {
        let nodes: Vec<_> = self.open.keys().copied().collect();
        let mut refused = Vec::new();
        for node in nodes {
            match self.persist(node) {
                Err(error) => refused.push((node, error)),
                Ok(()) if self.open[&node].holders == 0 => {
                    let inode = mapped("unheld inode", self.cache.block_on(self.fs.inode(node)))?;
                    if inode.nlink == 0 { mapped("orphan release", self.cache.block_on(self.fs.release(node)))?; }
                    self.open.remove(&node);
                }
                Ok(()) => {}
            }
        }
        mapped("sync", self.cache.block_on(self.fs.commit()))?;
        Ok(refused)
    }
    fn owes(&self) -> bool { self.open.values().any(|open| open.changed || !open.pages.is_empty()) }
    fn commit(&mut self, node: Option<Node>) -> Result<(), Error> {
        if let Some(node) = node { self.persist(node)?; }
        mapped("fsync", self.cache.block_on(self.fs.commit()))
    }
    fn room(&mut self) -> Result<toyos::fs::Room, Error> { Ok(toyos::fs::Room { free: self.fs.space().1, unit: BLOCK as u64 }) }
    fn describe(&self) -> String {
        let (total, free) = self.fs.space();
        format!("bcachefs DATA: {free}/{total} bytes free, {} open inodes", self.open.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ram::Ram;
    use crate::volume::{Buf, Writes};
    const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
    const EXISTING: OpenHow = OpenHow { create: false, create_new: false, truncate: false };
    fn clock() -> u64 { 1_700_000_000_000_000_000 }
    fn volume() -> DataVolume<Ram> {
        DataVolume::format(Ram::new(16384), 256, &["home", "apps"], clock).unwrap()
    }
    fn read(volume: &mut DataVolume<Ram>, node: Node) -> Vec<u8> {
        let mut bytes = vec![0xa5; volume.node_meta(node).unwrap().size as usize];
        assert_eq!(volume.read(node, 0, &mut Buf(&mut bytes)).unwrap(), bytes.len());
        bytes
    }
    fn reopen(volume: DataVolume<Ram>) -> DataVolume<Ram> {
        match DataVolume::probe(volume.stopped(), 256, &["home", "apps"], clock) {
            Probed::Mounted(volume) => volume,
            _ => panic!("committed DATA must mount"),
        }
    }
    #[test]
    fn sparse_data_and_shrunk_tails_survive_remount() {
        let mut v = volume();
        let f = v.open("home/f", CREATE).unwrap();
        v.write(f, 2 * BLOCK as u64 + 17, b"value").unwrap();
        let bytes = read(&mut v, f);
        assert!(bytes[..2 * BLOCK + 17].iter().all(|&b| b == 0));
        assert_eq!(&bytes[2 * BLOCK + 17..], b"value");
        v.truncate(f, 2 * BLOCK as u64 + 19).unwrap();
        v.truncate(f, 3 * BLOCK as u64).unwrap();
        v.sync().unwrap();
        let mut v = reopen(v);
        let f = v.open("home/f", EXISTING).unwrap();
        let bytes = read(&mut v, f);
        assert_eq!(&bytes[2 * BLOCK + 17..2 * BLOCK + 19], b"va");
        assert!(bytes[2 * BLOCK + 19..].iter().all(|&b| b == 0));
    }
    #[test]
    fn unlink_and_replacement_keep_live_inode_identity() {
        let mut v = volume();
        let old = v.open("home/name", CREATE).unwrap();
        v.write(old, 0, b"old").unwrap();
        let new = v.open("home/new", CREATE).unwrap();
        v.write(new, 0, b"new").unwrap();
        v.rename("home/new", "home/name").unwrap();
        assert_eq!(v.identity("home/name").unwrap(), new);
        assert!(!v.linked(old).unwrap());
        assert_eq!(read(&mut v, old), b"old");
        v.write(old, 0, b"OLD").unwrap();
        assert_eq!(read(&mut v, new), b"new");
        v.unlink("home/name").unwrap();
        assert_eq!(v.lstat("home/name"), Err(Error::NotFound));
        assert_eq!(read(&mut v, new), b"new");
        v.close(old).unwrap();
        v.close(new).unwrap();
        v.sync().unwrap();
        let mut v = reopen(v);
        assert!(v.list("home").unwrap().is_empty());
    }
    #[test]
    fn directory_moves_preserve_descendants_and_symlinks() {
        let mut v = volume();
        v.mkdir("home/dir").unwrap();
        let f = v.open("home/dir/file", CREATE).unwrap();
        v.write(f, 0, b"kept").unwrap();
        v.symlink("home/dir/link", "file").unwrap();
        v.rename("home/dir", "home/moved").unwrap();
        assert_eq!(v.identity("home/moved/file").unwrap(), f);
        assert_eq!(v.read_link("home/moved/link").unwrap(), "file");
        assert_eq!(read(&mut v, f), b"kept");
        assert_eq!(v.rmdir("home/moved"), Err(Error::NotEmpty));
        assert_eq!(v.list("home").unwrap()[0].name, "moved");
        v.sync().unwrap();
        let mut v = reopen(v);
        assert_eq!(v.read_link("home/moved/link").unwrap(), "file");
    }
    #[test]
    fn cancelling_one_reservation_keeps_an_earlier_dirty_page() {
        let mut v = volume();
        let f = v.open("home/f", CREATE).unwrap();
        v.reserve(f, 0, 4).unwrap();
        let before = v.room().unwrap().free;
        let later = v.reserve(f, BLOCK as u64, 4).unwrap();
        v.unreserve(f, later, 0).unwrap();
        assert_eq!(v.room().unwrap().free, before);
        assert_eq!(v.node_meta(f).unwrap().size, 4);
        v.write_back(f, 0, b"kept").unwrap();
        v.commit(Some(f)).unwrap();
        let mut v = reopen(v);
        let f = v.open("home/f", EXISTING).unwrap();
        assert_eq!(read(&mut v, f), b"kept");
    }
    #[test]
    fn designation_is_required_before_any_format_write() {
        let disk = Ram::new(16384);
        assert!(matches!(DataVolume::probe(disk, 256, &["home"], clock), Probed::Foreign));
        let disk = Ram::new(16384);
        toyos_blockdev::block_on_ready(disk.write(0, &[bcachefs::designation(16384)])).unwrap();
        assert!(matches!(DataVolume::probe(disk, 256, &["home"], clock), Probed::Mounted(_)));
    }
}
