use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::file_backing::FileBacking;
use crate::file_cache::{self, FileId};
use crate::fs_rename::{self, Committed, ReplaceRename};
use crate::mm::PAGE_BYTES;
use toyos_abi::syscall::SyscallError;

use crate::vfs::FileSystem;

struct TmpfsBacking {
    file_id: FileId,
    /// Shared by every backing for the entry; cleared by [`retire`], as
    /// `FileBlocks::revoke` clears `/home`'s.
    alive: Arc<AtomicBool>,
}

impl FileBacking for TmpfsBacking {
    /// `Err` once the file is deleted: a gone file is not a file of zeros.
    fn read_page(&self, file_offset: u64, buf: &mut [u8; PAGE_BYTES]) -> crate::block::BlockResult {
        // copy_page_out, not file_cache::read_page: reading through the miss path here would recurse.
        // A hole below the file size, left by a seek-and-write, reads as zero.
        if file_offset >= file_cache::size(self.file_id)
            || file_cache::copy_page_out(self.file_id, (file_offset / 4096) as u32, buf).is_none()
        {
            // After the miss: `retire` clears the flag before the pages drop.
            if !self.alive.load(Ordering::Acquire) {
                log!("tmpfs: read through a backing whose file was deleted");
                return Err(crate::block::BlockError::Device);
            }
            buf.fill(0);
        }
        Ok(())
    }

    fn file_size(&self) -> u64 {
        file_cache::size(self.file_id)
    }
}

/// Ends a file entry's life: cleared first (so every backing fails), pages after.
fn retire(id: FileId, alive: &AtomicBool) {
    alive.store(false, Ordering::Release);
    let _ = file_cache::mark_deleted(id);
}

/// One name's one entry — a file or a symlink, never both. A single map keys
/// every name once, so `create`, `create_symlink`, `rename` and `delete` cannot
/// each see a different namespace.
pub(crate) enum Entry {
    File { id: FileId, mtime: u64, alive: Arc<AtomicBool> },
    Symlink { target: String },
}

/// In-memory filesystem: file data lives in the file cache.
pub struct TmpFs {
    entries: BTreeMap<String, Entry>,
}

impl TmpFs {
    pub fn new() -> Self {
        Self { entries: BTreeMap::new() }
    }
}

impl ReplaceRename for TmpFs {
    type Displaced = Option<Entry>;

    fn source_present(&mut self, old: &str) -> Result<bool, SyscallError> {
        Ok(self.entries.contains_key(old))
    }

    fn same_object(&mut self, old: &str, new: &str) -> Result<bool, SyscallError> {
        // tmpfs keys each name exactly; equal strings are the one entry.
        Ok(old == new)
    }

    fn commit(&mut self, old: &str, new: &str) -> Result<Committed<Option<Entry>>, SyscallError> {
        let Some(moved) = self.entries.remove(old) else { return Err(SyscallError::NotFound) };
        let displaced = self.entries.remove(new);
        self.entries.insert(String::from(new), moved);
        Ok(Committed::new(displaced))
    }

    fn release(
        &mut self,
        _old: &str,
        _new: &str,
        committed: Committed<Option<Entry>>,
    ) -> Result<(), SyscallError> {
        if let Some(Entry::File { id, alive, .. }) = committed.into_displaced() {
            retire(id, &alive);
        }
        Ok(())
    }
}

impl FileSystem for TmpFs {
    // Nothing else caps file count here.
    fn list(&mut self, dir: &str, limit: usize) -> Result<Vec<(String, u64)>, SyscallError> {
        let mut out = Vec::new();
        for (name, entry) in &self.entries {
            if let Entry::File { id, .. } = entry {
                if !crate::vfs::under_directory(name, dir) {
                    continue;
                }
                if out.len() == limit {
                    return Err(SyscallError::ResourceExhausted);
                }
                out.push((name.clone(), file_cache::size(*id)));
            }
        }
        Ok(out)
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        match self.entries.get(name) {
            Some(Entry::File { mtime, .. }) => Ok(*mtime),
            _ => Err(SyscallError::NotFound),
        }
    }

    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError> {
        Ok(match self.entries.get(name) {
            Some(Entry::Symlink { target }) => Some(target.clone()),
            _ => None,
        })
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        match self.entries.get(name) {
            Some(Entry::File { id, .. }) => {
                file_cache::open(*id).commit();
                Ok((*id, None)) // tmpfs: no backing, data is in the file cache
            }
            _ => Err(SyscallError::NotFound),
        }
    }

    fn create(&mut self, name: &str, mtime: u64) -> Result<FileId, SyscallError> {
        if let Some(Entry::File { id, .. }) = self.entries.get(name) {
            return Ok(*id);
        }
        // A dangling symlink of this name is displaced: one name, one entry.
        let id = file_cache::create_file(false); // non-evictable
        self.entries
            .insert(String::from(name), Entry::File { id, mtime, alive: Arc::new(AtomicBool::new(true)) });
        Ok(id)
    }

    fn close_file(&mut self, _file_id: FileId) {
        // No-op: tmpfs pages already persist in the non-evictable file cache.
    }

    fn delete(&mut self, name: &str) -> Result<(), SyscallError> {
        match self.entries.remove(name) {
            Some(Entry::File { id, alive, .. }) => {
                retire(id, &alive);
                Ok(())
            }
            Some(Entry::Symlink { .. }) => Ok(()),
            None => Err(SyscallError::NotFound),
        }
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), SyscallError> {
        fs_rename::replace_rename(self, old, new)
    }

    // `NotSupported`, not a refusal: no directory representation here, so the
    // VFS carries created directories itself.
    fn create_dir(&mut self, _name: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }

    fn remove_dir(&mut self, _name: &str) -> Result<(), SyscallError> {
        Err(SyscallError::NotSupported)
    }

    fn write_page(&mut self, _file_id: FileId, _page_idx: u32, _data: &[u8; PAGE_BYTES]) -> Result<(), SyscallError> {
        Ok(()) // tmpfs: data is already in the file cache (canonical storage)
    }

    fn update_metadata(&mut self, file_id: FileId, _size: u64, mtime: u64) -> Result<(), SyscallError> {
        for entry in self.entries.values_mut() {
            if let Entry::File { id, mtime: mt, .. } = entry {
                if *id == file_id {
                    *mt = mtime;
                    break;
                }
            }
        }
        Ok(())
    }

    fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), SyscallError> {
        // Displaces whatever answered to this name: one name, one entry.
        if let Some(Entry::File { id, alive, .. }) =
            self.entries.insert(String::from(name), Entry::Symlink { target: String::from(target) })
        {
            retire(id, &alive);
        }
        Ok(())
    }

    fn sync(&mut self) -> Result<(), SyscallError> {
        Ok(())
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        match self.entries.get(name) {
            Some(Entry::File { id, alive, .. }) => {
                Ok(Arc::new(TmpfsBacking { file_id: *id, alive: Arc::clone(alive) }))
            }
            _ => Err(SyscallError::NotFound),
        }
    }

    /// `TmpfsBacking` reads the file cache, so it is never behind it.
    fn cached_file_id(&mut self, _name: &str) -> Option<FileId> {
        None
    }
}
