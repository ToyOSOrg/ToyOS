//! A pipe's two ends, as two object types.
//!
//! [`PipeReadEnd`] has no write path and [`PipeWriteEnd`] has no read path,
//! so the wrong direction is refused by the type before any policy runs.
//!
//! The ring and its refcounts stay in `crate::pipe`: adding them here would
//! double their ownership. These two types each own one counted reference,
//! released on the last handle so EOF rides `handle_count`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::mm::charge::Exhausted;
use crate::pipe::{PipeReader, PipeRef, PipeWriter};

use super::{Held, KObjectVariant, ObjectCore, ZeroHandles};

pub struct PipeReadEnd {
    pub(super) core: ObjectCore,
    /// The pipe, which outlives the counted reference's release.
    pipe: PipeRef,
    tty: AtomicBool,
    reference: Held<PipeReader>,
}

pub struct PipeWriteEnd {
    pub(super) core: ObjectCore,
    pipe: PipeRef,
    tty: AtomicBool,
    reference: Held<PipeWriter>,
}

impl PipeReadEnd {
    pub fn new(reader: PipeReader) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self {
            core: Self::new_core(0)?,
            pipe: reader.pipe().clone(),
            tty: AtomicBool::new(false),
            reference: Held::new(reader),
        }))
    }

    pub fn pipe(&self) -> &PipeRef {
        &self.pipe
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
    pub fn new(writer: PipeWriter) -> Result<Arc<Self>, Exhausted> {
        Ok(Arc::new(Self {
            core: Self::new_core(0)?,
            pipe: writer.pipe().clone(),
            tty: AtomicBool::new(false),
            reference: Held::new(writer),
        }))
    }

    pub fn pipe(&self) -> &PipeRef {
        &self.pipe
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
