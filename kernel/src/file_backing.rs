use alloc::sync::Arc;

use crate::mm::space::CopySource;
use crate::object::shm::SharedMemObject;
use toyos_abi::syscall::SyscallError;

/// `mm::PAGE_SIZE`: `usize` for buffer sizing, `u64` for file offsets.
const BLOCK_SIZE: usize = crate::mm::PAGE_SIZE as usize;

/// Why a page a backing holds could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockError {
    /// The backing itself failed the read.
    Device,
    /// No memory to keep the page in: the kernel holds back what is left.
    Exhausted,
}

pub type BlockResult = Result<(), BlockError>;

/// Backing store for a memory-mapped file: an image a caller handed over, or a
/// tmpfs file; callers do not know which.
pub trait FileBacking: Send + Sync {
    /// Reads one page of file data at `file_offset` into `buf`, zero-filling past EOF.
    #[must_use = "a failed read left the buffer zeroed; it does not hold the file's bytes"]
    fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) -> BlockResult;

    /// Total file size in bytes.
    fn file_size(&self) -> u64;

    /// The file's bytes as a spawn's segments copy them, each page at its
    /// first touch: `NotFound` once the file is gone, `ResourceExhausted`
    /// where what keeps them cannot be charged.
    fn source(&self) -> Result<Arc<dyn CopySource>, SyscallError>;
}

/// An executable in a shared memory object handed over by handle, since no
/// file server's volume is the kernel's: a file server's object for the file
/// (for ROOT, part of the boot image), or one a caller read into an object of
/// its own; or a boot image the loader verified.
///
/// **Nothing is copied at the call, and every page is copied once when it is
/// read.** The object is alive while any process pages from it, and a
/// writable one's bytes stay its holders' to change. So each read takes one
/// copy of the page into the reader's own buffer and what the loader keeps is
/// that copy: a change after the hand-off reaches only pages not yet read,
/// which is the caller changing its own child's program, and never a value
/// the kernel checked and then read again.
pub struct SharedImage {
    object: Arc<SharedMemObject>,
    size: u64,
}

impl SharedImage {
    /// The first `len` bytes of `object`. Refused unless they are ordinary
    /// memory — a device aperture is no program, and a read of one is a device
    /// access — and the object holds them all.
    pub fn over(object: Arc<SharedMemObject>, len: u64) -> Result<Self, SyscallError> {
        if len > object.size() || !object.is_ordinary() {
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
        // The one fetch of each byte: its holders may be writing it.
        self.object.read(file_offset, &mut buf[..valid]);
        Ok(())
    }

    fn file_size(&self) -> u64 {
        self.size
    }

    fn source(&self) -> Result<Arc<dyn CopySource>, SyscallError> {
        Ok(self.object.clone())
    }
}
