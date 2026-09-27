use alloc::vec::Vec;

use bcachefs::Extent;
use crate::block::BlockResult;
use crate::rootfs::MemoryImage;

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

/// An executable or library userland read and handed over whole: the bytes are
/// copied into pages of the kernel's own at the call, so nothing its sender
/// writes afterwards reaches a page this serves. What a program in `/apps` is
/// spawned and paged from, since no file server's volume is the kernel's.
pub struct ImageBacking {
    pages: Vec<crate::mm::pmm::PhysPage>,
    size: u64,
}

/// The most bytes one image may carry: the copy is kernel memory for the life
/// of every process paged from it, and nothing charges that to the sender.
pub const MAX_IMAGE_BYTES: u64 = 256 << 20;

impl ImageBacking {
    /// Copy `len` bytes of the caller's memory at `ptr`, a 2 MiB page's run at
    /// a time, which is the most one user window maps contiguously.
    pub fn copy_in(
        ctx: &crate::user_ptr::SyscallContext,
        ptr: crate::UserAddr,
        len: u64,
    ) -> Result<Self, toyos_abi::syscall::SyscallError> {
        use toyos_abi::syscall::SyscallError;
        const PAGE: u64 = crate::mm::PAGE_2M;
        if len == 0 || len > MAX_IMAGE_BYTES {
            return Err(SyscallError::InvalidArgument);
        }
        let mut pages = Vec::new();
        pages.try_reserve_exact(len.div_ceil(PAGE) as usize).map_err(|_| SyscallError::ResourceExhausted)?;
        for _ in 0..len.div_ceil(PAGE) {
            let page = crate::mm::pmm::alloc_page(crate::mm::pmm::Category::Elf)
                .ok_or(SyscallError::ResourceExhausted)?;
            pages.push(page);
        }
        let mut done = 0u64;
        while done < len {
            let at = ptr.raw().checked_add(done).ok_or(SyscallError::BadAddress)?;
            // A run ends at the sender's page boundary or the image's own, whichever is first.
            let run = (PAGE - at % PAGE).min(PAGE - done % PAGE).min(len - done);
            let window = ctx.user_bytes(crate::UserAddr::new(at), run).ok_or(SyscallError::BadAddress)?;
            let page = &pages[(done / PAGE) as usize];
            // SAFETY: the page is this backing's own and 2 MiB long; `done % PAGE + run <= PAGE` by the `min` above.
            let dst = unsafe {
                core::slice::from_raw_parts_mut(
                    page.direct_map().as_mut_ptr::<u8>().add((done % PAGE) as usize),
                    run as usize,
                )
            };
            window.read_at(0, dst);
            done += run;
        }
        Ok(Self { pages, size: len })
    }
}

impl FileBacking for ImageBacking {
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult {
        buf.fill(0);
        if file_offset >= self.size {
            return Ok(());
        }
        const PAGE: u64 = crate::mm::PAGE_2M;
        // Every backing is read a page at a time, and a 4 KiB-aligned page never crosses a 2 MiB one.
        assert!(file_offset % BLOCK_SIZE_U64 == 0, "an image read at {file_offset:#x} is not page-aligned");
        let valid = BLOCK_SIZE.min((self.size - file_offset) as usize);
        let page = &self.pages[(file_offset / PAGE) as usize];
        // SAFETY: the page is this backing's, immutable since `copy_in`, and `file_offset % PAGE + valid <= PAGE`.
        let src = unsafe {
            core::slice::from_raw_parts(
                page.direct_map().as_ptr::<u8>().add((file_offset % PAGE) as usize),
                valid,
            )
        };
        buf[..valid].copy_from_slice(src);
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }
}
