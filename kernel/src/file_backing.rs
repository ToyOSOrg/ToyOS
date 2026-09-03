use alloc::sync::Arc;
use alloc::vec::Vec;

use bcachefs::{BlockIO, BlockNum, Extent, SliceBlockIO};
use crate::block::{BlockError, BlockResult};
use crate::page_cache;
use crate::sync::Lock;

/// `mm::PAGE_SIZE`: `usize` for buffer sizing, `u64` for file offsets.
const BLOCK_SIZE: usize = crate::mm::PAGE_SIZE as usize;
const BLOCK_SIZE_U64: u64 = crate::mm::PAGE_SIZE;

/// Backing store for a memory-mapped file; callers don't know if it's NVMe, RAM, or something else.
pub trait FileBacking: Send + Sync {
    /// Reads one page of file data at `file_offset` into `buf`, zero-filling past EOF.
    #[must_use = "a failed read left the buffer zeroed; it does not hold the file's bytes"]
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult;

    /// Total file size in bytes.
    fn file_size(&self) -> u64;
}

/// Which blocks a `/home` file's data lives in, and whether they are still that file's.
pub struct FileBlocks {
    /// `None` once the filesystem has taken the blocks back.
    extents: Lock<Option<Vec<Extent>>>,
}

impl FileBlocks {
    pub fn new(extents: Vec<Extent>) -> Arc<Self> {
        Arc::new(Self { extents: Lock::new(Some(extents)) })
    }

    /// Gives the blocks up; every read through a backing that shares this fails from here on.
    pub fn revoke(&self) {
        // Not refcounted: a read after this must fail, not extend a freed block's life.
        *self.extents.lock() = None;
    }

    /// Runs `f` over the current extent list, or `None` if the file is gone.
    pub fn with<R>(&self, f: impl FnOnce(&mut Vec<Extent>) -> R) -> Option<R> {
        // Lock stays held across `f`: the write path resolves and allocates inside it.
        self.extents.lock().as_mut().map(f)
    }

    /// Keep the first `keep` blocks and hand back the dropped tail runs, for
    /// the caller to free once the shortened record is on the device. Every
    /// backing sharing this cell reads the dropped range as a hole from here on.
    pub fn truncate_to_blocks(&self, keep: u64) -> Vec<Extent> {
        let mut guard = self.extents.lock();
        let Some(runs) = guard.as_mut() else { return Vec::new() };
        let mut dropped = Vec::new();
        let mut remaining = keep;
        let mut kept = Vec::with_capacity(runs.len());
        for run in runs.drain(..) {
            let count = run.block_count as u64;
            if remaining >= count {
                remaining -= count;
                kept.push(run);
            } else {
                if remaining > 0 {
                    kept.push(Extent {
                        start_block: run.start_block,
                        block_count: remaining as u32,
                        _reserved: 0,
                    });
                }
                dropped.push(Extent {
                    start_block: run.start_block + remaining,
                    block_count: (count - remaining) as u32,
                    _reserved: 0,
                });
                remaining = 0;
            }
        }
        *runs = kept;
        dropped
    }
}

/// The block holding `file_offset`, if the extents reach that far.
fn offset_to_block(extents: &[Extent], file_offset: u64) -> Option<u64> {
    let block_idx = file_offset / BLOCK_SIZE_U64;
    let mut cursor = 0u64;
    for ext in extents {
        let count = ext.block_count as u64;
        if block_idx < cursor + count {
            return Some(ext.start_block + (block_idx - cursor));
        }
        cursor += count;
    }
    None
}

/// File backed by blocks of the partition one page cache serves.
pub struct NvmeBacking {
    cache: Arc<page_cache::Cached>,
    blocks: Arc<FileBlocks>,
    size: u64,
}

impl NvmeBacking {
    pub fn new(cache: Arc<page_cache::Cached>, blocks: Arc<FileBlocks>, size: u64) -> Self {
        Self { cache, blocks, size }
    }
}

impl FileBacking for NvmeBacking {
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult {
        buf.fill(0);
        if file_offset >= self.size {
            return Ok(());
        }
        // Unlinked: blocks may already belong to another file.
        let Some(block) = self.blocks.with(|extents| offset_to_block(extents, file_offset)) else {
            log!("file: read through a backing whose file was deleted");
            return Err(BlockError::Device);
        };
        if let Some(block) = block {
            // Bypasses block page cache; file cache is the sole cache for file data.
            let mut raw = [0u8; BLOCK_SIZE];
            // `buf` is already zeroed, so a failed read here returns a hole, not stale data.
            if self.cache.raw_read(block, &mut raw).is_err() {
                log!("file: read of block {block} failed; serving zeros");
                return Err(BlockError::Device);
            }
            let valid = BLOCK_SIZE.min((self.size - file_offset) as usize);
            buf[..valid].copy_from_slice(&raw[..valid]);
        }
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }
}

/// File backed by initrd memory (RAM). No PageCache, no disk I/O.
pub struct InitrdBacking {
    // Holds the whole image, not base+size, so an untrusted extent can be bounds-checked against it.
    image: SliceBlockIO,
    extents: Vec<Extent>,
    size: u64,
}

impl InitrdBacking {
    pub fn new(image: SliceBlockIO, extents: Vec<Extent>, size: u64) -> Self {
        Self { image, extents, size }
    }
}

impl FileBacking for InitrdBacking {
    /// `Err` for a block the image does not reach; there is no device under an initrd to retry against.
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult {
        buf.fill(0);
        if file_offset >= self.size {
            return Ok(());
        }
        // Past the extent list: a hole, and zeros are the file's own bytes.
        let Some(block) = offset_to_block(&self.extents, file_offset) else {
            return Ok(());
        };
        // A block outside the image is corruption, not a hole: refuse it instead of faulting in zeros.
        let Some(bytes) = self.image.block(BlockNum::new(block)) else {
            log!(
                "initrd: an extent names block {block}, which is not inside the \
                 {}-block image it was read out of",
                self.image.block_count()
            );
            return Err(BlockError::Device);
        };
        let valid = BLOCK_SIZE.min((self.size - file_offset) as usize);
        buf[..valid].copy_from_slice(&bytes[..valid]);
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }
}
