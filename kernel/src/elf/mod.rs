//! Loading a shared object: the effects half of ELF.
//!
//! Decoding lives in `toyos-elf`, pure and host-tested; this module owns
//! everything that touches memory — allocation, segment reads, the private
//! writable window, and the kernel's own ceilings. An ELF is untrusted input:
//! nothing here panics on a malformed one, and refusals are `&'static str` so
//! callers can log them beside the path.

// Every unsafe block in this module tree must carry a `SAFETY:` comment;
// this `warn` is what turns a missing one into a CI failure.
#![warn(clippy::undocumented_unsafe_blocks)]

mod cache;
mod index;
mod reloc;

pub use cache::{cache_loaded_lib, try_clone_cached, CachedRelocs};
pub use index::{parse_rela_entries, ParsedRelaEntries, RelocationIndex};
pub use reloc::{
    apply_dtpmod_relocs, apply_tpoff_relocs, defining_module, rebase_relative_relocs,
    resolve_dlopen_relocs, resolve_lib_bind_relocs,
};

use crate::mm::{align_2m_checked, KernelSlice, MAX_HEAP_ALLOC, PAGE_2M, PAGE_BYTES};
use crate::process::PageAlloc;
use crate::UserAddr;
use toyos_elf::dynamic::Dynamic;
use toyos_elf::section::{SectionTable, SHT_DYNSYM};
use toyos_elf::sym::SymTab;
use toyos_elf::{rela, GnuHash, Layout, RelaTable};

/// `toyos_elf::MAX_TLS_ALIGN` must equal the kernel's largest page.
const _: () = assert!(toyos_elf::MAX_TLS_ALIGN == PAGE_2M);

/// [`Layout::parse`] plus the kernel's ceiling on section header table size,
/// checked once here since not every caller can refuse a malformed file.
pub fn parse_layout(data: &[u8]) -> Result<Layout, &'static str> {
    let layout = Layout::parse(data).map_err(|e| e.as_str())?;
    if layout
        .section_headers
        .is_some_and(|s| s.byte_len() > MAX_HEAP_ALLOC)
    {
        return Err("ELF: section header table larger than one kernel allocation");
    }
    Ok(layout)
}

/// Ownership model for a loaded shared library's memory.
pub enum LibMemory {
    /// Fresh load: one allocation owns everything.
    Owned(PageAlloc),
    /// Cloned from cache: read-only pages are shared, writable pages are private.
    Shared {
        rw_alloc: PageAlloc,
        cached_image: KernelSlice,
        /// 2 MiB-aligned offset of the private writable region in the cached image.
        rw_offset: usize,
        /// Added to a cached address to reach the private writable copy.
        rw_delta: i64,
    },
}

/// One TLS module's placement within a thread's combined TLS block.
#[derive(Clone)]
pub struct TlsModule {
    pub template: Option<KernelSlice>,
    /// Total TLS size including `.tbss`, which is zeroed rather than copied.
    pub memsz: usize,
    /// Byte offset of this module within the combined block (static modules only).
    pub base_offset: usize,
    /// DTV module ID, 1-based; `__tls_get_addr` indexes the DTV with it.
    pub module_id: u64,
    /// True for modules present at process startup; a `dlopen`ed module's
    /// block is allocated on demand through `SYS_TLS_ALLOC_BLOCK`.
    pub is_static: bool,
}

/// Everything a cross-module TLS relocation has to resolve against.
pub struct TlsModuleInfo<'a> {
    pub libs: &'a [LoadedLib],
    pub modules: &'a [TlsModule],
}

pub struct LoadedLib {
    pub memory: LibMemory,
    pub user_base: UserAddr,
    /// Physical base address, for page table mappings.
    pub phys_base: u64,
    /// Bounds-checked view of the whole loaded image.
    pub image: KernelSlice,
    /// `.dynsym`, clamped to the entries it actually holds.
    dynsym: Option<KernelSlice>,
    dynstr: Option<KernelSlice>,
    pub tls_template: Option<KernelSlice>,
    pub tls_memsz: usize,
    pub tls_align: usize,
    rela: Option<KernelSlice>,
    jmprel: Option<KernelSlice>,
    gnu_hash: Option<KernelSlice>,
    cached_relocs: Option<CachedRelocs>,
    /// `.eh_frame_hdr`, relative to the module base, from `PT_GNU_EH_FRAME`.
    pub eh_frame_hdr_vaddr: u64,
    pub eh_frame_hdr_size: u64,
    /// `.init_array`, relative to the module base, from `DT_INIT_ARRAY`.
    pub init_array_vaddr: u64,
    pub init_array_size: u64,
    /// Bytes between the image's lowest and highest virtual address.
    pub span: u64,
    /// Exact (unrounded) writable range within the image; `(span, span)` if none.
    /// Rounding this outward, unlike `rw_offset`/`rw_size`, would make a page of
    /// some other module's `.text` writable.
    pub rw_lo: u64,
    pub rw_hi: u64,
}

impl LoadedLib {
    /// Protection for the page at `offset`: exec below the writable window,
    /// write inside it, read-only above — over-permissive, never under, for
    /// an unusual segment layout.
    fn page_prot(&self, offset: u64) -> crate::mm::paging::Prot {
        use crate::mm::paging::Prot;
        if offset < self.rw_lo {
            Prot::ReadExec
        } else if offset < self.rw_hi {
            Prot::ReadWrite
        } else {
            Prot::Read
        }
    }

    /// Give this module a virtual address in `pt` and map its pages there.
    /// Maps each window once, at its final protection — mapping the whole
    /// image writable first would leave `.text` writable in every process.
    /// A `Shared` module's split window holds a shared tail of `.text` plus
    /// the private copy, byte-identical there, so `ReadExec` is safe over either.
    pub fn map_into(&self, pt: &crate::process::PageTables) -> Option<UserAddr> {
        use crate::mm::paging::WindowProt;

        let (image_phys, image_size) = match &self.memory {
            LibMemory::Owned(alloc) => (
                crate::DirectMap::phys_of(alloc.ptr()),
                alloc.size() as u64,
            ),
            LibMemory::Shared { cached_image, .. } => {
                (cached_image.phys(), cached_image.size() as u64)
            }
        };

        let base = pt
            .lock()
            .alloc_region(image_size, crate::vma::RegionKind::Mapped)?;

        let mut offset = 0;
        while offset < image_size {
            // The cache's shared image, except inside the private writable copy.
            let phys = match &self.memory {
                LibMemory::Owned(_) => image_phys + offset,
                LibMemory::Shared { rw_alloc, rw_offset, .. } => {
                    let lo = *rw_offset as u64;
                    if (lo..lo + rw_alloc.size() as u64).contains(&offset) {
                        crate::DirectMap::phys_of(rw_alloc.ptr()) + (offset - lo)
                    } else {
                        image_phys + offset
                    }
                }
            };
            let mut prot = WindowProt::uniform(crate::mm::paging::Prot::Read);
            let mut page = 0;
            while page < PAGE_2M {
                prot.set(page, self.page_prot(offset + page));
                page += 4096;
            }
            pt.lock().map_window(UserAddr::new(base.raw() + offset), phys, &prot);
            offset += PAGE_2M;
        }
        Some(base)
    }

    /// This module's symbol table, or an empty one when it declares none.
    pub fn symbols(&self) -> SymTab<'_> {
        let Some(syms) = &self.dynsym else {
            return SymTab::empty();
        };
        // A missing `DT_STRTAB` still leaves the symbol count intact; only names are lost.
        let strs: &[u8] = match &self.dynstr {
            // SAFETY: `dynstr` came from `ModuleImage::slice`'s bounds check,
            // and outlives this borrow via `LoadedLib`.
            Some(strs) => unsafe { strs.as_slice() },
            None => &[],
        };
        // SAFETY: same bounds argument as `dynstr` above; `syms` is additionally
        // clamped to the declared symbol count.
        unsafe { SymTab::new(syms.as_slice(), strs) }
    }

    pub fn sym_count(&self) -> usize {
        self.symbols().count()
    }

    fn gnu_hash(&self) -> Option<GnuHash<'_>> {
        // SAFETY: same bounds argument as `symbols` above.
        GnuHash::parse(unsafe { self.gnu_hash.as_ref()?.as_slice() })
    }

    /// Every relocation in this module's `DT_RELA` and `DT_JMPREL` tables.
    fn relocations(&self) -> impl Iterator<Item = toyos_elf::Rela> + '_ {
        table_entries(&self.rela).chain(table_entries(&self.jmprel))
    }

    /// One past the last virtual address this module occupies.
    ///
    /// Derived, not stored, so it needs no update at every site that rebases
    /// `user_base`.
    pub fn user_end(&self) -> u64 {
        self.user_base.raw() + self.span
    }

    /// The address this module gives a symbol it defines.
    pub fn resolve(&self, name: &str) -> Option<UserAddr> {
        let symbols = self.symbols();
        let idx = self.gnu_hash()?.lookup(name, &symbols)?;
        Some(self.user_base + symbols.get(idx)?.value)
    }

    /// A `STT_TLS` symbol's offset within this module's TLS segment.
    pub fn resolve_tls(&self, name: &str) -> Option<u64> {
        self.symbols().find_tls(name)
    }
}

/// Look up a symbol by name, walking `.dynsym` rather than the hash table;
/// some symbols are absent from `.gnu.hash`.
pub fn dlsym(lib: &LoadedLib, name: &str) -> Option<UserAddr> {
    let symbols = lib.symbols();
    symbols.find(name).map(|(_, sym)| lib.user_base + sym.value)
}

/// A loaded module image, addressed by the module's own virtual addresses.
/// Converts a file-supplied vaddr to an in-image offset with a bounds check;
/// refuses rather than panics, since a malformed `.so` is untrusted input.
/// The bounds check must precede the `vaddr - vaddr_min` subtraction: on an
/// out-of-range vaddr that subtraction wraps, and a wrapped offset can pass
/// the slice's own bounds check.
struct ModuleImage {
    image: KernelSlice,
    vaddr_min: u64,
    vaddr_max: u64,
}

impl ModuleImage {
    fn slice(&self, vaddr: u64, size: u64) -> Result<KernelSlice, &'static str> {
        let end = vaddr.checked_add(size).ok_or("ELF: dynamic extent overflows")?;
        if vaddr < self.vaddr_min || end > self.vaddr_max {
            return Err("ELF: dynamic table outside the loaded image");
        }
        Ok(self
            .image
            .subslice((vaddr - self.vaddr_min) as usize, size as usize))
    }

    /// From `vaddr` to the end of the image (used where no tag records a size, e.g. `.gnu.hash`).
    fn slice_to_end(&self, vaddr: u64) -> Result<KernelSlice, &'static str> {
        self.slice(vaddr, self.vaddr_max.saturating_sub(vaddr))
    }

    fn optional(&self, vaddr: Option<u64>, size: u64) -> Result<Option<KernelSlice>, &'static str> {
        match vaddr {
            Some(v) => Ok(Some(self.slice(v, size)?)),
            None => Ok(None),
        }
    }
}

/// Read `dst.size()` bytes from a file backing straight into `dst`.
///
/// `dst` is a `KernelSlice`, so the read is bounded by its own allocation; `Err` leaves it partially zeroed, and callers must refuse rather than continue.
#[must_use = "an image assembled from a failed read is zeros, not the program"]
pub(crate) fn read_backing_into(
    backing: &dyn crate::file_backing::FileBacking,
    offset: u64,
    dst: KernelSlice,
) -> crate::block::BlockResult {
    let mut remaining = dst.size();
    let mut file_off = offset;
    let mut buf_off = 0usize;
    let mut page_buf = [0u8; PAGE_BYTES];
    while remaining > 0 {
        let off_in_block = (file_off % 4096) as usize;
        let chunk = (4096 - off_in_block).min(remaining);
        backing.read_page(file_off - off_in_block as u64, &mut page_buf)?;
        // SAFETY: `copy_from` asserts `buf_off + chunk <= dst.size()`; `page_buf`
        // and `dst` cannot overlap, and nothing else can see `dst` yet.
        unsafe { dst.copy_from(buf_off, &page_buf[off_in_block..off_in_block + chunk]) };
        file_off += chunk as u64;
        buf_off += chunk;
        remaining -= chunk;
    }
    Ok(())
}

/// Load a shared object into one contiguous allocation and apply its
/// `R_X86_64_RELATIVE` relocations, returning the module plus its 2 MiB-aligned
/// writable window (must match what `rw_alloc` later covers).
pub fn load_shared_lib(
    backing: &dyn crate::file_backing::FileBacking,
) -> Result<(LoadedLib, usize, usize), &'static str> {
    let header_size = 4096.min(backing.file_size() as usize);
    let header_data = crate::loader::read_file_range(backing, 0, header_size);
    let layout = parse_layout(&header_data)?;

    let (vaddr_min, vaddr_max) = (layout.vaddr_min, layout.vaddr_max);
    // No writable segment yields an empty window; no relocation can target it.
    let (rw_lo, rw_hi) = layout.writable_window().unwrap_or((layout.span(), layout.span()));

    // `span` is file-chosen; an unchecked round-up could wrap to a too-small allocation.
    let (Some(load_size), Some(rw_end_aligned)) = (
        usize::try_from(layout.span()).ok().and_then(align_2m_checked),
        usize::try_from(rw_hi).ok().and_then(align_2m_checked),
    ) else {
        return Err("ELF: image span does not fit an allocation");
    };
    let rw_offset = rw_lo as usize & !(PAGE_2M as usize - 1);
    let rw_size = rw_end_aligned - rw_offset;

    let t0 = crate::clock::nanos_since_boot();
    let alloc =
        PageAlloc::new(load_size, crate::mm::pmm::Category::Elf).ok_or("dlopen: allocation failed")?;
    let t1 = crate::clock::nanos_since_boot();
    // Every offset below is bounded against what the PMM actually returned, not `load_size`.
    let image = alloc.window();

    // SAFETY: `image` is freshly built from the exclusively-owned `alloc`; nothing
    // else holds a reference yet.
    unsafe {
        image.zero();
    }
    let t2 = crate::clock::nanos_since_boot();

    // In bounds only because `Layout` guarantees `filesz <= memsz`; the checked
    // subslice turns a weakening of that into an assert, not an overwrite.
    for seg in layout.segments() {
        let dst = image.subslice((seg.vaddr - vaddr_min) as usize, seg.filesz as usize);
        read_backing_into(backing, seg.file_offset, dst)
            .map_err(|_| "a segment could not be read off the device")?;
    }
    let t3 = crate::clock::nanos_since_boot();

    let module = ModuleImage { image, vaddr_min, vaddr_max };
    let dyn_info = match layout.dynamic {
        Some((_, vaddr, size)) => {
            let region = module.slice(vaddr, size)?;
            // SAFETY: `region` came from `ModuleImage::slice`'s bounds check;
            // `image` is still exclusively owned here.
            Dynamic::parse(unsafe { region.as_slice() })
        }
        None => Dynamic::default(),
    };

    let gnu_hash = match dyn_info.gnu_hash {
        Some(vaddr) => Some(module.slice_to_end(vaddr)?),
        None => None,
    };

    // Prefer the section header's count: `.gnu.hash` indexes only exports, so a
    // count derived from it is short whenever the module imports anything.
    let declared = dynsym_count_from_sections(backing, &layout)
        .filter(|&n| n > 0)
        .or_else(|| {
            // SAFETY: same bounds argument as above; still within the
            // exclusive-construction phase.
            let table = GnuHash::parse(unsafe { gnu_hash.as_ref()?.as_slice() })?;
            table.sym_count()
        })
        .unwrap_or(0);

    // Clamped to `declared`, so `r_sym` is bounded by the image and by the table, whichever is smaller.
    let dynsym = match dyn_info.symtab {
        Some(vaddr) => {
            let whole = module.slice_to_end(vaddr)?;
            let count = declared.min(whole.size() / toyos_elf::sym::ENTRY_SIZE);
            Some(whole.subslice(0, count * toyos_elf::sym::ENTRY_SIZE))
        }
        None => None,
    };
    let sym_count = dynsym.as_ref().map_or(0, |s| s.size() / toyos_elf::sym::ENTRY_SIZE);
    let dynstr = module.optional(dyn_info.strtab, dyn_info.strsz.unwrap_or(0))?;

    let rela = match dyn_info.rela {
        Some(t) => Some(module.slice(t.vaddr, t.size)?),
        None => None,
    };
    let jmprel = match dyn_info.jmprel {
        Some(t) => Some(module.slice(t.vaddr, t.size)?),
        None => None,
    };

    // Validate every entry before writing any: an unvalidated `DTPOFF64` with
    // `r_sym == 0` writes `r_addend` verbatim, an arbitrary 8-byte write.
    // Bound is the writable window, not the whole image — once cached, the
    // write lands in `rw_alloc`, which covers only that window.
    // `window` is image-relative; the `RELATIVE` pass below applies through
    // `ModuleImage::slice`, which subtracts `vaddr_min` — the two agree only
    // when `vaddr_min == 0`, true for every module a linker produces.
    let window = (rw_offset as u64, (rw_offset + rw_size) as u64);
    let entries = table_entries(&rela).chain(table_entries(&jmprel));
    // `None`: a library's image is written contiguously, with no fill-page edge.
    rela::validate(entries, window, sym_count, None).map_err(|e| e.as_str())?;

    let base_phys = image.phys();
    let mut reloc_count = 0u64;
    for entry in table_entries(&rela).chain(table_entries(&jmprel)) {
        if entry.kind == toyos_elf::RelocKind::Relative {
            let value = (base_phys as i64 + entry.addend) as u64;
            // SAFETY: `module.slice(entry.offset, 8)?` bounds-checks the write
            // independently of `rela::validate`; `image` is still exclusively owned.
            unsafe { module.slice(entry.offset, 8)?.write::<u64>(0, value) };
            reloc_count += 1;
        }
    }

    let (tls_template, tls_memsz, tls_align) = match layout.tls {
        Some(tls) => (
            Some(module.slice(tls.vaddr, tls.filesz)?),
            tls.memsz as usize,
            tls.align as usize,
        ),
        None => (None, 0, 0),
    };
    let (eh_frame_hdr_vaddr, eh_frame_hdr_size) = layout.eh_frame_hdr.unwrap_or((0, 0));
    let init_array = dyn_info.init_array;

    let t4 = crate::clock::nanos_since_boot();
    log!(
        "dlopen: base={:#x} {}MB alloc={}ms zero={}ms copy={}ms reloc={}ms ({} relocs, {} syms)",
        base_phys,
        load_size / (1024 * 1024),
        (t1 - t0) / 1_000_000,
        (t2 - t1) / 1_000_000,
        (t3 - t2) / 1_000_000,
        (t4 - t3) / 1_000_000,
        reloc_count,
        sym_count
    );

    Ok((
        LoadedLib {
            memory: LibMemory::Owned(alloc),
            user_base: UserAddr::new(base_phys),
            phys_base: base_phys,
            image,
            dynsym,
            dynstr,
            tls_template,
            tls_memsz,
            tls_align,
            rela,
            jmprel,
            gnu_hash,
            cached_relocs: None,
            eh_frame_hdr_vaddr,
            eh_frame_hdr_size,
            init_array_vaddr: init_array.map_or(0, |t| t.vaddr),
            init_array_size: init_array.map_or(0, |t| t.size),
            span: layout.span(),
            rw_lo,
            rw_hi,
        },
        rw_offset,
        rw_size,
    ))
}

/// `.dynsym`'s entry count as the section header table declares it, or `None`
/// when the file does not say.
fn dynsym_count_from_sections(
    backing: &dyn crate::file_backing::FileBacking,
    layout: &Layout,
) -> Option<usize> {
    let table = layout.section_headers?;
    let bytes = crate::loader::read_file_range(backing, table.file_offset, table.byte_len());
    let dynsym = SectionTable::new(&bytes).find(SHT_DYNSYM)?;
    let entry_size = dynsym.entry_size.max(toyos_elf::sym::ENTRY_SIZE as u64);
    Some((dynsym.size / entry_size) as usize)
}

/// The entries of one optional relocation table.
///
/// A free function, not a closure: the iterator borrows the kernel pages the
/// table lives in, not the caller's frame.
fn table_entries(table: &Option<KernelSlice>) -> impl Iterator<Item = toyos_elf::Rela> + '_ {
    // SAFETY: every `KernelSlice` here came from `ModuleImage::slice`'s bounds
    // check; the `RELATIVE` pass writes a different range, so this never races.
    table
        .iter()
        .flat_map(|slice| RelaTable::new(unsafe { slice.as_slice() }).iter())
}
