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
//! **What a kill costs.** A sync is the format's commit (`bcachefs`'s
//! `Mounted::sync`): a server killed anywhere leaves the volume as the last
//! sync that finished, or as the one it was inside, whole either way. A
//! directory's rename is one operation of the format's
//! (`Mounted::rename_all`): it moves whole, or is refused with nothing moved.
//!
//! Every name a client chose is bounded by the format ([`FsError::NameTooLong`])
//! before it reaches the tree.

use std::collections::BTreeMap;
use std::rc::Rc;

use bcachefs::{Extent, Formatted, FsError, Mounted, ReadWrite};
use toyos_abi::syscall::SyscallError;

use crate::cache::{Cache, Shared};
use crate::disk::{Disk, DiskError, BLOCK};
use crate::volume::{join, parent, Kind, Meta, Node, OpenHow, Out, Volume};

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

/// Where DATA's partition is, counted over both sources at once: the claims
/// the supervisor minted on the kernel's disks and the TOYOS-DATA partitions the block
/// service lists, or why it would not list them.
#[derive(Debug, PartialEq, Eq)]
pub enum Located {
    /// This machine has none: memory stands in.
    Nowhere,
    /// The one claim the supervisor minted.
    Claimed,
    /// The one partition the block service serves, by its unique GUID.
    Served([u8; 16]),
    /// Two or more, never guessed between, or none countable: DATA is absent.
    Refused(String),
}

pub fn find(claims: usize, served: Result<&[[u8; 16]], String>) -> Located {
    let served = match served {
        Ok(served) => served,
        Err(why) => return Located::Refused(why),
    };
    match (claims, served) {
        (0, []) => Located::Nowhere,
        (1, []) => Located::Claimed,
        (0, [one]) => Located::Served(*one),
        (claims, served) => Located::Refused(format!(
            "this machine has {} DATA partitions, {claims} on the kernel's disks and {} the block \
             service serves, and a volume is one",
            claims + served.len(),
            served.len()
        )),
    }
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
        println!("fileserver: {what} of '{path}' failed: {e:?}");
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
        println!("fileserver: block 0 would not read; this disk is not ours to format");
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
            "fileserver: a designation stamp at block 0 names {stamped} blocks, and this partition has {}; ignoring it",
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
                println!("fileserver: mounted the DATA volume");
                return Self::over(fs, cache, roots, clock).map_or_else(Probed::Unmountable, Probed::Mounted);
            }
            Err(e) if e.disowns_volume() => {}
            Err(e) => return Probed::Unmountable(format!("{e:?}")),
        }
        if !designated(&cache) {
            println!("fileserver: no volume of ours and no designation stamp; nothing will be written to this partition");
            return Probed::Foreign;
        }
        println!("fileserver: block 0 designates this partition for ToyOS; formatting it");
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

    /// Forget a node nobody holds and whose entry is written.
    fn release(&mut self, node: Node) {
        let open = self.open.remove(&node).expect("an open node");
        if self.by_path.get(&open.path) == Some(&node) {
            self.by_path.remove(&open.path);
        }
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
                let mtime = mapped("a lookup", path, self.fs.file_mtime(path))?.ok_or(SyscallError::NotFound)?;
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
                    let mtime = mapped("open", path, self.fs.file_mtime(path))?.ok_or(SyscallError::NotFound)?;
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
        if let Err(e) = self.persist(node) {
            println!("fileserver: node {node}'s entry was not written at its last close ({e:?}); the next sync writes it");
            return Err(e);
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
        Ok(Meta { kind: Kind::File, size: open.size, mtime: open.mtime })
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, SyscallError> {
        let cache = Rc::clone(&self.cache);
        let open = self.entry(node)?;
        if offset >= open.size {
            return Ok(0);
        }
        let n = out.len().min((open.size - offset) as usize);
        let mut done = 0;
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
                    let base = done;
                    cache.visit(first, run as usize, |k, block| out.put(base + k * BLOCK, block)).map_err(disk_word)?;
                    done += run as usize * BLOCK;
                    continue;
                }
            }
            let take = (BLOCK - within).min(n - done);
            match block_for(&open.extents, page) {
                Some(block) => {
                    let base = done;
                    cache.visit(block, 1, |_, b| out.put(base, &b[within..within + take])).map_err(disk_word)?;
                }
                // Past the extents: a hole, whose bytes are zeros.
                None => out.zero(done, take),
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
        // Every page past the extents is allocated before a byte is written,
        // so a write the file's one entry could not name is refused whole.
        let covered: u64 = open.extents.iter().map(|e| e.block_count as u64).sum();
        for page in (offset / BLOCK as u64).max(covered)..end.div_ceil(BLOCK as u64) {
            let grown = u32::try_from(page)
                .map_err(|_| SyscallError::ResourceExhausted)
                .and_then(|p| mapped("an allocation", &open.path, self.fs.resolve_or_alloc_block(&mut open.extents, p)))
                .and_then(|_| match bcachefs::file_entry_fits(&open.path, &open.extents) {
                    true => Ok(()),
                    false => {
                        println!(
                            "fileserver: a write to '{}' is refused: its entry would name {} runs of blocks, more than it holds",
                            open.path,
                            open.extents.len()
                        );
                        Err(SyscallError::ResourceExhausted)
                    }
                });
            if let Err(e) = grown {
                let dropped = keep_blocks(&mut open.extents, covered);
                mapped("a refused write's free", &open.path, self.fs.free_extents(&dropped))?;
                return Err(e);
            }
        }
        let mut done = 0;
        let mut page_buf = vec![0u8; BLOCK];
        while done < data.len() {
            let at = offset + done as u64;
            let page = at / BLOCK as u64;
            let within = (at % BLOCK as u64) as usize;
            let take = (BLOCK - within).min(data.len() - done);
            let block = block_for(&open.extents, page).expect("allocated above");
            if take == BLOCK {
                cache.write(block, &data[done..done + BLOCK]).map_err(disk_word)?;
            } else {
                // A page written in part keeps the rest of what it held, which
                // for a page the file never reached is zeros.
                if page < covered && page * (BLOCK as u64) < open.size {
                    cache.read(block, &mut page_buf).map_err(disk_word)?;
                } else {
                    page_buf.fill(0);
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
                // Orphaned only once the format has taken the rename: a refused
                // one leaves `to` naming its file, holders and unsynced length whole.
                mapped("rename", from, self.fs.rename(from, to))?;
                self.orphan(to);
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
                // Every entry beneath, and the directory's own, in one
                // operation of the format's: the directory moves whole or not at all.
                let prefix = format!("{from}/");
                let moving: Vec<(String, Kind)> = self
                    .names
                    .get_key_value(from)
                    .into_iter()
                    .chain(self.names.range(prefix.clone()..).take_while(|(n, _)| n.starts_with(&prefix)))
                    .map(|(n, k)| (n.clone(), *k))
                    .collect();
                for (name, _) in &moving {
                    if let Some(node) = self.by_path.get(name).copied() {
                        self.persist(node)?;
                    }
                }
                let renames: Vec<(String, String)> = moving
                    .iter()
                    .map(|(name, kind)| {
                        let moved = format!("{to}{}", &name[from.len()..]);
                        match kind {
                            Kind::Dir => (format!("{name}/"), format!("{moved}/")),
                            _ => (name.clone(), moved),
                        }
                    })
                    .collect();
                let pairs: Vec<(&str, &str)> = renames.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                mapped("rename", from, self.fs.rename_all(&pairs))?;
                for (name, kind) in moving {
                    let moved = format!("{to}{}", &name[from.len()..]);
                    self.names.remove(&name);
                    self.names.insert(moved.clone(), kind);
                    if let Some(node) = self.by_path.remove(&name) {
                        self.open.get_mut(&node).expect("indexed").path = moved.clone();
                        self.by_path.insert(moved, node);
                    }
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

    fn sync(&mut self) -> Result<Vec<(Node, SyscallError)>, SyscallError> {
        let mut unwritten = Vec::new();
        let nodes: Vec<Node> = self.open.keys().copied().collect();
        for node in nodes {
            match self.persist(node) {
                Err(e) => unwritten.push((node, e)),
                Ok(()) if self.open[&node].holders == 0 => self.release(node),
                Ok(()) => {}
            }
        }
        mapped("sync", "", self.fs.sync())?;
        Ok(unwritten)
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
    use std::collections::HashMap;

    use super::*;
    use crate::disk::Ram;
    use crate::volume::Buf;

    fn clock() -> u64 {
        1_000_000_000
    }

    fn vol() -> DataVolume<Ram> {
        DataVolume::format(Ram::new(4096), &["home", "apps"], clock).unwrap()
    }

    const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
    const PLAIN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };

    /// Two DATA partitions are refused wherever each is, and a block service
    /// that would not list what it has leaves DATA absent even beside a claim:
    /// memory stands in only where both sources say there is none.
    #[test]
    fn data_is_one_partition_counted_over_both_sources() {
        let (a, b) = ([1; 16], [2; 16]);
        assert_eq!(find(0, Ok(&[])), Located::Nowhere);
        assert_eq!(find(1, Ok(&[])), Located::Claimed);
        assert_eq!(find(0, Ok(&[a])), Located::Served(a));
        let two = |claims, served: &[[u8; 16]]| match find(claims, Ok(served)) {
            Located::Refused(why) => why,
            other => panic!("{claims} claims and {} served located {other:?}", served.len()),
        };
        assert!(two(1, &[a]).starts_with("this machine has 2 DATA partitions, 1 on the kernel's disks and 1"));
        assert!(two(2, &[]).starts_with("this machine has 2 DATA partitions, 2 on the kernel's disks and 0"));
        assert!(two(0, &[a, b]).starts_with("this machine has 2 DATA partitions, 0 on the kernel's disks and 2"));
        for claims in [0, 1] {
            assert_eq!(find(claims, Err("unlisted".into())), Located::Refused("unlisted".into()));
        }
    }

    #[test]
    fn a_file_reads_back_what_was_written_across_pages_and_holes() {
        let mut v = vol();
        let n = v.open("home/a", CREATE).unwrap();
        let data: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        v.write(n, 100, &data).unwrap();
        v.write(n, 20_000, b"tail").unwrap();
        let mut out = vec![0xFFu8; 20_004];
        assert_eq!(v.read(n, 0, &mut Buf(&mut out)).unwrap(), 20_004);
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
        v.close(n).unwrap();
        v.sync().unwrap();
        let mut again = remount(v);
        assert_eq!(again.lstat("home/toy/x").unwrap().size, 5000);
        assert_eq!(again.lstat("home/toy").unwrap().kind, Kind::Dir, "an empty directory outlives the mount");
        let n = again.open("home/toy/x", PLAIN).unwrap();
        let mut out = vec![0u8; 5000];
        again.read(n, 0, &mut Buf(&mut out)).unwrap();
        assert_eq!(out, vec![7; 5000]);
    }

    #[test]
    fn a_file_unlinked_while_open_answers_gone() {
        let mut v = vol();
        let n = v.open("home/a", CREATE).unwrap();
        v.write(n, 0, b"x").unwrap();
        v.unlink("home/a").unwrap();
        assert_eq!(v.read(n, 0, &mut Buf(&mut [0u8; 1])), Err(SyscallError::Gone));
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
        v.read(n, 0, &mut Buf(&mut out)).unwrap();
        assert_eq!(&out[..10], &[9; 10]);
        assert!(out[10..].iter().all(|&b| b == 0));
    }

    #[test]
    fn directories_are_listed_and_refuse_what_posix_refuses() {
        let mut v = vol();
        let f = v.open("home/nodir/a", CREATE).unwrap();
        v.close(f).unwrap();
        assert_eq!(v.lstat("home/nodir").unwrap().kind, Kind::Dir, "a create makes its directories");
        assert_eq!(v.open("home/nodir/a/b", CREATE), Err(SyscallError::NotFound), "never under a file");
        v.mkdir("home/d").unwrap();
        assert_eq!(v.mkdir("home/d"), Err(SyscallError::AlreadyExists));
        let n = v.open("home/d/f", CREATE).unwrap();
        v.close(n).unwrap();
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
        v.read(n, 0, &mut Buf(&mut out)).unwrap();
        assert_eq!(&out, b"hello");
        v.close(n).unwrap();
        let m = v.open("home/b/g", CREATE).unwrap();
        v.close(m).unwrap();
        v.rename("home/b/g", "home/b/f").unwrap();
        assert_eq!(v.lstat("home/b/f").unwrap().size, 0, "replaced");
    }

    fn remount(v: DataVolume<Ram>) -> DataVolume<Ram> {
        let DataVolume { fs, cache, .. } = v;
        drop(fs);
        let disk = Rc::try_unwrap(cache).ok().expect("one owner").into_disk();
        let Probed::Mounted(again) = DataVolume::probe(disk, &["home"], clock) else { panic!("remount") };
        again
    }

    /// A volume whose free blocks are all apart, so every run a file is given
    /// is one block: every block a file's data may take, taken in one run,
    /// every other one given back, and the run left at the end for nodes
    /// (`bcachefs`'s `NODE_RESERVE`, 16) taken after.
    fn fragmented() -> DataVolume<Ram> {
        let mut v = DataVolume::format(Ram::new(1024), &["home"], clock).unwrap();
        let mut run = Vec::new();
        let mut page = 0;
        while v.fs.resolve_or_alloc_block(&mut run, page).is_ok() {
            page += 1;
        }
        let taken: Vec<u64> = run.iter().flat_map(|e| e.start_block..e.start_block + e.block_count as u64).collect();
        let holes: Vec<Extent> =
            taken.iter().step_by(2).map(|&b| Extent { start_block: b, block_count: 1, _reserved: 0 }).collect();
        v.fs.free_extents(&holes).unwrap();
        let mut tail = vec![Extent { start_block: *taken.last().unwrap(), block_count: 1, _reserved: 0 }];
        v.fs.resolve_or_alloc_block(&mut tail, 16).unwrap();
        v
    }

    fn page_count(v: &mut DataVolume<Ram>, path: &str) -> u64 {
        v.lstat(path).unwrap().size / BLOCK as u64
    }

    /// The free count a sync leaves in the superblock.
    fn free_blocks(v: &mut DataVolume<Ram>) -> u64 {
        assert_eq!(v.sync(), Ok(Vec::new()));
        bcachefs::Superblock::read(v.fs.io()).unwrap().free_blocks
    }

    fn write_into(file: &mut Vec<u8>, offset: u64, data: &[u8]) {
        let end = offset as usize + data.len();
        if file.len() < end {
            file.resize(end, 0);
        }
        file[offset as usize..end].copy_from_slice(data);
    }

    /// Differential against a plain map of what each accepted write made
    /// each file: three files grow in turn past twice the runs one entry
    /// names at a block a run, through overwrites, holes, a shrink and writes
    /// the volume refuses, and every byte the map holds is what the remounted
    /// volume reads.
    #[test]
    fn every_file_reads_back_as_a_plain_map_of_its_accepted_writes() {
        const PAST: usize = (2 * 250 + 20) * BLOCK;
        let paths = ["home/a", "home/b", "home/c"];
        let mut v = vol();
        let nodes: Vec<Node> = paths.iter().map(|p| v.open(p, CREATE).unwrap()).collect();
        let mut model: HashMap<&str, Vec<u8>> = paths.iter().map(|p| (*p, Vec::new())).collect();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut round = 0u64;
        while paths.iter().any(|p| model[p].len() < PAST) {
            for (i, (&path, &node)) in paths.iter().zip(&nodes).enumerate() {
                let held = model.get_mut(path).unwrap();
                let len = held.len() as u64;
                let (offset, n) = match (round + i as u64) % 11 {
                    3 if len > 0 => (next() % len, (next() % (3 * BLOCK as u64)) as usize + 1),
                    7 => (len + next() % (2 * BLOCK as u64), BLOCK),
                    _ => (len, BLOCK + (next() % 64) as usize),
                };
                let data: Vec<u8> = (0..n).map(|_| next() as u8).collect();
                assert_eq!(v.write(node, offset, &data), Ok(()), "{path}: {n} bytes at {offset}, of {len} held");
                write_into(held, offset, &data);
            }
            if round % 97 == 50 {
                let at = round as usize % paths.len();
                assert_eq!(
                    v.write(nodes[at], 64 << 20, b"past every block"),
                    Err(SyscallError::ResourceExhausted),
                    "{}: a write the volume has no blocks for",
                    paths[at]
                );
            }
            if round == 300 {
                let b = model.get_mut("home/b").unwrap();
                let to = b.len() / 3 + 17;
                v.truncate(nodes[1], to as u64).unwrap();
                b.truncate(to);
            }
            round += 1;
        }
        assert_eq!(v.sync(), Ok(Vec::new()));
        for n in nodes {
            v.close(n).unwrap();
        }
        let mut v = remount(v);
        let listed: Vec<String> = v.list("home").unwrap().into_iter().map(|(n, _)| format!("home/{n}")).collect();
        assert_eq!(listed, paths);
        for path in paths {
            let held = &model[path];
            assert_eq!(v.lstat(path).unwrap().size, held.len() as u64, "{path}'s length");
            let n = v.open(path, PLAIN).unwrap();
            let mut out = vec![0xEEu8; held.len()];
            assert_eq!(v.read(n, 0, &mut Buf(&mut out)), Ok(held.len()));
            let first = out.iter().zip(held).position(|(a, b)| a != b);
            assert_eq!(first, None, "{path}: the first byte the volume and the map disagree on");
        }
    }

    /// On a volume whose free blocks are all apart, a file's entry fills: the
    /// write that would take it past what one entry names is refused whole,
    /// and every write before it reaches the volume.
    #[test]
    fn a_write_its_entry_could_not_name_is_refused_and_every_accepted_one_kept() {
        let mut v = fragmented();
        let a = v.open("home/a", CREATE).unwrap();
        let mut pages = 0u64;
        let refused = loop {
            match v.write(a, pages * BLOCK as u64, &[3; BLOCK]) {
                Ok(()) => pages += 1,
                Err(e) => break e,
            }
        };
        assert_eq!(refused, SyscallError::ResourceExhausted);
        // One block a run: (4064 − 24 − 19 − "home/a".len()) / 16 runs is
        // what the one entry holding `home/a` names.
        assert_eq!(pages, 250, "the entry filled, not the volume");
        assert_eq!(page_count(&mut v, "home/a"), pages, "the refused write changed nothing");
        let before = free_blocks(&mut v);
        assert_eq!(v.write(a, pages * BLOCK as u64, &[4; 4 * BLOCK]), Err(SyscallError::ResourceExhausted));
        assert_eq!(free_blocks(&mut v), before, "a refused write's blocks went back");
        v.close(a).unwrap();
        let mut v = remount(v);
        assert_eq!(page_count(&mut v, "home/a"), pages);
        let a = v.open("home/a", PLAIN).unwrap();
        let mut out = vec![0u8; pages as usize * BLOCK];
        v.read(a, 0, &mut Buf(&mut out)).unwrap();
        assert!(out.iter().all(|&b| b == 3));
    }

    /// One file whose entry is refused leaves every other file's sync whole,
    /// its own close refused, and its writes kept until a sync writes them.
    #[test]
    fn an_entry_refused_costs_only_its_own_file() {
        let mut v = vol();
        let a = v.open("home/a", CREATE).unwrap();
        let b = v.open("home/b", CREATE).unwrap();
        v.write(a, 0, &[1; BLOCK]).unwrap();
        v.write(b, 0, &[2; 3 * BLOCK]).unwrap();
        // More runs than the entry names, as no write can give it.
        let real = v.open[&a].extents.clone();
        let too_many = vec![real[0]; 300];
        v.open.get_mut(&a).unwrap().extents = too_many.clone();

        assert_eq!(v.sync(), Ok(vec![(a, SyscallError::ResourceExhausted)]));
        v.close(b).unwrap();
        assert_eq!(v.close(a), Err(SyscallError::ResourceExhausted), "a close that lost the writes is refused");
        assert_eq!(v.sync(), Ok(vec![(a, SyscallError::ResourceExhausted)]), "unheld, it is named again");
        assert!(v.open.contains_key(&a), "and kept again");
        assert_eq!(page_count(&mut v, "home/a"), 1, "the refused file is still what its writes made it");

        v.open.get_mut(&a).unwrap().extents = real;
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert!(v.open.is_empty(), "written, the unheld node goes");
        let mut v = remount(v);
        assert_eq!(page_count(&mut v, "home/a"), 1);
        assert_eq!(page_count(&mut v, "home/b"), 3);
    }

    /// A rename over an open, unsynced file that the format refuses leaves
    /// that file whole: readable through its holder, and its length reaching
    /// the volume at the next sync. The refusal is `EntryTooLarge`: `from`'s
    /// extents fit its own short name and not `to`'s long one.
    #[test]
    fn a_refused_rename_over_an_open_file_keeps_its_unsynced_writes() {
        let mut v = fragmented();
        let from = v.open("home/f", CREATE).unwrap();
        for page in 0..240u64 {
            v.write(from, page * BLOCK as u64, &[1; BLOCK]).unwrap();
        }
        let to_path = format!("home/{}", "t".repeat(300));
        let to = v.open(&to_path, CREATE).unwrap();
        v.write(to, 0, b"written and not synced").unwrap();

        assert_eq!(v.rename("home/f", &to_path), Err(SyscallError::ResourceExhausted));

        let mut back = [0u8; 22];
        assert_eq!(v.read(to, 0, &mut Buf(&mut back)), Ok(22), "the holder of `to` still reads it");
        assert_eq!(&back, b"written and not synced");
        assert_eq!(v.sync(), Ok(Vec::new()));
        v.close(to).unwrap();
        assert_eq!(v.lstat(&to_path).unwrap().size, 22, "`to`'s length reached the volume");
    }

    /// A disk whose blocks are a map the test can copy, that keeps the first
    /// `keep` block writes and loses every later one — the server killed there
    /// — or refuses the one numbered `refuse` and keeps the rest. A request of
    /// several blocks counts each, so a kill can land inside one.
    type Image = BTreeMap<u64, Box<[u8; BLOCK]>>;

    struct Stops {
        blocks: u64,
        /// Shared, so a test reads what the disk holds while the server runs.
        image: Rc<std::cell::RefCell<Image>>,
        writes: usize,
        keep: usize,
        refuse: Option<usize>,
    }

    impl Stops {
        fn new(blocks: u64) -> Self {
            Self { blocks, image: Rc::default(), writes: 0, keep: usize::MAX, refuse: None }
        }

        fn from(image: &Image, blocks: u64, keep: usize, refuse: Option<usize>) -> Self {
            Self { blocks, image: Rc::new(image.clone().into()), writes: 0, keep, refuse }
        }
    }

    impl Disk for Stops {
        fn blocks(&self) -> u64 {
            self.blocks
        }

        fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
            for (i, chunk) in out.chunks_exact_mut(BLOCK).enumerate() {
                match self.image.borrow().get(&(first + i as u64)) {
                    Some(block) => chunk.copy_from_slice(&block[..]),
                    None => chunk.fill(0),
                }
            }
            Ok(())
        }

        fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
            for (i, chunk) in data.chunks_exact(BLOCK).enumerate() {
                let n = self.writes;
                self.writes += 1;
                if Some(n) == self.refuse {
                    return Err(DiskError::Device);
                }
                if n < self.keep {
                    self.image.borrow_mut().insert(first + i as u64, Box::new(chunk.try_into().expect("a block")));
                }
            }
            Ok(())
        }

        fn flush(&mut self) -> Result<(), DiskError> {
            Ok(())
        }
    }

    fn take(v: DataVolume<Stops>) -> Stops {
        let DataVolume { fs, cache, .. } = v;
        drop(fs);
        Rc::try_unwrap(cache).ok().expect("one owner").into_disk()
    }

    /// Every file under `dir` with its bytes, by its path beneath `dir`; a
    /// directory with nothing under it is listed with a trailing `/`.
    fn tree<D: Disk>(v: &mut DataVolume<D>, dir: &str) -> Option<BTreeMap<String, Vec<u8>>> {
        let listed = v.list(dir).ok()?;
        let mut out = BTreeMap::new();
        if listed.is_empty() {
            out.insert("/".to_string(), Vec::new());
        }
        for (name, meta) in listed {
            let path = join(dir, &name);
            match meta.kind {
                Kind::Dir => {
                    for (below, bytes) in tree(v, &path)? {
                        let below = if below == "/" { format!("{name}/") } else { format!("{name}/{below}") };
                        out.insert(below, bytes);
                    }
                }
                _ => {
                    let n = v.open(&path, PLAIN).ok()?;
                    let mut bytes = vec![0u8; meta.size as usize];
                    let read = v.read(n, 0, &mut Buf(&mut bytes));
                    v.close(n).ok()?;
                    (read.ok()? == bytes.len()).then_some(())?;
                    out.insert(name, bytes);
                }
            }
        }
        Some(out)
    }

    const STAGED: &str = "home/staged";
    const INSTALLED: &str = "apps/pkg";

    /// Where the directory is: `Ok(true)` whole under its new name and absent
    /// under its old, `Ok(false)` the reverse, and otherwise what was found.
    fn whole_under_one<D: Disk>(v: &mut DataVolume<D>, want: &BTreeMap<String, Vec<u8>>) -> Result<bool, String> {
        let (old, new) = (tree(v, STAGED), tree(v, INSTALLED));
        match (&old, &new) {
            (Some(old), None) if old == want => Ok(false),
            (None, Some(new)) if new == want => Ok(true),
            _ => Err(format!(
                "{} names under {STAGED} and {} under {INSTALLED}",
                old.as_ref().map_or(0, |t| t.len()),
                new.as_ref().map_or(0, |t| t.len())
            )),
        }
    }

    /// A staged package: files enough to fill several leaves, a subdirectory,
    /// an empty one, and names beside it the rename must not move; synced.
    fn staged() -> (Image, BTreeMap<String, Vec<u8>>) {
        let mut v = DataVolume::format(Stops::new(1024), &["home", "apps"], clock).unwrap();
        for i in 0..80 {
            let n = v.open(&format!("home/beside{i}"), CREATE).unwrap();
            v.write(n, 0, format!("beside {i}").as_bytes()).unwrap();
            v.close(n).unwrap();
        }
        v.mkdir(STAGED).unwrap();
        v.mkdir(&format!("{STAGED}/empty")).unwrap();
        for i in 0..60 {
            let path = if i % 4 == 0 { format!("{STAGED}/sub/f{i}") } else { format!("{STAGED}/f{i}") };
            let n = v.open(&path, CREATE).unwrap();
            v.write(n, 0, format!("file {i} of the package").as_bytes()).unwrap();
            v.close(n).unwrap();
        }
        assert_eq!(v.sync(), Ok(Vec::new()));
        let want = tree(&mut v, STAGED).unwrap();
        (take(v).image.take(), want)
    }

    fn mounted(disk: Stops) -> Result<DataVolume<Stops>, String> {
        match DataVolume::probe(disk, &["home", "apps"], clock) {
            Probed::Mounted(v) => Ok(v),
            Probed::Unmountable(why) => Err(format!("unmountable: {why}")),
            Probed::Foreign => Err("foreign".into()),
        }
    }

    /// The rename and the sync that makes it durable, against a disk that
    /// stops at every block write the two make in turn: what the disk then
    /// holds mounts, and names the directory whole under exactly one name.
    #[test]
    fn a_directory_rename_is_whole_under_one_name_wherever_the_server_is_killed() {
        let (image, want) = staged();
        let blocks = 1024;
        let mut v = mounted(Stops::from(&image, blocks, usize::MAX, None)).unwrap();
        v.rename(STAGED, INSTALLED).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert_eq!(whole_under_one(&mut v, &want), Ok(true));
        let writes = take(v).writes;
        assert!(writes > 4, "the rename and its sync wrote {writes} blocks");

        let mut torn = Vec::new();
        for keep in 0..=writes {
            let mut v = mounted(Stops::from(&image, blocks, keep, None)).unwrap();
            let _ = v.rename(STAGED, INSTALLED);
            let _ = v.sync();
            let verdict = mounted(take(v)).and_then(|mut again| whole_under_one(&mut again, &want));
            if let Err(why) = verdict {
                torn.push(format!("killed after {keep} of {writes} writes: {why}"));
            }
        }
        assert!(torn.is_empty(), "{} of {} kill points tear the directory:\n{}", torn.len(), writes + 1, torn.join("\n"));
    }

    /// The same, with the one write numbered `n` refused and the server
    /// alive: what it answers and what it then serves agree, the disk holds
    /// the directory whole under one name past the refused sync, and the next
    /// sync makes the answer what the disk holds.
    #[test]
    fn a_directory_rename_is_whole_under_one_name_whichever_write_is_refused() {
        let (image, want) = staged();
        let blocks = 1024;
        let mut v = mounted(Stops::from(&image, blocks, usize::MAX, None)).unwrap();
        v.rename(STAGED, INSTALLED).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let writes = take(v).writes;
        let held = |on_disk: &Image| {
            mounted(Stops::from(on_disk, blocks, usize::MAX, None)).and_then(|mut v| whole_under_one(&mut v, &want))
        };

        let mut torn = Vec::new();
        for refuse in 0..writes {
            let disk = Stops::from(&image, blocks, usize::MAX, Some(refuse));
            let on_disk = Rc::clone(&disk.image);
            let mut v = mounted(disk).unwrap();
            let renamed = v.rename(STAGED, INSTALLED).is_ok();
            let first = v.sync();
            let between = held(&on_disk.borrow());
            let served = whole_under_one(&mut v, &want);
            let second = v.sync();
            let after = held(&on_disk.borrow());
            if served != Ok(renamed) || between.is_err() || second != Ok(Vec::new()) || after != Ok(renamed) {
                torn.push(format!(
                    "write {refuse} of {writes} refused: answered {renamed}, served {served:?}, \
                     the sync {first:?} left {between:?}, the next {second:?} left {after:?}"
                ));
            }
        }
        assert!(torn.is_empty(), "{} of {writes} refusals tear the directory:\n{}", torn.len(), torn.join("\n"));
    }

    /// A rename the format refuses part-way — `z`'s extents fit its own name
    /// and not the long one — leaves the directory whole under its old name.
    #[test]
    fn a_directory_rename_the_format_refuses_part_way_moves_nothing() {
        let mut v = fragmented();
        let a = v.open("home/staged/a", CREATE).unwrap();
        v.write(a, 0, b"small").unwrap();
        v.close(a).unwrap();
        let z = v.open("home/staged/z", CREATE).unwrap();
        for page in 0..240u64 {
            v.write(z, page * BLOCK as u64, &[1; BLOCK]).unwrap();
        }
        v.close(z).unwrap();
        let to = format!("home/{}", "t".repeat(300));

        assert_eq!(v.rename("home/staged", &to), Err(SyscallError::ResourceExhausted));

        let names = |v: &mut DataVolume<Ram>, dir: &str| -> Vec<String> {
            v.list(dir).map(|l| l.into_iter().map(|(n, _)| n).collect()).unwrap_or_default()
        };
        assert_eq!(names(&mut v, "home/staged"), ["a", "z"]);
        assert_eq!(v.lstat(&to), Err(SyscallError::NotFound));
        assert_eq!(v.sync(), Ok(Vec::new()));
        let mut v = remount(v);
        assert_eq!(names(&mut v, "home/staged"), ["a", "z"]);
        assert_eq!(v.lstat(&to), Err(SyscallError::NotFound));
    }
}
