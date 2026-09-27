//! A thread's TLS block, in this machine's psABI variant with the DTV in front of it. The layout
//! arithmetic is `toyos_elf::tls`; this is the allocation, the template copies, the TCB and the DTV.
//!
//! A block is built in frames no user mapping reaches ([`crate::process::Unpublished`]), holding
//! physical addresses, and rebased to the address it is given inside the one call that maps it:
//! a process's other threads never see a pointer the kernel has yet to fix, and the kernel never
//! reads the block once they can write it.

use crate::elf::TlsModule;
use crate::process::{MappedPages, OwnedAlloc, PageAlloc, PageTables, Unpublished};
use crate::UserAddr;
use toyos_elf::tls::{Static, Variant};
use toyos_elf::Layout;

/// This machine's TLS layout.
pub const VARIANT: Variant = Variant::of(crate::arch::ELF_MACHINE);

/// Variant II's TCB; variant I's is the gap `toyos_elf::tls` leaves below the data.
const TCB_SIZE: usize = 64;
/// Module entries a thread's DTV can hold; `SYS_TLS_ALLOC_BLOCK` refuses a module id above it: there is nowhere to record the answer.
pub const DTV_INITIAL_CAPACITY: usize = 64;
/// Generation word, then length word.
const DTV_HEADER_SIZE: usize = 16;
const DTV_BYTES: usize = DTV_HEADER_SIZE + DTV_INITIAL_CAPACITY * 8;
/// A DTV slot for a module whose block has not been allocated yet.
const DTV_UNALLOCATED: u64 = !0u64;

/// One thread's TLS block, built and not yet mapped.
pub struct TlsBlock {
    frames: Unpublished,
    /// Where the thread pointer goes, from the block's first byte.
    tp_offset: usize,
}

impl TlsBlock {
    /// The block for every static module in `modules`, laid out by `tls`; a
    /// DTV and TCB alone when there is none. `None` when no allocation holds
    /// the layout.
    pub fn build(modules: &[TlsModule], tls: Static) -> Option<TlsBlock> {
        if modules.is_empty() {
            let alone = Static::new(VARIANT, 0, tls.max_align(), tls.max_align())?;
            return build_combined(&[TlsModule { template: None, memsz: 0, base_offset: 0, module_id: 1, is_static: true }], alone);
        }
        build_combined(modules, tls)
    }

    /// Map the block into `pt`, returning its mapping, the thread pointer, and
    /// where that pointer lies in the block. `None`, with the frames freed,
    /// when `pt` has no room.
    pub fn publish(self, pt: &PageTables) -> Option<(MappedPages, u64, usize)> {
        let tp_offset = self.tp_offset;
        let pages = self.frames.publish(pt, crate::mm::paging::Prot::ReadWrite, |frames, at| {
            // SAFETY: `frames` is the block `build_combined` wrote, reachable
            // by no mapping until `publish` maps it after this returns.
            unsafe { rebase(frames, tp_offset, at) }
        })?;
        let fs_base = (pages.vaddr() + tp_offset as u64).raw();
        Some((pages, fs_base, tp_offset))
    }
}

/// Every static module's template copied in, and the TCB and DTV pointing at
/// the block's physical address, which [`rebase`] moves once it has another.
fn build_combined(modules: &[TlsModule], tls: Static) -> Option<TlsBlock> {
    let plan = tls.plan(TCB_SIZE, DTV_BYTES, crate::mm::PAGE_2M as usize)?;
    let frames = Unpublished::new(PageAlloc::new(plan.alloc_size, crate::mm::pmm::Category::InitTls)?);
    let block = frames.ptr();

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

    let block_phys = frames.phys();
    let tp_phys = block_phys + plan.tp_offset as u64;
    // SAFETY: the plan reserves `TCB_SIZE` bytes at `tp_offset` inside `alloc_size`.
    let tp_kernel = unsafe { block.add(plan.tp_offset) } as *mut u64;
    // The thread's id (`toyos_abi::TCB_TID`) is TP+16 on variant II and TP+8 on
    // variant I, zero here — a process's first thread is tid 0 — and written by
    // `process::spawn_thread` for every other.
    // SAFETY: two words of the TCB the plan reserves at `tp_kernel` (`TCB_SIZE`, or
    // variant I's gap of at least 16); `block` is still unpublished.
    unsafe {
        match VARIANT {
            // TP+0 the psABI self-pointer, TP+8 the DTV pointer.
            Variant::II => {
                *tp_kernel = tp_phys;
                *tp_kernel.add(1) = block_phys;
            }
            // TP+0 the DTV pointer, TP+8 the implementation's word, the tid (zeroed above).
            Variant::I => *tp_kernel = block_phys,
        }
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

    Some(TlsBlock { frames, tp_offset: plan.tp_offset })
}

/// Move the block's self-referential pointers from its physical address to
/// `at`, in place.
///
/// No Rust type expresses a DTV whose entries point into itself; this models the psABI layout
/// directly. The walk is `DTV_INITIAL_CAPACITY` entries, the builder's own constant: the DTV's
/// length word is the process's to rewrite once the block is mapped, and is never read here.
///
/// # Safety
/// `frames` is a block [`build_combined`] wrote with its thread pointer at `tp_offset`, and no
/// mapping reaches it.
unsafe fn rebase(frames: &Unpublished, tp_offset: usize, at: UserAddr) {
    let phys = frames.phys();
    let moved = |p: u64| (at + (p - phys)).raw();
    // SAFETY: the caller's contract; every access is inside the TCB and DTV the builder reserved.
    unsafe {
        let block = frames.ptr();
        let tp = block.add(tp_offset) as *mut u64;
        match VARIANT {
            Variant::II => {
                *tp = moved(*tp);
                *tp.add(1) = moved(*tp.add(1));
            }
            Variant::I => *tp = moved(*tp),
        }
        let dtv = block as *mut u64;
        for i in 0..DTV_INITIAL_CAPACITY {
            let entry = *dtv.add(2 + i);
            if entry != DTV_UNALLOCATED {
                *dtv.add(2 + i) = moved(entry);
            }
        }
    }
}

/// One combined block for every startup module; `None` when they do not fit, since a missing module would mean relocations resolving against a block that is not there.
/// The executable's module goes where its linker resolved its own accesses: next to the thread
/// pointer, last in variant II and first in variant I.
pub fn build_tls_layout(
    loaded_libs: &[crate::elf::LoadedLib],
    layout: &Layout,
    exe_tls_template: Option<&OwnedAlloc>,
) -> Option<(alloc::vec::Vec<TlsModule>, Static, u64)> {
    // (template, memsz, placed bytes, align, module id). Module id 1 is the executable's;
    // libraries start at 2.
    let exe = match layout.tls().filter(|t| t.memsz() > 0) {
        None => None,
        Some(tls) => {
            let (memsz, align) = (tls.memsz() as usize, tls.align() as usize);
            // Variant II's executable ends at the thread pointer at its extent, not its `memsz`:
            // its linker fixed every local-exec offset against the rounded size.
            let placed = match VARIANT {
                Variant::II => toyos_elf::tls::exe_extent(memsz, align)?,
                Variant::I => memsz,
            };
            Some((exe_tls_template.map(|buf| buf.slice(tls.template().len() as usize)), memsz, placed, align, 1))
        }
    };
    let libs = loaded_libs
        .iter()
        .filter(|lib| lib.tls_memsz > 0)
        .zip(2u64..)
        .map(|(lib, id)| (lib.tls_template, lib.tls_memsz, lib.tls_memsz, lib.tls_align, id));
    let next_module_id = 2 + loaded_libs.iter().filter(|lib| lib.tls_memsz > 0).count() as u64;
    let order: alloc::vec::Vec<_> = match VARIANT {
        Variant::II => libs.chain(exe).collect(),
        Variant::I => exe.into_iter().chain(libs).collect(),
    };

    let mut modules = alloc::vec::Vec::with_capacity(order.len());
    let mut cursor = 0usize;
    let mut max_align = 1usize;
    for (template, memsz, placed, align, module_id) in order.iter().copied() {
        let (base_offset, next) = toyos_elf::tls::place_module(cursor, placed, align)?;
        cursor = next;
        max_align = max_align.max(align);
        modules.push(TlsModule { template, memsz, base_offset, module_id, is_static: true });
    }
    let first_align = order.first().map_or(1, |&(_, _, _, align, _)| align);
    let tls = Static::new(VARIANT, cursor, max_align, first_align)?;
    Some((modules, tls, next_module_id))
}
