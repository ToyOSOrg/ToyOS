use alloc::sync::Arc;

use crate::mm::space::CopySource;
use crate::object::shm::SharedMemObject;
use toyos_abi::syscall::SyscallError;

/// `mm::PAGE_SIZE`: `usize` for buffer sizing, `u64` for file offsets.
const BLOCK_SIZE: usize = crate::mm::PAGE_SIZE as usize;

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

    pub fn read_page(&self, file_offset: u64, buf: &mut [u8; BLOCK_SIZE]) {
        buf.fill(0);
        if file_offset >= self.size {
            return;
        }
        let valid = BLOCK_SIZE.min((self.size - file_offset) as usize);
        // The one fetch of each byte: its holders may be writing it.
        self.object.read(file_offset, &mut buf[..valid]);
    }

    pub fn file_size(&self) -> u64 {
        self.size
    }

    pub fn source(&self) -> Arc<dyn CopySource> {
        self.object.clone()
    }
}
