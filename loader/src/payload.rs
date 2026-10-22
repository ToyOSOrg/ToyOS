//! A section of the slot's signed image in pages of its own, which the kernel
//! keeps out of its allocator by the address it is handed
//! (`toyos_abi::boot::BootImage`).
//!
//! **The contract with the kernel**: the pages are `LoaderData`, whole and
//! page-aligned, the bytes past `len` in the last one are zero, and nothing
//! writes them between the read the slot's hash is checked over and the jump.
//! A check over [`Payload::bytes`] is therefore a check over exactly what the
//! kernel is handed.

use toyos_abi::boot::BootImage;

use crate::efi::{AllocateType, BootServices, PAGE_SIZE};

/// `len` bytes at the physical address `at`, identity-mapped while boot
/// services live.
pub struct Payload {
    at: u64,
    len: u64,
}

impl Payload {
    /// Zeroed pages for `len` bytes, `len` non-zero.
    pub fn allocate(bs: &BootServices, len: u64) -> Result<Self, alloc::string::String> {
        assert!(len != 0, "a payload of no bytes has no pages to hand over");
        let pages = usize::try_from(len.div_ceil(PAGE_SIZE as u64))
            .map_err(|_| alloc::format!("{len} bytes are more pages than this loader can count"))?;
        let at = bs
            .allocate_pages(AllocateType::AnyPages, pages)
            .map_err(|e| alloc::format!("firmware would not give {len} bytes: {e:?}"))?;
        // SAFETY: the `pages` pages at `at` were just allocated to this loader,
        // are identity-mapped while boot services live, and nothing else holds them.
        unsafe { core::ptr::write_bytes(at as *mut u8, 0, pages * PAGE_SIZE) };
        Ok(Self { at, len })
    }

    pub fn bytes(&self) -> &[u8] {
        // SAFETY: `at` and `len` are the pages `allocate` took, identity-mapped
        // while boot services live, and released only by `free`, which takes
        // `self`.
        unsafe { core::slice::from_raw_parts(self.at as *const u8, self.len as usize) }
    }

    pub fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `bytes`, and `&mut self` is the one borrow of them.
        unsafe { core::slice::from_raw_parts_mut(self.at as *mut u8, self.len as usize) }
    }

    /// Give the pages back: a payload whose slot was refused is not the kernel's.
    pub fn free(self, bs: &BootServices) {
        let pages = self.len.div_ceil(PAGE_SIZE as u64) as usize;
        // SAFETY: the pages are this payload's own allocation, and `self` is
        // consumed, so nothing reads them after this.
        if let Err(e) = unsafe { bs.free_pages(self.at, pages) } {
            println!("Payload: firmware would not take back the {} bytes at {:#x} ({e:?})", self.len, self.at);
        }
    }

    /// Where the kernel is told the payload is.
    pub fn handoff(&self) -> BootImage {
        BootImage { addr: self.at, len: self.len }
    }
}
