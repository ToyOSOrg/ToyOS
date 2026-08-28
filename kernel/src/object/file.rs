//! An open file: two handles to one `FileObject` share the cursor; an independent cursor opens the path again.
//! `SYS_CLOSE` does not report write-back errors — a durability claim must go through `SYS_FSYNC`.

use alloc::string::String;
use alloc::sync::Arc;

use crate::file_cache::{self, FileId, Release};
use crate::sync::Lock;

use super::{KObjectVariant, ObjectCore};

pub struct OpenFileState {
    pub path: String,
    pub file_id: FileId,
    pub position: usize,
    pub mtime: u64,
}

// Drop runs under `Lock<ProcessData>` and cannot take a sleep lock or wait on a device, so it enqueues to writeback instead of flushing.
impl Drop for OpenFileState {
    fn drop(&mut self) {
        if let Release::TeardownOwed = file_cache::release_to_writeback(self.file_id) {
            crate::writeback::enqueue(self.file_id, core::mem::take(&mut self.path), self.mtime);
        }
    }
}

pub struct FileObject {
    pub(super) core: ObjectCore,
    state: Lock<OpenFileState>,
}

impl FileObject {
    pub fn new(state: OpenFileState) -> Arc<Self> {
        Arc::new(Self { core: Self::new_core(), state: Lock::new(state) })
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut OpenFileState) -> R) -> R {
        f(&mut self.state.lock())
    }
}
