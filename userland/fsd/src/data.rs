//! The DATA role's volume: the bcachefs crate's format, read and written
//! through the server's cache.
//!
//! **The format's namespace is flat**: one entry per file or symlink, keyed by
//! its whole path, and a directory is only a prefix of those. So a directory
//! nothing is under yet is kept as an entry of its own, its path and a
//! trailing `/`, empty — which is what makes it outlive the server and the
//! boot — and a directory with anything under it is one whether or not that
//! entry exists, since volumes written before these entries carry none.
//!
//! **Every name is indexed in memory at mount** ([`DataVolume::names`]), so a
//! lookup, a listing and "is this a directory" are a map query and never the
//! walk of the whole tree the format's own `is_dir` is.
//!
//! **A file's data lives in the blocks its extents name, in the cache.** A
//! write resolves (allocating where it must) the block of each page it
//! touches and writes the page into the cache; its new length and extents
//! reach the file's entry at its close, an fsync, or the volume's sync, and a
//! sync writes every such entry before the cache is flushed. A length that
//! shrinks is recorded before the blocks past it are freed, so a failure
//! between the two leaks blocks rather than leaving an entry naming freed ones.
//!
//! **What a kill costs.** The format updates its btree in place and keeps no
//! journal (`issues/kernel/bcachefs-crate-is-not-bcachefs.md`): what the disk
//! holds is what the last sync wrote, and a server that dies inside a sync can
//! leave a node half of that sync's. Nothing but a sync writes a dirty block,
//! unless the cache is holding more than it keeps.
//!
//! Every name a client chose is bounded by the format ([`FsError::NameTooLong`])
//! before it reaches the tree.

use std::collections::BTreeMap;
use std::rc::Rc;

use bcachefs::{Extent, Formatted, FsError, Mounted, ReadWrite};
use toyos_abi::syscall::SyscallError;

use crate::cache::{Cache, Shared};
use crate::disk::{Disk, DiskError, BLOCK};
use crate::volume::{join, parent, Kind, Meta, Node, OpenHow, Volume};

/// The longest symlink target read back: the wire's path bound.
const MAX_LINK: u64 = toyos::fs::MAX_PATH as u64;

/// One file open, while any client holds it.
struct Open {
    path: String,
    extents: Vec<Extent>,
    size: u64,
    mtime: u64,
    /// Length or extents changed since the entry was last written.
    dirty: bool,
    /// Unlinked or renamed over: its blocks are no longer its own.
    gone: bool,
    holders: u32,
}

pub struct DataVolume<D: Disk> {
    fs: Mounted<Shared<D>, ReadWrite>,
    cache: Rc<Cache<D>>,
    /// Every name on the volume, a directory entry's without its `/`.
    names: BTreeMap<String, Kind>,
    /// Directories that exist whatever the volume holds: each connection's root.
    roots: Vec<String>,
    open: BTreeMap<Node, Open>,
    by_path: BTreeMap<String, Node>,
    next: Node,
    clock: fn() -> u64,
}

/// What a DATA partition turned out to be, as the kernel's mount decided it.
pub enum Probed<D: Disk> {
    /// A volume of ours, mounted, or a designated one freshly formatted.
    Mounted(DataVolume<D>),
    /// A volume of ours that does not mount; nothing stands in and nothing is
    /// written to it.
    Unmountable(String),
    /// No volume of ours and no designation stamp: never written.
    Foreign,
}

fn io(e: &FsError) -> SyscallError {
    match e {
        FsError::NotFound => SyscallError::NotFound,
        FsError::NoSpace { .. } | FsError::EntryTooLarge { .. } | FsError::ListTooLong { .. } => {
            SyscallError::ResourceExhausted
        }
        FsError::NameTooLong { .. } => SyscallError::InvalidArgument,
        FsError::DeviceRead(..)
        | FsError::DeviceWrite(..)
        | FsError::DeviceSync(_)
        | FsError::BadMagic { .. }
        | FsError::UnsupportedVersion(_)
        | FsError::ChecksumMismatch { .. }
        | FsError::CorruptedKey(_)
        | FsError::CorruptedNode(_)
        | FsError::BlockOffDevice { .. }
        | FsError::NotEnoughBlocks { .. }
        | FsError::TreeTooDeep(_)
        | FsError::BadSuperblock { .. }
        | FsError::NodeOverfull { .. }
        | FsError::TargetTooLong { .. } => SyscallError::Io,
    }
}

/// Logs the format's own account, answers the word a client can act on.
fn mapped<T>(what: &str, path: &str, r: Result<T, FsError>) -> Result<T, SyscallError> {
    r.map_err(|e| {
        println!("fsd: {what} of '{path}' failed: {e:?}");
        io(&e)
    })
}

fn disk_word(e: DiskError) -> SyscallError {
    match e {
        DiskError::Device | DiskError::Range => SyscallError::Io,
        DiskError::Gone => SyscallError::Gone,
    }
}

/// The block holding page `page`, if the extents reach it.
fn block_for(extents: &[Extent], page: u64) -> Option<u64> {
    let mut at = 0u64;
    for e in extents {
        let end = at.checked_add(e.block_count as u64)?;
        if page < end {
            return e.start_block.checked_add(page - at);
        }
        at = end;
    }
    None
}

/// Keep the first `keep` blocks of `extents`; answer the runs dropped.
fn keep_blocks(extents: &mut Vec<Extent>, keep: u64) -> Vec<Extent> {
    let mut dropped = Vec::new();
    let mut left = keep;
    let mut kept = Vec::with_capacity(extents.len());
    for e in extents.drain(..) {
        let n = e.block_count as u64;
        if left >= n {
            left -= n;
            kept.push(e);
        } else {
            if left > 0 {
                kept.push(Extent { start_block: e.start_block, block_count: left as u32, _reserved: 0 });
            }
            dropped.push(Extent { start_block: e.start_block + left, block_count: (n - left) as u32, _reserved: 0 });
            left = 0;
        }
    }
    *extents = kept;
    dropped
}

/// Whether block 0 carries a designation stamp naming this disk's size.
fn designated<D: Disk>(cache: &Cache<D>) -> bool {
    let mut block0 = [0u8; BLOCK];
    if cache.read_block(0, &mut block0).is_err() {
        println!("fsd: block 0 would not read; this disk is not ours to format");
        return false;
    }
    let magic = bcachefs::DESIGNATION_MAGIC;
    let at = bcachefs::DESIGNATION_BLOCKS_OFFSET;
    if block0[..magic.len()] != magic {
        return false;
    }
    let stamped = u64::from_le_bytes(block0[at..at + 8].try_into().expect("eight bytes"));
    if stamped != cache.blocks() {
        println!(
            "fsd: a designation stamp at block 0 names {stamped} blocks, and this partition has {}; ignoring it",
            cache.blocks()
        );
        return false;
    }
    true
}

impl<D: Disk> DataVolume<D> {
    /// Mount the volume on `disk`, format it if it is designated, and refuse
    /// it otherwise — the kernel's DATA mount's decisions, unchanged.
    pub fn probe(disk: D, roots: &[&str], clock: fn() -> u64) -> Probed<D> {
        let cache = Rc::new(Cache::new(disk));
        match Mounted::<Shared<D>, ReadWrite>::open(Shared(Rc::clone(&cache))) {
            Ok(fs) => {
                println!("fsd: mounted the DATA volume");
                return Self::over(fs, cache, roots, clock).map_or_else(Probed::Unmountable, Probed::Mounted);
            }
            Err(e) if e.disowns_volume() => {}
            Err(e) => return Probed::Unmountable(format!("{e:?}")),
        }
        if !designated(&cache) {
            println!("fsd: no volume of ours and no designation stamp; nothing will be written to this partition");
            return Probed::Foreign;
        }
        println!("fsd: block 0 designates this partition for ToyOS; formatting it");
        match Formatted::format(Shared(Rc::clone(&cache))) {
            Ok(fs) => Self::over(fs.mount(), cache, roots, clock).map_or_else(Probed::Unmountable, Probed::Mounted),
            Err(e) => Probed::Unmountable(format!("the format failed: {e:?}")),
        }
    }

    /// A fresh volume on `disk`, which holds nothing of anyone's: memory.
    pub fn format(disk: D, roots: &[&str], clock: fn() -> u64) -> Result<Self, String> {
        let cache = Rc::new(Cache::new(disk));
        let fs = Formatted::format(Shared(Rc::clone(&cache))).map_err(|e| format!("{e:?}"))?;
        Self::over(fs.mount(), cache, roots, clock)
    }

    fn over(
        fs: Mounted<Shared<D>, ReadWrite>,
        cache: Rc<Cache<D>>,
        roots: &[&str],
        clock: fn() -> u64,
    ) -> Result<Self, String> {
        let listed = fs.list(usize::MAX, &|_| true).map_err(|e| format!("the volume would not list: {e:?}"))?;
        let mut names = BTreeMap::new();
        for (name, _) in listed {
            let kind = match name.strip_suffix('/') {
                Some(dir) => {
                    names.insert(dir.to_string(), Kind::Dir);
                    continue;
                }
                None if fs.is_symlink(&name).map_err(|e| format!("{e:?}"))? => Kind::Symlink,
                None => Kind::File,
            };
            names.insert(name, kind);
        }
        Ok(Self {
            fs,
            cache,
            names,
            roots: roots.iter().map(|r| r.to_string()).collect(),
            open: BTreeMap::new(),
            by_path: BTreeMap::new(),
            next: 1,
            clock,
        })
    }

    /// Whether anything is named beneath `dir`.
    fn has_children(&self, dir: &str) -> bool {
        let prefix = format!("{dir}/");
        self.names.range(prefix.clone()..).next().is_some_and(|(name, _)| name.starts_with(&prefix))
    }

    fn is_dir(&self, path: &str) -> bool {
        path.is_empty()
            || self.roots.iter().any(|r| r == path)
            || self.names.get(path) == Some(&Kind::Dir)
            || (!self.names.contains_key(path) && self.has_children(path))
    }

    fn entry(&mut self, node: Node) -> Result<&mut Open, SyscallError> {
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        Ok(open)
    }

    /// Write one open file's length and extents to its entry.
    fn persist(&mut self, node: Node) -> Result<(), SyscallError> {
        let Some(open) = self.open.get_mut(&node) else { return Ok(()) };
        if !open.dirty || open.gone {
            return Ok(());
        }
        mapped("the entry", &open.path, self.fs.update_metadata(&open.path, &open.extents, open.size, open.mtime))?;
        open.dirty = false;
        Ok(())
    }

    /// Hand every open file at `path` its end: its blocks go with the name.
    fn orphan(&mut self, path: &str) {
        if let Some(node) = self.by_path.remove(path) {
            if let Some(open) = self.open.get_mut(&node) {
                open.gone = true;
            }
        }
    }

    fn new_node(&mut self, path: &str, extents: Vec<Extent>, size: u64, mtime: u64) -> Node {
        let node = self.next;
        self.next += 1;
        self.open.insert(node, Open { path: path.to_string(), extents, size, mtime, dirty: false, gone: false, holders: 0 });
        self.by_path.insert(path.to_string(), node);
        node
    }

    /// A name is made under directories that need not exist yet — the format
    /// makes every prefix one, as the kernel's mount did — but never under a
    /// file or a symlink, which would then be two things at once.
    fn require_parent_dir(&self, path: &str) -> Result<(), SyscallError> {
        let mut dir = parent(path);
        while !dir.is_empty() {
            if matches!(self.names.get(dir), Some(Kind::File | Kind::Symlink)) {
                return Err(SyscallError::NotFound);
            }
            dir = parent(dir);
        }
        Ok(())
    }
}

impl<D: Disk> Volume for DataVolume<D> {
    fn writable(&self) -> bool {
        true
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, SyscallError> {
        match self.names.get(path).copied() {
            Some(Kind::Dir) => Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 }),
            Some(kind) => {
                if let Some(open) = self.by_path.get(path).and_then(|n| self.open.get(n)) {
                    return Ok(Meta { kind, size: open.size, mtime: open.mtime });
                }
                let (_, size) = mapped("a lookup", path, self.fs.file_extents(path))?.ok_or(SyscallError::NotFound)?;
                let mtime = mapped("a lookup", path, self.fs.file_mtime(path))?.unwrap_or(0);
                Ok(Meta { kind, size, mtime })
            }
            None if self.is_dir(path) => Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 }),
            None => Err(SyscallError::NotFound),
        }
    }

    fn read_link(&mut self, path: &str) -> Result<String, SyscallError> {
        if self.names.get(path) != Some(&Kind::Symlink) {
            return Err(if self.names.contains_key(path) || self.is_dir(path) {
                SyscallError::InvalidArgument
            } else {
                SyscallError::NotFound
            });
        }
        mapped("readlink", path, self.fs.read_link(path, MAX_LINK))?.ok_or(SyscallError::Io)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<(String, Meta)>, SyscallError> {
        if !self.is_dir(dir) {
            return Err(if self.names.contains_key(dir) { SyscallError::InvalidArgument } else { SyscallError::NotFound });
        }
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut children: BTreeMap<String, Kind> = BTreeMap::new();
        for (name, kind) in self.names.range(prefix.clone()..) {
            let Some(rest) = name.strip_prefix(&prefix) else { break };
            match rest.split_once('/') {
                Some((child, _)) => {
                    children.insert(child.to_string(), Kind::Dir);
                }
                None => {
                    children.entry(rest.to_string()).or_insert(*kind);
                }
            }
        }
        let mut out = Vec::with_capacity(children.len());
        for (name, kind) in children {
            let meta = match kind {
                Kind::Dir => Meta { kind, size: 0, mtime: 0 },
                _ => self.lstat(&join(dir, &name))?,
            };
            out.push((name, meta));
        }
        Ok(out)
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, SyscallError> {
        if self.is_dir(path) {
            return Err(SyscallError::InvalidArgument);
        }
        let node = match self.by_path.get(path).copied() {
            Some(_) if how.create_new => return Err(SyscallError::AlreadyExists),
            Some(node) => node,
            None => match self.names.get(path).copied() {
                Some(_) if how.create_new => return Err(SyscallError::AlreadyExists),
                Some(Kind::File) => {
                    let (extents, size) =
                        mapped("open", path, self.fs.file_extents(path))?.ok_or(SyscallError::NotFound)?;
                    let mtime = mapped("open", path, self.fs.file_mtime(path))?.unwrap_or(0);
                    self.new_node(path, extents, size, mtime)
                }
                // A link the resolver did not follow is one with nothing behind it.
                Some(_) => return Err(SyscallError::NotFound),
                None if how.create || how.create_new => {
                    self.require_parent_dir(path)?;
                    let now = (self.clock)();
                    mapped("create", path, self.fs.create(path, &[], now))?;
                    self.names.insert(path.to_string(), Kind::File);
                    self.new_node(path, Vec::new(), 0, now)
                }
                None => return Err(SyscallError::NotFound),
            },
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
        if let Err(e) = self.persist(node) {
            println!("fsd: node {node}'s entry was not written at its last close: {e:?}");
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
        Ok(Meta { kind: Kind::File, size: open.size, mtime: open.mtime })
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut [u8]) -> Result<usize, SyscallError> {
        let cache = Rc::clone(&self.cache);
        let open = self.entry(node)?;
        if offset >= open.size {
            return Ok(0);
        }
        let n = out.len().min((open.size - offset) as usize);
        let mut done = 0;
        let mut page_buf = vec![0u8; BLOCK];
        while done < n {
            let at = offset + done as u64;
            let page = at / BLOCK as u64;
            let within = (at % BLOCK as u64) as usize;
            // Whole pages whose blocks are consecutive go as one cache read.
            if within == 0 && n - done >= BLOCK {
                if let Some(first) = block_for(&open.extents, page) {
                    let mut run = 1u64;
                    while ((run + 1) as usize) * BLOCK <= n - done
                        && run < 256
                        && block_for(&open.extents, page + run) == Some(first + run)
                    {
                        run += 1;
                    }
                    let span = &mut out[done..done + run as usize * BLOCK];
                    cache.read(first, span).map_err(disk_word)?;
                    done += run as usize * BLOCK;
                    continue;
                }
            }
            let take = (BLOCK - within).min(n - done);
            match block_for(&open.extents, page) {
                Some(block) => {
                    cache.read(block, &mut page_buf).map_err(disk_word)?;
                    out[done..done + take].copy_from_slice(&page_buf[within..within + take]);
                }
                // Past the extents: a hole, whose bytes are zeros.
                None => out[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(n)
    }

    fn write(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), SyscallError> {
        let end = offset.checked_add(data.len() as u64).ok_or(SyscallError::InvalidArgument)?;
        let cache = Rc::clone(&self.cache);
        let now = (self.clock)();
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        let mut done = 0;
        let mut page_buf = vec![0u8; BLOCK];
        while done < data.len() {
            let at = offset + done as u64;
            let page = at / BLOCK as u64;
            let within = (at % BLOCK as u64) as usize;
            let take = (BLOCK - within).min(data.len() - done);
            let existed = block_for(&open.extents, page);
            let block = match existed {
                Some(block) => block,
                None => {
                    let page_idx = u32::try_from(page).map_err(|_| SyscallError::ResourceExhausted)?;
                    mapped("an allocation", &open.path, self.fs.resolve_or_alloc_block(&mut open.extents, page_idx))?
                }
            };
            if take == BLOCK {
                cache.write(block, &data[done..done + BLOCK]).map_err(disk_word)?;
            } else {
                // A page written in part keeps the rest of what it held, which
                // for a page the file never reached is zeros.
                match existed {
                    Some(_) if page * (BLOCK as u64) < open.size => cache.read(block, &mut page_buf).map_err(disk_word)?,
                    _ => page_buf.fill(0),
                }
                page_buf[within..within + take].copy_from_slice(&data[done..done + take]);
                cache.write(block, &page_buf).map_err(disk_word)?;
            }
            done += take;
        }
        open.size = open.size.max(end);
        open.mtime = now;
        open.dirty = true;
        Ok(())
    }

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), SyscallError> {
        let cache = Rc::clone(&self.cache);
        let now = (self.clock)();
        let open = self.open.get_mut(&node).ok_or(SyscallError::NotFound)?;
        if open.gone {
            return Err(SyscallError::Gone);
        }
        if size >= open.size {
            open.size = size;
            open.mtime = now;
            open.dirty = true;
            return Ok(());
        }
        // What the last kept page holds past the new end is zeroed, so a file
        // that grows again reads zeros there and not what it held before.
        let within = (size % BLOCK as u64) as usize;
        if within != 0 {
            if let Some(block) = block_for(&open.extents, size / BLOCK as u64) {
                let mut page = vec![0u8; BLOCK];
                cache.read(block, &mut page).map_err(disk_word)?;
                page[within..].fill(0);
                cache.write(block, &page).map_err(disk_word)?;
            }
        }
        let dropped = keep_blocks(&mut open.extents, size.div_ceil(BLOCK as u64));
        open.size = size;
        open.mtime = now;
        // Recorded first, freed second.
        mapped("a truncate", &open.path, self.fs.update_metadata(&open.path, &open.extents, size, now))?;
        open.dirty = false;
        let path = open.path.clone();
        mapped("a truncate's free", &path, self.fs.free_extents(&dropped))
    }

    fn mkdir(&mut self, path: &str) -> Result<(), SyscallError> {
        if self.names.contains_key(path) || self.is_dir(path) {
            return Err(SyscallError::AlreadyExists);
        }
        self.require_parent_dir(path)?;
        let now = (self.clock)();
        mapped("mkdir", path, self.fs.create(&format!("{path}/"), &[], now))?;
        self.names.insert(path.to_string(), Kind::Dir);
        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), SyscallError> {
        if self.roots.iter().any(|r| r == path) || path.is_empty() {
            return Err(SyscallError::PermissionDenied);
        }
        if !self.is_dir(path) {
            return Err(if self.names.contains_key(path) { SyscallError::InvalidArgument } else { SyscallError::NotFound });
        }
        if self.has_children(path) {
            return Err(SyscallError::InvalidArgument);
        }
        let marker = format!("{path}/");
        if !mapped("rmdir", path, self.fs.delete(&marker))? {
            return Err(SyscallError::NotFound);
        }
        self.names.remove(path);
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), SyscallError> {
        match self.names.get(path) {
            Some(Kind::File) | Some(Kind::Symlink) => {}
            Some(Kind::Dir) => return Err(SyscallError::InvalidArgument),
            None if self.is_dir(path) => return Err(SyscallError::InvalidArgument),
            None => return Err(SyscallError::NotFound),
        }
        // Asked of the volume first: a delete the device refused leaves every
        // holder of the file its file.
        if !mapped("unlink", path, self.fs.delete(path))? {
            return Err(SyscallError::NotFound);
        }
        self.orphan(path);
        self.names.remove(path);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), SyscallError> {
        if from == to {
            return self.lstat(from).map(drop);
        }
        let kind = self.lstat(from)?.kind;
        self.require_parent_dir(to)?;
        if self.roots.iter().any(|r| r == from || r == to) {
            return Err(SyscallError::PermissionDenied);
        }
        match kind {
            Kind::File | Kind::Symlink => {
                if self.is_dir(to) {
                    return Err(SyscallError::InvalidArgument);
                }
                // Written first, so a file renamed while open keeps its
                // length under its new name.
                if let Some(node) = self.by_path.get(from).copied() {
                    self.persist(node)?;
                }
                self.orphan(to);
                mapped("rename", from, self.fs.rename(from, to))?;
                self.names.remove(from);
                self.names.insert(to.to_string(), kind);
                if let Some(node) = self.by_path.remove(from) {
                    self.open.get_mut(&node).expect("indexed").path = to.to_string();
                    self.by_path.insert(to.to_string(), node);
                }
                Ok(())
            }
            Kind::Dir => {
                if self.names.contains_key(to) || self.is_dir(to) {
                    return Err(SyscallError::AlreadyExists);
                }
                if to.starts_with(&format!("{from}/")) {
                    return Err(SyscallError::InvalidArgument);
                }
                // One entry at a time: the format has no rename of a prefix,
                // so a kill in the middle leaves the directory in two halves,
                // every entry under exactly one of its names.
                let prefix = format!("{from}/");
                let moving: Vec<(String, Kind)> = self
                    .names
                    .range(prefix.clone()..)
                    .take_while(|(n, _)| n.starts_with(&prefix))
                    .map(|(n, k)| (n.clone(), *k))
                    .collect();
                for (name, kind) in &moving {
                    let moved = format!("{to}/{}", &name[prefix.len()..]);
                    if let Some(node) = self.by_path.get(name).copied() {
                        self.persist(node)?;
                    }
                    let (on_disk_from, on_disk_to) = match kind {
                        Kind::Dir => (format!("{name}/"), format!("{moved}/")),
                        _ => (name.clone(), moved.clone()),
                    };
                    mapped("rename", name, self.fs.rename(&on_disk_from, &on_disk_to))?;
                    self.names.remove(name);
                    self.names.insert(moved.clone(), *kind);
                    if let Some(node) = self.by_path.remove(name) {
                        self.open.get_mut(&node).expect("indexed").path = moved.clone();
                        self.by_path.insert(moved, node);
                    }
                }
                if self.names.get(from) == Some(&Kind::Dir) {
                    mapped("rename", from, self.fs.rename(&format!("{from}/"), &format!("{to}/")))?;
                    self.names.remove(from);
                    self.names.insert(to.to_string(), Kind::Dir);
                }
                Ok(())
            }
        }
    }

    fn symlink(&mut self, path: &str, target: &str) -> Result<(), SyscallError> {
        if self.names.contains_key(path) || self.is_dir(path) {
            return Err(SyscallError::AlreadyExists);
        }
        self.require_parent_dir(path)?;
        mapped("symlink", path, self.fs.create_symlink(path, target))?;
        self.names.insert(path.to_string(), Kind::Symlink);
        Ok(())
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        let nodes: Vec<Node> = self.open.keys().copied().collect();
        for node in nodes {
            self.persist(node)?;
        }
        mapped("sync", "", self.fs.sync())
    }

    fn describe(&self) -> String {
        let c = self.cache.counts();
        format!(
            "bcachefs, {} names, {} files open; cache {} blocks ({} dirty), {} of {} reads hit",
            self.names.len(),
            self.open.len(),
            c.cached,
            c.dirty,
            c.hits,
            c.reads
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk::Ram;

    fn clock() -> u64 {
        1_000_000_000
    }

    fn vol() -> DataVolume<Ram> {
        DataVolume::format(Ram::new(4096), &["home", "apps"], clock).unwrap()
    }

    const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
    const PLAIN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };

    #[test]
    fn a_file_reads_back_what_was_written_across_pages_and_holes() {
        let mut v = vol();
        let n = v.open("home/a", CREATE).unwrap();
        let data: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        v.write(n, 100, &data).unwrap();
        v.write(n, 20_000, b"tail").unwrap();
        let mut out = vec![0xFFu8; 20_004];
        assert_eq!(v.read(n, 0, &mut out).unwrap(), 20_004);
        assert!(out[..100].iter().all(|&b| b == 0));
        assert_eq!(&out[100..10_100], &data[..]);
        assert!(out[10_100..20_000].iter().all(|&b| b == 0), "a hole reads zeros");
        assert_eq!(&out[20_000..], b"tail");
    }

    #[test]
    fn a_closed_file_keeps_its_length_across_a_remount() {
        let mut v = vol();
        v.mkdir("home/toy").unwrap();
        let n = v.open("home/toy/x", CREATE).unwrap();
        v.write(n, 0, &[7; 5000]).unwrap();
        v.close(n);
        v.sync().unwrap();
        let DataVolume { fs, cache, .. } = v;
        drop(fs);
        let disk = Rc::try_unwrap(cache).ok().expect("one owner").into_disk();
        let Probed::Mounted(mut again) = DataVolume::probe(disk, &["home"], clock) else { panic!("remount") };
        assert_eq!(again.lstat("home/toy/x").unwrap().size, 5000);
        assert_eq!(again.lstat("home/toy").unwrap().kind, Kind::Dir, "an empty directory outlives the mount");
        let n = again.open("home/toy/x", PLAIN).unwrap();
        let mut out = vec![0u8; 5000];
        again.read(n, 0, &mut out).unwrap();
        assert_eq!(out, vec![7; 5000]);
    }

    #[test]
    fn a_file_unlinked_while_open_answers_gone() {
        let mut v = vol();
        let n = v.open("home/a", CREATE).unwrap();
        v.write(n, 0, b"x").unwrap();
        v.unlink("home/a").unwrap();
        assert_eq!(v.read(n, 0, &mut [0u8; 1]), Err(SyscallError::Gone));
        assert_eq!(v.lstat("home/a"), Err(SyscallError::NotFound));
    }

    #[test]
    fn a_shrink_zeroes_what_it_cut_and_regrowth_reads_zeros() {
        let mut v = vol();
        let n = v.open("home/a", CREATE).unwrap();
        v.write(n, 0, &[9; 8192]).unwrap();
        v.truncate(n, 10).unwrap();
        v.truncate(n, 8192).unwrap();
        let mut out = vec![0xFFu8; 8192];
        v.read(n, 0, &mut out).unwrap();
        assert_eq!(&out[..10], &[9; 10]);
        assert!(out[10..].iter().all(|&b| b == 0));
    }

    #[test]
    fn directories_are_listed_and_refuse_what_posix_refuses() {
        let mut v = vol();
        let f = v.open("home/nodir/a", CREATE).unwrap();
        v.close(f);
        assert_eq!(v.lstat("home/nodir").unwrap().kind, Kind::Dir, "a create makes its directories");
        assert_eq!(v.open("home/nodir/a/b", CREATE), Err(SyscallError::NotFound), "never under a file");
        v.mkdir("home/d").unwrap();
        assert_eq!(v.mkdir("home/d"), Err(SyscallError::AlreadyExists));
        let n = v.open("home/d/f", CREATE).unwrap();
        v.close(n);
        assert_eq!(v.rmdir("home/d"), Err(SyscallError::InvalidArgument), "not empty");
        let names: Vec<String> = v.list("home").unwrap().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["d", "nodir"]);
        v.unlink("home/d/f").unwrap();
        v.rmdir("home/d").unwrap();
        assert_eq!(v.lstat("home/d"), Err(SyscallError::NotFound));
        assert_eq!(v.rmdir("home"), Err(SyscallError::PermissionDenied), "a root stays");
    }

    #[test]
    fn a_rename_moves_an_open_file_and_a_directory_with_its_contents() {
        let mut v = vol();
        v.mkdir("home/a").unwrap();
        let n = v.open("home/a/f", CREATE).unwrap();
        v.write(n, 0, b"hello").unwrap();
        v.rename("home/a", "home/b").unwrap();
        assert_eq!(v.lstat("home/b/f").unwrap().size, 5);
        let mut out = [0u8; 5];
        v.read(n, 0, &mut out).unwrap();
        assert_eq!(&out, b"hello");
        v.close(n);
        let m = v.open("home/b/g", CREATE).unwrap();
        v.close(m);
        v.rename("home/b/g", "home/b/f").unwrap();
        assert_eq!(v.lstat("home/b/f").unwrap().size, 0, "replaced");
    }
}
