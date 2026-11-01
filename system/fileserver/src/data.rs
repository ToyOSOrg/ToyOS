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
//! **Every name is indexed in memory at mount** ([`DataVolume::names`]), with
//! what it is, kept as an open file changes, so a lookup, a stat, a listing
//! and "is this a directory" are map queries and never a search of the tree.
//!
//! **A file's data lives in the blocks its extents name**, read and written
//! past the cache ([`crate::cache`]). A reserve resolves (allocating where it
//! must) the block of each page a write will touch and moves the length; the
//! write-back puts the pages; the new length and extents reach the file's
//! entry at its close, an fsync, or the volume's sync, and a sync writes every
//! such entry before the cache is flushed. An fsync's commit writes its own
//! file's and those of the files it carries ([`Volume::carried`]): any whose
//! entry is written, or whose blocks were taken, since the last. A length that
//! shrinks is recorded before the blocks past it are freed, so a failure
//! between the two leaks blocks rather than leaving an entry naming freed ones.
//!
//! **What a kill costs.** A sync is the format's commit (`bcachefs`'s
//! `Mounted::sync`): a server killed anywhere leaves every name, and every
//! file's length and extents, as the last sync that finished or as the one it
//! was inside, whole either way; a file's bytes are not so held, since a page
//! written over is written in place. A directory's rename is one operation of
//! the format's (`Mounted::rename_all`): it moves whole, or is refused with
//! nothing moved.
//!
//! Every name a client chose is bounded by the format ([`FsError::NameTooLong`])
//! before it reaches the tree.

use std::collections::BTreeMap;
use std::ops::Bound;
use std::rc::Rc;

use bcachefs::{Extent, Formatted, FsError, Mounted, ReadWrite};
use toyos::fs::Error;

use toyos_blockdev::{Block, BlockDev, BLOCK};

use crate::cache::{Cache, Disk, Shared};
use crate::volume::{join, parent, Identities, Kind, Listed, Meta, Node, OpenHow, Out, Reserved, Volume};

/// The longest symlink target read back: the wire's path bound.
const MAX_LINK: u64 = toyos::fs::MAX_PATH as u64;

/// The most blocks one data request carries.
const RUN: u64 = 256;

/// One file open, while any client holds it.
struct Open {
    path: String,
    extents: Vec<Extent>,
    size: u64,
    mtime: u64,
    /// Length or extents changed since the entry was last written.
    dirty: bool,
    /// The entry was written, or blocks taken, since the last commit
    /// ([`Volume::carried`]).
    carried: bool,
    /// Unlinked or renamed over: its blocks are no longer its own.
    gone: bool,
    holders: u32,
}

pub struct DataVolume<D: Disk> {
    fs: Mounted<Shared<D>, ReadWrite>,
    cache: Rc<Cache<D>>,
    /// Every name on the volume, a directory entry's without its `/`, and
    /// what it is: for a file its length and time, an open node's as it
    /// changes ([`noted`]), which its entry holds once written.
    names: BTreeMap<String, Meta>,
    /// Directories that exist whatever the volume holds: each connection's root.
    roots: Vec<String>,
    open: BTreeMap<Node, Open>,
    by_path: BTreeMap<String, Node>,
    next: Node,
    clock: fn() -> u64,
    /// By path: the format keeps nothing that names a file across a rename.
    ids: Identities<String>,
    /// Where a data request's blocks land, kept so one allocates nothing.
    span: Vec<Block>,
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

/// Where DATA's partition is, from the TOYOS-DATA partitions the block
/// service lists, or why it would not list them.
#[derive(Debug, PartialEq, Eq)]
pub enum Located {
    /// This machine has none: memory stands in.
    Nowhere,
    /// The one partition the block service serves, by its unique GUID.
    Served([u8; 16]),
    /// Two or more, never guessed between, or none countable: DATA is absent.
    Refused(String),
}

pub fn find(served: Result<&[[u8; 16]], String>) -> Located {
    match served {
        Err(why) => Located::Refused(why),
        Ok([]) => Located::Nowhere,
        Ok([one]) => Located::Served(*one),
        Ok(served) => Located::Refused(format!(
            "this machine has {} DATA partitions the block service serves, and a volume is one",
            served.len()
        )),
    }
}

pub(crate) fn refusal(e: &FsError) -> Error {
    match e {
        FsError::NotFound => Error::NotFound,
        FsError::NoSpace { .. } => Error::NoSpace,
        FsError::EntryTooLarge { .. } | FsError::ListTooLong { .. } => Error::LimitExceeded,
        FsError::NameTooLong { .. } => Error::NameTooLong,
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
        | FsError::TargetTooLong { .. } => Error::Io,
    }
}

/// Logs the format's own account, answers the word a client can act on.
fn mapped<T>(what: &str, path: &str, r: Result<T, FsError>) -> Result<T, Error> {
    r.map_err(|e| {
        println!("fileserver: {what} of '{path}' failed: {e:?}");
        refusal(&e)
    })
}

/// A block service that ended is a device that did not do it, as far as a
/// client of this volume can act on.
fn disk_word(e: toyos_blockdev::Error) -> Error {
    match e {
        toyos_blockdev::Error::Device | toyos_blockdev::Error::Range | toyos_blockdev::Error::Gone => Error::Io,
        toyos_blockdev::Error::ReadOnly => Error::ReadOnly,
    }
}

/// What a directory is: the format keeps no length or time for one.
const DIR: Meta = Meta { kind: Kind::Dir, size: 0, mtime: 0 };

/// An open file's length and time, now its name's too: a lookup or a listing
/// reads the index alone.
fn noted(names: &mut BTreeMap<String, Meta>, open: &Open) {
    if let Some(meta) = names.get_mut(&open.path) {
        (meta.size, meta.mtime) = (open.size, open.mtime);
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
    let mut block0 = [[0u8; BLOCK]];
    if cache.block_on(cache.read(0, &mut block0)).is_err() {
        println!("fileserver: block 0 would not read; this disk is not ours to format");
        return false;
    }
    let [block0] = block0;
    let Some(stamped) = bcachefs::designated(&block0) else { return false };
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
    pub fn probe(disk: D, clean_limit: usize, roots: &[&str], clock: fn() -> u64) -> Probed<D> {
        let cache = Rc::new(Cache::new(disk, clean_limit));
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
    pub fn format(disk: D, clean_limit: usize, roots: &[&str], clock: fn() -> u64) -> Result<Self, String> {
        let cache = Rc::new(Cache::new(disk, clean_limit));
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
        for (name, recorded) in listed {
            let meta = match (name.strip_suffix('/'), recorded.symlink) {
                (Some(dir), _) => {
                    names.insert(dir.to_string(), DIR);
                    continue;
                }
                (None, true) => Meta { kind: Kind::Symlink, size: recorded.size, mtime: recorded.mtime },
                (None, false) => Meta { kind: Kind::File, size: recorded.size, mtime: recorded.mtime },
            };
            names.insert(name, meta);
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
            ids: Identities::new(),
            span: Vec::new(),
        })
    }

    /// `path` is gone, and with it every directory above it that existed only
    /// for having something beneath.
    fn forget(&mut self, path: &str) {
        self.ids.forget(&path.to_string());
        self.forget_emptied(path);
    }

    /// Every directory above `path` that existed only for having something
    /// beneath, and has nothing now.
    fn forget_emptied(&mut self, path: &str) {
        let mut dir = parent(path);
        while !dir.is_empty() && !self.is_dir(dir) {
            self.ids.forget(&dir.to_string());
            dir = parent(dir);
        }
    }

    /// Whether anything is named beneath `dir`.
    fn has_children(&self, dir: &str) -> bool {
        let prefix = format!("{dir}/");
        self.names.range(prefix.clone()..).next().is_some_and(|(name, _)| name.starts_with(&prefix))
    }

    fn kind(&self, path: &str) -> Option<Kind> {
        self.names.get(path).map(|meta| meta.kind)
    }

    fn is_dir(&self, path: &str) -> bool {
        path.is_empty()
            || self.roots.iter().any(|r| r == path)
            || self.kind(path) == Some(Kind::Dir)
            || (!self.names.contains_key(path) && self.has_children(path))
    }

    fn entry(&mut self, node: Node) -> Result<&mut Open, Error> {
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        if open.gone {
            return Err(Error::Stale);
        }
        Ok(open)
    }

    /// Write one open file's length and extents to its entry.
    fn persist(&mut self, node: Node) -> Result<(), Error> {
        let Some(open) = self.open.get_mut(&node) else { return Ok(()) };
        if !open.dirty || open.gone {
            return Ok(());
        }
        mapped("the entry", &open.path, self.fs.update_metadata(&open.path, &open.extents, open.size, open.mtime))?;
        open.dirty = false;
        open.carried = true;
        if let Some(meta) = self.names.get_mut(&open.path) {
            (meta.size, meta.mtime) = (open.size, open.mtime);
        }
        Ok(())
    }

    /// The disk as a stop now leaves it: what the last flush wrote.
    #[cfg(test)]
    pub(crate) fn stopped(self) -> D {
        drop(self.fs);
        Rc::try_unwrap(self.cache).ok().expect("one owner").into_disk()
    }

    /// A commit landed: no entry it made durable is owed to the next.
    fn committed(&mut self) {
        for open in self.open.values_mut().filter(|open| !open.dirty) {
            open.carried = false;
        }
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
        self.open.insert(node, Open { path: path.to_string(), extents, size, mtime, dirty: false, carried: false, gone: false, holders: 0 });
        self.by_path.insert(path.to_string(), node);
        node
    }

    /// A name is made under directories that need not exist yet — the format
    /// makes every prefix one, as the kernel's mount did — but never under a
    /// file or a symlink, which would then be two things at once.
    fn require_parent_dir(&self, path: &str) -> Result<(), Error> {
        let mut dir = parent(path);
        while !dir.is_empty() {
            if matches!(self.kind(dir), Some(Kind::File | Kind::Symlink)) {
                return Err(Error::NotADirectory);
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

    fn lstat(&mut self, path: &str) -> Result<Meta, Error> {
        match self.names.get(path) {
            Some(&recorded) => Ok(recorded),
            None if self.is_dir(path) => Ok(DIR),
            None => Err(Error::NotFound),
        }
    }

    fn read_link(&mut self, path: &str) -> Result<String, Error> {
        if self.kind(path) != Some(Kind::Symlink) {
            return Err(if self.names.contains_key(path) || self.is_dir(path) { Error::NotALink } else { Error::NotFound });
        }
        mapped("readlink", path, self.fs.read_link(path, MAX_LINK))?.ok_or(Error::Io)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error> {
        if !self.is_dir(dir) {
            return Err(if self.names.contains_key(dir) { Error::NotADirectory } else { Error::NotFound });
        }
        // The names beneath a child directory are passed over whole: a
        // listing costs the directory's children, not everything below it.
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut children = Vec::new();
        let mut from = prefix.clone();
        loop {
            let mut past = None;
            for (name, &recorded) in self.names.range::<str, _>((Bound::Included(from.as_str()), Bound::Unbounded)) {
                let Some(rest) = name.strip_prefix(&prefix) else { break };
                let Some((child, _)) = rest.split_once('/') else {
                    children.push((rest.to_string(), recorded));
                    continue;
                };
                // A child with an entry of its own was listed at it.
                if !self.names.contains_key(&format!("{prefix}{child}")) {
                    children.push((child.to_string(), DIR));
                }
                // Every name beneath `child/`, and nothing else, sorts before
                // `child0`: `0` follows `/`.
                past = Some(format!("{prefix}{child}0"));
                break;
            }
            match past {
                Some(next) => from = next,
                None => break,
            }
        }
        children.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));
        Ok(children.into_iter().map(|(name, meta)| Listed { identity: self.ids.of(join(dir, &name)), name, meta }).collect())
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, Error> {
        if self.is_dir(path) {
            return Err(Error::IsADirectory);
        }
        let node = match self.by_path.get(path).copied() {
            Some(_) if how.create_new => return Err(Error::AlreadyExists),
            Some(node) => node,
            None => match self.names.get(path).copied() {
                Some(_) if how.create_new => return Err(Error::AlreadyExists),
                Some(Meta { kind: Kind::File, mtime, .. }) => {
                    let (extents, size) =
                        mapped("open", path, self.fs.file_extents(path))?.ok_or(Error::NotFound)?;
                    self.new_node(path, extents, size, mtime)
                }
                // A link the resolver did not follow is one with nothing behind it.
                Some(_) => return Err(Error::NotFound),
                None if how.create || how.create_new => {
                    self.require_parent_dir(path)?;
                    let now = (self.clock)();
                    mapped("create", path, self.fs.create(path, &[], now))?;
                    self.names.insert(path.to_string(), Meta { kind: Kind::File, size: 0, mtime: now });
                    self.new_node(path, Vec::new(), 0, now)
                }
                None => return Err(Error::NotFound),
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

    fn close(&mut self, node: Node) -> Result<(), Error> {
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

    fn node_meta(&mut self, node: Node) -> Result<Meta, Error> {
        let open = self.entry(node)?;
        Ok(Meta { kind: Kind::File, size: open.size, mtime: open.mtime })
    }

    fn identity(&mut self, path: &str) -> Result<u64, Error> {
        if !self.names.contains_key(path) && !self.is_dir(path) {
            return Err(Error::NotFound);
        }
        Ok(self.ids.of(path.to_string()))
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, Error> {
        let cache = Rc::clone(&self.cache);
        let mut span = std::mem::take(&mut self.span);
        let open = self.entry(node)?;
        if offset >= open.size {
            self.span = span;
            return Ok(0);
        }
        let n = out.len().min((open.size - offset) as usize);
        let mut done = 0;
        let mut read = Ok(());
        while done < n && read.is_ok() {
            let at = offset + done as u64;
            let page = at / BLOCK as u64;
            let within = (at % BLOCK as u64) as usize;
            let take = (BLOCK - within).min(n - done);
            let Some(first) = block_for(&open.extents, page) else {
                // Past the extents: a hole, whose bytes are zeros.
                out.zero(done, take);
                done += take;
                continue;
            };
            // The pages from here whose blocks are consecutive go as one read.
            let need = (within + (n - done)).div_ceil(BLOCK) as u64;
            let mut run = 1u64;
            while run < need.min(RUN) && block_for(&open.extents, page + run) == Some(first + run) {
                run += 1;
            }
            span.resize(run as usize, [0; BLOCK]);
            read = cache.block_on(cache.read_data(first, &mut span[..run as usize])).map_err(disk_word);
            if read.is_ok() {
                let bytes = &span[..run as usize].as_flattened()[within..];
                let take = bytes.len().min(n - done);
                out.put(done, &bytes[..take]);
                done += take;
            }
        }
        self.span = span;
        read.map(|()| n)
    }

    fn reserve(&mut self, node: Node, offset: u64, len: u64) -> Result<Reserved, Error> {
        let end = offset.checked_add(len).ok_or(Error::FileTooLarge)?;
        let now = (self.clock)();
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        if open.gone {
            return Err(Error::Stale);
        }
        // Every page past the extents is allocated before the size moves, so
        // a write the file's one entry could not name is refused whole.
        let covered: u64 = open.extents.iter().map(|e| e.block_count as u64).sum();
        let reserved = Reserved { size: open.size, held: covered };
        let last = end.div_ceil(BLOCK as u64).saturating_sub(1);
        let grown = match len != 0 && last >= covered {
            false => Ok(()),
            true => u32::try_from(offset / BLOCK as u64)
                .and_then(|first| Ok(first..=u32::try_from(last)?))
                .map_err(|_| Error::FileTooLarge)
                // Runs a block-ring request long, so each costs the flush one request.
                .and_then(|pages| mapped("an allocation", &open.path, self.fs.allocate(&mut open.extents, pages, toyos_blockring::MAX_REQUEST_BLOCKS)))
                .and_then(|()| match bcachefs::file_entry_fits(&open.path, &open.extents) {
                    true => Ok(()),
                    false => {
                        println!(
                            "fileserver: a write to '{}' is refused: its entry would name {} runs of blocks, more than it holds",
                            open.path,
                            open.extents.len()
                        );
                        Err(Error::LimitExceeded)
                    }
                }),
        };
        if let Err(e) = grown {
            let dropped = keep_blocks(&mut open.extents, covered);
            mapped("a refused write's free", &open.path, self.fs.free_extents(&dropped))?;
            return Err(e);
        }
        open.size = open.size.max(end);
        open.mtime = now;
        open.dirty = true;
        open.carried |= open.extents.iter().map(|e| e.block_count as u64).sum::<u64>() > covered;
        noted(&mut self.names, open);
        Ok(reserved)
    }

    fn unreserve(&mut self, node: Node, reserved: Reserved, landed: u64) -> Result<(), Error> {
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        // A block a write took in a hole below the old end lies inside the
        // old size, so it goes back by what the file held, not by the size.
        let dropped = keep_blocks(&mut open.extents, reserved.held.max(landed.div_ceil(BLOCK as u64)));
        open.size = reserved.size.max(landed);
        noted(&mut self.names, open);
        // No entry names them: only the file's own calls write its entry.
        let path = open.path.clone();
        mapped("a refused write's free", &path, self.fs.free_extents(&dropped))
    }

    fn write_back(&mut self, node: Node, offset: u64, data: &[u8]) -> Result<(), Error> {
        let cache = Rc::clone(&self.cache);
        let mut span = std::mem::take(&mut self.span);
        let open = self.entry(node)?;
        let written = (|| {
            let end = offset.checked_add(data.len() as u64).ok_or(Error::FileTooLarge)?;
            if !offset.is_multiple_of(BLOCK as u64) || end > open.size {
                println!("fileserver: a write-back of '{}' at {offset}+{} lies outside its {} bytes", open.path, data.len(), open.size);
                return Err(Error::Io);
            }
            let first = offset / BLOCK as u64;
            let pages = data.len().div_ceil(BLOCK);
            let mut k = 0;
            while k < pages {
                let block = block_for(&open.extents, first + k as u64).ok_or(Error::Io)?;
                let mut run = 1;
                while k + run < pages && run < RUN as usize && block_for(&open.extents, first + (k + run) as u64) == Some(block + run as u64) {
                    run += 1;
                }
                span.clear();
                span.resize(run, [0; BLOCK]);
                let bytes = &data[k * BLOCK..((k + run) * BLOCK).min(data.len())];
                span.as_flattened_mut()[..bytes.len()].copy_from_slice(bytes);
                cache.block_on(cache.write_data(block, &span)).map_err(disk_word)?;
                k += run;
            }
            Ok(())
        })();
        self.span = span;
        written
    }

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), Error> {
        let cache = Rc::clone(&self.cache);
        let now = (self.clock)();
        let open = self.open.get_mut(&node).ok_or(Error::Stale)?;
        if open.gone {
            return Err(Error::Stale);
        }
        if size >= open.size {
            open.size = size;
            open.mtime = now;
            open.dirty = true;
            noted(&mut self.names, open);
            return Ok(());
        }
        // What the last kept page holds past the new end is zeroed, so a file
        // that grows again reads zeros there and not what it held before.
        let within = (size % BLOCK as u64) as usize;
        if within != 0 {
            if let Some(block) = block_for(&open.extents, size / BLOCK as u64) {
                let mut page: [Block; 1] = [[0; BLOCK]];
                cache.block_on(cache.read_data(block, &mut page)).map_err(disk_word)?;
                page[0][within..].fill(0);
                cache.block_on(cache.write_data(block, &page)).map_err(disk_word)?;
            }
        }
        let dropped = keep_blocks(&mut open.extents, size.div_ceil(BLOCK as u64));
        open.size = size;
        open.mtime = now;
        // Recorded first, freed second.
        mapped("a truncate", &open.path, self.fs.update_metadata(&open.path, &open.extents, size, now))?;
        open.dirty = false;
        open.carried = true;
        if let Some(meta) = self.names.get_mut(&open.path) {
            (meta.size, meta.mtime) = (size, now);
        }
        let path = open.path.clone();
        mapped("a truncate's free", &path, self.fs.free_extents(&dropped))
    }

    fn set_mtime(&mut self, node: Node, mtime: u64) -> Result<(), Error> {
        let open = self.open.get_mut(&node).filter(|open| !open.gone).ok_or(Error::Stale)?;
        open.mtime = mtime;
        open.dirty = true;
        noted(&mut self.names, open);
        Ok(())
    }

    fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        if self.names.contains_key(path) || self.is_dir(path) {
            return Err(Error::AlreadyExists);
        }
        self.require_parent_dir(path)?;
        let now = (self.clock)();
        mapped("mkdir", path, self.fs.create(&format!("{path}/"), &[], now))?;
        self.names.insert(path.to_string(), DIR);
        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        if self.roots.iter().any(|r| r == path) || path.is_empty() {
            return Err(Error::Root);
        }
        if !self.is_dir(path) {
            return Err(if self.names.contains_key(path) { Error::NotADirectory } else { Error::NotFound });
        }
        if self.has_children(path) {
            return Err(Error::NotEmpty);
        }
        let marker = format!("{path}/");
        if !mapped("rmdir", path, self.fs.delete(&marker))? {
            return Err(Error::NotFound);
        }
        self.names.remove(path);
        self.forget(path);
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), Error> {
        match self.kind(path) {
            Some(Kind::File) | Some(Kind::Symlink) => {}
            Some(Kind::Dir) => return Err(Error::IsADirectory),
            None if self.is_dir(path) => return Err(Error::IsADirectory),
            None => return Err(Error::NotFound),
        }
        // Asked of the volume first: a delete the device refused leaves every
        // holder of the file its file.
        if !mapped("unlink", path, self.fs.delete(path))? {
            return Err(Error::NotFound);
        }
        self.orphan(path);
        self.names.remove(path);
        self.forget(path);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        if from == to {
            return self.lstat(from).map(drop);
        }
        let kind = self.lstat(from)?.kind;
        self.require_parent_dir(to)?;
        if self.roots.iter().any(|r| r == from || r == to) {
            return Err(Error::Root);
        }
        match kind {
            Kind::File | Kind::Symlink => {
                if self.is_dir(to) {
                    return Err(Error::IsADirectory);
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
                let moved = self.names.remove(from).expect("a file the volume names");
                self.names.insert(to.to_string(), moved);
                if let Some(node) = self.by_path.remove(from) {
                    self.open.get_mut(&node).expect("indexed").path = to.to_string();
                    self.by_path.insert(to.to_string(), node);
                }
                self.ids.moved(&from.to_string(), to.to_string());
                self.forget_emptied(from);
                Ok(())
            }
            Kind::Dir => {
                if self.names.contains_key(to) || self.is_dir(to) {
                    return Err(Error::AlreadyExists);
                }
                if to.starts_with(&format!("{from}/")) {
                    return Err(Error::IntoItself);
                }
                // Every entry beneath, and the directory's own, in one
                // operation of the format's: the directory moves whole or not at all.
                let prefix = format!("{from}/");
                let moving: Vec<(String, Meta)> = self
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
                    .map(|(name, meta)| {
                        let moved = format!("{to}{}", &name[from.len()..]);
                        match meta.kind {
                            Kind::Dir => (format!("{name}/"), format!("{moved}/")),
                            _ => (name.clone(), moved),
                        }
                    })
                    .collect();
                let pairs: Vec<(&str, &str)> = renames.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                mapped("rename", from, self.fs.rename_all(&pairs))?;
                for (name, _) in moving {
                    let moved = format!("{to}{}", &name[from.len()..]);
                    // The record as it is now, which the persists above updated.
                    let meta = self.names.remove(&name).expect("a name the volume holds");
                    self.names.insert(moved.clone(), meta);
                    if let Some(node) = self.by_path.remove(&name) {
                        self.open.get_mut(&node).expect("indexed").path = moved.clone();
                        self.by_path.insert(moved, node);
                    }
                }
                self.ids.moved_tree(from, to);
                self.forget_emptied(from);
                Ok(())
            }
        }
    }

    fn symlink(&mut self, path: &str, target: &str) -> Result<(), Error> {
        if self.names.contains_key(path) || self.is_dir(path) {
            return Err(Error::AlreadyExists);
        }
        self.require_parent_dir(path)?;
        mapped("symlink", path, self.fs.create_symlink(path, target))?;
        // What the format's symlink entry records: its target's length, undated.
        self.names.insert(path.to_string(), Meta { kind: Kind::Symlink, size: target.len() as u64, mtime: 0 });
        Ok(())
    }

    fn sync(&mut self) -> Result<Vec<(Node, Error)>, Error> {
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
        self.committed();
        Ok(unwritten)
    }

    fn carried(&self, node: Node) -> bool {
        self.open.get(&node).is_some_and(|open| open.carried && !open.gone)
    }

    fn commit(&mut self, node: Option<Node>) -> Result<(), Error> {
        if let Some(node) = node {
            self.persist(node)?;
        }
        let carried: Vec<Node> = self.open.keys().copied().filter(|&n| self.carried(n)).collect();
        for n in carried {
            match self.persist(n) {
                Err(e) => println!("fileserver: node {n}'s entry was not written at a commit ({e:?}); the next one writes it"),
                Ok(()) if self.open[&n].holders == 0 => self.release(n),
                Ok(()) => {}
            }
        }
        mapped("sync", "", self.fs.sync())?;
        self.committed();
        Ok(())
    }

    fn room(&mut self) -> Result<toyos::fs::Room, Error> {
        Ok(toyos::fs::Room { free: self.fs.free_bytes(), unit: BLOCK as u64 })
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
    use crate::volume::Writes;
    use crate::ram::Ram;
    use crate::volume::Buf;

    fn clock() -> u64 {
        1_000_000_000
    }

    /// The clean blocks every volume's cache here keeps: 64 MiB.
    const CLEAN_LIMIT: usize = 16 * 1024;

    fn vol() -> DataVolume<Ram> {
        DataVolume::format(Ram::new(4096), CLEAN_LIMIT, &["home", "apps"], clock).unwrap()
    }

    const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
    const PLAIN: OpenHow = OpenHow { create: false, create_new: false, truncate: false };

    /// Two DATA partitions are refused, and a block service that would not
    /// list what it has leaves DATA absent: memory stands in only where it
    /// says there is none.
    #[test]
    fn data_is_one_partition_the_block_service_serves() {
        let (a, b) = ([1; 16], [2; 16]);
        assert_eq!(find(Ok(&[])), Located::Nowhere);
        assert_eq!(find(Ok(&[a])), Located::Served(a));
        let Located::Refused(why) = find(Ok(&[a, b])) else { panic!("two located one") };
        assert!(why.starts_with("this machine has 2 DATA partitions"), "{why}");
        assert_eq!(find(Err("unlisted".into())), Located::Refused("unlisted".into()));
    }

    /// **A symlink where a grant's root is, is no directory**, so a connection
    /// granted that root reaches nothing through it: the folder it named is
    /// listed by nobody and nothing is made under it, and the directory the
    /// link names is untouched. The supervisor checks a folder before it
    /// grants it; this is what holds once a declared row replaces it after.
    #[test]
    fn a_symlink_at_a_grant_s_root_is_followed_by_nothing() {
        let mut v = vol();
        v.mkdir("home/toy/Apps/other").unwrap();
        let kept = v.open("home/toy/Apps/other/kept", CREATE).unwrap();
        v.close(kept).unwrap();
        v.symlink("home/toy/Games", "Apps/other").unwrap();
        let root = "home/toy/Games";
        assert_eq!(v.lstat(root).unwrap().kind, Kind::Symlink);
        assert_eq!(v.list(root).map(drop), Err(Error::NotADirectory));
        assert_eq!(v.lstat("home/toy/Games/kept"), Err(Error::NotFound));
        assert_eq!(v.open("home/toy/Games/kept", PLAIN).map(drop), Err(Error::NotFound));
        assert_eq!(v.open("home/toy/Games/made", CREATE).map(drop), Err(Error::NotADirectory));
        assert_eq!(v.mkdir("home/toy/Games/sub"), Err(Error::NotADirectory));
        let listed: Vec<String> = v.list("home/toy/Apps/other").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(listed, ["kept"]);
    }

    /// A file's pages are the paged objects' to keep: written and read, they
    /// leave the block cache holding the volume's structures and no more.
    #[test]
    fn a_files_pages_pass_the_cache_by() {
        let mut v = vol();
        let n = v.open("home/big", CREATE).unwrap();
        v.write(n, 0, &vec![7u8; 256 * BLOCK]).unwrap();
        v.sync().unwrap();
        let before = v.cache.counts().cached;
        let mut out = vec![0u8; 256 * BLOCK];
        assert_eq!(v.read(n, 0, &mut Buf(&mut out)).unwrap(), out.len());
        assert!(out.iter().all(|&b| b == 7));
        assert_eq!(v.cache.counts().cached, before, "a data read keeps nothing");
        assert!(before < 64, "{before} blocks held for one file's structures");
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
        assert_eq!(v.read(n, 0, &mut Buf(&mut [0u8; 1])), Err(Error::Stale));
        assert_eq!(v.lstat("home/a"), Err(Error::NotFound));
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
        assert_eq!(v.open("home/nodir/a/b", CREATE), Err(Error::NotADirectory), "never under a file");
        v.mkdir("home/d").unwrap();
        assert_eq!(v.mkdir("home/d"), Err(Error::AlreadyExists));
        let n = v.open("home/d/f", CREATE).unwrap();
        v.close(n).unwrap();
        assert_eq!(v.rmdir("home/d"), Err(Error::NotEmpty));
        let names: Vec<String> = v.list("home").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["d", "nodir"]);
        v.unlink("home/d/f").unwrap();
        v.rmdir("home/d").unwrap();
        assert_eq!(v.lstat("home/d"), Err(Error::NotFound));
        assert_eq!(v.rmdir("home"), Err(Error::Root), "a root stays");
    }

    /// A file's number is one across renames, its own directory's and one
    /// over another file; every file that exists at once has its own; and a
    /// directory that held only what was removed is gone with its number.
    #[test]
    fn a_file_is_one_number_across_renames_and_another_files_is_another() {
        let mut v = vol();
        for path in ["home/a/f", "home/a/g", "home/x/only"] {
            let n = v.open(path, CREATE).unwrap();
            v.close(n).unwrap();
        }
        v.mkdir("home/empty").unwrap();
        let ids: Vec<u64> = ["home/a/f", "home/a/g", "home/a", "home/x", "home/x/only", "home/empty", "home", ""]
            .iter()
            .map(|p| v.identity(p).unwrap())
            .collect();
        assert!(ids.iter().all(|&n| n != 0 && ids.iter().filter(|&&m| m == n).count() == 1), "{ids:?}");
        let (f, g, a, x) = (ids[0], ids[1], ids[2], ids[3]);
        assert_eq!(v.identity("home/nothing"), Err(Error::NotFound));

        v.rename("home/a", "home/b").unwrap();
        assert_eq!(v.identity("home/b"), Ok(a), "a directory's rename keeps its number");
        assert_eq!(v.identity("home/b/f"), Ok(f), "and every file's beneath it");
        v.rename("home/b/g", "home/b/f").unwrap();
        assert_eq!(v.identity("home/b/f"), Ok(g), "a rename over a file takes its name, not its number");

        v.unlink("home/x/only").unwrap();
        assert_eq!(v.identity("home/x"), Err(Error::NotFound), "a directory that held only the file went");
        let n = v.open("home/x/only", CREATE).unwrap();
        v.close(n).unwrap();
        let (dir, file) = (v.identity("home/x").unwrap(), v.identity("home/x/only").unwrap());
        assert!(!ids.contains(&dir) && !ids.contains(&file), "made again, each is a new file: {dir}, {file} of {ids:?}");
        assert_ne!(dir, x);
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

    /// Every name the index records is what the format's own entry holds,
    /// for each closed file and symlink: the stat a client is answered from
    /// memory is the one a search of the tree would answer.
    fn assert_index_is_the_format_s(v: &DataVolume<Ram>) {
        for (name, meta) in &v.names {
            if meta.kind == Kind::Dir || v.by_path.contains_key(name) {
                continue;
            }
            let (_, size) = v.fs.file_extents(name).unwrap().unwrap_or_else(|| panic!("{name} has no entry"));
            let mtime = v.fs.file_mtime(name).unwrap().unwrap();
            assert_eq!((meta.size, meta.mtime), (size, mtime), "{name}");
            assert_eq!(meta.kind == Kind::Symlink, v.fs.is_symlink(name).unwrap(), "{name}");
        }
    }

    /// A clock that reads a later nanosecond at every call, so a time left
    /// stale anywhere is a time that differs.
    fn advancing_clock() -> u64 {
        static NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_000_000_000);
        NOW.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[test]
    fn a_stat_from_the_index_is_what_the_entry_records() {
        let mut v = DataVolume::format(Ram::new(4096), CLEAN_LIMIT, &["home"], advancing_clock).unwrap();
        let a = v.open("home/a", CREATE).unwrap();
        v.write(a, 0, &[1; 5000]).unwrap();
        assert_eq!(v.lstat("home/a").unwrap().size, 5000, "an open file's own length, before its entry has it");
        v.close(a).unwrap();
        assert_index_is_the_format_s(&v);

        let b = v.open("home/b", CREATE).unwrap();
        v.write(b, 0, &[2; 9000]).unwrap();
        v.truncate(b, 100).unwrap();
        v.close(b).unwrap();
        v.symlink("home/l", "a").unwrap();
        v.rename("home/a", "home/sub/c").unwrap();
        v.rename("home/b", "home/sub/c").unwrap();
        // Written and still open while its directory moves.
        let open = v.open("home/sub/open", CREATE).unwrap();
        v.write(open, 0, &[3; 7000]).unwrap();
        let written = v.lstat("home/sub/open").unwrap();
        v.mkdir("home/d").unwrap();
        v.rename("home/sub", "home/d/moved").unwrap();
        v.close(open).unwrap();
        assert_index_is_the_format_s(&v);
        assert_eq!(v.lstat("home/d/moved/open").unwrap(), written, "the open file's length and time moved with it");
        assert_eq!(v.lstat("home/d/moved/c").unwrap().size, 100);
        assert_eq!(v.lstat("home/l").unwrap(), Meta { kind: Kind::Symlink, size: 1, mtime: 0 });
        v.unlink("home/l").unwrap();
        assert_eq!(v.lstat("home/l"), Err(Error::NotFound));

        let before: Vec<(String, Meta)> = v.names.iter().map(|(n, m)| (n.clone(), *m)).collect();
        v.sync().unwrap();
        let DataVolume { fs, cache, .. } = v;
        drop(fs);
        let disk = Rc::try_unwrap(cache).ok().expect("one owner").into_disk();
        let Probed::Mounted(again) = DataVolume::probe(disk, CLEAN_LIMIT, &["home"], advancing_clock) else { panic!("remount") };
        let after: Vec<(String, Meta)> = again.names.iter().map(|(n, m)| (n.clone(), *m)).collect();
        assert_eq!(after, before, "the index a mount reads is the one the server kept");
        assert_index_is_the_format_s(&again);
    }

    /// A listing passes over what is beneath a child directory and names
    /// each child once: `x` with an entry of its own and names beneath, `y`
    /// only the prefix of two, and the names beside `x` that sort among its
    /// own (`-` and `.` before `/`, `0` after).
    #[test]
    fn a_listing_names_each_child_once_and_nothing_beneath_one() {
        let mut v = vol();
        v.mkdir("home/x").unwrap();
        for path in ["home/x-a", "home/x.b", "home/x/deep/er/f", "home/x/g", "home/x0", "home/y/z", "home/y/z2"] {
            let n = v.open(path, CREATE).unwrap();
            v.close(n).unwrap();
        }
        v.mkdir("home/x/kept").unwrap();
        v.mkdir("home/e").unwrap();
        let listed: Vec<(String, Kind)> = v.list("home").unwrap().into_iter().map(|l| (l.name, l.meta.kind)).collect();
        let want = [("e", Kind::Dir), ("x", Kind::Dir), ("x-a", Kind::File), ("x.b", Kind::File), ("x0", Kind::File), ("y", Kind::Dir)];
        assert_eq!(listed, want.map(|(n, k)| (n.to_string(), k)));
        let inner: Vec<String> = v.list("home/x").unwrap().into_iter().map(|l| l.name).collect();
        assert_eq!(inner, ["deep", "g", "kept"]);
        let root: Vec<String> = v.list("").unwrap().into_iter().map(|l| l.name).collect();
        assert_eq!(root, ["home"]);
    }

    /// A clock that reads no whole second.
    fn nanosecond_clock() -> u64 {
        1_717_245_296_123_456_789
    }

    /// DATA stamps a write with the nanosecond its clock read, and the entry
    /// keeps it whole across a remount: a stamp of whole seconds would round
    /// it away.
    #[test]
    fn a_write_s_stamp_keeps_its_nanosecond_across_a_remount() {
        let mut v = DataVolume::format(Ram::new(4096), CLEAN_LIMIT, &["home"], nanosecond_clock).unwrap();
        let n = v.open("home/stamped", CREATE).unwrap();
        v.write(n, 0, b"stamped").unwrap();
        v.close(n).unwrap();
        v.sync().unwrap();
        let DataVolume { fs, cache, .. } = v;
        drop(fs);
        let disk = Rc::try_unwrap(cache).ok().expect("one owner").into_disk();
        let Probed::Mounted(mut again) = DataVolume::probe(disk, CLEAN_LIMIT, &["home"], nanosecond_clock) else { panic!("remount") };
        assert_eq!(again.lstat("home/stamped").unwrap().mtime, nanosecond_clock());
    }

    fn remount(v: DataVolume<Ram>) -> DataVolume<Ram> {
        let disk = v.stopped();
        let Probed::Mounted(again) = DataVolume::probe(disk, CLEAN_LIMIT, &["home"], clock) else { panic!("remount") };
        again
    }

    /// A volume whose free blocks are all apart, so every run a file is given
    /// is one block: every block a file's data may take, taken in one run,
    /// every other one given back, and the run the reserve kept
    /// (`bcachefs`'s `NODE_RESERVE`, 16) taken after.
    fn fragmented() -> DataVolume<Ram> {
        let mut v = DataVolume::format(Ram::new(1024), CLEAN_LIMIT, &["home"], clock).unwrap();
        let mut run = Vec::new();
        let mut page = 0;
        while v.fs.allocate(&mut run, page..=page, 1).is_ok() {
            page += 1;
        }
        let taken: Vec<u64> = run.iter().flat_map(|e| e.start_block..e.start_block + e.block_count as u64).collect();
        let holes: Vec<Extent> =
            taken.iter().step_by(2).map(|&b| Extent { start_block: b, block_count: 1, _reserved: 0 }).collect();
        v.fs.free_extents(&holes).unwrap();
        let mut tail = vec![Extent { start_block: *taken.last().unwrap(), block_count: 1, _reserved: 0 }];
        v.fs.allocate(&mut tail, 16..=16, 1).unwrap();
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
                    Err(Error::NoSpace),
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
        let listed: Vec<String> = v.list("home").unwrap().into_iter().map(|e| format!("home/{}", e.name)).collect();
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
        assert_eq!(refused, Error::LimitExceeded);
        // One block a run: (4064 − 24 − 19 − "home/a".len()) / 16 runs is
        // what the one entry holding `home/a` names.
        assert_eq!(pages, 250, "the entry filled, not the volume");
        assert_eq!(page_count(&mut v, "home/a"), pages, "the refused write changed nothing");
        let before = free_blocks(&mut v);
        assert_eq!(v.write(a, pages * BLOCK as u64, &[4; 4 * BLOCK]), Err(Error::LimitExceeded));
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

        assert_eq!(v.sync(), Ok(vec![(a, Error::LimitExceeded)]));
        v.close(b).unwrap();
        assert_eq!(v.close(a), Err(Error::LimitExceeded), "a close that lost the writes is refused");
        assert_eq!(v.sync(), Ok(vec![(a, Error::LimitExceeded)]), "unheld, it is named again");
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

        assert_eq!(v.rename("home/f", &to_path), Err(Error::LimitExceeded));

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
        reads: std::cell::Cell<usize>,
        writes: std::cell::Cell<usize>,
        flushes: std::cell::Cell<usize>,
        keep: usize,
        refuse: Option<usize>,
        /// A block whose read the disk refuses.
        refuse_read: std::cell::Cell<Option<u64>>,
    }

    impl Stops {
        fn new(blocks: u64) -> Self {
            Self { blocks, image: Rc::default(), reads: Default::default(), writes: Default::default(), flushes: Default::default(), keep: usize::MAX, refuse: None, refuse_read: Default::default() }
        }

        fn from(image: &Image, blocks: u64, keep: usize, refuse: Option<usize>) -> Self {
            Self { blocks, image: Rc::new(image.clone().into()), reads: Default::default(), writes: Default::default(), flushes: Default::default(), keep, refuse, refuse_read: Default::default() }
        }
    }

    impl BlockDev for Stops {
        fn blocks(&self) -> u64 {
            self.blocks
        }

        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), toyos_blockdev::Error> {
            self.reads.set(self.reads.get() + out.len());
            if self.refuse_read.get().is_some_and(|b| (lba..lba + out.len() as u64).contains(&b)) {
                return Err(toyos_blockdev::Error::Device);
            }
            let image = self.image.borrow();
            for (block, out) in (lba..).zip(out) {
                match image.get(&block) {
                    Some(data) => *out = **data,
                    None => out.fill(0),
                }
            }
            Ok(())
        }

        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), toyos_blockdev::Error> {
            for (block, data) in (lba..).zip(data) {
                let n = self.writes.get();
                self.writes.set(n + 1);
                if Some(n) == self.refuse {
                    return Err(toyos_blockdev::Error::Device);
                }
                if n < self.keep {
                    self.image.borrow_mut().insert(block, Box::new(*data));
                }
            }
            Ok(())
        }

        async fn flush(&self) -> Result<(), toyos_blockdev::Error> {
            self.flushes.set(self.flushes.get() + 1);
            Ok(())
        }
    }

    impl Disk for Stops {}

    /// A sync with nothing written since the last asks the disk for nothing,
    /// and one after a write over a page the file holds writes the page and
    /// makes it durable. The sync after a commit still writes the blocks that
    /// commit freed, which are marked free only once it has landed.
    #[test]
    fn a_sync_with_nothing_new_asks_the_disk_for_nothing() {
        let mut v = DataVolume::format(Stops::new(1024), CLEAN_LIMIT, &["home", "apps"], clock).unwrap();
        let n = v.open("home/f", CREATE).unwrap();
        v.write(n, 0, &[1; BLOCK]).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert_eq!(v.sync(), Ok(Vec::new()));
        let asked = |v: &DataVolume<Stops>| (v.cache.disk().writes.get(), v.cache.disk().flushes.get());
        let synced = asked(&v);
        assert_eq!(v.sync(), Ok(Vec::new()));
        assert_eq!(asked(&v), synced);

        v.write(n, 0, &[2; BLOCK]).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let (writes, flushes) = asked(&v);
        assert!(writes > synced.0 && flushes > synced.1, "the write over the page asked {:?} after {synced:?}", (writes, flushes));
        let block = block_for(&v.open[&n].extents, 0).unwrap();
        assert_eq!(v.cache.disk().image.borrow()[&block][..], [2; BLOCK][..]);
    }

    /// A write refused after its pages were allocated gives them back: a file
    /// extended over where it would have reached reads zeros there, not the
    /// bytes a deleted file left in the blocks the write was given.
    #[test]
    fn a_write_refused_after_allocating_leaves_no_old_bytes_past_the_end() {
        // Every block the volume does not write holds what an earlier tenant left.
        let stale: Image = (0..1024).map(|b| (b, Box::new([0xAB; BLOCK]))).collect();
        let mut v = DataVolume::format(Stops::from(&stale, 1024, usize::MAX, None), 0, &["home", "apps"], clock).unwrap();

        let n = v.open("home/f", CREATE).unwrap();
        v.write(n, 0, &[1; BLOCK]).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let first = block_for(&v.open[&n].extents, 0).unwrap();
        v.cache.disk().refuse_read.set(Some(first));
        // Half of page 0, whose read is refused, and page 1, allocated first.
        let room = v.room().unwrap().free;
        assert_eq!(v.write(n, BLOCK as u64 / 2, &[2; BLOCK]), Err(Error::Io));
        assert_eq!(v.room().unwrap().free, room, "the refused write kept blocks, or gave back more");
        v.cache.disk().refuse_read.set(None);

        v.truncate(n, 2 * BLOCK as u64).unwrap();
        let mut out = vec![0u8; BLOCK];
        assert_eq!(v.read(n, BLOCK as u64, &mut Buf(&mut out)).unwrap(), BLOCK);
        assert!(out.iter().all(|&b| b == 0), "page 1 reads {:#04x}, not zeros", out[out.iter().position(|&b| b != 0).unwrap()]);
    }

    /// The same inside the size: a refused write into a hole the file was
    /// extended over gives back the block it took there, which no cut back to
    /// the old size would, and the hole still reads zeros.
    #[test]
    fn a_write_refused_in_a_hole_leaves_the_hole_reading_zeros() {
        let stale: Image = (0..1024).map(|b| (b, Box::new([0xAB; BLOCK]))).collect();
        let mut v = DataVolume::format(Stops::from(&stale, 1024, usize::MAX, None), 0, &["home", "apps"], clock).unwrap();

        let n = v.open("home/f", CREATE).unwrap();
        v.write(n, 0, &[1; BLOCK]).unwrap();
        v.truncate(n, 3 * BLOCK as u64).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        let first = block_for(&v.open[&n].extents, 0).unwrap();
        v.cache.disk().refuse_read.set(Some(first));
        // Half of page 0, whose read is refused, and page 1, in the hole.
        let room = v.room().unwrap().free;
        assert_eq!(v.write(n, BLOCK as u64 / 2, &[2; BLOCK]), Err(Error::Io));
        assert_eq!(v.room().unwrap().free, room, "the refused write kept the hole's block, or gave back more");
        v.cache.disk().refuse_read.set(None);

        assert_eq!(v.node_meta(n).unwrap().size, 3 * BLOCK as u64);
        let mut out = vec![0u8; BLOCK];
        assert_eq!(v.read(n, BLOCK as u64, &mut Buf(&mut out)).unwrap(), BLOCK);
        assert!(out.iter().all(|&b| b == 0), "page 1 reads {:#04x}, not zeros", out[out.iter().position(|&b| b != 0).unwrap()]);
    }

    /// A byte written into a hole below the old end: the rest of its page
    /// reads zeros, not what the block its reserve took last held.
    #[test]
    fn a_write_in_part_into_a_hole_reads_zeros_around_it() {
        let stale: Image = (0..1024).map(|b| (b, Box::new([0xAB; BLOCK]))).collect();
        let mut v = DataVolume::format(Stops::from(&stale, 1024, usize::MAX, None), 0, &["home", "apps"], clock).unwrap();

        let n = v.open("home/f", CREATE).unwrap();
        v.write(n, 0, &[1; BLOCK]).unwrap();
        v.truncate(n, 3 * BLOCK as u64).unwrap();
        assert_eq!(v.sync(), Ok(Vec::new()));
        v.write(n, 6000, &[2]).unwrap();

        let mut out = vec![0u8; BLOCK];
        assert_eq!(v.read(n, BLOCK as u64, &mut Buf(&mut out)).unwrap(), BLOCK);
        let mut want = vec![0u8; BLOCK];
        want[6000 - BLOCK] = 2;
        assert!(out == want, "page 1 reads {:#04x} where nothing was written", out[out.iter().zip(&want).position(|(a, b)| a != b).unwrap()]);
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
        for Listed { name, meta, .. } in listed {
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
        let mut v = DataVolume::format(Stops::new(1024), CLEAN_LIMIT, &["home", "apps"], clock).unwrap();
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
        match DataVolume::probe(disk, CLEAN_LIMIT, &["home", "apps"], clock) {
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
        let writes = take(v).writes.get();
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
        let writes = take(v).writes.get();
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

        assert_eq!(v.rename("home/staged", &to), Err(Error::LimitExceeded));

        let names = |v: &mut DataVolume<Ram>, dir: &str| -> Vec<String> {
            v.list(dir).map(|l| l.into_iter().map(|e| e.name).collect()).unwrap_or_default()
        };
        assert_eq!(names(&mut v, "home/staged"), ["a", "z"]);
        assert_eq!(v.lstat(&to), Err(Error::NotFound));
        assert_eq!(v.sync(), Ok(Vec::new()));
        let mut v = remount(v);
        assert_eq!(names(&mut v, "home/staged"), ["a", "z"]);
        assert_eq!(v.lstat(&to), Err(Error::NotFound));
    }

    /// What a write takes is what the room answers less, block for block,
    /// and the room never names the blocks the reserve keeps for a delete.
    #[test]
    fn a_write_takes_its_blocks_out_of_the_room() {
        let mut v = vol();
        let n = v.open("apps/x", CREATE).unwrap();
        let before = v.room().unwrap();
        assert_eq!(before.unit, BLOCK as u64);
        v.write(n, 0, &vec![7u8; 3 * BLOCK + 1]).unwrap();
        assert_eq!(v.room().unwrap().free, before.free - 4 * BLOCK as u64);
        v.close(n).unwrap();
        let free = free_blocks(&mut v);
        assert!(v.room().unwrap().free < free * BLOCK as u64, "the reserve's blocks were counted as room");
    }
}
