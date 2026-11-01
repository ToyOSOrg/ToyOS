use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::file_backing::FileBacking;
use crate::file_cache::{self, FileId};
use crate::fs_rename::{self, Committed, ReplaceRename};
use crate::mm::charge::{self, Charge};
use crate::mm::space::CopySource;
use crate::mm::PAGE_BYTES;
use toyos_abi::syscall::SyscallError;

use crate::vfs::FileSystem;

struct TmpfsBacking {
    file_id: FileId,
    /// Shared by every backing for the entry; cleared by [`retire`].
    alive: Arc<AtomicBool>,
}

impl FileBacking for TmpfsBacking {
    /// `Err` once the file is deleted: a gone file is not a file of zeros.
    fn read_page(&self, file_offset: u64, buf: &mut [u8; PAGE_BYTES]) -> crate::file_backing::BlockResult {
        // copy_page_out, not file_cache::read_page: reading through the miss path here would recurse.
        // A hole below the file size, left by a seek-and-write, reads as zero.
        if file_offset >= file_cache::size(self.file_id)
            || !file_cache::copy_page_out(self.file_id, (file_offset / PAGE_BYTES as u64) as u32, buf)
        {
            // After the miss: `retire` clears the flag before the pages drop.
            if !self.alive.load(Ordering::Acquire) {
                log!("tmpfs: read through a backing whose file was deleted");
                return Err(crate::file_backing::BlockError::Device);
            }
            buf.fill(0);
        }
        Ok(())
    }

    fn file_size(&self) -> u64 {
        file_cache::size(self.file_id)
    }

    fn source(&self) -> Result<Arc<dyn CopySource>, SyscallError> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(SyscallError::NotFound);
        }
        Ok(Arc::new(file_cache::snapshot(self.file_id)?))
    }
}

/// Ends a file entry's life: cleared first (so every backing fails), pages after.
fn retire(id: FileId, alive: &AtomicBool) {
    alive.store(false, Ordering::Release);
    file_cache::mark_deleted(id);
}

/// One name's one entry — a file or a symlink, never both. A single map keys
/// every name once, so `create`, `create_symlink`, `rename` and `delete` cannot
/// each see a different namespace.
pub(crate) enum Entry {
    File { id: FileId, alive: Arc<AtomicBool> },
    /// `node` is drawn at the link's making and moves with its entry.
    Symlink { target: String, node: u64 },
}

/// In-memory filesystem: file data lives in the file cache.
pub struct TmpFs {
    /// Each name with the charge of what it keeps here: its place, its bytes
    /// and its entry's.
    entries: BTreeMap<String, (Entry, Charge)>,
}

/// The charge of `name` holding an entry that keeps `kept` heap bytes besides.
fn name_charge(name: &str, kept: usize) -> Result<Charge, SyscallError> {
    Ok(Charge::current(charge::map_entry::<String, (Entry, Charge)>() + charge::array::<u8>(name.len()) + kept)?)
}

/// The heap a file's entry keeps besides its name.
fn file_heap() -> usize {
    charge::arc::<AtomicBool>()
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
        let kept = match self.entries.get(old) {
            Some((Entry::File { .. }, _)) => file_heap(),
            Some((Entry::Symlink { target, .. }, _)) => charge::array::<u8>(target.len()),
            None => return Err(SyscallError::NotFound),
        };
        // Charged under its new name before anything moves.
        let charge = name_charge(new, kept)?;
        let (moved, _) = self.entries.remove(old).expect("the name just found");
        let displaced = self.entries.remove(new).map(|(entry, _)| entry);
        self.entries.insert(String::from(new), (moved, charge));
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
    fn list(&mut self, dir: &str, limit: usize, charge: &mut Charge) -> Result<Vec<String>, SyscallError> {
        let mut out = Vec::new();
        for (name, (entry, _)) in &self.entries {
            if let Entry::File { .. } = entry {
                if !crate::vfs::under_directory(name, dir) {
                    continue;
                }
                if out.len() == limit {
                    return Err(SyscallError::ResourceExhausted);
                }
                charge.grow(crate::vfs::listed_heap::<String>(name))?;
                out.push(name.clone());
            }
        }
        Ok(out)
    }

    fn node(&mut self, name: &str) -> Result<u64, SyscallError> {
        match self.entries.get(name) {
            Some((Entry::File { id, .. }, _)) => Ok(*id),
            Some((Entry::Symlink { node, .. }, _)) => Ok(*node),
            None => Err(SyscallError::NotFound),
        }
    }

    /// The first name at or after `dir/` in key order is the only one that can
    /// lie beneath it.
    fn is_dir(&mut self, dir: &str) -> Result<bool, SyscallError> {
        if dir.is_empty() {
            return Ok(true);
        }
        let prefix = alloc::format!("{dir}/");
        Ok(self.entries.range::<str, _>((core::ops::Bound::Included(prefix.as_str()), core::ops::Bound::Unbounded))
            .next()
            .is_some_and(|(name, _)| name.starts_with(&prefix)))
    }

    fn file_mtime(&mut self, name: &str) -> Result<u64, SyscallError> {
        match self.entries.get(name) {
            Some((Entry::File { id, .. }, _)) => Ok(file_cache::mtime(*id)),
            _ => Err(SyscallError::NotFound),
        }
    }

    fn read_link(&mut self, name: &str) -> Result<Option<String>, SyscallError> {
        Ok(match self.entries.get(name) {
            Some((Entry::Symlink { target, .. }, _)) => Some(target.clone()),
            _ => None,
        })
    }

    fn open_file(&mut self, name: &str) -> Result<(FileId, Option<Arc<dyn FileBacking>>), SyscallError> {
        match self.entries.get(name) {
            Some((Entry::File { id, .. }, _)) => {
                file_cache::open(*id).commit();
                Ok((*id, None)) // tmpfs: no backing, data is in the file cache
            }
            _ => Err(SyscallError::NotFound),
        }
    }

    fn create(&mut self, name: &str, mtime: u64) -> Result<FileId, SyscallError> {
        if let Some((Entry::File { id, .. }, _)) = self.entries.get(name) {
            return Ok(*id);
        }
        let charge = name_charge(name, file_heap())?;
        // A dangling symlink of this name is displaced: one name, one entry.
        let id = file_cache::create_file(false, mtime)?; // non-evictable
        self.entries.insert(String::from(name), (Entry::File { id, alive: Arc::new(AtomicBool::new(true)) }, charge));
        Ok(id)
    }

    fn delete(&mut self, name: &str) -> Result<(), SyscallError> {
        match self.entries.remove(name) {
            Some((Entry::File { id, alive, .. }, _)) => {
                retire(id, &alive);
                Ok(())
            }
            Some((Entry::Symlink { .. }, _)) => Ok(()),
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

    fn create_symlink(&mut self, name: &str, target: &str) -> Result<(), SyscallError> {
        let charge = name_charge(name, charge::array::<u8>(target.len()))?;
        // Displaces whatever answered to this name: one name, one entry.
        let link = Entry::Symlink { target: String::from(target), node: file_cache::draw_node() };
        if let Some((Entry::File { id, alive, .. }, _)) = self.entries.insert(String::from(name), (link, charge)) {
            retire(id, &alive);
        }
        Ok(())
    }

    fn open_backing(&mut self, name: &str) -> Result<Arc<dyn FileBacking>, SyscallError> {
        match self.entries.get(name) {
            Some((Entry::File { id, alive, .. }, _)) => {
                Ok(Arc::new(TmpfsBacking { file_id: *id, alive: Arc::clone(alive) }))
            }
            _ => Err(SyscallError::NotFound),
        }
    }
}
