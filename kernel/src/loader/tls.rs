//! A thread's TLS block: x86-64 variant II with the DTV in front of the data, built holding
//! physical addresses that `rebase_block` shifts once the block is mapped. The layout
//! arithmetic is `toyos_elf::tls`; this is the allocation, the template copies and the DTV.

use crate::elf::TlsModule;
use crate::mm::KernelSlice;
use crate::process::{OwnedAlloc, PageAlloc};
use crate::DirectMap;
use toyos_elf::Layout;

const TCB_SIZE: usize = 64;
/// Module entries a thread's DTV can hold; `SYS_TLS_ALLOC_BLOCK` refuses a module id above it: there is nowhere to record the answer.
pub const DTV_INITIAL_CAPACITY: usize = 64;
/// Generation word, then length word.
const DTV_HEADER_SIZE: usize = 16;
const DTV_BYTES: usize = DTV_HEADER_SIZE + DTV_INITIAL_CAPACITY * 8;
/// A DTV slot for a module whose block has not been allocated yet.
const DTV_UNALLOCATED: u64 = !0u64;

/// One module's TLS area, for a thread that has only the executable's.
pub fn setup_tls(
    tls_template: Option<KernelSlice>,
    tls_memsz: usize,
    tls_align: usize,
) -> Option<(PageAlloc, u64)> {
    setup_combined_tls(
        &[TlsModule {
            template: tls_template,
            memsz: tls_memsz,
            base_offset: 0,
            module_id: 1,
            is_static: true,
        }],
        tls_memsz,
        tls_align,
    )
}

/// One thread's TLS block for every static module; `None` when no allocation holds the layout.
pub fn setup_combined_tls(
    modules: &[TlsModule],
    total_memsz: usize,
    tls_align: usize,
) -> Option<(PageAlloc, u64)> {
    let plan = toyos_elf::tls::plan(
        total_memsz,
        tls_align,
        TCB_SIZE,
        DTV_BYTES,
        crate::mm::PAGE_2M as usize,
    )?;
    let page_alloc = PageAlloc::new(plan.alloc_size, crate::mm::pmm::Category::InitTls)?;
    let block = page_alloc.ptr();

    // SAFETY: `block` is the fresh, unpublished `plan.alloc_size`-byte allocation above.
    unsafe {
        core::ptr::write_bytes(block, 0, plan.alloc_size);
    }

    for module in modules.iter().filter(|m| m.is_static) {
        if let Some(template) = &module.template {
            // SAFETY: `KernelSlice` is bounds-checked and `toyos_elf::tls` bounds the copy inside the unpublished `block`.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    template.base(),
                    block.add(plan.tls_start + module.base_offset),
                    template.size(),
                );
            }
        }
    }

    let block_phys = DirectMap::from_ptr(block).phys();
    let tp_user = block_phys + plan.tp_offset as u64;
    // SAFETY: the plan reserves `TCB_SIZE` bytes at `tp_offset` inside `alloc_size`.
    let tp_kernel = unsafe { block.add(plan.tp_offset) } as *mut u64;
    // TP+0 is the psABI self-pointer, TP+8 the DTV pointer.
    // SAFETY: two words of the `TCB_SIZE` reserved at `tp_kernel`; `block` is still unpublished.
    unsafe {
        *tp_kernel = tp_user;
        *tp_kernel.add(1) = block_phys;
    }

    let dtv = block as *mut u64;
    // SAFETY: every write lands in `[0, DTV_BYTES)`, reserved by the plan at the front of `block`.
    unsafe {
        *dtv = 1;
        *dtv.add(1) = DTV_INITIAL_CAPACITY as u64;
        for i in 0..DTV_INITIAL_CAPACITY {
            *dtv.add(2 + i) = DTV_UNALLOCATED;
        }
        // A `dlopen`ed module's slot stays unallocated until `__tls_get_addr` asks for it.
        for module in modules.iter().filter(|m| m.is_static) {
            let idx = module.module_id as usize;
            if idx > 0 && idx <= DTV_INITIAL_CAPACITY {
                *dtv.add(2 + idx - 1) = block_phys + (plan.tls_start + module.base_offset) as u64;
            }
        }
    }

    Some((page_alloc, tp_user))
}

/// Rebase a fresh TLS block's self-referential pointers from physical to virtual, in place.
/// No Rust type expresses a DTV whose entries point into itself; this models the psABI layout directly, the same untyped-by-nature work `elf::reloc` does one level down.
/// # Safety
/// `phys`/`tp_offset` name the block `setup_combined_tls` just built; nothing else touches it until this returns.
pub(crate) unsafe fn rebase_block(phys: u64, tp_offset: usize, fs_base: u64, rebase: i64) {
    // SAFETY: the caller's contract; word 1's DTV length is the builders' own, never userland's.
    unsafe {
        let block = DirectMap::from_phys(phys).as_mut_ptr::<u8>();
        let tp = block.add(tp_offset) as *mut u64;
        *tp = fs_base;
        *tp.add(1) = (*tp.add(1) as i64 + rebase) as u64;
        let dtv = block as *mut u64;
        let dtv_len = *dtv.add(1) as usize;
        for i in 0..dtv_len {
            let entry = *dtv.add(2 + i);
            if entry != DTV_UNALLOCATED && entry != 0 {
                *dtv.add(2 + i) = (entry as i64 + rebase) as u64;
            }
        }
    }
}

/// Build one thread's TLS block and map it into the child address space; `None` when either fails.
pub fn map_block(
    child_pt: &crate::process::PageTables,
    modules: &[TlsModule],
    total_memsz: usize,
    max_align: usize,
) -> Option<(crate::process::MappedPages, u64)> {
    let (alloc, fs_base) = if total_memsz > 0 {
        setup_combined_tls(modules, total_memsz, max_align)?
    } else {
        setup_tls(None, 0, 1)?
    };

    let phys = alloc.phys();
    let (vaddr, _) = crate::process::vma_map(child_pt, phys, alloc.size() as u64,
        crate::mm::paging::Prot::ReadWrite)?;
    let rebase = vaddr.raw() as i64 - phys as i64;
    let fs_base = (fs_base as i64 + rebase) as u64;
    // SAFETY: nothing runs in the unscheduled child yet, and `fs_base - vaddr` is the `tp_offset` the builder bounded.
    unsafe {
        rebase_block(phys, (fs_base - vaddr.raw()) as usize, fs_base, rebase);
    }
    Some((crate::process::MappedPages::new(vaddr, alloc), fs_base))
}

/// One combined block for every startup module; `None` when they do not fit, since a missing module would mean relocations resolving against a block that is not there.
pub fn build_tls_layout(
    loaded_libs: &[crate::elf::LoadedLib],
    layout: &Layout,
    exe_tls_template: Option<&OwnedAlloc>,
) -> Option<(alloc::vec::Vec<TlsModule>, usize, usize, u64)> {
    let exe = layout.tls.filter(|t| t.memsz > 0);
    let libs = loaded_libs.iter().filter(|lib| lib.tls_memsz > 0);

    let mut modules = alloc::vec::Vec::with_capacity(loaded_libs.len() + 1);
    let mut cursor = 0usize;
    let mut max_align = 1usize;
    // Module id 1 is the executable's; libraries start at 2.
    let mut next_module_id = 2u64;

    for lib in libs {
        let (base_offset, next) =
            toyos_elf::tls::place_module(cursor, lib.tls_memsz, lib.tls_align)?;
        cursor = next;
        max_align = max_align.max(lib.tls_align);
        modules.push(TlsModule {
            template: lib.tls_template,
            memsz: lib.tls_memsz,
            base_offset,
            module_id: next_module_id,
            is_static: true,
        });
        next_module_id += 1;
    }

    if let Some(tls) = exe {
        let (base_offset, next) =
            toyos_elf::tls::place_module(cursor, tls.memsz as usize, tls.align as usize)?;
        cursor = next;
        max_align = max_align.max(tls.align as usize);
        modules.push(TlsModule {
            template: exe_tls_template.map(|buf| buf.slice(tls.filesz as usize)),
            memsz: tls.memsz as usize,
            base_offset,
            module_id: 1,
            is_static: true,
        });
    }

    Some((modules, cursor, max_align, next_module_id))
}
