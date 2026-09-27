//! A role whose volume is not there this boot, or is ours and did not mount:
//! each directory the role serves exists and is empty, and nothing can be made
//! in it — a write into memory under the paths the owner's data lives at would
//! be taken and then lost, which is the harm a missing volume must not do.

use toyos_abi::syscall::SyscallError;

use crate::volume::{Kind, Meta, Node, OpenHow, Out, Volume};

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

    fn lstat(&mut self, path: &str) -> Result<Meta, SyscallError> {
        match self.is_root(path) {
            true => Ok(Meta { kind: Kind::Dir, size: 0, mtime: 0 }),
            false => Err(SyscallError::NotFound),
        }
    }

    fn read_link(&mut self, _path: &str) -> Result<String, SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn list(&mut self, dir: &str) -> Result<Vec<(String, Meta)>, SyscallError> {
        match self.is_root(dir) {
            true => Ok(Vec::new()),
            false => Err(SyscallError::NotFound),
        }
    }

    fn open(&mut self, _path: &str, _how: OpenHow) -> Result<Node, SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn close(&mut self, _node: Node) {}

    fn hold(&mut self, _node: Node) {}

    fn node_meta(&mut self, _node: Node) -> Result<Meta, SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn ident(&mut self, _node: Node) -> Result<u64, SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn read(&mut self, _node: Node, _offset: u64, _out: &mut dyn Out) -> Result<usize, SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn write(&mut self, _node: Node, _offset: u64, _data: &[u8]) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn truncate(&mut self, _node: Node, _size: u64) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn mkdir(&mut self, _path: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn rmdir(&mut self, _path: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn unlink(&mut self, _path: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn rename(&mut self, _from: &str, _to: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn symlink(&mut self, _path: &str, _target: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotFound)
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        Ok(())
    }

    fn describe(&self) -> String {
        format!("absent: {}", self.why)
    }
}
