//! A tree of directories, files, symlinks and hard links for [`mkfs`] to
//! write. Everything it holds was checked as it was added, so mkfs writes
//! any tree it is given.
//!
//! [`mkfs`]: super::mkfs

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use super::bkey::{Bpos, Key};
use super::mkfs::{BLOCK_SECTORS, FIRST_INODE, ROOT_INODE, ROOT_SNAPSHOT, ROOT_SUBVOLUME};
use super::read::{hash, MAX_SYMLINK};
use super::value::mode::{S_IFDIR, S_IFLNK, S_IFREG};
use super::value::{self, csum_type, inode_field, DirentType, Extent, InodeValue, Ptr, MAX_NAME};
use super::{kind, Btree, Error, Result, SECTOR};
use crate::crc32c::crc32c_zero;

/// A node of a [`Tree`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeRef(usize);

/// A node's owner and permission bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Mode bits below the file type, at most 0o7777.
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
}

enum Content<'a> {
    /// Entries by name.
    Dir(BTreeMap<Vec<u8>, NodeRef>),
    File(&'a [u8]),
    /// The target and the NUL stored after it.
    Symlink(Vec<u8>),
}

struct Node<'a> {
    content: Content<'a>,
    meta: Meta,
    /// The directory holding its first name; a directory has no other.
    parent: usize,
    names: u32,
}

/// Directories, files, symlinks and hard links under the volume's root
/// directory, which is mode 040755 and owned by root.
pub struct Tree<'a> {
    nodes: Vec<Node<'a>>,
}

impl Default for Tree<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Tree<'a> {
    pub const ROOT: NodeRef = NodeRef(0);

    /// The root directory alone.
    pub fn new() -> Self {
        let root = Node { content: Content::Dir(BTreeMap::new()), meta: Meta { perm: 0o755, uid: 0, gid: 0 }, parent: 0, names: 1 };
        Tree { nodes: alloc::vec![root] }
    }

    pub fn dir(&mut self, parent: NodeRef, name: &[u8], meta: Meta) -> Result<NodeRef> {
        self.add(parent, name, Content::Dir(BTreeMap::new()), meta)
    }

    /// A regular file holding `data`.
    pub fn file(&mut self, parent: NodeRef, name: &[u8], data: &'a [u8], meta: Meta) -> Result<NodeRef> {
        self.add(parent, name, Content::File(data), meta)
    }

    pub fn symlink(&mut self, parent: NodeRef, name: &[u8], target: &[u8], meta: Meta) -> Result<NodeRef> {
        if target.is_empty() || target.len() as u64 >= MAX_SYMLINK || target.contains(&0) {
            return Err(Error::Invalid("symlink target empty, past 4095 bytes or holding NUL"));
        }
        self.add(parent, name, Content::Symlink([target, &[0]].concat()), meta)
    }

    /// Another name for the file or symlink `node`.
    pub fn link(&mut self, parent: NodeRef, name: &[u8], node: NodeRef) -> Result<()> {
        match self.nodes.get(node.0).map(|n| &n.content) {
            None => return Err(Error::Invalid("no node of this tree")),
            Some(Content::Dir(_)) => return Err(Error::Invalid("a hard link to a directory")),
            Some(_) => {}
        }
        if self.nodes[node.0].names == u32::MAX {
            return Err(Error::Invalid("a node with 2^32 names"));
        }
        self.name(parent, name, node)?;
        self.nodes[node.0].names += 1;
        Ok(())
    }

    fn add(&mut self, parent: NodeRef, name: &[u8], content: Content<'a>, meta: Meta) -> Result<NodeRef> {
        if meta.perm > 0o7777 {
            return Err(Error::Invalid("permission bits past 0o7777"));
        }
        let node = NodeRef(self.nodes.len());
        self.name(parent, name, node)?;
        self.nodes.push(Node { content, meta, parent: parent.0, names: 1 });
        Ok(node)
    }

    /// Enters `name` for `node` in the directory `parent`.
    fn name(&mut self, parent: NodeRef, name: &[u8], node: NodeRef) -> Result<()> {
        if name.is_empty() || name.len() > MAX_NAME || name.contains(&0) || name.contains(&b'/') || name == b"." || name == b".." {
            return Err(Error::Invalid("name empty, past 512 bytes, holding NUL or '/', or a dot entry"));
        }
        let Some(Node { content: Content::Dir(entries), .. }) = self.nodes.get_mut(parent.0) else {
            return Err(Error::Invalid("a parent that is no directory of this tree"));
        };
        if entries.contains_key(name) {
            return Err(Error::Invalid("a name already in the directory"));
        }
        entries.insert(name.to_vec(), node);
        Ok(())
    }

    /// Nodes besides the root, each of which takes an inode number and a
    /// hash seed.
    pub(crate) fn len(&self) -> usize {
        self.nodes.len() - 1
    }

    /// Whether any node holds data, which goes in extents.
    pub(crate) fn has_data(&self) -> bool {
        self.nodes.iter().any(|n| match &n.content {
            Content::File(data) => !data.is_empty(),
            Content::Symlink(_) => true,
            Content::Dir(_) => false,
        })
    }

    /// The bytes node `i` stores as data, if any.
    fn data(&self, i: usize) -> Option<&[u8]> {
        match &self.nodes[i].content {
            Content::File(data) if !data.is_empty() => Some(data),
            Content::Symlink(target) => Some(target),
            _ => None,
        }
    }

    /// The inode, dirent, extent and cursor keys of this tree, and where its
    /// data goes: from `first` on, extents of at most `max_extent` sectors,
    /// each inside one bucket of `bucket` sectors. `root_seed` is the root
    /// directory's hash seed; `seeds` every other node's.
    pub(crate) fn populate(&self, first: u64, bucket: u64, max_extent: u64, root_seed: u64, seeds: &[u64]) -> Populated<'_, 'a> {
        assert_eq!(seeds.len(), self.len(), "a hash seed per node");
        let ino = |i: usize| if i == 0 { ROOT_INODE } else { FIRST_INODE + i as u64 - 1 };
        let seed = |i: usize| if i == 0 { root_seed } else { seeds[i - 1] };
        let block = BLOCK_SECTORS;
        let mut keys = Vec::new();
        let mut extents = Vec::new();
        let mut at = first;

        // Data, file by file, packed from `first` and split at buckets.
        let mut sectors = alloc::vec![0u64; self.nodes.len()];
        for (i, node_sectors) in sectors.iter_mut().enumerate() {
            let Some(data) = self.data(i) else { continue };
            let total = (data.len() as u64).div_ceil(block * SECTOR as u64) * block;
            let mut done = 0;
            while done < total {
                let n = (total - done).min(bucket - at % bucket).min(max_extent);
                let from = (done * SECTOR as u64) as usize;
                let to = (((done + n) * SECTOR as u64) as usize).min(data.len());
                let mut padded = alloc::vec![0u8; (n * SECTOR as u64) as usize];
                padded[..to - from].copy_from_slice(&data[from..to]);
                let csum = crc32c_zero(&padded);
                let ptr = Ptr { sector: at, dev: 0, gen: 0, cached: false, unwritten: false };
                let key = Key { size: n as u32, ..Key::new(kind::EXTENT, Bpos::new(ino(i), done + n, ROOT_SNAPSHOT)) };
                keys.push((Btree::Extents, key, Extent::encode(n as u32, csum_type::CRC32C, csum, ptr)));
                extents.push(Placed { sector: at, node: i, from, to });
                at += n;
                done += n;
            }
            *node_sectors = total;
        }

        // Dirents, each at its name's hash slot or the first free one after
        // it. A node's back-reference is the first dirent written for it,
        // directories taken in the order they were added and names in byte
        // order.
        let mut backref = alloc::vec![(0u64, 0u64); self.nodes.len()];
        let mut subdirs = alloc::vec![0u64; self.nodes.len()];
        for (d, node) in self.nodes.iter().enumerate() {
            let Content::Dir(entries) = &node.content else { continue };
            let dir = InodeValue { hash_seed: seed(d), flags: SIPHASH, ..Default::default() };
            let mut taken = BTreeSet::new();
            for (name, child) in entries {
                let mut offset = hash(&dir, name).expect("siphash directories");
                while !taken.insert(offset) {
                    offset += 1;
                }
                let kind_ = self.kind(child.0);
                if kind_ == DirentType::Directory {
                    subdirs[d] += 1;
                }
                if backref[child.0] == (0, 0) {
                    backref[child.0] = (ino(d), offset);
                }
                let pos = Bpos::new(ino(d), offset, ROOT_SNAPSHOT);
                keys.push((Btree::Dirents, Key::new(kind::DIRENT, pos), value::dirent(ino(child.0), kind_, name)));
            }
        }

        // Inodes; a directory's depth is one more than its parent's, which
        // was added before it.
        let mut depth = alloc::vec![0u64; self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate() {
            let (type_bits, size) = match &node.content {
                Content::Dir(_) => (S_IFDIR, 0),
                Content::File(data) => (S_IFREG, data.len() as u64),
                Content::Symlink(target) => (S_IFLNK, target.len() as u64),
            };
            let mode = type_bits | node.meta.perm;
            let mut inode = InodeValue { hash_seed: seed(i), flags: u64::from(mode) << 36 | SIPHASH, sectors: sectors[i], size, ..Default::default() };
            inode.set(inode_field::UID, u64::from(node.meta.uid));
            inode.set(inode_field::GID, u64::from(node.meta.gid));
            let (dir, offset) = backref[i];
            inode.set(inode_field::DIR, dir);
            inode.set(inode_field::DIR_OFFSET, offset);
            if type_bits == S_IFDIR {
                inode.set(inode_field::NLINK, subdirs[i]);
                if i > 0 {
                    depth[i] = depth[node.parent] + 1;
                }
                inode.set(inode_field::DEPTH, depth[i]);
            } else {
                inode.set(inode_field::NLINK, u64::from(node.names - 1));
            }
            if i == 0 {
                inode.set(inode_field::SUBVOL, u64::from(ROOT_SUBVOLUME));
            }
            keys.push((Btree::Inodes, Key::new(kind::INODE_V3, Bpos::new(0, ino(i), ROOT_SNAPSHOT)), inode.encode()));
        }
        let next = FIRST_INODE + self.len() as u64;
        keys.push((Btree::LoggedOps, Key::new(kind::INODE_ALLOC_CURSOR, Bpos::new(1, 1, 0)), value::inode_cursor(0, next).to_vec()));
        Populated { tree: self, keys, extents, data_end: at }
    }

    fn kind(&self, i: usize) -> DirentType {
        match self.nodes[i].content {
            Content::Dir(_) => DirentType::Directory,
            Content::File(_) => DirentType::Regular,
            Content::Symlink(_) => DirentType::Symlink,
        }
    }
}

/// Hash type 3, siphash, in an inode's flags.
const SIPHASH: u64 = 3 << 20;

/// A tree's keys and the extents its data fills.
pub(crate) struct Populated<'t, 'a> {
    tree: &'t Tree<'a>,
    /// Every leaf key the tree gives, by btree.
    pub keys: Vec<(Btree, Key, Vec<u8>)>,
    extents: Vec<Placed>,
    /// The sector past the last extent.
    pub data_end: u64,
}

/// An extent's place: its first sector, and which bytes of which node's
/// data it holds.
struct Placed {
    sector: u64,
    node: usize,
    from: usize,
    to: usize,
}

impl Populated<'_, '_> {
    /// Each extent's first sector and its bytes, which the caller pads with
    /// zeros to whole blocks.
    pub(crate) fn extents(&self) -> impl Iterator<Item = (u64, &[u8])> + '_ {
        self.extents.iter().map(|e| (e.sector, &self.tree.data(e.node).expect("an extent of a node with data")[e.from..e.to]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: Meta = Meta { perm: 0o644, uid: 0, gid: 0 };

    #[test]
    fn a_name_no_dirent_can_hold_is_refused() {
        let mut t = Tree::new();
        for name in [&b""[..], b".", b"..", b"a/b", b"a\0b", &[b'x'; 513]] {
            assert_eq!(t.dir(Tree::ROOT, name, META), Err(Error::Invalid("name empty, past 512 bytes, holding NUL or '/', or a dot entry")));
        }
        assert!(t.dir(Tree::ROOT, &[b'x'; 512], META).is_ok());
    }

    #[test]
    fn a_name_twice_in_one_directory_is_refused_once_in_two_is_not() {
        let mut t = Tree::new();
        let d = t.dir(Tree::ROOT, b"d", META).unwrap();
        let f = t.file(Tree::ROOT, b"f", b"x", META).unwrap();
        assert_eq!(t.file(Tree::ROOT, b"d", b"", META), Err(Error::Invalid("a name already in the directory")));
        assert_eq!(t.link(Tree::ROOT, b"f", f), Err(Error::Invalid("a name already in the directory")));
        assert_eq!(t.link(d, b"f", f), Ok(()));
    }

    #[test]
    fn a_directory_takes_no_second_name_and_holds_what_is_put_in_it() {
        let mut t = Tree::new();
        let d = t.dir(Tree::ROOT, b"d", META).unwrap();
        let f = t.file(Tree::ROOT, b"f", b"x", META).unwrap();
        assert_eq!(t.link(Tree::ROOT, b"again", d), Err(Error::Invalid("a hard link to a directory")));
        assert_eq!(t.file(f, b"in-a-file", b"", META), Err(Error::Invalid("a parent that is no directory of this tree")));
        assert_eq!(t.link(Tree::ROOT, b"x", NodeRef(99)), Err(Error::Invalid("no node of this tree")));
    }

    #[test]
    fn a_symlink_target_the_reader_would_refuse_is_refused() {
        let mut t = Tree::new();
        for target in [&b""[..], b"a\0b", &[b'x'; 4096]] {
            assert_eq!(t.symlink(Tree::ROOT, b"l", target, META), Err(Error::Invalid("symlink target empty, past 4095 bytes or holding NUL")));
        }
        assert!(t.symlink(Tree::ROOT, b"l", &[b'x'; 4095], META).is_ok());
        assert_eq!(t.file(Tree::ROOT, b"f", b"", Meta { perm: 0o10000, ..META }), Err(Error::Invalid("permission bits past 0o7777")));
    }
}
