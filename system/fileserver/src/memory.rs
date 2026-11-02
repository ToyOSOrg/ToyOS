//! An in-memory namespace. File bytes live only in their retained paged
//! objects; a page absent from an object is a zero-filled hole.

use std::collections::BTreeMap;

use toyos::fs::{Error, Room, MAX_FILE_BYTES};

use crate::volume::{parent, Kind, Listed, Meta, Node, OpenHow, Out, Reserved, Volume};

struct Inode {
    meta: Meta,
    target: Option<String>,
    children: BTreeMap<String, Node>,
    opens: usize,
    linked: bool,
}

pub struct MemoryVolume {
    nodes: BTreeMap<Node, Inode>,
    next: Node,
    clock: fn() -> u64,
}

impl MemoryVolume {
    pub fn new(roots: &[&str], clock: fn() -> u64) -> Self {
        let mut volume = Self { nodes: BTreeMap::new(), next: 1, clock };
        volume.allocate(Kind::Dir, None);
        for root in roots.iter().filter(|r| !r.is_empty()) {
            volume.mkdir(root).expect("memory volume roots are distinct direct children");
        }
        volume
    }

    fn allocate(&mut self, kind: Kind, target: Option<String>) -> Node {
        let node = self.next;
        self.next = self.next.checked_add(1).expect("memory inode numbers exhausted");
        let size = target.as_ref().map_or(0, |t| t.len() as u64);
        self.nodes.insert(node, Inode { meta: Meta { kind, size, mtime: (self.clock)() }, target, children: BTreeMap::new(), opens: 0, linked: true });
        node
    }

    fn insert(&mut self, path: &str, kind: Kind, target: Option<String>) -> Result<Node, Error> {
        self.vacant(path)?;
        let parent = self.node(parent(path))?;
        let node = self.allocate(kind, target);
        self.nodes.get_mut(&parent).expect("the parent exists").children.insert(name(path).into(), node);
        Ok(node)
    }

    fn node(&self, path: &str) -> Result<Node, Error> {
        let mut node = 1;
        for name in path.split('/').filter(|part| !part.is_empty()) {
            let inode = &self.nodes[&node];
            if inode.meta.kind != Kind::Dir { return Err(Error::NotADirectory); }
            node = *inode.children.get(name).ok_or(Error::NotFound)?;
        }
        Ok(node)
    }

    fn inode(&mut self, node: Node) -> Result<&mut Inode, Error> {
        self.nodes.get_mut(&node).ok_or(Error::Stale)
    }

    fn directory(&self, path: &str) -> Result<(), Error> {
        match self.nodes[&self.node(path)?].meta.kind {
            Kind::Dir => Ok(()),
            _ => Err(Error::NotADirectory),
        }
    }

    fn vacant(&self, path: &str) -> Result<(), Error> {
        match self.node(path) {
            Ok(_) => return Err(Error::AlreadyExists),
            Err(Error::NotFound) => {}
            Err(e) => return Err(e),
        }
        self.directory(parent(path))
    }

    fn detach(&mut self, path: &str) {
        let parent = self.node(parent(path)).expect("an existing name has a parent");
        let node = self.nodes.get_mut(&parent).expect("the parent exists").children.remove(name(path)).expect("detaching an existing name");
        let inode = self.nodes.get_mut(&node).expect("a name has an inode");
        inode.linked = false;
        if inode.opens == 0 {
            self.nodes.remove(&node);
        }
    }
}

impl Volume for MemoryVolume {
    fn memory(&self) -> bool { true }
    fn writable(&self) -> bool { true }

    fn linked(&mut self, node: Node) -> Result<bool, Error> {
        Ok(self.inode(node)?.linked)
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, Error> {
        self.node_meta(self.node(path)?)
    }

    fn read_link(&mut self, path: &str) -> Result<String, Error> {
        self.inode(self.node(path)?)?.target.clone().ok_or(Error::NotALink)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error> {
        self.directory(dir)?;
        Ok(self.nodes[&self.node(dir)?].children.iter().map(|(name, node)| Listed {
            name: name.clone(),
            meta: self.nodes[node].meta,
            identity: *node,
        }).collect())
    }

    fn open(&mut self, path: &str, how: OpenHow) -> Result<Node, Error> {
        let node = match self.node(path) {
            Ok(_) if how.create_new => return Err(Error::AlreadyExists),
            Ok(node) => node,
            Err(Error::NotFound) if how.create || how.create_new => {
                self.insert(path, Kind::File, None)?
            }
            Err(e) => return Err(e),
        };
        match self.nodes[&node].meta.kind {
            Kind::File => {}
            Kind::Dir => return Err(Error::IsADirectory),
            Kind::Symlink => return Err(Error::InvalidRequest),
        }
        if how.truncate { self.truncate(node, 0)?; }
        self.inode(node)?.opens += 1;
        Ok(node)
    }

    fn close(&mut self, node: Node) -> Result<(), Error> {
        let inode = self.inode(node)?;
        inode.opens = inode.opens.checked_sub(1).expect("an open memory inode is closed once");
        if inode.opens == 0 && !inode.linked { self.nodes.remove(&node); }
        Ok(())
    }

    fn node_meta(&mut self, node: Node) -> Result<Meta, Error> { Ok(self.inode(node)?.meta) }
    fn identity(&mut self, path: &str) -> Result<u64, Error> { self.node(path) }

    fn read(&mut self, node: Node, offset: u64, out: &mut dyn Out) -> Result<usize, Error> {
        let size = self.inode(node)?.meta.size;
        let len = out.len().min(size.saturating_sub(offset) as usize);
        out.zero(0, len);
        Ok(len)
    }

    fn reserve(&mut self, node: Node, offset: u64, len: u64) -> Result<Reserved, Error> {
        let end = offset.checked_add(len).filter(|&n| n <= MAX_FILE_BYTES).ok_or(Error::FileTooLarge)?;
        let size = self.inode(node)?.meta.size;
        self.truncate(node, size.max(end))?;
        Ok(Reserved { size, contents: size, ticket: 0 })
    }

    fn write_back(&mut self, _: Node, _: u64, _: &[u8]) -> Result<(), Error> {
        unreachable!("memory file bytes stay in their paged object")
    }

    fn truncate(&mut self, node: Node, size: u64) -> Result<(), Error> {
        if size > MAX_FILE_BYTES { return Err(Error::FileTooLarge); }
        let now = (self.clock)();
        let inode = self.inode(node)?;
        inode.meta.size = size;
        inode.meta.mtime = now;
        Ok(())
    }

    fn set_mtime(&mut self, node: Node, mtime: u64) -> Result<(), Error> {
        self.inode(node)?.meta.mtime = mtime;
        Ok(())
    }

    fn mkdir(&mut self, path: &str) -> Result<(), Error> {
        self.insert(path, Kind::Dir, None)?;
        Ok(())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), Error> {
        if path.is_empty() { return Err(Error::Root); }
        self.directory(path)?;
        if !self.nodes[&self.node(path)?].children.is_empty() { return Err(Error::NotEmpty); }
        self.detach(path);
        Ok(())
    }

    fn unlink(&mut self, path: &str) -> Result<(), Error> {
        if self.lstat(path)?.kind == Kind::Dir { return Err(Error::IsADirectory); }
        self.detach(path);
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), Error> {
        if from.is_empty() || to.is_empty() { return Err(Error::Root); }
        let node = self.node(from)?;
        if from == to { return Ok(()); }
        self.directory(parent(to))?;
        let kind = self.nodes[&node].meta.kind;
        if to.strip_prefix(from).is_some_and(|rest| rest.starts_with('/')) { return Err(Error::IntoItself); }
        if let Ok(over) = self.node(to) {
            match (kind == Kind::Dir, self.nodes[&over].meta.kind == Kind::Dir) {
                (false, true) => return Err(Error::IsADirectory),
                (true, false) => return Err(Error::NotADirectory),
                (true, true) if !self.nodes[&over].children.is_empty() => return Err(Error::NotEmpty),
                _ => {}
            }
            self.detach(to);
        }
        let old_parent = self.node(parent(from))?;
        let new_parent = self.node(parent(to))?;
        self.nodes.get_mut(&old_parent).expect("the old parent exists").children.remove(name(from));
        self.nodes.get_mut(&new_parent).expect("the new parent exists").children.insert(name(to).into(), node);
        Ok(())
    }

    fn symlink(&mut self, path: &str, target: &str) -> Result<(), Error> {
        self.insert(path, Kind::Symlink, Some(target.into()))?;
        Ok(())
    }

    fn sync(&mut self) -> Result<Vec<(Node, Error)>, Error> { Ok(Vec::new()) }
    fn commit(&mut self, _: Option<Node>) -> Result<(), Error> { Ok(()) }
    fn room(&mut self) -> Result<Room, Error> { Err(Error::Unsupported) }
    fn describe(&self) -> String { format!("memory: {} inodes", self.nodes.len()) }
}

fn name(path: &str) -> &str {
    path.rsplit('/').next().expect("a path has a name")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATE: OpenHow = OpenHow { create: true, create_new: false, truncate: false };
    fn clock() -> u64 { 123 }

    #[test]
    fn unlink_and_replacement_keep_open_inode_identity() {
        let mut volume = MemoryVolume::new(&[], clock);
        let old = volume.open("old", CREATE).unwrap();
        let new = volume.open("new", CREATE).unwrap();
        volume.truncate(old, 12).unwrap();
        volume.rename("new", "old").unwrap();
        assert_eq!(volume.identity("old"), Ok(new));
        assert_eq!(volume.node_meta(old).unwrap().size, 12);
        assert!(!volume.linked(old).unwrap());
        volume.close(old).unwrap();
        assert_eq!(volume.node_meta(old), Err(Error::Stale));
        volume.unlink("old").unwrap();
        assert_eq!(volume.node_meta(new).unwrap().size, 0);
        let fresh = volume.open("old", CREATE).unwrap();
        assert!(fresh > old && fresh > new);
        volume.close(new).unwrap();
        assert_eq!(volume.node_meta(new), Err(Error::Stale));
    }

    #[test]
    fn directory_rename_carries_inodes_without_matching_sibling_prefixes() {
        let mut volume = MemoryVolume::new(&["home"], clock);
        volume.mkdir("home/a").unwrap();
        let node = volume.open("home/a/file", CREATE).unwrap();
        let sibling = volume.open("home/a-file", CREATE).unwrap();
        volume.rename("home/a", "home/b").unwrap();
        assert_eq!(volume.identity("home/b/file"), Ok(node));
        assert_eq!(volume.identity("home/a-file"), Ok(sibling));
        assert_eq!(volume.rmdir("home/b"), Err(Error::NotEmpty));
        assert_eq!(volume.rename("home/b", "home/b/inner"), Err(Error::IntoItself));
    }

    #[test]
    fn symlinks_keep_their_names_and_never_replace_existing_entries() {
        let mut volume = MemoryVolume::new(&[], clock);
        let node = volume.open("file", CREATE).unwrap();
        assert_eq!(volume.symlink("file", "target"), Err(Error::AlreadyExists));
        assert_eq!(volume.identity("file"), Ok(node));
        assert_eq!(volume.read_link("file"), Err(Error::NotALink));
        volume.symlink("link", "/home/toy/target").unwrap();
        let listed = volume.list("").unwrap();
        assert_eq!(listed[1].name, "link");
        assert_eq!(listed[1].meta.kind, Kind::Symlink);
        assert_eq!(volume.read_link("link"), Ok("/home/toy/target".into()));
    }

    #[test]
    fn a_directory_can_replace_only_an_empty_directory() {
        let mut volume = MemoryVolume::new(&[], clock);
        volume.mkdir("source").unwrap();
        volume.mkdir("target").unwrap();
        let file = volume.open("source/file", CREATE).unwrap();
        let directory = volume.identity("source").unwrap();
        volume.rename("source", "target").unwrap();
        assert_eq!(volume.identity("target"), Ok(directory));
        assert_eq!(volume.identity("target/file"), Ok(file));
        assert_eq!(volume.lstat("source"), Err(Error::NotFound));
        volume.mkdir("empty").unwrap();
        assert_eq!(volume.rename("empty", "target"), Err(Error::NotEmpty));
        assert_eq!(volume.rename("target/file", "empty"), Err(Error::IsADirectory));
        assert_eq!(volume.rename("empty", "target/file"), Err(Error::NotADirectory));
        assert_eq!(volume.identity("target/file"), Ok(file));
    }
}
