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
        let pages = self.frames.publish(pt, crate::mm::policy::Prot::ReadWrite, |frames, at| {
            if crate::actuator::tls_rebase_window() {
                rebase_window::hold(frames, at);
            }
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

/// `tls-rebase-window`: a sibling's store staged between a block being given
/// an address and its pointers being rebased to it. Only a spawn whose
/// argument is [`MARK`](rebase_window::MARK) is watched, so the test program
/// chooses the spawns it races.
pub(crate) mod rebase_window {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::process::Unpublished;
    use crate::time::Duration;
    use crate::UserAddr;

    /// The thread argument that asks for a watched spawn.
    const MARK: u64 = 0x5eed_c0de_71b0_0001;
    /// How long a reachable block waits for a sibling's store before it says
    /// the test staged nothing.
    const BOUND: Duration = Duration::from_secs(10);

    /// The pid a watched spawn is in flight for, plus one; zero while none is.
    static WATCHED: AtomicU64 = AtomicU64::new(0);

    /// `spawn_thread` is about to publish a block for a thread given `arg`.
    pub(crate) fn spawning(arg: u64) {
        if arg == MARK {
            WATCHED.store(crate::process::current_process().0 as u64 + 1, Ordering::SeqCst);
        }
    }

    pub(super) fn hold(frames: &Unpublished, at: UserAddr) {
        // `None` while the kernel spawns init, with no thread running.
        let Some(pid) = crate::arch::percpu::current_pid() else { return };
        let pid = pid.0 as u64 + 1;
        if WATCHED.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return;
        }
        // SAFETY: DTV slot 0 is inside the DTV `build_combined` wrote at the front of `frames`.
        let slot = unsafe { frames.ptr().add(super::DTV_HEADER_SIZE) }.cast::<u64>();
        // SAFETY: as above; a volatile read, since a user store may land there.
        let written = unsafe { slot.read_volatile() };
        // `spawn_thread` publishes into the address space this CPU runs.
        if !crate::mm::paging::present_in_current_tables(at.raw()) {
            log!("tls-rebase-window: pid {} block at {:#x} is not reachable before its rebase", pid - 1, at.raw());
            return;
        }
        let deadline = crate::clock::now() + BOUND;
        // SAFETY: as above.
        while unsafe { slot.read_volatile() } == written {
            assert!(
                crate::clock::now() < deadline,
                "tls-rebase-window: pid {} block at {:#x} was reachable before its rebase and nothing stored into it in {BOUND}",
                pid - 1,
                at.raw()
            );
            // `IF` is clear in a syscall: a sibling's shootdown is answered here.
            crate::arch::tlb::poll();
            core::hint::spin_loop();
        }
        log!("tls-rebase-window: pid {} block at {:#x} was reachable before its rebase, and a store landed in it", pid - 1, at.raw());
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
    let exe = match layout.tls().and_then(TlsSegment::occupied) {
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
    let with_tls = || {
        loaded_libs.iter().filter_map(|lib| Some((lib.tls_template, lib.tls()?.occupied()?)))
    };
    let libs = with_tls().zip(2u64..).map(|((template, tls), id)| {
        let (memsz, align) = (tls.memsz() as usize, tls.align() as usize);
        (template, memsz, memsz, align, id)
    });
    let next_module_id = 2 + with_tls().count() as u64;
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
