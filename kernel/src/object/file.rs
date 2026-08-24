//! An open file.
//!
//! **Two handles to one `FileObject` share the cursor.** That is a change from
//! the descriptor table, where `dup` cloned the `OpenFile` and the two moved
//! apart; it is what an object model means, and it is POSIX's answer for `dup`
//! as well. A caller that wants an independent cursor opens the path again.
//!
//! **`SYS_CLOSE` is not an error channel, and a program that needs one calls
//! `SYS_FSYNC`.** Closing a file returns before its dirty pages have reached
//! any device: the last handle's drop hands them to the write-back queue and
//! returns, so the only thing that could refuse them runs on `iod` long after
//! the closing thread is gone, with no caller left to answer. A refusal there
//! is logged and the pages are lost (`crate::writeback`'s `drain_one`). Every
//! other way of asking is honest — `SYS_FSYNC` flushes inline and returns the
//! device's own word, and a `write` whose page was refused says so — so a
//! program whose durability claim rests on the bytes having landed asks with
//! one of those and never with a close.

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

/// **VFS-free and device-free by design.** It releases one cache reference and,
/// if that was the last, hands the file to the write-back queue — it does not
/// flush and does not take the VFS lock. That is what a `Drop` may do: it runs
/// in contexts that hold `Lock<ProcessData>` (`ops::close`, `ops::close_all`)
/// and cannot take a sleep lock or wait on a device, and the flush the last
/// close owes now rides `iod` instead of this `Drop` (`crate::writeback`,
/// wall 4 of `issues/kernel/every-wait-in-this-kernel-is-a-spin.md`). There is
/// no per-handle `modified` flag any more: the file owns its own dirty state
/// (`file_cache`'s `dirty_meta`), so a reader closing last still flushes what a
/// writer dirtied.
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
