//! An open file: two handles to one `FileObject` share the cursor; an independent cursor opens the path again.

use alloc::sync::Arc;

use crate::file_cache::{self, FileId};
use crate::sleeplock::SleepLock;

use super::ObjectCore;

pub struct OpenFileState {
    pub file_id: FileId,
    /// The volume it was opened on (`vfs::Vfs::open_target`).
    pub volume: u64,
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
    /// A sleeping lock: a read or a write copies to or from its caller under
    /// it, which keeps the position's update atomic with the transfer (POSIX
    /// XSH 2.9.7) and holds no spin lock across the copy.
    state: SleepLock<OpenFileState>,
}

impl FileObject {
    /// Over `core`, which the open charged before it opened the file.
    pub fn new(core: ObjectCore, state: OpenFileState) -> Arc<Self> {
        Arc::new(Self { core, state: SleepLock::new(state) })
    }

    /// `f` over the open file's state; called holding no spin lock, as a
    /// syscall does once it has cloned the object out of its handle table.
    pub fn with<R>(&self, f: impl FnOnce(&mut OpenFileState) -> R) -> R {
        let parkable = crate::scheduler::Parkable::at_entry();
        let mut state = self.state.lock(&parkable);
        f(&mut state)
    }
}
