//! ROOT as the VFS reads it: the signed image, a read-only bcachefs volume in
//! memory. The only filesystem this kernel mounts; every writable one is a
//! file server's (`/system/bin/fileserver`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use bcachefs::{FsError, Mounted, ReadOnly};
use crate::file_backing::{FileBacking, ReadOnlyBacking};
use crate::file_cache::{self, FileId};
use crate::rootfs::MemoryImage;
use toyos_abi::syscall::SyscallError;

use crate::vfs::FileSystem;

/// Exhaustive match: corruption maps to `Io`, never `NotFound` — a btree that won't decode isn't "not there".
fn as_syscall_error(err: &FsError) -> SyscallError {
    match err {
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

/// Ceiling on the one allocation `read_link` materialises from a disk-declared
/// size: the kernel's heap ceiling, which `bcachefs` cannot know and the
/// volume bound does not reach.
const MAX_LINK_TARGET: u64 = crate::user_ptr::MAX_USER_STR;
const _: () = assert!(MAX_LINK_TARGET <= crate::mm::MAX_HEAP_ALLOC as u64);

/// Logs the error's detail, then maps it to the `SyscallError` a caller can act on.
fn mapped<T>(op: &str, name: &str, result: Result<T, FsError>) -> Result<T, SyscallError> {
    result.map_err(|err| {
        log!("bcachefs: {} of '{}' failed: {:?}", op, name, err);
        as_syscall_error(&err)
    })
}

/// The same, for a `bcachefs` call whose `Ok(None)` means the name is absent.
fn present<T>(op: &str, name: &str, result: Result<Option<T>, FsError>) -> Result<T, SyscallError> {
    mapped(op, name, result)?.ok_or(SyscallError::NotFound)
}


/// What a name on ROOT is numbered by, once asked.
#[derive(Clone, Copy)]
enum Node {
    /// A file: the `FileId` every open of it answers.
    File(FileId),
    /// A symlink: a number `file_cache::draw_node` drew, which no file holds.
    Symlink(u64),
}

/// VFS adapter for ROOT, a read-only bcachefs volume in memory.
///
/// Every backing it hands out reads the image the mount was opened over, so a
/// file cannot be served off bytes its filesystem does not occupy.
pub struct ReadOnlyBcacheFsAdapter {
    fs: Mounted<MemoryImage, ReadOnly>,
    /// Every name's node once asked. The volume never changes, so a name is
    /// one entry for the boot.
    nodes: BTreeMap<String, Node>,
}

impl ReadOnlyBcacheFsAdapter {
    pub fn new(fs: Mounted<MemoryImage, ReadOnly>) -> Self {
        Self { fs, nodes: BTreeMap::new() }
    }

    /// The `FileId` of the file `name`, `size` bytes long, made with no
    /// reference held when this is its first ask; `Io` for a name the volume
    /// answered as a symlink before and as a file now, which only a corrupt
    /// image can.
    fn file_id(&mut self, name: &str, size: u64) -> Result<FileId, SyscallError> {
        match self.nodes.get(name) {
            Some(&Node::File(file_id)) => return Ok(file_id),
            Some(&Node::Symlink(_)) => {
                log!("bcachefs: '{}' has extents and was a symlink", name);
                return Err(SyscallError::Io);
            }
            None => {}
        }
        // Its mtime is the volume's (`file_mtime`), never the cache's.
        let file_id = file_cache::create_file(true, 0);
        file_cache::set_size(file_id, size);
        file_cache::release(file_id);
        self.nodes.insert(String::from(name), Node::File(file_id));
        Ok(file_id)
    }
}

impl FileSystem for ReadOnlyBcacheFsAdapter {
    fn list(&mut self, dir: &str, limit: usize) -> Result<Vec<String>, SyscallError> {
        let listed = mapped("list", dir, self.fs.list(limit, &|name| crate::vfs::under_directory(name, dir)))?;
        Ok(listed.into_iter().map(|(name, _)| name).collect())
    }

    fn node(&mut self, name: &str) -> Result<u64, SyscallError> {
        match self.nodes.get(name) {
            Some(&(Node::File(node) | Node::Symlink(node))) => return Ok(node),
            None => {}
        }
        if mapped("is_symlink", name, self.fs.is_symlink(name))? {
            let node = file_cache::draw_node();
            self.nodes.insert(String::from(name), Node::Symlink(node));
            return Ok(node);
        }
        let (_, size) = present("node", name, self.fs.file_extents(name))?;
        self.file_id(name, size)
    }

    fn is_dir(&mut self, dir: &str) -> Result<bool, SyscallError> {
        mapped("is_dir", dir, self.fs.is_dir(dir))
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        present("file_mtime", name, self.fs.file_mtime(name))
    }

    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError> {
        mapped("read_link", name, self.fs.read_link(name, MAX_LINK_TARGET))
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        let (extents, size) = present("open", name, self.fs.file_extents(name))?;
        let file_id = self.file_id(name, size)?;
        file_cache::open(file_id).commit();
        let backing = Arc::new(ReadOnlyBacking::new(*self.fs.io(), extents, size));
        Ok((file_id, Some(backing)))
    }

    fn create(&mut self, _name: &str, _mtime: u64) -> Result<FileId, SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn delete(&mut self, _name: &str) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn rename(&mut self, _old: &str, _new: &str) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    // `NotSupported`, not a refusal: no directory representation here, so the
    // VFS carries created directories itself.
    fn create_dir(&mut self, _name: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }

    fn remove_dir(&mut self, _name: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }

    fn create_symlink(&mut self, _name: &str, _target: &str) -> Result<(), SyscallError> {
        Err(SyscallError::PermissionDenied)
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        let (extents, size) = present("open_backing", name, self.fs.file_extents(name))?;
        Ok(Arc::new(ReadOnlyBacking::new(*self.fs.io(), extents, size)))
    }
}

