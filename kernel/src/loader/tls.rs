//! A thread's TLS block, in this machine's psABI variant with the DTV in front of it. The layout
//! arithmetic is `toyos_elf::tls`; this is the block's bytes, the template copies, the TCB and the
//! DTV.
//!
//! A block is laid out on the kernel's heap with every pointer in it an offset from its start, and
//! moved to the address it is given inside the one hold of the space that maps it: a process's
//! other threads never see a pointer the kernel has yet to fix, and the kernel never reads the
//! block once they can write it.

use alloc::vec::Vec;

use crate::elf::TlsModule;
use crate::mm::space::At;
use crate::object::shm::SharedMemObject;
use crate::process::{OwnedAlloc, PageTables, KernelMapping};
use toyos_elf::tls::{Static, Variant};
use toyos_elf::{Layout, TlsSegment};

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
    bytes: Vec<u8>,
    /// Where the thread pointer goes, from the block's first byte.
    tp_offset: usize,
    /// What its start is aligned to.
    align: u64,
}

impl TlsBlock {
    /// The block for every static module in `modules`, laid out by `tls`; a
    /// DTV and TCB alone when there is none. `None` when no allocation holds
    /// the layout.
    pub fn build(modules: &[TlsModule], tls: Static) -> Option<TlsBlock> {
        if modules.is_empty() {
            let alone = Static::new(VARIANT, 0, tls.align())?;
            return build_combined(&[TlsModule { template: None, memsz: 0, base_offset: 0, module_id: 1, is_static: true }], alone);
        }
        build_combined(modules, tls)
    }

    /// Map the block into `space`, counted to its job, answering its mapping, the thread pointer,
    /// and where that pointer lies in the block. `None` when `space` has no
    /// room or no memory is left for it.
    pub fn publish(mut self, pt: &PageTables) -> Option<(KernelMapping, u64, usize)> {
        let job = alloc::sync::Arc::clone(pt.lock().job());
        let object = SharedMemObject::create(&job, self.bytes.len() as u64).ok()?;
        let mut space = pt.lock();
        let at = space.reserve(object.size(), At::Anywhere(self.align), kernel::vm::Prot::None).ok()?;
        rebase(&mut self.bytes, self.tp_offset, at.raw());
        object.write(0, &self.bytes);
        let len = object.size();
        space.map(object.clone(), 0, len, kernel::vm::Prot::ReadWrite, At::Fixed(at)).ok()?;
        let thread_pointer = at.raw() + self.tp_offset as u64;
        Some((KernelMapping { object, at }, thread_pointer, self.tp_offset))
    }
}

/// Every static module's template copied in, and the TCB and DTV pointing at
/// offsets from the block's start, which [`rebase`] moves once it has an address.
fn build_combined(modules: &[TlsModule], tls: Static) -> Option<TlsBlock> {
    let plan = tls.plan(TCB_SIZE, DTV_BYTES, crate::mm::PAGE_SIZE as usize)?;
    if plan.alloc_size > crate::mm::MAX_HEAP_ALLOC {
        return None;
    }
    let mut block = alloc::vec![0u8; plan.alloc_size];
    for module in modules.iter().filter(|m| m.is_static) {
        if let Some(template) = &module.template {
            let at = plan.tls_start + module.base_offset;
            // SAFETY: a bounds-checked view of the template, which its module keeps alive.
            let bytes = unsafe { template.as_slice() };
            block[at..at + bytes.len()].copy_from_slice(bytes);
        }
    }
    let mut put = |at: usize, value: u64| block[at..at + 8].copy_from_slice(&value.to_ne_bytes());

    // The thread's id (`toyos_abi::TCB_TID`) is TP+16 on variant II and TP+8 on
    // variant I, zero here — a process's first thread is tid 0 — and written by
    // `process::spawn_thread` for every other.
    match VARIANT {
        // TP+0 the psABI self-pointer, TP+8 the DTV pointer.
        Variant::II => {
            put(plan.tp_offset, plan.tp_offset as u64);
            put(plan.tp_offset + 8, 0);
        }
        // TP+0 the DTV pointer, TP+8 the implementation's word, the tid (zeroed above).
        Variant::I => put(plan.tp_offset, 0),
    }

    put(0, 1);
    put(8, DTV_INITIAL_CAPACITY as u64);
    for i in 0..DTV_INITIAL_CAPACITY {
        put(16 + i * 8, DTV_UNALLOCATED);
    }
    // A `dlopen`ed module's slot stays unallocated until `__tls_get_addr` asks for it.
    for module in modules.iter().filter(|m| m.is_static) {
        let idx = module.module_id as usize;
        if idx > 0 && idx <= DTV_INITIAL_CAPACITY {
            put(16 + (idx - 1) * 8, (plan.tls_start + module.base_offset) as u64);
        }
    }

    let align = (tls.align() as u64).max(crate::mm::PAGE_SIZE);
    Some(TlsBlock { bytes: block, tp_offset: plan.tp_offset, align })
}

/// Move the block's self-referential pointers from offsets to addresses at `at`.
///
/// The walk is `DTV_INITIAL_CAPACITY` entries, the builder's own constant: the DTV's length word is
/// the process's to rewrite once the block is mapped, and is never read here.
fn rebase(block: &mut [u8], tp_offset: usize, at: u64) {
    let word = |block: &[u8], offset: usize| u64::from_ne_bytes(block[offset..offset + 8].try_into().expect("eight bytes"));
    let mut offsets: Vec<usize> = match VARIANT {
        Variant::II => alloc::vec![tp_offset, tp_offset + 8],
        Variant::I => alloc::vec![tp_offset],
    };
    offsets.extend((0..DTV_INITIAL_CAPACITY).map(|i| 16 + i * 8).filter(|&offset| word(block, offset) != DTV_UNALLOCATED));
    for offset in offsets {
        let moved = word(block, offset) + at;
        block[offset..offset + 8].copy_from_slice(&moved.to_ne_bytes());
    }
}

/// The id the first `dlopen`ed module with TLS takes: the executable's is 1.
pub const FIRST_DLOPEN_MODULE: u64 = 2;

/// The static block's one module, the executable's, and its layout; no module for an executable
/// without TLS, and `None` when it does not fit. The module goes where its linker resolved its own
/// accesses: ending at the thread pointer in variant II, starting at it in variant I.
pub fn build_tls_layout(
    layout: &Layout,
    exe_tls_template: Option<&OwnedAlloc>,
) -> Option<(alloc::vec::Vec<TlsModule>, Static)> {
    let Some(tls) = layout.tls().and_then(TlsSegment::occupied) else {
        return Some((alloc::vec::Vec::new(), Static::new(VARIANT, 0, 1)?));
    };
    let (memsz, align) = (tls.memsz() as usize, tls.align() as usize);
    // Variant II's executable ends at the thread pointer at its extent, not its `memsz`:
    // its linker fixed every local-exec offset against the rounded size.
    let placed = match VARIANT {
        Variant::II => toyos_elf::tls::exe_extent(memsz, align)?,
        Variant::I => memsz,
    };
    let template = exe_tls_template.map(|buf| buf.slice(tls.template().len() as usize));
    let module = TlsModule { template, memsz, base_offset: 0, module_id: 1, is_static: true };
    Some((alloc::vec![module], Static::new(VARIANT, placed, align)?))
}
