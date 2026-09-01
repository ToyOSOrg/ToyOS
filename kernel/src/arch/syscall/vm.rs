//! The caller's own address space: `mmap`/`munmap` place and remove anonymous
//! regions, `dlopen` maps a library image, `SYS_TLS_ALLOC_BLOCK` maps a
//! thread's per-module TLS block, `SYS_QUERY_MODULES` reports what is loaded.
//! Exhausting address space is an error return, never an `.expect`.
//!
//! A removed mapping's `Unmapped` drops outside `with_process_data`: the drop
//! shoots down and waits, and a sibling thread can be spinning on that same
//! lock with `IF` clear.

use crate::mm::paging::{CachePolicy, Occupancy, Prot};
use crate::user_ptr::UserBytesMut;
use crate::UserAddr;
use crate::{log, process, vfs};

use toyos_abi::syscall::*;

/// Map anonymous memory honouring `prot`; `MmapFlags::FIXED` places it at
/// exactly `req_addr`, replacing at most one whole mapping this process made.
pub(super) fn sys_mmap(req_addr: u64, size: u64, prot: MmapProt, flags: MmapFlags) -> u64 {
    // `size` crossed the trust boundary: zero, and a size whose 2 MiB rounding
    // would wrap, are refused rather than silently turned into a small request.
    // No cap beyond that: the PMM's own `free_count` check is the physical limit.
    if size == 0 || (size as usize).checked_add(crate::mm::PAGE_2M as usize - 1).is_none() {
        return SyscallError::InvalidArgument.to_u64();
    }
    let aligned = crate::mm::align_2m(size as usize);
    let fixed = flags.contains(MmapFlags::FIXED);
    // Anonymous memory is never executable: `MmapProt` has no bit for it and
    // there is no `mprotect` to add one later.
    let mapping_prot = if prot.contains(MmapProt::WRITE) { Prot::ReadWrite } else { Prot::Read };

    // A misaligned or kernel-half `req_addr` is refused, not rounded or
    // clamped: `ensure_table` would OR `PAGE_USER` onto the shared kernel PML4
    // entry, opening a user-writable window in every process's page tables.
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
        // Never rounded: `mmap_regions` and `munmap` key on this exact address.
        Some(req_addr)
    } else {
        None
    };

    // Allocate only once the request is known to be satisfiable, so a refused
    // fixed mapping leaks no pages.
    let pages = if prot == MmapProt::NONE {
        // No physical page is pinned behind a reservation whose purpose is to
        // fault: `handle_page_fault` refuses to fill a `Mapped` region.
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
        // the arm below: process data, then address space.
        let replaced = process::with_process_data(|data| {
            let mut as_guard = pt.lock();
            // Only a whole mapping this process itself made may be replaced;
            // a partial overlap is refused — `map_range` would otherwise
            // assert on an already-present PDE.
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
            // `insert_region` never overlaps.
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
            // Dropped here with no lock held: the drop shoots down and waits,
            // and only a replaced mapping owes that wait.
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

/// Frees an anonymous mapping and shoots down every sibling thread's
/// translation for its range.
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
    // Dropped here, outside the closure: the drop shoots down and waits, and a
    // sibling can be spinning on the process-data lock with `IF` clear.
    drop(unmapped);
    0
}

pub(super) fn sys_dlopen(ctx: &crate::user_ptr::SyscallContext, path: &str, init_out: Option<UserAddr>) -> u64 {
    let cwd = process::with_process_data(|d| d.cwd.clone());
    let resolved = vfs::lock().resolve_absolute(&cwd, path);

    // A repeat load of a name this process holds shares that module and maps
    // nothing again — a `dlopen` loop used to grow the address space unbounded.
    // POSIX-shaped: the shared module's initializers do not re-run, so empty init.
    if let Some(idx) =
        process::with_process_data(|d| d.elf.lib_paths.iter().position(|p| *p == resolved))
    {
        if let Some(out) = init_out {
            if ctx.copy_out(out, &[0u64, 0]).is_err() {
                return SyscallError::BadAddress.to_u64();
            }
        }
        return idx as u64;
    }

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

    let pt = process::current_address_space();
    let mapped = process::with_process_data(|_data| {
        // The module's own program headers decide which pages are writable
        // and which executable; mapping the whole image writable would make
        // it executable too.
        let vaddr = lib.map_into(&pt).ok_or(SyscallError::ResourceExhausted)?;
        // A `Shared` module's windows may reuse a range already handed out;
        // `map_window`'s shootdown reached only this CPU, so the rest of the
        // machine is told here.
        if matches!(lib.memory, crate::elf::LibMemory::Shared { .. }) {
            crate::arch::tlb::shootdown(crate::arch::tlb::Origin::Dlopen);
        }
        let delta = vaddr.raw() as i64 - lib.user_base.raw() as i64;
        if delta != 0 {
            crate::elf::rebase_relative_relocs(&lib, delta);
        }
        lib.user_base = vaddr;
        Ok::<UserAddr, SyscallError>(vaddr)
    });
    let base = match mapped {
        Ok(base) => base,
        Err(e) => {
            log!("dlopen: {}: out of virtual address space", resolved);
            return e.to_u64();
        }
    };

    // The mapping is committed before the fallible copy-out below; this guard
    // unwinds it — VA region and its shootdown — if the copy-out refuses, so a
    // refused dlopen leaves no library mapped or registered.
    let mapping = crate::rollback::Rollback::new(move || {
        process::with_process_data(|_data| {
            pt.lock().free_and_unmap(base);
        });
        crate::arch::tlb::shootdown(crate::arch::tlb::Origin::Dlopen);
    });

    let lib_has_tls = lib.tls_memsz > 0;
    let data_arc = process::process_data();
    let (init_info, tls_module) = {
        let data = data_arc.lock();
        crate::elf::resolve_dlopen_relocs(&lib, &data.elf.loaded_libs);

        if data.elf.tls_total_memsz > 0 {
            let tls_info = crate::elf::TlsModuleInfo {
                libs: &data.elf.loaded_libs,
                modules: &data.elf.tls_modules,
            };
            crate::elf::apply_tpoff_relocs(&lib, 0, data.elf.tls_total_memsz, &tls_info);
        }

        // Read here and bumped only at the registration below, so nothing reserves it
        // against a sibling load of another name
        // (`issues/kernel/two-dlopens-of-different-names-share-one-tls-module-id.md`).
        let tls_module = lib_has_tls.then(|| {
            let module_id = data.elf.next_tls_module_id;
            let tls_info = crate::elf::TlsModuleInfo {
                libs: &data.elf.loaded_libs,
                modules: &data.elf.tls_modules,
            };
            crate::elf::apply_dtpmod_relocs(&lib, module_id, &tls_info);
            crate::elf::TlsModule {
                template: lib.tls_template,
                memsz: lib.tls_memsz,
                base_offset: 0,
                module_id,
                is_static: false,
            }
        });

        // init_info layout: [init_array_vaddr, init_array_count], vaddr rebased to user_base.
        let init_info = [
            if lib.init_array_vaddr != 0 { lib.user_base.raw() + lib.init_array_vaddr } else { 0 },
            lib.init_array_size / 8,
        ];
        (init_info, tls_module)
    };

    // The point of no return: copy the init info out first, then register. A
    // refused copy-out registers nothing and the mapping guard rolls back.
    if let Some(out) = init_out {
        if ctx.copy_out(out, &init_info).is_err() {
            return SyscallError::BadAddress.to_u64();
        }
    }

    let mut data = data_arc.lock();
    // Asked again under the guard that registers: the lookup at the top released
    // this lock across the whole load, so a sibling can have registered the name
    // meanwhile. The loser commits nothing and its mapping goes down with the guard.
    if let Some(idx) = data.elf.lib_paths.iter().position(|p| *p == resolved) {
        drop(data);
        // Empty init, the same answer the lookup at the top of the call gives.
        if let Some(out) = init_out {
            if ctx.copy_out(out, &[0u64, 0]).is_err() {
                return SyscallError::BadAddress.to_u64();
            }
        }
        return idx as u64;
    }
    mapping.commit();

    let idx = data.elf.loaded_libs.len();
    if let Some(module) = tls_module {
        data.elf.next_tls_module_id = module.module_id + 1;
        data.elf.tls_modules.push(module);
    }
    data.elf.lib_paths.push(resolved);
    data.elf.loaded_libs.push(lib);
    idx as u64
}

/// Allocates a TLS block for the current thread's DTV entry for `module_id`,
/// returning its virtual address. `module_id` crosses the trust boundary:
/// every rejection is an error return, never a panic.
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
    // Bounded by the kernel's own `DTV_INITIAL_CAPACITY`, never the DTV's own
    // `len` field, which the process can rewrite.
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

    // A DTV entry leaves DTV_UNALLOCATED once and never returns; serving a
    // fresh block on a repeat call would leave the first mapping present,
    // USER and writable, over whatever the PMM hands out next.
    let tid = process::current_tid();
    let existing = process::with_process_data(|data| {
        data.elf.dynamic_tls_blocks.get(&(tid, module_id)).map(|b| b.vaddr())
    });

    let tls_vaddr = match existing {
        Some(vaddr) => vaddr,
        None => {
            let page_alloc = process::PageAlloc::new(tls_memsz.max(1), crate::mm::pmm::Category::Tls)
                .ok_or(SyscallError::ResourceExhausted)?;
            // SAFETY: `page_alloc` is a fresh, unaliased allocation of at
            // least `tls_memsz.max(1)` bytes; `template.size()` comes from the
            // same program header as `m.memsz`; the two regions (a fresh
            // physical page, kernel ELF image data) cannot overlap.
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

    // Found through the thread's own kernel-side TLS allocation, never by
    // chasing a pointer out of the FS base, which addresses user-writable
    // memory. Every thread gets an allocation from `setup_tls`/
    // `setup_combined_tls`; its absence here is a kernel bug.
    process::with_current_data(|data| {
        let tls = data.tls_pages.as_ref().expect("sys_tls_alloc_block: thread has no TLS allocation");
        let dtv_kern = tls.ptr() as *mut u64;
        // SAFETY: `module_id` is bounded non-zero and at most
        // `DTV_INITIAL_CAPACITY` at the top of this function, so
        // `2 + (module_id - 1)` indexes within the DTV's entries after its two
        // header words, in this thread's own allocation.
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

/// Describes every loaded module into `buf`, returning the required byte
/// length; nothing is written unless the whole answer fits, and the length
/// never lands in `SyscallError`'s encoded range.
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

        // Record array ends at the first module's `path_offset`; paths are
        // packed after it in module order.
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
