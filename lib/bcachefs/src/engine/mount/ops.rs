//! The namespace: reads through the cache, and the operations that change it
//! (§2.3). A file that loses its last name, or is made with none, is an
//! orphan until released: unlinked, with its `deleted_inodes` key, in the
//! same transaction (A8.10–A8.12). A directory is never an orphan: the
//! transaction that removes its last name deletes it.

use super::*;

/// Hash type 3, siphash, in an inode's flags.
const SIPHASH: u64 = 3 << 20;
/// Hash whiteouts of a removed directory each transaction deletes.
const WHITEOUTS: usize = 8;

// Each operation's grant: a leaf for each position it changes, as its
// transaction lists them. The accounting positions are a snapshot class
// for each of inodes and dirents (A7.14) and the inode count (A7.10).
/// The new inode and its directory's, its dirent, the inode cursor, and
/// three accounting keys.
const CREATE: usize = 2 + 1 + 1 + 3;
/// The new inode, its `deleted_inodes` key, the cursor and two accounting
/// keys.
const CREATE_UNNAMED: usize = 1 + 1 + 1 + 2;
/// The node, the directory and a replaced node's inodes, the dirent, the
/// node's and the replaced node's `deleted_inodes` keys, and two accounting
/// keys.
const LINK: usize = 3 + 1 + 2 + 2;
/// The directory's inode and the removed one's, the dirent or its whiteout,
/// a `deleted_inodes` key and three accounting keys.
const UNLINK: usize = 2 + 1 + 1 + 3;
/// Both nodes' inodes and both directories', both dirents, the replaced
/// node's `deleted_inodes` key and three accounting keys.
const RENAME: usize = 4 + 2 + 1 + 3;
/// The inode, its `deleted_inodes` key and two accounting keys.
const RELEASE: usize = 1 + 1 + 2;
/// The whiteouts and one accounting key.
const DROP_WHITEOUTS: usize = WHITEOUTS + 1;
const _: () = assert!(RENAME == FOOTPRINT && CREATE <= FOOTPRINT && LINK <= FOOTPRINT && UNLINK <= FOOTPRINT && DROP_WHITEOUTS <= FOOTPRINT);

/// What a new name links.
pub enum Link {
    /// A name not yet in the directory.
    New,
    /// The name's node, which must still be `expected`: a safe replacement,
    /// `Stale` once someone else has replaced or removed it.
    Replace { expected: u64 },
}

/// How a rename treats a name already at its destination.
pub enum Rename {
    /// Takes it: a file there loses that name, an empty directory there is
    /// deleted.
    Replace,
    /// Refuses with `Exists`.
    NoReplace,
    /// Swaps the two.
    Exchange,
}

/// A node an operation took a name from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Removed {
    pub node: u64,
    /// It has no name left: a file is now an orphan until released; a
    /// directory is gone.
    pub last_link: bool,
}

/// A name a dirent may hold (A5.22).
fn valid_name(name: &[u8]) -> Result<()> {
    if name.is_empty() || name.len() > MAX_NAME || name.contains(&0) || name.contains(&b'/') || name == b"." || name == b".." {
        return Err(Error::Invalid("name empty, past 512 bytes, holding NUL or '/', or a dot entry"));
    }
    Ok(())
}

fn is_dir(v: &InodeValue) -> bool {
    v.mode() & mode::S_IFMT == mode::S_IFDIR
}

/// A link count one more: a file's new name, or a directory's new
/// subdirectory. Refused where the count read back, one more than stored
/// for a file and two for a directory, would pass 32 bits.
fn one_link_more(inode: &mut InodeValue) -> Result<()> {
    let bias = if is_dir(inode) { 2 } else { 1 };
    let nlink = inode.field(inode_field::NLINK) + 1;
    if nlink + bias > u64::from(u32::MAX) {
        return Err(Error::NoSpace("a link count at its largest"));
    }
    inode.set(inode_field::NLINK, nlink);
    Ok(())
}

/// The depth of a directory made in `parent` (A5.17), refused past the
/// field's 32 bits.
fn deeper(parent: &InodeValue) -> Result<u64> {
    let depth = parent.field(inode_field::DEPTH) + 1;
    if depth > u64::from(u32::MAX) {
        return Err(Error::NoSpace("a directory depth at its largest"));
    }
    Ok(depth)
}

/// A directory's link count less the subdirectory it loses.
fn one_link_less(dir: &mut InodeValue) -> Result<()> {
    let nlink = dir.field(inode_field::NLINK).checked_sub(1).ok_or(Error::Corrupt("a directory counting fewer subdirectories than it holds"))?;
    dir.set(inode_field::NLINK, nlink);
    Ok(())
}

fn inode_pos(ino: u64) -> Bpos {
    Bpos::new(0, ino, ROOT_SNAPSHOT)
}

/// A dirent found by name: its slot, and what it names.
struct Named {
    slot: u64,
    ino: u64,
    kind: DirentType,
}

/// The namespace, read and changed.
impl<D: BlockDev> Mount<D> {
    /// The root directory's inode number.
    pub fn root(&self) -> u64 {
        self.root
    }

    pub async fn inode(&self, ino: u64) -> Result<Inode> {
        Source::inode(self, ino).await
    }

    pub async fn lookup(&self, dir: u64, name: &[u8]) -> Result<Option<read::DirEntry>> {
        Source::lookup(self, dir, name).await
    }

    pub async fn readdir(&self, dir: u64, cursor: u64, max: usize) -> Result<(Vec<read::DirEntry>, bool)> {
        Source::readdir(self, dir, cursor, max).await
    }

    /// A new file or directory `name` in directory `dir`, owned by `owner`,
    /// its times `now`, in nanoseconds since the Unix epoch. Its number is
    /// the inode cursor's next free one (A8.7), its name in the first free
    /// slot from its hash (A5.25). A symlink waits for the data path.
    pub async fn create(&self, dir: u64, name: &[u8], new: New, owner: Owner, now: i64) -> Result<Inode> {
        valid_name(name)?;
        let (type_bits, dirent_type) = match new {
            New::File => (mode::S_IFREG, DirentType::Regular),
            New::Dir => (mode::S_IFDIR, DirentType::Directory),
            New::Symlink(_) => return Err(Error::Unsupported("a symlink, whose target is file data")),
        };
        let seed = self.nonzero()?;
        self.observe(now);
        let ino = self
            .transact(K_TX / 2, CREATE, Draw::Ordinary, async |txn: &mut Txn| {
                let mut parent = self.directory(txn, dir).await?;
                let slot = self.free_slot(txn, dir, &parent, name).await?;
                let ino = self.next_inode(txn).await?;
                let mut inode = self.new_inode(type_bits, owner, seed, now)?;
                inode.set(inode_field::DIR, dir);
                inode.set(inode_field::DIR_OFFSET, slot);
                if type_bits == mode::S_IFDIR {
                    inode.set(inode_field::DEPTH, deeper(&parent)?);
                    one_link_more(&mut parent)?;
                }
                self.set_inode(txn, ino, &inode).await?;
                self.set_dirent(txn, dir, slot, Some((ino, dirent_type, name))).await?;
                self.touch(&mut parent, now);
                self.set_inode(txn, dir, &parent).await?;
                Ok(ino)
            })
            .await?;
        Source::inode(self, ino).await
    }

    /// A new file with no name: an orphan from birth, until a link names it
    /// or a release frees it (A8.11). `near` is the directory it is meant
    /// for, which it does not change.
    pub async fn create_unnamed(&self, near: u64, owner: Owner, now: i64) -> Result<Inode> {
        let seed = self.nonzero()?;
        self.observe(now);
        let ino = self
            .transact(K_TX / 2, CREATE_UNNAMED, Draw::Ordinary, async |txn: &mut Txn| {
                self.directory(txn, near).await?;
                let ino = self.next_inode(txn).await?;
                let mut inode = self.new_inode(mode::S_IFREG, owner, seed, now)?;
                inode.flags |= inode_flag::UNLINKED;
                self.set_inode(txn, ino, &inode).await?;
                self.set_orphan(txn, ino, true).await?;
                Ok(ino)
            })
            .await?;
        Source::inode(self, ino).await
    }

    /// Gives file `node` the name `name` in `dir`: a new name, or, as
    /// `Replace`, the one `expected` holds, which loses it. An orphan named
    /// stops being one (A5.8). A directory takes no second name.
    pub async fn link(&self, node: u64, dir: u64, name: &[u8], how: Link, now: i64) -> Result<Option<Removed>> {
        valid_name(name)?;
        self.observe(now);
        self.transact(K_TX / 2, LINK, Draw::Ordinary, async |txn: &mut Txn| {
            let mut parent = self.directory(txn, dir).await?;
            let mut inode = self.inode_in(txn, node).await?;
            if is_dir(&inode) {
                return Err(Error::IsDirectory);
            }
            let kind = DirentType::of_mode(inode.mode()).ok_or(Error::Corrupt("an inode of no file type"))?;
            let (slot, removed) = match how {
                Link::New => (self.free_slot(txn, dir, &parent, name).await?, None),
                Link::Replace { expected } => {
                    let named = self.named(txn, dir, &parent, name).await?.ok_or(Error::Stale)?;
                    if named.ino != expected {
                        return Err(Error::Stale);
                    }
                    if named.kind == DirentType::Directory {
                        return Err(Error::IsDirectory);
                    }
                    if expected == node {
                        return Ok(None);
                    }
                    (named.slot, Some(self.unname(txn, expected, dir, named.slot, now).await?))
                }
            };
            self.name_inode(txn, node, &mut inode, dir, slot, now).await?;
            self.set_dirent(txn, dir, slot, Some((node, kind, name))).await?;
            self.touch(&mut parent, now);
            self.set_inode(txn, dir, &parent).await?;
            Ok(removed)
        })
        .await
    }

    /// Removes the name `name` from `dir`: a file's, or with `dir_too` an
    /// empty directory's, which is deleted in this transaction with as many
    /// of its hash whiteouts as it holds; later transactions delete the rest.
    pub async fn unlink(&self, dir: u64, name: &[u8], dir_too: bool, now: i64) -> Result<Removed> {
        valid_name(name)?;
        self.observe(now);
        let removed = self
            .transact(K_TX / 2, UNLINK, Draw::Freeing, async |txn: &mut Txn| {
                let mut parent = self.directory(txn, dir).await?;
                let named = self.named(txn, dir, &parent, name).await?.ok_or(Error::NotFound)?;
                let removed = match (named.kind == DirentType::Directory, dir_too) {
                    (true, false) => return Err(Error::IsDirectory),
                    (false, true) => return Err(Error::NotDirectory),
                    (true, true) => {
                        self.delete_dir(txn, named.ino).await?;
                        one_link_less(&mut parent)?;
                        Removed { node: named.ino, last_link: true }
                    }
                    (false, false) => self.unname(txn, named.ino, dir, named.slot, now).await?,
                };
                self.remove_dirent(txn, dir, &parent, named.slot).await?;
                self.touch(&mut parent, now);
                self.set_inode(txn, dir, &parent).await?;
                Ok(removed)
            })
            .await?;
        if dir_too {
            self.drop_whiteouts(removed.node).await?;
        }
        Ok(removed)
    }

    /// Moves the name `from` to `to`, as `how` treats a name already at
    /// `to`. A directory moves with its subtree, never beneath itself.
    pub async fn rename(&self, from: (u64, &[u8]), to: (u64, &[u8]), how: Rename, now: i64) -> Result<Option<Removed>> {
        valid_name(from.1)?;
        valid_name(to.1)?;
        self.observe(now);
        let removed = self
            .transact(K_TX, RENAME, Draw::Ordinary, async |txn: &mut Txn| {
                let mut src_parent = self.directory(txn, from.0).await?;
                let src = self.named(txn, from.0, &src_parent, from.1).await?.ok_or(Error::NotFound)?;
                let mut dst_parent = if to.0 == from.0 { src_parent.clone() } else { self.directory(txn, to.0).await? };
                let dst = self.named(txn, to.0, &dst_parent, to.1).await?;
                if dst.as_ref().is_some_and(|d| d.ino == src.ino) {
                    return Ok(None);
                }
                let src_dir = src.kind == DirentType::Directory;
                if src_dir && to.0 != from.0 {
                    self.not_beneath(txn, src.ino, to.0).await?;
                }
                let mut removed = None;
                match (how, &dst) {
                    (Rename::NoReplace, Some(_)) => return Err(Error::Exists),
                    (Rename::Exchange, None) => return Err(Error::NotFound),
                    (Rename::Exchange, Some(d)) => {
                        let dst_dir = d.kind == DirentType::Directory;
                        if dst_dir && to.0 != from.0 {
                            self.not_beneath(txn, d.ino, from.0).await?;
                        }
                        self.repoint(txn, d.ino, from.0, src.slot, &src_parent).await?;
                        self.repoint(txn, src.ino, to.0, d.slot, &dst_parent).await?;
                        self.set_dirent(txn, from.0, src.slot, Some((d.ino, d.kind, from.1))).await?;
                        self.set_dirent(txn, to.0, d.slot, Some((src.ino, src.kind, to.1))).await?;
                        if src_dir != dst_dir && to.0 != from.0 {
                            let (gains, loses) = if src_dir { (&mut dst_parent, &mut src_parent) } else { (&mut src_parent, &mut dst_parent) };
                            one_link_more(gains)?;
                            one_link_less(loses)?;
                        }
                    }
                    (Rename::Replace | Rename::NoReplace, dst) => {
                        let slot = match dst {
                            Some(d) => {
                                match (src_dir, d.kind == DirentType::Directory) {
                                    (false, true) => return Err(Error::IsDirectory),
                                    (true, false) => return Err(Error::NotDirectory),
                                    (true, true) => {
                                        self.delete_dir(txn, d.ino).await?;
                                        one_link_less(&mut dst_parent)?;
                                        removed = Some(Removed { node: d.ino, last_link: true });
                                    }
                                    (false, false) => removed = Some(self.unname(txn, d.ino, to.0, d.slot, now).await?),
                                }
                                d.slot
                            }
                            None => self.free_slot(txn, to.0, &dst_parent, to.1).await?,
                        };
                        // The new name in first: the source's whiteout, if
                        // its run needs one, sees it.
                        self.repoint(txn, src.ino, to.0, slot, &dst_parent).await?;
                        self.set_dirent(txn, to.0, slot, Some((src.ino, src.kind, to.1))).await?;
                        self.remove_dirent(txn, from.0, &src_parent, src.slot).await?;
                        if src_dir && to.0 != from.0 {
                            one_link_less(&mut src_parent)?;
                            one_link_more(&mut dst_parent)?;
                        }
                    }
                }
                self.touch(&mut src_parent, now);
                if to.0 == from.0 {
                    // One directory: what happened to the destination's copy
                    // happened to the source's.
                    src_parent.set(inode_field::NLINK, dst_parent.field(inode_field::NLINK));
                    self.set_inode(txn, from.0, &src_parent).await?;
                } else {
                    self.touch(&mut dst_parent, now);
                    self.set_inode(txn, from.0, &src_parent).await?;
                    self.set_inode(txn, to.0, &dst_parent).await?;
                }
                Ok(removed)
            })
            .await?;
        if let Some(Removed { node, .. }) = removed.filter(|r| r.last_link) {
            self.drop_whiteouts(node).await?;
        }
        Ok(removed)
    }

    /// Frees orphan `node` once nothing holds it: its inode and its
    /// `deleted_inodes` key go (A8.13). A node with a name is left as it is;
    /// a key that names no orphan goes alone.
    pub async fn release(&self, node: u64) -> Result<()> {
        self.transact(K_TX / 2, RELEASE, Draw::Freeing, async |txn: &mut Txn| {
            let orphan = self.inode_opt(txn, node).await?.filter(|v| v.flags & inode_flag::UNLINKED != 0 && !is_dir(v));
            if orphan.is_some() {
                self.set(txn, Btree::Inodes, inode_pos(node), None).await?;
            }
            if self.get_in(txn, Btree::DeletedInodes, inode_pos(node)).await?.is_some() {
                self.set_orphan(txn, node, false).await?;
            }
            Ok(())
        })
        .await
    }

    /// Releases every orphan, each in its own transaction: what a server
    /// does after mounting, when nothing yet holds one, beside its serving
    /// (§2.3); and what a clean unmount does first (A8.14). How many it
    /// released.
    pub async fn release_orphans(&self) -> Result<usize> {
        let mut released = 0;
        loop {
            let mut next = None;
            Source::walk(self, Btree::DeletedInodes, Bpos::MIN, |k, _| {
                next = (k.kind == kind::SET).then_some(k.pos.offset);
                Ok(next.is_none())
            })
            .await?;
            let Some(ino) = next else { return Ok(released) };
            self.release(ino).await?;
            released += 1;
        }
    }

    /// The directory `dir`, live.
    async fn directory(&self, txn: &Txn, dir: u64) -> Result<InodeValue> {
        let v = self.inode_in(txn, dir).await?;
        if !is_dir(&v) || v.flags & inode_flag::UNLINKED != 0 {
            return Err(Error::NotDirectory);
        }
        Ok(v)
    }

    async fn inode_in(&self, txn: &Txn, ino: u64) -> Result<InodeValue> {
        self.inode_opt(txn, ino).await?.ok_or(Error::NotFound)
    }

    async fn inode_opt(&self, txn: &Txn, ino: u64) -> Result<Option<InodeValue>> {
        match self.get_in(txn, Btree::Inodes, inode_pos(ino)).await? {
            Some((k, v)) if k.kind == kind::INODE_V3 => Ok(Some(InodeValue::decode(&v)?)),
            Some((k, _)) if matches!(k.kind, kind::INODE | kind::INODE_V2) => Err(Error::Unsupported("inode in a format before inode_v3")),
            _ => Ok(None),
        }
    }

    /// A new inode's value: no name yet, its times `now`.
    fn new_inode(&self, type_bits: u16, owner: Owner, seed: u64, now: i64) -> Result<InodeValue> {
        if owner.perm > 0o7777 {
            return Err(Error::Invalid("permission bits past 0o7777"));
        }
        let mut inode = InodeValue { hash_seed: seed, flags: u64::from(type_bits | owner.perm) << 36 | SIPHASH, ..Default::default() };
        let time = self.stored_time(now);
        for f in [inode_field::ATIME, inode_field::CTIME, inode_field::MTIME, inode_field::OTIME] {
            inode.set(f, time);
        }
        inode.set(inode_field::UID, u64::from(owner.uid));
        inode.set(inode_field::GID, u64::from(owner.gid));
        Ok(inode)
    }

    /// A directory's change and modification times set to `now`.
    fn touch(&self, dir: &mut InodeValue, now: i64) {
        let time = self.stored_time(now);
        dir.set(inode_field::MTIME, time);
        dir.set(inode_field::CTIME, time);
    }

    /// Names `inode`, the file `node`, at `slot` of `dir`: an orphan stops
    /// being one, another name adds a link, and the newest name is its
    /// back-reference (A5.19).
    async fn name_inode(&self, txn: &mut Txn, node: u64, inode: &mut InodeValue, dir: u64, slot: u64, now: i64) -> Result<()> {
        if inode.flags & inode_flag::UNLINKED != 0 {
            inode.flags &= !inode_flag::UNLINKED;
            self.set_orphan(txn, node, false).await?;
        } else {
            one_link_more(inode)?;
        }
        inode.set(inode_field::DIR, dir);
        inode.set(inode_field::DIR_OFFSET, slot);
        inode.set(inode_field::CTIME, self.stored_time(now));
        self.set_inode(txn, node, inode).await
    }

    /// File `node` loses its name at `slot` of `dir`: one link fewer, or at
    /// its last an orphan (A5.8, A8.12). A back-reference to that name is
    /// cleared: a file's other names need none (A5.20).
    async fn unname(&self, txn: &mut Txn, node: u64, dir: u64, slot: u64, now: i64) -> Result<Removed> {
        let mut inode = self.inode_in(txn, node).await?;
        let last_link = inode.field(inode_field::NLINK) == 0;
        if last_link {
            inode.flags |= inode_flag::UNLINKED;
            self.set_orphan(txn, node, true).await?;
        } else {
            inode.set(inode_field::NLINK, inode.field(inode_field::NLINK) - 1);
        }
        if (inode.field(inode_field::DIR), inode.field(inode_field::DIR_OFFSET)) == (dir, slot) {
            inode.set(inode_field::DIR, 0);
            inode.set(inode_field::DIR_OFFSET, 0);
        }
        inode.set(inode_field::CTIME, self.stored_time(now));
        self.set_inode(txn, node, &inode).await?;
        Ok(Removed { node, last_link })
    }

    /// Node `node` named at `slot` of `dir` instead: its back-reference, and
    /// a directory's depth, one past its new parent's (A5.17).
    async fn repoint(&self, txn: &mut Txn, node: u64, dir: u64, slot: u64, parent: &InodeValue) -> Result<()> {
        let mut inode = self.inode_in(txn, node).await?;
        inode.set(inode_field::DIR, dir);
        inode.set(inode_field::DIR_OFFSET, slot);
        if is_dir(&inode) {
            inode.set(inode_field::DEPTH, deeper(parent)?);
        }
        self.set_inode(txn, node, &inode).await
    }

    /// Deletes the directory `node`, which must hold no name; its hash
    /// whiteouts need no inode (A9.37) and go after it.
    async fn delete_dir(&self, txn: &mut Txn, node: u64) -> Result<()> {
        let mut named = false;
        Source::walk(self, Btree::Dirents, Bpos::new(node, 0, 0), |k, _| {
            if k.pos.inode != node {
                return Ok(false);
            }
            named |= k.kind == kind::DIRENT;
            Ok(!named)
        })
        .await?;
        let pending = txn.updates.range((Btree::Dirents, 0, Bpos::new(node, 0, 0))..=(Btree::Dirents, 0, Bpos::new(node, u64::MAX, u32::MAX)));
        if named || pending.into_iter().any(|(_, kv)| kv.as_ref().is_some_and(|(k, _)| k.kind == kind::DIRENT)) {
            return Err(Error::NotEmpty);
        }
        self.set(txn, Btree::Inodes, inode_pos(node), None).await
    }

    /// Deletes what is left of removed directory `node`'s hash whiteouts,
    /// [`WHITEOUTS`] to a transaction.
    async fn drop_whiteouts(&self, node: u64) -> Result<()> {
        loop {
            let mut left = Vec::new();
            Source::walk(self, Btree::Dirents, Bpos::new(node, 0, 0), |k, _| {
                if k.pos.inode != node || left.len() == WHITEOUTS {
                    return Ok(false);
                }
                left.push(k.pos);
                Ok(true)
            })
            .await?;
            if left.is_empty() {
                return Ok(());
            }
            self.transact(K_TX / 2, DROP_WHITEOUTS, Draw::Freeing, async |txn: &mut Txn| {
                for &pos in &left {
                    self.set(txn, Btree::Dirents, pos, None).await?;
                }
                Ok(())
            })
            .await?;
        }
    }

    /// Fails `Invalid` when directory `node` is `dir` or one of its
    /// ancestors: a directory never moves beneath itself.
    async fn not_beneath(&self, txn: &Txn, node: u64, dir: u64) -> Result<()> {
        let mut at = dir;
        let mut seen = BTreeSet::new();
        while at != self.root {
            if at == node {
                return Err(Error::Invalid("a directory moved beneath itself"));
            }
            if !seen.insert(at) {
                return Err(Error::Corrupt("directory back-references that loop"));
            }
            at = self.inode_in(txn, at).await?.field(inode_field::DIR);
            if at == 0 {
                return Err(Error::Corrupt("a directory whose back-references reach no root"));
            }
        }
        Ok(())
    }

    /// The dirent `name` in `dir`, by its hash chain (A5.25).
    async fn named(&self, txn: &Txn, dir: u64, parent: &InodeValue, name: &[u8]) -> Result<Option<Named>> {
        let mut at = read::hash(parent, name)?;
        loop {
            match self.get_in(txn, Btree::Dirents, Bpos::new(dir, at, ROOT_SNAPSHOT)).await? {
                None => return Ok(None),
                Some((k, v)) if k.kind == kind::DIRENT => {
                    let d = value::Dirent::decode(&v)?;
                    if d.name == name {
                        return Ok(Some(Named { slot: at, ino: d.target, kind: d.kind }));
                    }
                }
                Some((k, _)) if k.kind == kind::HASH_WHITEOUT => {}
                Some(_) => return Err(Error::Corrupt("dirents key of another type in a hash chain")),
            }
            at = at.checked_add(1).ok_or(Error::Corrupt("a hash chain past the last offset"))?;
        }
    }

    /// The first slot from `name`'s hash that holds no dirent: a hash
    /// whiteout's, or the first empty one (A5.25). `Exists` when the name is
    /// already there.
    async fn free_slot(&self, txn: &Txn, dir: u64, parent: &InodeValue, name: &[u8]) -> Result<u64> {
        let mut at = read::hash(parent, name)?;
        let mut whiteout = None;
        loop {
            match self.get_in(txn, Btree::Dirents, Bpos::new(dir, at, ROOT_SNAPSHOT)).await? {
                None => return Ok(whiteout.unwrap_or(at)),
                Some((k, _)) if k.kind == kind::HASH_WHITEOUT => {
                    whiteout.get_or_insert(at);
                }
                Some((k, v)) if k.kind == kind::DIRENT => {
                    if value::Dirent::decode(&v)?.name == name {
                        return Err(Error::Exists);
                    }
                }
                Some(_) => return Err(Error::Corrupt("dirents key of another type in a hash chain")),
            }
            at = at.checked_add(1).ok_or(Error::NoSpace("a directory's hash chain at its last offset"))?;
        }
    }

    /// Removes the dirent at `slot` of `dir`: a hash whiteout stays where a
    /// later entry of the run after it hashes at or before it, so its chain
    /// still reaches it; otherwise the slot empties (A5.25).
    async fn remove_dirent(&self, txn: &mut Txn, dir: u64, parent: &InodeValue, slot: u64) -> Result<()> {
        let mut at = slot;
        let mut needed = false;
        while let Some(next) = at.checked_add(1) {
            at = next;
            match self.get_in(txn, Btree::Dirents, Bpos::new(dir, at, ROOT_SNAPSHOT)).await? {
                None => break,
                Some((k, v)) if k.kind == kind::DIRENT => {
                    if read::hash(parent, value::Dirent::decode(&v)?.name)? <= slot {
                        needed = true;
                        break;
                    }
                }
                Some(_) => {}
            }
        }
        let pos = Bpos::new(dir, slot, ROOT_SNAPSHOT);
        let whiteout = needed.then(|| (Key::new(kind::HASH_WHITEOUT, pos), Vec::new()));
        self.set(txn, Btree::Dirents, pos, whiteout).await
    }

    /// Sets the dirent at `slot` of `dir` to name `node`, or empties it.
    async fn set_dirent(&self, txn: &mut Txn, dir: u64, slot: u64, entry: Option<(u64, DirentType, &[u8])>) -> Result<()> {
        let pos = Bpos::new(dir, slot, ROOT_SNAPSHOT);
        let kv = entry.map(|(node, kind_, name)| (Key::new(kind::DIRENT, pos), value::dirent(node, kind_, name)));
        self.set(txn, Btree::Dirents, pos, kv).await
    }

    /// Sets inode `ino` to `v`, its journal-sequence field the
    /// transaction's (A5.14).
    async fn set_inode(&self, txn: &mut Txn, ino: u64, v: &InodeValue) -> Result<()> {
        let pos = inode_pos(ino);
        self.set(txn, Btree::Inodes, pos, Some((Key::new(kind::INODE_V3, pos), v.encode()))).await?;
        if !txn.stamped.contains(&pos) {
            txn.stamped.push(pos);
        }
        Ok(())
    }

    /// Sets or removes inode `ino`'s `deleted_inodes` key.
    async fn set_orphan(&self, txn: &mut Txn, ino: u64, orphan: bool) -> Result<()> {
        let pos = inode_pos(ino);
        self.set(txn, Btree::DeletedInodes, pos, orphan.then(|| (Key::new(kind::SET, pos), Vec::new()))).await
    }

    /// Sets `pos` of `btree`'s leaves to `kv`, or deletes it, with the
    /// accounting the change makes: a key of a snapshot btree counts, and
    /// its bytes, in its snapshot's class (A7.14); an inode counts once
    /// more (A7.10).
    async fn set(&self, txn: &mut Txn, btree: Btree, pos: Bpos, kv: Option<(Key, Vec<u8>)>) -> Result<()> {
        let old = self.get_in(txn, btree, pos).await?;
        let counted = |kv: &Option<(Key, Vec<u8>)>| match kv {
            Some((k, v)) if matches!(btree, Btree::Extents | Btree::Inodes | Btree::Dirents | Btree::Xattrs | Btree::Damage) && !matches!(k.kind, kind::DELETED | kind::WHITEOUT | kind::EXTENT_WHITEOUT) => {
                Some(key_bytes(v) as u64)
            }
            _ => None,
        };
        let (before, after) = (counted(&old), counted(&kv));
        if before != after {
            let mut record = alloc::vec![class::SNAPSHOT];
            record.extend_from_slice(&ROOT_SNAPSHOT.to_le_bytes());
            record.extend_from_slice(&(btree as u32).to_le_bytes());
            let keys = u64::from(after.is_some()).wrapping_sub(u64::from(before.is_some()));
            let bytes = after.unwrap_or(0).wrapping_sub(before.unwrap_or(0));
            self.account(txn, &record, &[keys, bytes, 0]).await?;
            if btree == Btree::Inodes && before.is_some() != after.is_some() {
                self.account(txn, &[class::NR_INODES], &[keys]).await?;
            }
        }
        self.put(txn, btree, 0, pos, kv).await
    }

    /// The cursor's next inode number not yet taken, the cursor moved past
    /// it. Numbers are never reused, so generation 0 always (A8.9).
    async fn next_inode(&self, txn: &mut Txn) -> Result<u64> {
        let pos = Bpos::new(1, 1, 0);
        let mut ino = match self.get_in(txn, Btree::LoggedOps, pos).await? {
            Some((k, v)) if k.kind == kind::INODE_ALLOC_CURSOR && v.len() >= 16 => super::super::le64(&v, 8).max(mkfs::FIRST_INODE),
            _ => mkfs::FIRST_INODE,
        };
        while self.get_in(txn, Btree::Inodes, inode_pos(ino)).await?.is_some() {
            ino = ino.checked_add(1).ok_or(Error::NoSpace("inode numbers"))?;
        }
        let cursor = value::inode_cursor(0, ino.checked_add(1).ok_or(Error::NoSpace("inode numbers"))?).to_vec();
        self.put(txn, Btree::LoggedOps, 0, pos, Some((Key::new(kind::INODE_ALLOC_CURSOR, pos), cursor))).await?;
        Ok(ino)
    }

    /// `now` in the volume's units since its time base (A5.11).
    fn stored_time(&self, now: i64) -> u64 {
        let precision = i128::from(self.sb.header.time_precision);
        (i128::from(now) / precision - i128::from(self.sb.header.time_base) / precision) as i64 as u64
    }

    /// Notes an operation at `now`, nanoseconds since the epoch: entries
    /// carry the newest as their datetime.
    fn observe(&self, now: i64) {
        let mut st = self.st.borrow_mut();
        st.now = st.now.max((now / 1_000_000_000).max(0) as u64);
    }
}
