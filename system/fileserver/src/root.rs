//! The SYSTEM role's volume: ROOT, the bcachefs volume of the slot's signed
//! image, served read-only out of the memory the kernel handed the supervisor
//! (`toyos_abi::syscall::ROOT_IMAGE_LABEL`). No disk and no cache: the bytes
//! are already in memory, and nobody writes them.
//!
//! **ROOT crossed the loader's hash and nothing else.** Every block the format
//! names is bounds-checked against the bytes ([`Bytes`]), and every refusal of
//! the format is a client's `Io`, never a panic of this server.
//!
//! Every name is indexed at mount, as DATA's are ([`crate::data`]): ROOT is
//! written once by the build and never changes, so a directory is a prefix of
//! names and nothing more.

use std::collections::BTreeMap;

use bcachefs::{BlockBuf, BlockIO, BlockNum, DeviceError, Extent, Mounted, ReadOnly, TransferError};
use toyos::fs::Error;

use crate::data::{block_for, mapped};
use crate::volume::{join, Identities, Kind, Listed, Meta, Node, OpenHow, Out, Volume};

/// The longest symlink target read back: the wire's path bound.
const MAX_LINK: u64 = toyos::fs::MAX_PATH as u64;

/// ROOT's unit, which is the format's block.
const BLOCK: usize = 4096;

/// ROOT's bytes, as the format reads them: whole blocks of them, and none past.
pub struct Bytes(&'static [u8]);

impl Bytes {
    /// Block `block`, or `None` past the bytes.
    fn block(&self, block: u64) -> Option<&[u8]> {
        let at = usize::try_from(block).ok()?.checked_mul(BLOCK)?;
        self.0.get(at..at.checked_add(BLOCK)?)
    }
}

/// A block outside the bytes, or a write to them.
struct Refused;

impl TransferError for Refused {
    fn refused_before_attempt(&self) -> bool {
        false
    }
}

impl BlockIO for Bytes {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        buf.0.copy_from_slice(self.block(block.raw()).ok_or(DeviceError::classify(&Refused))?);
        Ok(())
    }

    /// A `ReadOnly` mount has no path here.
    fn write_block(&self, _: BlockNum, _: &BlockBuf) -> Result<(), DeviceError> {
        Err(DeviceError::classify(&Refused))
    }

    fn block_count(&self) -> u64 {
        (self.0.len() / BLOCK) as u64
    }
}

/// One file open, while any client holds it.
struct Open {
    extents: Vec<Extent>,
    size: u64,
    mtime: u64,
    holders: u32,
}

pub struct RootVolume {
    fs: Mounted<Bytes, ReadOnly>,
    names: BTreeMap<String, Kind>,
    open: BTreeMap<Node, Open>,
    by_path: BTreeMap<String, Node>,
    next: Node,
    ids: Identities<String>,
}

impl RootVolume {
    /// The volume `bytes` hold, every name indexed; why not, by the format's word.
    pub fn mount(bytes: &'static [u8]) -> Result<Self, String> {
        let fs = Mounted::<_, ReadOnly>::open(Bytes(bytes)).map_err(|e| format!("ROOT does not mount: {e:?}"))?;
        let listed = fs.list(usize::MAX, &|_| true).map_err(|e| format!("ROOT would not list: {e:?}"))?;
        let mut names = BTreeMap::new();
        for (name, _) in listed {
            let kind = match name.strip_suffix('/') {
                Some(dir) => {
                    names.insert(dir.to_string(), Kind::Dir);
                    continue;
                }
                None if fs.is_symlink(&name).map_err(|e| format!("ROOT: {name}: {e:?}"))? => Kind::Symlink,
                None => Kind::File,
            };
            names.insert(name, kind);
        }
        Ok(Self { fs, names, open: BTreeMap::new(), by_path: BTreeMap::new(), next: 1, ids: Identities::new() })
    }

    /// The filesystem's name, which the build derives from every file ROOT carries.
    pub fn uuid(&self) -> String {
        self.fs.uuid().to_string()
    }

    fn has_children(&self, dir: &str) -> bool {
        let prefix = format!("{dir}/");
        self.names.range(prefix.clone()..).next().is_some_and(|(name, _)| name.starts_with(&prefix))
    }

    fn is_dir(&self, path: &str) -> bool {
        path.is_empty()
            || self.names.get(path) == Some(&Kind::Dir)
            || (!self.names.contains_key(path) && self.has_children(path))
    }

    fn entry(&self, node: Node) -> Result<&Open, Error> {
        self.open.get(&node).ok_or(Error::Stale)
    }
}

impl Volume for RootVolume {
    fn writable(&self) -> bool {
        false
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, Error> {
        match self.names.get(path).copied() {
            Some(Kind::Dir) => Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 }),
            Some(kind) => {
                let (_, size) = mapped("a lookup", path, self.fs.file_extents(path))?.ok_or(Error::NotFound)?;
                let mtime = mapped("a lookup", path, self.fs.file_mtime(path))?.ok_or(Error::NotFound)?;
                Ok(Meta { kind, size, mtime })
            }
            None if self.is_dir(path) => Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 }),
            None => Err(Error::NotFound),
        }
    }

    fn read_link(&mut self, path: &str) -> Result<String, Error> {
        if self.names.get(path) != Some(&Kind::Symlink) {
            return Err(if self.names.contains_key(path) || self.is_dir(path) { Error::NotALink } else { Error::NotFound });
        }
        mapped("readlink", path, self.fs.read_link(path, MAX_LINK))?.ok_or(Error::Io)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error> {
        if !self.is_dir(dir) {
            return Err(if self.names.contains_key(dir) { Error::NotADirectory } else { Error::NotFound });
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
            let path = join(dir, &name);
            let meta = match kind {
                Kind::Dir => Meta { kind, size: 0, mtime: 0 },
                _ => self.lstat(&path)?,
            };
            out.push(Listed { name, meta, identity: self.ids.of(path) });
        }
        Ok(out)
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, Error> {
        if self.is_dir(path) {
            return Err(Error::IsADirectory);
        }
        match self.names.get(path) {
            Some(_) if how.create_new => return Err(Error::AlreadyExists),
            None if how.create || how.create_new => return Err(Error::ReadOnly),
            None => return Err(Error::NotFound),
            // A link the resolver did not follow is one with nothing behind it.
            Some(Kind::Symlink | Kind::Dir) => return Err(Error::NotFound),
            Some(Kind::File) if how.truncate => return Err(Error::ReadOnly),
            Some(Kind::File) => {}
        }
        let node = match self.by_path.get(path).copied() {
            Some(node) => node,
            None => {
                let (extents, size) = mapped("open", path, self.fs.file_extents(path))?.ok_or(Error::NotFound)?;
                let mtime = mapped("open", path, self.fs.file_mtime(path))?.ok_or(Error::NotFound)?;
                let node = self.next;
                self.next += 1;
                self.open.insert(node, Open { extents, size, mtime, holders: 0 });
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
        let open = self.entry(node)?;
        if offset >= open.size {
            return Ok(0);
        }
        let n = out.len().min((open.size - offset) as usize);
        let mut done = 0;
        while done < n {
            let at = offset + done as u64;
            let within = (at % BLOCK as u64) as usize;
            let take = (BLOCK - within).min(n - done);
            match block_for(&open.extents, at / BLOCK as u64) {
                Some(block) => {
                    let Some(bytes) = self.fs.io().block(block) else {
                        println!("fileserver: a file on ROOT names block {block}, past ROOT's end");
                        return Err(Error::Io);
                    };
                    out.put(done, &bytes[within..within + take]);
                }
                // Past the extents: a hole, whose bytes are zeros.
                None => out.zero(done, take),
            }
            done += take;
        }
        Ok(n)
    }

    fn write(&mut self, _: Node, _: u64, _: &[u8]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    fn truncate(&mut self, _: Node, _: u64) -> Result<(), Error> {
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
        format!("{}{}, {} blocks read-only from memory", toyos_osrelease::ROOT_SAID, self.uuid(), self.fs.io().block_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volume::Buf;

    /// A ROOT the format writes, as the build writes one: two files, one of
    /// them several blocks, a symlink and a nested directory.
    fn root() -> RootVolume {
        let mut fs = bcachefs::Formatted::format(bcachefs::VecBlockIO::new(256)).expect("format");
        fs.create("etc/system.manifest", b"program shell /system/bin/shell\n", 7).unwrap();
        fs.create("bin/toybox", &big(), 9).unwrap();
        fs.create_symlink("bin/ls", "/system/bin/toybox", 0).unwrap();
        fs.create("share/fonts/a.font", b"font", 0).unwrap();
        let bytes = fs.into_io().expect("write").into_vec();
        RootVolume::mount(Box::leak(bytes.into_boxed_slice())).expect("mount")
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
        assert_eq!(v.lstat("bin/toybox").unwrap().mtime, 9);
        assert_eq!(v.read_link("bin/ls").unwrap(), "/system/bin/toybox");
        assert_eq!(v.read_link("bin/toybox"), Err(Error::NotALink));
        let listed: Vec<(String, Kind)> = v.list("").unwrap().into_iter().map(|e| (e.name, e.meta.kind)).collect();
        assert_eq!(listed, [("bin".into(), Kind::Dir), ("etc".into(), Kind::Dir), ("share".into(), Kind::Dir)]);
        let bin: Vec<String> = v.list("bin").unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(bin, ["ls", "toybox"]);
        assert_eq!(v.lstat("share/fonts").unwrap().kind, Kind::Dir);
        assert_eq!(v.lstat("nothing"), Err(Error::NotFound));
        assert!(v.describe().starts_with(toyos_osrelease::ROOT_SAID));
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

    /// Bytes that are no volume, and a block past them, are refusals and never
    /// a panic of this server.
    #[test]
    fn bytes_that_are_no_volume_or_too_few_are_refused() {
        assert!(RootVolume::mount(Box::leak(vec![0u8; 8 * BLOCK].into_boxed_slice())).is_err());
        let bytes = Bytes(Box::leak(vec![1u8; 2 * BLOCK].into_boxed_slice()));
        let mut buf = BlockBuf::zeroed();
        assert!(bytes.read_block(BlockNum::new(1), &mut buf).is_ok());
        assert!(bytes.read_block(BlockNum::new(2), &mut buf).is_err());
        assert!(bytes.read_block(BlockNum::new(u64::MAX), &mut buf).is_err());
        assert!(bytes.write_block(BlockNum::new(0), &buf).is_err());
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
