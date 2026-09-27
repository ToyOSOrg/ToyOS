use alloc::sync::Arc;
use alloc::vec::Vec;

use bcachefs::Extent;
use crate::block::BlockResult;
use crate::object::shm::SharedMemObject;
use crate::rootfs::MemoryImage;
use toyos_abi::syscall::SyscallError;

/// `mm::PAGE_SIZE`: `usize` for buffer sizing, `u64` for file offsets.
const BLOCK_SIZE: usize = crate::mm::PAGE_SIZE as usize;
const BLOCK_SIZE_U64: u64 = crate::mm::PAGE_SIZE;

/// Backing store for a memory-mapped file: ROOT's image, an image a caller
/// handed over, or a tmpfs file; callers do not know which.
pub trait FileBacking: Send + Sync {
    /// Reads one page of file data at `file_offset` into `buf`, zero-filling past EOF.
    #[must_use = "a failed read left the buffer zeroed; it does not hold the file's bytes"]
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult;

    /// Total file size in bytes.
    fn file_size(&self) -> u64;
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

/// File on ROOT, backed by a fixed extent list over the image in memory.
///
/// No revocation cell: nothing can delete or truncate a
/// file here, so the blocks a backing was opened over stay that file's for as
/// long as it lives. A block outside the image is refused by
/// [`MemoryImage::read`], which is where every bound on a number the image
/// chose belongs.
pub struct ReadOnlyBacking {
    image: MemoryImage,
    extents: Vec<Extent>,
    size: u64,
}

impl ReadOnlyBacking {
    pub fn new(image: MemoryImage, extents: Vec<Extent>, size: u64) -> Self {
        Self { image, extents, size }
    }
}

impl FileBacking for ReadOnlyBacking {
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult {
        buf.fill(0);
        if file_offset >= self.size {
            return Ok(());
        }
        // Past the extent list: a hole, and zeros are the file's own bytes.
        let Some(block) = offset_to_block(&self.extents, file_offset) else {
            return Ok(());
        };
        let mut raw = [0u8; BLOCK_SIZE];
        // `buf` is already zeroed, so a failed read returns a hole, not stale data.
        if let Err(e) = self.image.read(block, &mut raw) {
            log!("root: read of block {block} failed");
            return Err(e);
        }
        let valid = BLOCK_SIZE.min((self.size - file_offset) as usize);
        buf[..valid].copy_from_slice(&raw[..valid]);
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }
}

/// An executable a caller read into a shared memory object of its
/// own and handed over by handle: what a program in `/apps` is spawned and
/// paged from, since no file server's volume is the kernel's.
///
/// **Nothing is copied at the call, and every page is copied once when it is
/// read.** The object is the caller's memory, charged to it and alive while
/// any process pages from it; its bytes stay the caller's to change. So each
/// read takes one copy of the page into the reader's own buffer and what the
/// loader keeps is that copy: a change after the hand-off reaches only pages
/// not yet read, which is the caller changing its own child's program, and
/// never a value the kernel checked and then read again.
pub struct SharedImage {
    object: Arc<SharedMemObject>,
    size: u64,
}

impl SharedImage {
    /// The first `len` bytes of `object`. Refused unless they are ordinary
    /// memory the kernel allocated — a device aperture is no program, and a
    /// read of one is a device access — and the object holds them all.
    pub fn over(object: Arc<SharedMemObject>, len: u64) -> Result<Self, SyscallError> {
        if len == 0 || len > object.size() || object.ram().is_none() {
            return Err(SyscallError::InvalidArgument);
        }
        Ok(Self { object, size: len })
    }
}

impl FileBacking for SharedImage {
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult {
        buf.fill(0);
        if file_offset >= self.size {
            return Ok(());
        }
        let valid = BLOCK_SIZE.min((self.size - file_offset) as usize);
        // SAFETY: `over` held `size` inside the object, which is one physically
        // contiguous run the kernel allocated (`ram`) and keeps while `object`
        // lives, so `file_offset + valid <= size` bytes from its direct-map
        // address are mapped; `buf` is the reader's own and never the object.
        // No reference is formed over the object's bytes, which its holders
        // may be writing: this is the one fetch of each.
        unsafe {
            let from = self.object.phys().as_ptr::<u8>().add(file_offset as usize);
            core::ptr::copy_nonoverlapping(from, buf.as_mut_ptr(), valid);
        }
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }
}
