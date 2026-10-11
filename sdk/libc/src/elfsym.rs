//! What `dladdr` reads of a loaded image: whether an address lies in it, where
//! its first mapped byte is, and the tables `toyos-elf` chooses its symbol
//! from, each a slice of what the image maps. It reads nothing but the image
//! it is handed, so the host tests it on an image it lays out
//! (`toyos-libc-copies`).
//!
//! The loader relocates nothing in `PT_DYNAMIC`, so every address the dynamic
//! table holds is relative to the image's base.

use core::ffi::CStr;

use toyos_elf::header::{ProgramHeader, PROGRAM_HEADER_SIZE, PT_DYNAMIC, PT_LOAD};
use toyos_elf::{sym, Dynamic, SymTab};

/// A loaded image, as `dl_iterate_phdr` describes it.
#[derive(Clone, Copy)]
pub(crate) struct Image {
    pub(crate) base: u64,
    pub(crate) phdr: *const u8,
    pub(crate) phnum: usize,
}

impl Image {
    /// # Safety
    /// `phdr` points at `phnum` readable program headers.
    unsafe fn segments(self) -> impl Iterator<Item = ProgramHeader> {
        // SAFETY: the caller's.
        let table = unsafe { core::slice::from_raw_parts(self.phdr, self.phnum * PROGRAM_HEADER_SIZE) };
        (0..self.phnum).map_while(move |i| ProgramHeader::parse(table, i))
    }

    /// Whether a `PT_LOAD` segment of the image holds `addr`.
    ///
    /// # Safety
    /// As [`Image::segments`].
    pub(crate) unsafe fn contains(self, addr: u64) -> bool {
        unsafe { self.segments() }.any(|s| s.kind == PT_LOAD && addr.wrapping_sub(self.base + s.vaddr) < s.memsz)
    }

    /// The image's first mapped byte: the page its lowest `PT_LOAD` starts in.
    ///
    /// # Safety
    /// As [`Image::segments`].
    pub(crate) unsafe fn start(self) -> u64 {
        let low = unsafe { self.segments() }.filter(|s| s.kind == PT_LOAD).map(|s| s.vaddr).min().unwrap_or(0);
        self.base + (low & !0xfff)
    }

    /// The image's bytes from `vaddr` to the end of the `PT_LOAD` segment
    /// holding it, and at most `len` of them.
    ///
    /// # Safety
    /// As [`Image::segments`], and every `PT_LOAD` segment is mapped.
    unsafe fn mapped(self, vaddr: u64, len: u64) -> Option<&'static [u8]> {
        let load = unsafe { self.segments() }.find(|s| s.kind == PT_LOAD && vaddr.wrapping_sub(s.vaddr) < s.memsz)?;
        let len = len.min(load.vaddr + load.memsz - vaddr);
        // SAFETY: inside a mapped segment, which the image keeps mapped.
        Some(unsafe { core::slice::from_raw_parts((self.base + vaddr) as *const u8, len as usize) })
    }

    /// The name and address of the dynamic symbol of the image that holds
    /// `addr`, as [`SymTab::dladdr`] chooses it, from as many symbols as
    /// [`Dynamic::sym_count`] counts.
    ///
    /// # Safety
    /// As [`Image::mapped`].
    pub(crate) unsafe fn symbol(self, addr: u64) -> Option<(&'static CStr, u64)> {
        let dynamic = unsafe { self.segments() }.find(|s| s.kind == PT_DYNAMIC)?;
        let dynamic = Dynamic::parse(unsafe { self.mapped(dynamic.vaddr, dynamic.memsz) }?);
        let gnu_hash = match dynamic.gnu_hash {
            Some(vaddr) => Some(unsafe { self.mapped(vaddr, u64::MAX) }?),
            None => None,
        };
        let count = dynamic.sym_count(gnu_hash).unwrap_or_else(|gap| gap);
        let syms = unsafe { self.mapped(dynamic.symtab?, (count as u64).saturating_mul(sym::ENTRY_SIZE as u64)) }?;
        let strs = unsafe { self.mapped(dynamic.strtab?, dynamic.strsz?) }?;
        let (name, within) = SymTab::new(syms, strs).dladdr(addr.checked_sub(self.base)?)?;
        Some((name, addr - within))
    }
}
