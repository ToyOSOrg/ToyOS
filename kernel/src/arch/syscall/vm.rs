//! The caller's own address space, and the modules loaded into it.
//!
//! Four things share this file because they share one resource: `mmap` and
//! `munmap` place and remove anonymous regions, `dlopen` maps a library image
//! into the same space, `SYS_TLS_ALLOC_BLOCK` maps a thread's per-module block,
//! and `SYS_QUERY_MODULES` reports what is in there. A process's virtual address
//! space is a resource like any other, so exhausting it is an error return and
//! never an `.expect` in syscall context.
//!
//! **Pages leave a mapping under no lock but their own.** The `Unmapped` a
//! removal produces is dropped outside `with_process_data`, because the drop
//! shoots down and waits and a sibling thread can be spinning on that same lock
//! with `IF` clear.

use crate::mm::paging::{CachePolicy, Occupancy, Prot};
use crate::user_ptr::UserBytesMut;
use crate::UserAddr;
use crate::{log, process, vfs};

use toyos_abi::syscall::*;

/// Map anonymous memory, honouring `prot`.
///
/// A mapping made readable and writable whatever the caller asked for turns
/// `userland/libc`'s translation of POSIX `PROT_NONE` into a writable guard
/// page, and the stack-overflow detection built on it into nothing.
///
/// With 2 MiB pages and no `mprotect`, protection is decided once, here. A
/// mapping without `WRITE` gets a read-only PDE, and `MmapProt::NONE` gets no
/// PDE at all: the range is reserved so nothing else lands in it, no physical
/// memory is pinned behind a page whose purpose is to fault, and
/// `process::handle_page_fault` refuses to fill a `RegionKind::Mapped` region
/// so the reservation cannot be demand-paged back into existence.
///
/// `MmapFlags::FIXED` places the mapping at exactly `req_addr` rather than
/// wherever the placement search would put it, and the range it names is its
/// own to answer for: it may replace exactly one whole mapping this same
/// syscall made, and every other overlap — part of a region, several regions,
/// a range belonging to the loader or a device claim — is refused with
/// `InvalidArgument`. POSIX unmaps whatever is in the way and says nothing;
/// this kernel does not have that silence to give, and the address a C program
/// passes is as untrusted as any other syscall argument.
pub(super) fn sys_mmap(req_addr: u64, size: u64, prot: MmapProt, flags: MmapFlags) -> u64 {
    // `size` crossed the trust boundary. Zero is a request for nothing and a
    // size whose 2 MiB rounding does not fit cannot be expressed at all;
    // neither is an allocation failure, so neither is ResourceExhausted. The
    // rounding must not be allowed to wrap — that would silently turn a huge
    // request into a small one. No policy ceiling is needed above that: the
    // PMM's own `free_count` check is a physical limit.
    if size == 0 || (size as usize).checked_add(crate::mm::PAGE_2M as usize - 1).is_none() {
        return SyscallError::InvalidArgument.to_u64();
    }
    let aligned = crate::mm::align_2m(size as usize);
    let fixed = flags.contains(MmapFlags::FIXED);
    // **Anonymous memory is never executable, and `MmapProt` has no bit that
    // asks for it.** There is no JIT in this system and no `mprotect` to turn
    // a page into code afterwards, so the heap, every guard page and every
    // `MAP_ANONYMOUS` arena a libc hands out are data — which is what makes a
    // stack or heap overflow a fault instead of a foothold.
    let mapping_prot = if prot.contains(MmapProt::WRITE) { Prot::ReadWrite } else { Prot::Read };

    // A fixed mapping bypasses `find_gap`, so it has to respect `find_gap`'s
    // range itself: `PageTables::remap` only asserts 2 MiB alignment, so a
    // kernel-half `req_addr` reaches `ensure_table`, which ORs PAGE_USER onto
    // the *shared* kernel PML4 entry (`new_user` shallow-copies PML4[256..512])
    // and writes a PDE into the shared kernel page directory — a user-writable
    // window visible to the kernel and every other process.
    //
    // A 2 MiB-page kernel cannot honour a finer-grained `req_addr`, and there
    // is nothing to clamp a request to when the granularity itself is what
    // cannot be met, so a misaligned one is refused rather than rounded. That
    // is also what `toyos-abi`'s `mmap` documents, and it keeps `start ==
    // req_addr`, so the address recorded in `mmap_regions` is the one handed
    // back and `munmap` can find it.
    let fixed_start = if fixed && req_addr != 0 {
        let Some(end) = req_addr.checked_add(aligned as u64) else {
            return SyscallError::InvalidArgument.to_u64();
        };
        if req_addr & (crate::mm::PAGE_2M - 1) != 0
            || req_addr < crate::vma::alloc_floor()
            || end > crate::vma::ALLOC_CEILING
            || !toyos_userbound::in_user_half(req_addr, aligned as u64)
        {
            return SyscallError::InvalidArgument.to_u64();
        }
        Some(req_addr)
    } else {
        None
    };

    // Allocate only once the request is known to be satisfiable, so a refused
    // fixed mapping does not leak its pages.
    let pages = if prot == MmapProt::NONE {
        None
    } else {
        match process::PageAlloc::new(aligned, crate::mm::pmm::Category::Mmap) {
            Some(pages) => Some(pages),
            None => return SyscallError::ResourceExhausted.to_u64(),
        }
    };

    if let Some(start) = fixed_start {
        let pt = process::current_address_space();
        let start = UserAddr::new(start);
        // Both ledgers move together, under both locks, in the same order as
        // the arm below: the process data, then the address space.
        let replaced = process::with_process_data(|data| {
            let mut as_guard = pt.lock();
            // A placed mapping names its own range, so the question `find_gap`
            // answers for every other mapping has to be asked here. A mapping
            // that reached `mmap_regions` and not `regions` — which is what
            // the placement search reads — would hand the next anonymous
            // `mmap` the range this one is living in, and `map_range` would
            // assert on a present PDE: three ordinary syscalls from any C
            // program that passes `MAP_FIXED`, and the machine is gone.
            //
            // One whole mapping of this process's own making is replaced — the
            // address keeps its meaning and changes what it names. Every other
            // overlap is refused: taking part of a region would need a split
            // the address space has no machinery for, and a range an ELF
            // segment, a library image, the stack or a shared window owns is
            // not `mmap`'s to take. Neither is honoured halfway, and neither
            // reaches `map_range`, whose assert is a kernel-bug assert again
            // rather than one syscall away.
            let replacing = match as_guard.occupancy(start, aligned as u64) {
                Occupancy::Free => None,
                Occupancy::Whole => {
                    let mine = data
                        .mmap_regions
                        .iter()
                        .position(|r| r.addr == start && r.size == aligned);
                    match mine {
                        Some(idx) => Some(idx),
                        None => return Err(SyscallError::InvalidArgument),
                    }
                }
                Occupancy::Partial => return Err(SyscallError::InvalidArgument),
            };
            // Out of both ledgers before the new mapping goes into either, so
            // `insert_region` is never asked to overlap and the pages of what
            // was there leave with it.
            let old = replacing.map(|idx| {
                let old = data.mmap_regions.swap_remove(idx);
                as_guard
                    .free_and_unmap(old.addr)
                    .expect("an mmap region is registered in the address space it was placed in");
                old
            });
            as_guard.insert_region(
                start,
                crate::vma::Region {
                    size: aligned as u64,
                    kind: crate::vma::RegionKind::Mapped,
                },
            );
            if let Some(pages) = &pages {
                as_guard.map_range(
                    start,
                    pages.phys(),
                    aligned as u64,
                    mapping_prot,
                    CachePolicy::DeferToMtrr,
                );
            }
            data.mmap_regions.push(process::MmapRegion {
                addr: start, size: aligned, _pages: pages,
            });
            data.alloc_count += 1;
            let mem = data.mmap_regions.iter().map(|r| r.size as u64).sum::<u64>();
            if mem > data.peak_memory { data.peak_memory = mem; }
            Ok(old.map(crate::mm::Unmapped::new))
        });
        match replaced {
            // Dropped out here, with nothing held: the drop shoots down and
            // waits, and a replacement is what owes that wait — a sibling
            // thread holds a translation for exactly this range and the pages
            // behind the old mapping are on their way back to the PMM. A
            // mapping placed where nothing was owes none, which is why the arm
            // below shoots down nowhere either.
            Ok(old) => {
                drop(old);
                req_addr
            }
            Err(e) => e.to_u64(),
        }
    } else {
        let pt = process::current_address_space();
        let vaddr = process::with_process_data(|data| {
            let placed = match &pages {
                Some(pages) => pt.lock().alloc_and_map(pages.phys(), aligned as u64, mapping_prot, CachePolicy::DeferToMtrr).map(|(v, _)| v),
                None => pt.lock().alloc_region(aligned as u64, crate::vma::RegionKind::Mapped),
            };
            let Some(vaddr) = placed else { return Err(()) };
            data.mmap_regions.push(process::MmapRegion {
                addr: vaddr, size: aligned, _pages: pages,
            });
            data.alloc_count += 1;
            let mem = data.mmap_regions.iter().map(|r| r.size as u64).sum::<u64>();
            if mem > data.peak_memory { data.peak_memory = mem; }
            Ok(vaddr)
        });
        match vaddr {
            Ok(v) => v.raw(),
            Err(()) => SyscallError::ResourceExhausted.to_u64(),
        }
    }
}

/// The pages go back to the PMM here, so this is the syscall the shootdown
/// matters most on: a sibling thread of the same process holds translations for
/// exactly this range and has to be told.
///
/// One path for every mapping, placed or not — a second free path that cleared
/// page-table entries would leave the mapping registered nowhere.
pub(super) fn sys_munmap(addr: u64, _size: u64) -> u64 {
    let pt = process::current_address_space();
    let taken = process::with_process_data(|data| {
        let idx = data.mmap_regions.iter().position(|r| r.addr.raw() == addr)?;
        let region = data.mmap_regions.swap_remove(idx);
        data.free_count += 1;
        pt.lock()
            .free_and_unmap(region.addr)
            .expect("an mmap region is registered in the address space it was placed in");
        Some(crate::mm::Unmapped::new(region))
    });
    let Some(unmapped) = taken else {
        return SyscallError::NotFound.to_u64();
    };
    // Dropped out here, not inside the closure: the drop shoots down and waits,
    // and the process-data lock the closure holds is one a sibling can be spinning
    // on with `IF` clear.
    drop(unmapped);
    0
}

pub(super) fn sys_dlopen(ctx: &crate::user_ptr::SyscallContext, path: &str, init_out: Option<UserAddr>) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let resolved = vfs::lock().resolve_absolute(&cwd, path);

    let lib = crate::elf::try_clone_cached(&resolved);
    let mut lib = match lib {
        Some(lib) => lib,
        None => {
            let backing = match vfs::lock().open_backing(&resolved) {
                Ok(b) => b,
                Err(e) => {
                    log!("dlopen: {}: {e}", resolved);
                    return e.to_u64();
                }
            };

            let (lib, rw_offset, rw_size) = match crate::elf::load_shared_lib(backing.as_ref()) {
                Ok(result) => result,
                Err(msg) => {
                    log!("dlopen: {}", msg);
                    return SyscallError::Unknown.to_u64();
                }
            };

            crate::elf::cache_loaded_lib(&resolved, lib, rw_offset, rw_size)
        }
    };

    // A process's virtual address space is a resource like any other, and
    // `SYS_DLOPEN` neither dedups a path nor frees anything on `SYS_DLCLOSE`,
    // so exhausting it is a loop any process can write. Exhaustion is an error
    // return, not an `.expect` in syscall context.
    let pt = process::current_address_space();
    let mapped = process::with_process_data(|_data| {
        // One `map_into` for both ownership modes, and the module's own program
        // headers decide which of its pages may be written and which may be
        // executed. An arm that mapped the whole image writable would make
        // every library in every process writable *and* executable.
        let Some(vaddr) = lib.map_into(&pt) else {
            return Err(SyscallError::ResourceExhausted);
        };
        // A `Shared` module's windows are written over a range this address
        // space may already have handed out and reused, and a sibling thread
        // can be running in it: what `map_window` discharged reaches this CPU
        // only, and the rest of the machine is told here.
        if matches!(lib.memory, crate::elf::LibMemory::Shared { .. }) {
            crate::arch::tlb::shootdown();
        }
        let delta = vaddr.raw() as i64 - lib.user_base.raw() as i64;
        if delta != 0 {
            crate::elf::rebase_relative_relocs(&lib, delta);
        }
        lib.user_base = vaddr;
        Ok(())
    });
    if let Err(e) = mapped {
        log!("dlopen: {}: out of virtual address space", resolved);
        return e.to_u64();
    }

    let lib_has_tls = lib.tls_memsz > 0;

    let data_arc = process::process_data();
    {
        let mut data = data_arc.lock();
        crate::elf::resolve_dlopen_relocs(&lib, &data.elf.loaded_libs);

        // Apply TPOFF relocs for cross-module IE references (symbols from static-linked modules
        // like std/core whose TLS lives in the static block with known TP-relative offsets).
        if data.elf.tls_total_memsz > 0 {
            let tls_info = crate::elf::TlsModuleInfo {
                libs: &data.elf.loaded_libs,
                modules: &data.elf.tls_modules,
            };
            crate::elf::apply_tpoff_relocs(&lib, 0, data.elf.tls_total_memsz, &tls_info);
        }

        if lib_has_tls {
            let module_id = data.elf.next_tls_module_id;
            data.elf.next_tls_module_id += 1;
            data.elf.tls_modules.push(crate::elf::TlsModule {
                template: lib.tls_template,
                memsz: lib.tls_memsz, base_offset: 0, module_id,
                is_static: false,
            });
            // Apply DTPMOD64/DTPOFF64: write module_id + per-symbol offset into GOT slot pairs.
            // For cross-module GD TLS (r_sym != 0, symbol undefined), resolve to the
            // defining module's ID and TLS offset. DTV entries are left DTV_UNALLOCATED;
            // __tls_get_addr allocates on first access.
            let tls_info = crate::elf::TlsModuleInfo {
                libs: &data.elf.loaded_libs,
                modules: &data.elf.tls_modules,
            };
            crate::elf::apply_dtpmod_relocs(&lib, module_id, &tls_info);
        }
    }

    // Format: [init_array_vaddr: u64, init_array_count: u64], the vaddr rebased
    // to the library's user_base.
    let init_info = [
        if lib.init_array_vaddr != 0 { lib.user_base.raw() + lib.init_array_vaddr } else { 0 },
        lib.init_array_size / 8,
    ];

    let idx = {
        let mut data = data_arc.lock();
        let idx = data.elf.loaded_libs.len();
        data.elf.lib_paths.push(resolved);
        data.elf.loaded_libs.push(lib);
        idx
    };

    // After the library is registered, because it is mapped either way: a
    // failure here is the caller losing its handle, not the address space
    // losing track of a mapping.
    if let Some(out) = init_out {
        if ctx.copy_out(out, &init_info).is_err() {
            return SyscallError::BadAddress.to_u64();
        }
    }
    idx as u64
}

/// Allocate a TLS block for the current thread's DTV entry for `module_id`.
/// Called by __tls_get_addr's slow path when the DTV entry is DTV_UNALLOCATED.
/// Returns the block's virtual address, also written into the DTV.
///
/// `module_id` crosses the trust boundary: every rejection here is an error
/// return, never a panic.
///
/// The DTV is found through the thread's own kernel-side TLS allocation, never
/// by chasing a pointer out of the FS base: CR4.FSGSBASE is on, so userland
/// owns that register, and a raw `AddressSpace::translate` of TCB[8] applies no
/// user-half check and resolves kernel addresses through the direct map
/// shallow-copied into every user PML4.
pub(super) fn sys_tls_alloc_block(module_id: u64) -> u64 {
    match tls_alloc_block(module_id) {
        Ok(vaddr) => vaddr,
        Err(e) => e.to_u64(),
    }
}

fn tls_alloc_block(module_id: u64) -> Result<u64, SyscallError> {
    // The valid set is the process's own module list, which the kernel built.
    if module_id == 0 {
        return Err(SyscallError::InvalidArgument);
    }
    // The DTV is a fixed-capacity array the kernel wrote; a module past its
    // end has nowhere to be recorded. Bounded by the kernel's own constant,
    // never by the `len` field in the DTV, which the process can rewrite.
    if module_id > crate::loader::DTV_INITIAL_CAPACITY as u64 {
        return Err(SyscallError::ResourceExhausted);
    }

    let owner_arc = process::process_data();
    let (tls_memsz, tls_template) = {
        let data = owner_arc.lock();
        let m = data.elf.tls_modules.iter().find(|m| m.module_id == module_id)
            .ok_or(SyscallError::InvalidArgument)?;
        (m.memsz, m.template)
    };

    // A DTV entry leaves DTV_UNALLOCATED once and never returns, so a repeat
    // call for the same (thread, module) is the same block asked for twice.
    // Serving a fresh one frees pages userland still points into while the
    // first mapping stays present, USER and writable, over whatever the PMM
    // hands out next.
    let tid = process::current_tid();
    let existing = process::with_process_data(|data| {
        data.elf.dynamic_tls_blocks.get(&(tid, module_id)).map(|b| b.vaddr())
    });

    let tls_vaddr = match existing {
        Some(vaddr) => vaddr,
        None => {
            let page_alloc = process::PageAlloc::new(tls_memsz.max(1), crate::mm::pmm::Category::Tls)
                .ok_or(SyscallError::ResourceExhausted)?;
            // SAFETY: `page_alloc` is a fresh `PageAlloc` of at least
            // `tls_memsz.max(1)` bytes that nothing else has a pointer to yet —
            // it is mapped into the process below, not above. `template` is the
            // module's TLS image out of the loaded ELF, live for as long as the
            // module is, and `template.size()` is its own length, which
            // `elf::tls_modules` derives from the same program header as
            // `m.memsz`. The two regions are a fresh physical page and kernel
            // image data, so they cannot overlap.
            //
            // Irreducible only for want of a bounded window over `PageAlloc`:
            // the length checked here is the *source's*, and nothing types the
            // destination's — the root-file sweep filed exactly that
            // (`issues/kernel/pagealloc-has-no-checked-window.md`), and this is a
            // third site of the same shape.
            unsafe {
                if let Some(template) = &tls_template {
                    core::ptr::copy_nonoverlapping(template.base(), page_alloc.ptr(), template.size());
                }
            }

            let block_phys = page_alloc.phys();
            let pt = process::current_address_space();
            process::with_process_data(|data| {
                let (vaddr, _) = process::vma_map(&pt, block_phys, page_alloc.size() as u64, Prot::ReadWrite)
                    .ok_or(SyscallError::ResourceExhausted)?;
                data.alloc_count += 1;
                data.elf.dynamic_tls_blocks
                    .insert((tid, module_id), process::MappedPages::new(vaddr, page_alloc));
                Ok(vaddr)
            })?
        }
    };

    // The DTV lives at offset 0 of the thread's own TLS allocation. Every user
    // thread gets one from `setup_tls`/`setup_combined_tls`, so its absence is
    // a kernel bug.
    process::with_current_data(|data| {
        let tls = data.tls_pages.as_ref().expect("sys_tls_alloc_block: thread has no TLS allocation");
        let dtv_kern = tls.ptr() as *mut u64;
        // SAFETY: `module_id` crossed the trust boundary and is bounded at the
        // top of `tls_alloc_block` — non-zero and at most
        // `loader::DTV_INITIAL_CAPACITY`, checked against the kernel's own
        // constant and never against the `len` word in the DTV, which the
        // process can rewrite. `loader` lays the DTV out at offset 0 of the
        // thread's kernel-side TLS allocation with `DTV_INITIAL_CAPACITY` entries
        // after the two header words, so `2 + (module_id - 1)` is in bounds. The
        // allocation is this thread's own and this thread is the one running.
        //
        // **The bound and the write are fifty lines and one function apart**,
        // which is the same missing type as the `copy_nonoverlapping` above:
        // nothing here would notice the check moving.
        unsafe { *dtv_kern.add(2 + (module_id - 1) as usize) = tls_vaddr.raw(); }
    });
    Ok(tls_vaddr.raw())
}

pub(super) fn sys_dlsym(handle: u64, name: &str) -> u64 {
    let data_arc = process::process_data();
    let data = data_arc.lock();
    let idx = handle as usize;
    if idx >= data.elf.loaded_libs.len() {
        return SyscallError::NotFound.to_u64();
    }
    match crate::elf::dlsym(&data.elf.loaded_libs[idx], name) {
        Some(addr) => addr.raw(),
        None => u64::MAX,
    }
}

/// Describe every loaded module into `buf`; return the length it *needs*.
///
/// Same contract as `sys_getcwd` and `sys_readdir`, and for the same reason: a
/// bare `InvalidArgument` leaves a caller no way to size a retry, because
/// `SyscallError` cannot carry the length it would need.
///
/// The answer is a byte length and never a module count: the records carry
/// packed path strings, so a count cannot size the buffer. Nothing is written
/// unless all of it fits, which makes an empty buffer a size query.
///
/// The record array is `buf[..records[0].path_offset]` — every module writes
/// its path after the last record, so the first module's `path_offset` is
/// where the array ends.
///
/// Every module holds address space for as long as it is loaded, so the count
/// is bounded by the process's own arena and the required length stays far
/// below the range `SyscallError` encodes — it can never be misread as one.
pub(super) fn sys_query_modules(out: &mut UserBytesMut) -> u64 {
    use toyos_abi::syscall::ModuleInfo;
    let info_size = core::mem::size_of::<ModuleInfo>();

    process::with_process_data(|data| {
        let module_count = 1 + data.elf.loaded_libs.len();

        let exe_path_bytes = data.exe_path.as_bytes();
        let total_path_bytes: usize = exe_path_bytes.len()
            + data.elf.lib_paths.iter().map(|p| p.len()).sum::<usize>();

        let required = module_count * info_size + total_path_bytes;
        if out.len() < required {
            return required as u64;
        }

        let mut path_offset = (module_count * info_size) as u32;

        let (eh_vaddr, eh_size) = (data.elf.exe_eh_frame_hdr_vaddr, data.elf.exe_eh_frame_hdr_size);
        let exe_info = ModuleInfo {
            base: data.elf.elf_base.raw(),
            text_end: data.elf.exe_vaddr_max,
            eh_frame_hdr: if eh_vaddr != 0 { data.elf.elf_base.raw() + eh_vaddr } else { 0 },
            eh_frame_hdr_size: eh_size,
            path_offset,
            path_len: exe_path_bytes.len() as u32,
        };
        out.write_at(0, exe_info.as_bytes());
        out.write_at(path_offset as usize, exe_path_bytes);
        path_offset += exe_path_bytes.len() as u32;

        for (i, lib) in data.elf.loaded_libs.iter().enumerate() {
            let lib_path_bytes = if i < data.elf.lib_paths.len() {
                data.elf.lib_paths[i].as_bytes()
            } else {
                b""
            };
            let lib_info = ModuleInfo {
                base: lib.user_base.raw(),
                text_end: lib.user_end(),
                eh_frame_hdr: if lib.eh_frame_hdr_vaddr != 0 {
                    lib.user_base.raw() + lib.eh_frame_hdr_vaddr
                } else { 0 },
                eh_frame_hdr_size: lib.eh_frame_hdr_size,
                path_offset,
                path_len: lib_path_bytes.len() as u32,
            };
            out.write_at((1 + i) * info_size, lib_info.as_bytes());
            out.write_at(path_offset as usize, lib_path_bytes);
            path_offset += lib_path_bytes.len() as u32;
        }

        required as u64
    })
}
