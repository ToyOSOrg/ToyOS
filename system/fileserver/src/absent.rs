//! A role whose volume is not there this boot, or is ours and did not mount:
//! each directory the role serves exists and is empty, and nothing can be made
//! in it — a write into memory under the paths the owner's data lives at would
//! be taken and then lost, which is the harm a missing volume must not do.

use toyos::fs::Error;

use crate::volume::{Kind, Listed, Meta, Node, OpenHow, Out, Reserved, Volume};

pub struct Absent {
    roots: Vec<String>,
    why: String,
}

impl Absent {
    pub fn new(roots: &[&str], why: String) -> Self {
        Self { roots: roots.iter().map(|r| r.to_string()).collect(), why }
    }

    fn is_root(&self, path: &str) -> bool {
        path.is_empty() || self.roots.iter().any(|r| r == path)
    }
}

impl Volume for Absent {
    fn writable(&self) -> bool {
        false
    }

    fn lstat(&mut self, path: &str) -> Result<Meta, Error> {
        match self.is_root(path) {
            true => Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 }),
            false => Err(Error::NotFound),
        }
    }

    fn read_link(&mut self, _path: &str) -> Result<String, Error> {
        Err(Error::NotFound)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<Listed>, Error> {
        match self.is_root(dir) {
            true => Ok(Vec::new()),
            false => Err(Error::NotFound),
        }
    }

    fn open(&mut self, _path: &str, _how: OpenHow) -> Result<Node, Error> {
        Err(Error::NotFound)
    }

    fn close(&mut self, _node: Node) -> Result<(), Error> {
        Ok(())
    }

    fn node_meta(&mut self, _node: Node) -> Result<Meta, Error> {
        Err(Error::NotFound)
    }

    /// The volume's root 1, each served directory its place after it.
    fn identity(&mut self, path: &str) -> Result<u64, Error> {
        if path.is_empty() {
            return Ok(1);
        }
        let at = self.roots.iter().position(|r| r == path).ok_or(Error::NotFound)?;
        Ok(at as u64 + 2)
    }

    fn read(&mut self, _node: Node, _offset: u64, _out: &mut dyn Out) -> Result<usize, Error> {
        Err(Error::NotFound)
    }

    fn reserve(&mut self, _node: Node, _offset: u64, _len: u64) -> Result<Reserved, Error> {
        Err(Error::NotFound)
    }

    fn write_back(&mut self, _node: Node, _offset: u64, _data: &[u8]) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn truncate(&mut self, _node: Node, _size: u64) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn set_mtime(&mut self, _node: Node, _mtime: u64) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn rmdir(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn symlink(&mut self, _path: &str, _target: &str) -> Result<(), Error> {
        Err(Error::NotFound)
    }

    fn sync(&mut self) -> Result<Vec<(Node, Error)>, Error> {
        Ok(Vec::new())
    }

    fn room(&mut self) -> Result<toyos::fs::Room, Error> {
        Ok(toyos::fs::Room { free: 0, unit: 1 })
    }

    fn describe(&self) -> String {
        format!("absent: {}", self.why)
    }
}
