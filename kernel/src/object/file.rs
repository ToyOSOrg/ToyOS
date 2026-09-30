//! An open file: two handles to one `FileObject` share the cursor; an independent cursor opens the path again.

use alloc::sync::Arc;

use crate::file_cache::{self, FileId};
use crate::sync::Lock;

use super::{KObjectVariant, ObjectCore};

pub struct OpenFileState {
    pub file_id: FileId,
    pub position: usize,
    pub mtime: u64,
}

// Drop runs under `Lock<ProcessData>`, so it takes the file cache's lock and no other.
impl Drop for OpenFileState {
    fn drop(&mut self) {
        file_cache::release(self.file_id);
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
