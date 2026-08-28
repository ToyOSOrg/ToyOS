use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::file_backing::FileBacking;
use crate::file_cache::{self, FileId};
use crate::mm::PAGE_BYTES;
use toyos_abi::syscall::SyscallError;

use crate::vfs::FileSystem;

struct TmpfsBacking {
    file_id: FileId,
}

impl FileBacking for TmpfsBacking {
    /// Never `Err`: the pages are the file, so there is no device to refuse.
    fn read_page(&self, file_offset: u64, buf: &mut [u8; PAGE_BYTES]) -> crate::block::BlockResult {
        // copy_page_out, not file_cache::read_page: reading through the miss path here would recurse.
        // A hole below the file size, left by a seek-and-write, reads as zero.
        if file_offset >= file_cache::size(self.file_id)
            || !file_cache::copy_page_out(self.file_id, (file_offset / 4096) as u32, buf)
        {
            buf.fill(0);
        }
        Ok(())
    }

    fn file_size(&self) -> u64 {
        file_cache::size(self.file_id)
    }
}

/// In-memory filesystem: file data lives in the file cache.
pub struct TmpFs {
    /// name → (FileId, mtime)
    files: BTreeMap<String, (FileId, u64)>,
    symlinks: BTreeMap<String, String>,
}

impl TmpFs {
    pub fn new() -> Self {
        Self { files: BTreeMap::new(), symlinks: BTreeMap::new() }
    }
}

impl FileSystem for TmpFs {
    // Nothing else caps file count here.
    fn list(&mut self, limit: usize) -> Result<Vec<(String, u64)>, SyscallError> {
        if self.files.len() > limit {
            return Err(SyscallError::ResourceExhausted);
        }
        Ok(self.files.iter().map(|(name, (file_id, _))| {
            (name.clone(), file_cache::size(*file_id))
        }).collect())
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        self.files.get(name).map(|(_, mtime)| *mtime).ok_or(SyscallError::NotFound)
    }

    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError> {
        Ok(self.symlinks.get(name).cloned())
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        let (file_id, _) = self.files.get(name).ok_or(SyscallError::NotFound)?;
        file_cache::open(*file_id).commit();
        Ok((*file_id, None)) // tmpfs: no backing, data is in the file cache
    }

    fn create(&mut self, name: &str, mtime: u64) -> Result<FileId, SyscallError> {
        if let Some((file_id, _)) = self.files.get(name) {
            return Ok(*file_id);
        }
        let file_id = file_cache::create_file(false); // non-evictable
        self.files.insert(String::from(name), (file_id, mtime));
        Ok(file_id)
    }

    fn close_file(&mut self, _file_id: FileId) {
        // No-op: tmpfs pages already persist in the non-evictable file cache.
    }

    fn delete(&mut self, name: &str) -> Result<(), SyscallError> {
        if let Some((file_id, _)) = self.files.remove(name) {
            let _ = file_cache::mark_deleted(file_id);
            return Ok(());
        }
        if self.symlinks.remove(name).is_some() {
            return Ok(());
        }
        Err(SyscallError::NotFound)
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), SyscallError> {
        if let Some((target_id, _)) = self.files.remove(new) {
            let _ = file_cache::mark_deleted(target_id);
        }
        if let Some(entry) = self.files.remove(old) {
            self.files.insert(String::from(new), entry);
            Ok(())
        } else if let Some(target) = self.symlinks.remove(old) {
            self.symlinks.insert(String::from(new), target);
            Ok(())
        } else {
            Err(SyscallError::NotFound)
        }
    }

    fn write_page(&mut self, _file_id: FileId, _page_idx: u32, _data: &[u8; PAGE_BYTES]) -> Result<(), SyscallError> {
        Ok(()) // tmpfs: data is already in the file cache (canonical storage)
    }

    fn update_metadata(&mut self, file_id: FileId, _size: u64, mtime: u64) -> Result<(), SyscallError> {
        for (fid, mt) in self.files.values_mut() {
            if *fid == file_id {
                *mt = mtime;
                return Ok(());
            }
        }
        Ok(())
    }

    fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), SyscallError> {
        self.symlinks.insert(String::from(name), String::from(target));
        Ok(())
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        Ok(())
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        let (file_id, _) = self.files.get(name).ok_or(SyscallError::NotFound)?;
        Ok(Arc::new(TmpfsBacking { file_id: *file_id }))
    }
}
