//! A process's notice: what a quit asks it, readable until it is read.
//!
//! Each process is started holding its own under `QUIT_LABEL`, and nothing
//! else holds it. A read takes the reason asked, one byte
//! (`toyos_abi::quit::Reason::to_byte`), and blocks while none is; a read or
//! an armed readable watch makes the process a listener, which a quit asks
//! rather than kills (`toyos_proclife::quit`).

use alloc::sync::Arc;

use toyos_proclife::quit::{Notice, Reason};
use toyos_abi::handle::Rights;

use crate::mm::charge::{self, Exhausted};
use crate::sync::Lock;
use crate::watch::Watch;

use super::{KObjectVariant, ObjectCore};

pub struct NoticeObject {
    pub(super) core: ObjectCore,
    /// A leaf: a quit takes it under `PROCESS_TABLE`, a read under the
    /// reader's `ProcessData`, and nothing is taken under it.
    state: Lock<Notice>,
    /// Posted when a reason is put in it.
    watch: Arc<Watch>,
}

impl NoticeObject {
    /// `READ` to take a reason, `WAIT` to watch for one. Neither `DUP` nor
    /// `TRANSFER`: a notice is its process's alone.
    pub const RIGHTS: Rights = Rights::READ.union(Rights::WAIT);

    pub fn new() -> Result<Arc<Self>, Exhausted> {
        let core = Self::new_core(charge::arc::<Watch>())?;
        Ok(Arc::new(Self { core, state: Lock::new(Notice::new()), watch: Arc::new(Watch::new()) }))
    }

    /// The notice's state, for a decision about it; `f` takes no other lock.
    pub fn with<R>(&self, f: impl FnOnce(&mut Notice) -> R) -> R {
        f(&mut self.state.lock())
    }

    /// Take the reason asked, listening from now on.
    pub fn read(&self) -> Option<Reason> {
        self.with(Notice::read)
    }

    /// A readable watch is armed on it: the process listens from now on.
    pub fn listen(&self) {
        self.with(Notice::listen);
    }

    pub fn readable(&self) -> bool {
        self.with(|notice| notice.readable())
    }

    pub fn watch(&self) -> &Arc<Watch> {
        &self.watch
    }
}
