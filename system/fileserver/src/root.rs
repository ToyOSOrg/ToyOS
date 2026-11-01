//! The SYSTEM role's volume: ROOT, the bcachefs volume of the slot's signed
//! image, served read-only out of the memory the kernel handed the supervisor
//! (`toyos_abi::syscall::ROOT_IMAGE_LABEL`). No disk and no cache: the bytes
//! are already in memory, and nobody writes them.
//!
//! **ROOT crossed the loader's hash and nothing else.** The engine's reader
//! checks every block it is pointed at against the bytes and every checksum
//! against what it covers, and every refusal of the format is a client's
//! `Io`, never a panic of this server.
//!
//! Every name is indexed at mount: ROOT is written once by the build and
//! never changes. A node's identity is its inode number.
//!
//! **A file whose bytes lie in ROOT as they read is handed out in place**
//! ([`Volume::in_place`]), its extents verified at its first ask: a spawn
//! pages a program from ROOT's own pages and copies nothing here.

use std::collections::BTreeMap;

use bcachefs::engine::{self, DirentType};
use toyos::fs::Error;
use toyos_blockdev::{block_on_ready, ReadOnlyBytes, BLOCK};

use crate::volume::{join, Kind, Listed, Meta, Node, OpenHow, Out, Reserved, Volume};

/// The longest read asked of the reader at once, so a large client window
/// costs this much memory and no more.
const READ_CHUNK: usize = 256 << 10;

// A block of ROOT is a page of the memory it is in.
const _: () = assert!(BLOCK == toyos_abi::syscall::PAGE_SIZE);

/// One file open, while any client holds it.
struct Open {
    ino: u64,
    meta: Meta,
    holders: u32,
}

pub struct RootVolume {
    fs: engine::Volume<ReadOnlyBytes<'static>>,
    blocks: usize,
    /// Every path, `""` the root directory, with its inode and kind.
    names: BTreeMap<String, (u64, Kind)>,
    open: BTreeMap<Node, Open>,
    by_path: BTreeMap<String, Node>,
    next: Node,
    /// What [`Volume::in_place`] verified, by inode: ROOT never changes.
    in_place: BTreeMap<u64, Option<(u64, u64)>>,
}

impl RootVolume {
    /// The volume `bytes` hold, every name indexed; why not, by the format's word.
    pub fn mount(bytes: &'static [u8]) -> Result<Self, String> {
        let dev = ReadOnlyBytes::new(bytes).ok_or_else(|| format!("ROOT is {} bytes, no whole {BLOCK}-byte blocks", bytes.len()))?;
        let fs = block_on_ready(engine::Volume::open(dev)).map_err(|e| format!("ROOT does not mount: {e:?}"))?;
        let listed = block_on_ready(fs.names()).map_err(|e| format!("ROOT would not list: {e:?}"))?;
        let mut names = BTreeMap::from([(String::new(), (fs.root(), Kind::Dir))]);
        for (path, entry) in listed {
            let path = String::from_utf8(path).map_err(|e| format!("ROOT names a path that is not UTF-8: {:?}", e.as_bytes()))?;
            let kind = match entry.kind {
                DirentType::Directory => Kind::Dir,
                DirentType::Regular => Kind::File,
                DirentType::Symlink => Kind::Symlink,
                other => return Err(format!("ROOT: {path} is a {other:?}, which ROOT never holds")),
            };
            names.insert(path, (entry.ino, kind));
        }
        Ok(Self { fs, blocks: bytes.len() / BLOCK, names, open: BTreeMap::new(), by_path: BTreeMap::new(), next: 1, in_place: BTreeMap::new() })
    }

    /// The filesystem's name, which the build derives from every file ROOT carries.
    pub fn uuid(&self) -> String {
        self.fs.superblock().user_uuid().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn meta(&self, path: &str, ino: u64, kind: Kind) -> Result<Meta, Error> {
        let inode = mapped("a lookup", path, block_on_ready(self.fs.inode(ino)))?;
        let size = if kind == Kind::Dir { 0 } else { inode.size };
        Ok(Meta { kind, size, mtime: inode.mtime.max(0) as u64 })
    }

    fn entry(&self, node: Node) -> Result<&Open, Error> {
        self.open.get(&node).ok_or(Error::Stale)
    }
}

/// Logs the format's own account, answers the word a client can act on.
fn mapped<T>(what: &str, path: &str, r: Result<T, engine::Error>) -> Result<T, Error> {
    r.map_err(|e| {
        println!("fileserver: {what} of '{path}' on ROOT failed: {e:?}");
        match e {
            engine::Error::NotFound => Error::NotFound,
            engine::Error::NotDirectory => Error::NotADirectory,
            _ => Error::Io,
        }
    })
}

impl Volume for RootVolume {
    fn writable(&self) -> bool {
        false
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, Error> {
        let &(ino, kind) = self.names.get(path).ok_or(Error::NotFound)?;
        self.meta(path, ino, kind)
    }

    fn read_link(&mut self, path: &str) -> Result<String, Error> {
        match self.names.get(path) {
            Some(&(ino, Kind::Symlink)) => {
                let target = mapped("readlink", path, block_on_ready(self.fs.readlink(ino)))?;
                String::from_utf8(target).map_err(|_| Error::Io)
            }
            Some(_) => Err(Error::NotALink),
            None => Err(Error::NotFound),
        }
    }

    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error> {
        match self.names.get(dir) {
            Some((_, Kind::Dir)) => {}
            Some(_) => return Err(Error::NotADirectory),
            None => return Err(Error::NotFound),
        }
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let children: Vec<(String, u64, Kind)> = self
            .names
            .range(prefix.clone()..)
            .take_while(|(name, _)| name.starts_with(&prefix))
            .filter_map(|(name, &(ino, kind))| {
                let rest = &name[prefix.len()..];
                (!rest.is_empty() && !rest.contains('/')).then(|| (rest.to_string(), ino, kind))
            })
            .collect();
        children
            .into_iter()
            .map(|(name, ino, kind)| Ok(Listed { meta: self.meta(&join(dir, &name), ino, kind)?, name, identity: ino }))
            .collect()
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, Error> {
        let ino = match self.names.get(path).copied() {
            Some((_, Kind::Dir)) => return Err(Error::IsADirectory),
            Some(_) if how.create_new => return Err(Error::AlreadyExists),
            None if how.create || how.create_new => return Err(Error::ReadOnly),
            None => return Err(Error::NotFound),
            // A link the resolver did not follow is one with nothing behind it.
            Some((_, Kind::Symlink)) => return Err(Error::NotFound),
            Some((_, Kind::File)) if how.truncate => return Err(Error::ReadOnly),
            Some((ino, Kind::File)) => ino,
        };
        let node = match self.by_path.get(path).copied() {
            Some(node) => node,
            None => {
                let meta = self.meta(path, ino, Kind::File)?;
                let node = self.next;
                self.next += 1;
                self.open.insert(node, Open { ino, meta, holders: 0 });
                self.by_path.insert(path.to_string(), node);
                node
            }
        };
        self.open.get_mut(&node).expect("just found or made").holders += 1;
        Ok(node)
    }

    fn close(&mut self, node: Node) -> Result<(), Error> {
        let Some(open) = self.open.get_mut(&node) else { return Ok(()) };
        open.holders -= 1;
        if open.holders == 0 {
            self.open.remove(&node);
            self.by_path.retain(|_, n| *n != node);
        }
        Ok(())
    }

    fn node_meta(&mut self, node: Node) -> Result<Meta, Error> {
        Ok(self.entry(node)?.meta)
    }

    fn identity(&mut self, path: &str) -> Result<u64, Error> {
        self.names.get(path).map(|&(ino, _)| ino).ok_or(Error::NotFound)
    }

    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, Error> {
        let ino = self.entry(node)?.ino;
        let mut buf = vec![0u8; out.len().min(READ_CHUNK)];
        let mut done = 0;
        while done < out.len() {
            let want = (out.len() - done).min(buf.len());
            let n = mapped("a read", "an open file", block_on_ready(self.fs.read(ino, offset + done as u64, &mut buf[..want])))?;
            out.put(done, &buf[..n]);
            done += n;
            if n < want {
                break;
            }
        }
        Ok(done)
    }

    fn in_place(&mut self, node: Node) -> Result<Option<(u64, u64)>, Error> {
        let ino = self.entry(node)?.ino;
        if let Some(&run) = self.in_place.get(&ino) {
            return Ok(run);
        }
        let run = mapped("a verify", "an open file", block_on_ready(self.fs.in_place(ino)))?;
        self.in_place.insert(ino, run);
        Ok(run)
    }

    fn reserve(&mut self, _: Node, _: u64, _: u64) -> Result<Reserved, Error> {
        Err(Error::ReadOnly)
    }

    fn write_back(&mut self, _: Node, _: u64, _: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn truncate(&mut self, _: Node, _: u64) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn set_mtime(&mut self, _: Node, _: u64) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn mkdir(&mut self, _: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn rmdir(&mut self, _: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn unlink(&mut self, _: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn rename(&mut self, _: &str, _: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn symlink(&mut self, _: &str, _: &str) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn sync(&mut self) -> Result<Vec<(Node, Error)>, Error> {
        Ok(Vec::new())
    }

    /// None: ROOT is read-only.
    fn room(&mut self) -> Result<toyos::fs::Room, Error> {
        Ok(toyos::fs::Room { free: 0, unit: BLOCK as u64 })
    }

    fn describe(&self) -> String {
        format!("{}{}, {} blocks read-only from memory", toyos_osrelease::ROOT_SAID, self.uuid(), self.blocks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volume::Writes;
    use crate::volume::Buf;
    use bcachefs::engine::{Meta as Owner, Options, Tree};

    /// Draws for mkfs: a fixed stream, since nothing here judges the seeds.
    struct Fixed(u8);

    impl toyos_crypto::Random for Fixed {
        fn fill(&mut self, out: &mut [u8]) -> Result<(), toyos_crypto::Error> {
            for b in out {
                self.0 = self.0.wrapping_mul(13).wrapping_add(7);
                *b = self.0;
            }
            Ok(())
        }
    }

    const MTIME: u64 = 1_761_177_600_000_000_000;
    const FILE: Owner = Owner { perm: 0o644, uid: 0, gid: 0 };
    const DIR: Owner = Owner { perm: 0o755, uid: 0, gid: 0 };

    fn image(tree: &Tree) -> Vec<u8> {
        let dev = toyos_blockdev::Bytes::new(vec![0u8; 8 << 20]);
        let options = Options { label: b"ROOT", time: MTIME, encryption: None };
        block_on_ready(engine::mkfs(&dev, &options, tree, &mut Fixed(1))).expect("mkfs");
        dev.into_inner()
    }

    fn mount(bytes: Vec<u8>) -> RootVolume {
        RootVolume::mount(Box::leak(bytes.into_boxed_slice())).expect("mount")
    }

    /// A ROOT the engine writes, as the build writes one: two files, one of
    /// them several blocks, a symlink and a nested directory.
    fn root_image() -> Vec<u8> {
        let toybox = big();
        let mut tree = Tree::new();
        let bin = tree.dir(Tree::ROOT, b"bin", DIR).unwrap();
        tree.symlink(bin, b"ls", b"/system/bin/toybox", FILE).unwrap();
        tree.file(bin, b"toybox", &toybox, FILE).unwrap();
        let etc = tree.dir(Tree::ROOT, b"etc", DIR).unwrap();
        tree.file(etc, b"system.manifest", b"program shell /system/bin/shell\n", FILE).unwrap();
        let share = tree.dir(Tree::ROOT, b"share", DIR).unwrap();
        let fonts = tree.dir(share, b"fonts", DIR).unwrap();
        tree.file(fonts, b"a.font", b"font", FILE).unwrap();
        image(&tree)
    }

    fn root() -> RootVolume {
        mount(root_image())
    }

    fn big() -> Vec<u8> {
        (0..3 * BLOCK as u32 + 17).map(|i| (i * 7) as u8).collect()
    }

    const READ: OpenHow = OpenHow { create: false, create_new: false, truncate: false };

    fn read_all(v: &mut RootVolume, path: &str) -> Vec<u8> {
        let node = v.open(path, READ).expect(path);
        let size = v.node_meta(node).unwrap().size as usize;
        let mut out = vec![0; size + 5];
        let n = v.read(node, 0, &mut Buf(&mut out)).unwrap();
        v.close(node).unwrap();
        out.truncate(n);
        out
    }

    #[test]
    fn root_reads_back_what_the_build_put_in_it() {
        let mut v = root();
        assert_eq!(read_all(&mut v, "etc/system.manifest"), b"program shell /system/bin/shell\n");
        assert_eq!(read_all(&mut v, "bin/toybox"), big());
        let node = v.open("bin/toybox", READ).unwrap();
        let mut part = [0u8; 10];
        assert_eq!(v.read(node, BLOCK as u64 - 3, &mut Buf(&mut part)).unwrap(), 10);
        assert_eq!(part[..], big()[BLOCK - 3..BLOCK + 7]);
        assert_eq!(v.lstat("bin/toybox").unwrap().mtime, MTIME);
        assert_eq!(v.read_link("bin/ls").unwrap(), "/system/bin/toybox");
        assert_eq!(v.read_link("bin/toybox"), Err(Error::NotALink));
        let listed: Vec<(String, Kind)> = v.list("").unwrap().into_iter().map(|e| (e.name, e.meta.kind)).collect();
        assert_eq!(listed, [("bin".into(), Kind::Dir), ("etc".into(), Kind::Dir), ("share".into(), Kind::Dir)]);
        let bin: Vec<String> = v.list("bin").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(bin, ["ls", "toybox"]);
        assert_eq!(v.lstat("share/fonts").unwrap().kind, Kind::Dir);
        assert_eq!(v.lstat("nothing"), Err(Error::NotFound));
        assert_eq!(v.list("bin/toybox").err(), Some(Error::NotADirectory));
        assert!(v.describe().starts_with(toyos_osrelease::ROOT_SAID));
    }

    /// A read longer than one chunk of the reader comes back whole.
    #[test]
    fn a_read_past_one_chunk_is_whole() {
        let data: Vec<u8> = (0..READ_CHUNK * 2 + 4097).map(|i| (i % 253) as u8).collect();
        let mut tree = Tree::new();
        tree.file(Tree::ROOT, b"f", &data, FILE).unwrap();
        let mut v = mount(image(&tree));
        assert!(read_all(&mut v, "f") == data);
    }

    /// A listing names each entry, directory, file and link, by the identity
    /// a stat of its path answers.
    #[test]
    fn a_listing_names_each_entry_by_its_identity() {
        let mut v = root();
        for dir in ["", "bin", "share"] {
            for e in v.list(dir).unwrap() {
                let path = join(dir, &e.name);
                assert_eq!(Ok(e.identity), v.identity(&path), "{path}");
            }
        }
    }

    /// Every request that would change ROOT is refused read-only, and an open
    /// that would make or empty a file is refused before anything is opened.
    #[test]
    fn every_change_is_refused() {
        let mut v = root();
        assert!(!v.writable());
        let create = OpenHow { create: true, ..READ };
        assert_eq!(v.open("etc/new", create), Err(Error::ReadOnly));
        assert_eq!(v.open("bin/toybox", OpenHow { truncate: true, ..READ }), Err(Error::ReadOnly));
        assert_eq!(v.open("bin/toybox", OpenHow { create_new: true, ..READ }), Err(Error::AlreadyExists));
        let node = v.open("bin/toybox", READ).unwrap();
        assert_eq!(v.write(node, 0, b"x"), Err(Error::ReadOnly));
        assert_eq!(v.truncate(node, 0), Err(Error::ReadOnly));
        assert_eq!(v.mkdir("d"), Err(Error::ReadOnly));
        assert_eq!(v.rmdir("bin"), Err(Error::ReadOnly));
        assert_eq!(v.unlink("bin/ls"), Err(Error::ReadOnly));
        assert_eq!(v.rename("bin/ls", "bin/dir"), Err(Error::ReadOnly));
        assert_eq!(v.symlink("x", "y"), Err(Error::ReadOnly));
        assert_eq!(read_all(&mut v, "bin/toybox"), big(), "a refused write changed nothing");
    }

    /// Bytes that are no volume, or no whole blocks, are refusals and never a
    /// panic of this server; so is a file whose data a damaged image changed.
    #[test]
    fn bytes_that_are_no_volume_are_refused_and_a_damaged_extent_is_io() {
        assert!(RootVolume::mount(Box::leak(vec![0u8; 8 * BLOCK].into_boxed_slice())).is_err());
        assert!(RootVolume::mount(Box::leak(vec![0u8; 8 * BLOCK + 512].into_boxed_slice())).is_err());
        let mut bytes = root_image();
        let at = bytes.windows(16).position(|w| w == &big()[..16]).expect("the file's bytes are on ROOT");
        bytes[at] ^= 1;
        let mut v = mount(bytes);
        let node = v.open("bin/toybox", READ).unwrap();
        let mut out = [0u8; 8];
        assert_eq!(v.read(node, 0, &mut Buf(&mut out)), Err(Error::Io));
    }

    /// A program is handed out where its bytes are in ROOT, and a damaged one
    /// is `Io` there as its read is.
    #[test]
    fn a_file_is_in_place_where_root_holds_its_bytes_and_a_damaged_one_is_io() {
        let bytes = root_image();
        let mut v = mount(bytes.clone());
        let node = v.open("bin/toybox", READ).unwrap();
        let (at, len) = v.in_place(node).unwrap().expect("one run");
        assert_eq!(bytes[at as usize..(at + len) as usize], big()[..]);
        assert_eq!(v.in_place(node), Ok(Some((at, len))), "asked again");

        let mut damaged = bytes;
        damaged[at as usize] ^= 1;
        let mut v = mount(damaged);
        let node = v.open("bin/toybox", READ).unwrap();
        assert_eq!(v.in_place(node), Err(Error::Io));
        v.close(node).unwrap();
        assert_eq!(v.in_place(node), Err(Error::Stale), "a closed node");
    }

    /// A node closed by its last holder is gone; a second open is a fresh one.
    #[test]
    fn a_closed_node_is_stale() {
        let mut v = root();
        let node = v.open("bin/toybox", READ).unwrap();
        v.close(node).unwrap();
        assert_eq!(v.node_meta(node), Err(Error::Stale));
        assert!(v.open("bin/toybox", READ).is_ok());
    }
}
