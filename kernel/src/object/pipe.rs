//! A pipe's two ends, as two object types.
//!
//! [`PipeReadEnd`] has no write path and [`PipeWriteEnd`] has no read path,
//! so the wrong direction is refused by the type before any policy runs.
//!
//! The ring, its refcounts and `PipeId` stay in `crate::pipe`: adding them
//! here would double their ownership. These two types each own one counted
//! reference, released on the last handle so EOF rides `handle_count`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::pipe::{PipeId, PipeReader, PipeWriter};

use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

pub struct PipeReadEnd {
    pub(super) core: ObjectCore,
    /// A plain copy, not an `Arc` clone: the hot read path can't pay for one,
    /// and a released `PipeId` is never reissued.
    id: PipeId,
    tty: AtomicBool,
    reference: Held<PipeReader>,
}

pub struct PipeWriteEnd {
    pub(super) core: ObjectCore,
    id: PipeId,
    tty: AtomicBool,
    reference: Held<PipeWriter>,
}

impl PipeReadEnd {
    /// A second counted reference to the same pipe, for `SYS_CONNECTION_JOIN`
    /// to build another object over it.
    pub fn reference(&self) -> PipeReader {
        self.reference.get().expect("a live handle names this end")
    }

    pub fn new(reader: PipeReader) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            id: reader.id(),
            tty: AtomicBool::new(false),
            reference: Held::new(reader),
        })
    }

    pub fn id(&self) -> PipeId {
        self.id
    }

    pub fn is_tty(&self) -> bool {
        self.tty.load(Ordering::Relaxed)
    }

    /// A mark on this end, not the pipe: `SYS_MARK_TTY` converts one handle
    /// at a time.
    pub fn mark_tty(&self) {
        self.tty.store(true, Ordering::Relaxed);
    }
}

impl PipeWriteEnd {
    pub fn reference(&self) -> PipeWriter {
        self.reference.get().expect("a live handle names this end")
    }

    pub fn new(writer: PipeWriter) -> Arc<Self> {
        Arc::new(Self {
            core: Self::new_core(),
            id: writer.id(),
            tty: AtomicBool::new(false),
            reference: Held::new(writer),
        })
    }

    pub fn id(&self) -> PipeId {
        self.id
    }

    pub fn is_tty(&self) -> bool {
        self.tty.load(Ordering::Relaxed)
    }

    pub fn mark_tty(&self) {
        self.tty.store(true, Ordering::Relaxed);
    }
}

impl ZeroHandles for PipeReadEnd {
    fn on_zero_handles(&self) {
        self.reference.release();
    }
}

impl ZeroHandles for PipeWriteEnd {
    fn on_zero_handles(&self) {
        self.reference.release();
    }
}
