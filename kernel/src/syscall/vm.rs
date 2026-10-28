//! The caller's own address space: `vm_*` reserve, commit, protect, discard,
//! resize and take away private memory, `dlopen` maps a library image, `SYS_TLS_ALLOC_BLOCK`
//! maps a thread's per-module TLS block, `SYS_QUERY_MODULES` reports what is
//! loaded. Exhausting address space is an error return, never an `.expect`.
//!
//! **A length from userland is refused before any arithmetic touches it**:
//! rounding it to pages is the checked sum, and one no page count holds is
//! `InvalidArgument`.

use kernel::vm::Prot;

use crate::mm::charge::{self, Charge};
use crate::mm::space::At;
use crate::user_ptr::UserBytesMut;
use crate::UserAddr;
use crate::process;

use toyos_abi::syscall::*;

/// `size` bytes in whole pages, or `None` for none or a size no page count holds.
fn whole_pages(size: u64) -> Option<u64> {
    size.checked_next_multiple_of(crate::mm::PAGE_SIZE).filter(|&bytes| bytes != 0)
}

/// The protection a `VmProt` word names. Executable memory is not yet one a
/// program may ask for: `ReadExec` is `NotSupported`.
fn vm_prot(word: u64) -> Result<Prot, SyscallError> {
    match VmProt::from_u64(word).ok_or(SyscallError::InvalidArgument)? {
        VmProt::None => Ok(Prot::None),
        VmProt::Read => Ok(Prot::Read),
        VmProt::ReadWrite => Ok(Prot::ReadWrite),
        VmProt::ReadExec => Err(SyscallError::NotSupported),
    }
}

/// `[addr, addr + len)` as a page-aligned start and whole pages.
fn range(addr: u64, len: u64) -> Result<(UserAddr, u64), SyscallError> {
    match (UserAddr::checked(addr), whole_pages(len)) {
        (Some(start), Some(len)) if addr.is_multiple_of(crate::mm::PAGE_SIZE) => Ok((start, len)),
        _ => Err(SyscallError::InvalidArgument),
    }
}

fn answer(done: Result<(), SyscallError>) -> u64 {
    done.map_or_else(SyscallError::to_u64, |()| 0)
}

/// Private memory, committed unless `prot` is `None` and populated as it is
/// touched, placed anywhere at a multiple of `place` or, for `VM_AT_FIXED`,
/// exactly at `place` in place of whatever those pages held.
pub(super) fn sys_vm_reserve(len: u64, prot: u64, place: u64, kind: u64) -> u64 {
    let reserved = (|| {
        let prot = vm_prot(prot)?;
        let len = whole_pages(len).ok_or(SyscallError::InvalidArgument)?;
        let at = match kind {
            VM_AT_ANYWHERE => At::Anywhere(place),
            VM_AT_FIXED => At::Fixed(range(place, len)?.0),
            _ => return Err(SyscallError::InvalidArgument),
        };
        process::current_address_space().lock().reserve(len, at, prot)
    })();
    reserved.map_or_else(SyscallError::to_u64, UserAddr::raw)
}

/// Take every page of `[addr, addr + len)` away, as POSIX `munmap` does.
pub(super) fn sys_vm_release(addr: u64, len: u64) -> u64 {
    answer(range(addr, len).and_then(|(start, len)| {
        let space = process::current_address_space();
        let mut space = space.lock();
        space.room(start.raw(), start.raw() + len, 0)?;
        space.release(start, len);
        Ok(())
    }))
}

pub(super) fn sys_vm_protect(addr: u64, len: u64, prot: u64) -> u64 {
    answer((|| {
        let prot = vm_prot(prot)?;
        let (start, len) = range(addr, len)?;
        process::current_address_space().lock().protect(start, len, prot)
    })())
}

pub(super) fn sys_vm_discard(addr: u64, len: u64) -> u64 {
    answer(range(addr, len).and_then(|(start, len)| process::current_address_space().lock().discard(start, len)))
}

pub(super) fn sys_vm_decommit(addr: u64, len: u64) -> u64 {
    answer(range(addr, len).and_then(|(start, len)| process::current_address_space().lock().decommit(start, len)))
}

/// Resize committed private memory in place, or move it when `may_move` is 1.
pub(super) fn sys_vm_remap(addr: u64, old: u64, new: u64, may_move: u64) -> u64 {
    let remapped = (|| {
        let may_move = match may_move {
            0 => false,
            1 => true,
            _ => return Err(SyscallError::InvalidArgument),
        };
        let (start, old) = range(addr, old)?;
        let new = whole_pages(new).ok_or(SyscallError::InvalidArgument)?;
        process::current_address_space().lock().remap(start, old, new, may_move)
    })();
    remapped.map_or_else(SyscallError::to_u64, UserAddr::raw)
}

/// The first `len` bytes of the shared memory object `handle` names, as a
/// program image; `Err` is the syscall's answer.
pub(super) fn shared_image(handle: u64, len: u64) -> Result<crate::file_backing::SharedImage, u64> {
    let Ok(raw) = u32::try_from(handle) else { return Err(SyscallError::InvalidArgument.to_u64()) };
    let object = process::with_process_data(|data| {
        data.handles.get::<crate::object::shm::SharedMemObject>(
            toyos_abi::handle::RawHandle(raw),
            toyos_abi::handle::Rights::MAP,
        )
    })
    .map_err(|e| e.refuse())?;
    crate::file_backing::SharedImage::over(object, len).map_err(|e| e.to_u64())
}

/// Load the library `image` holds into this process as `name` (`DlopenArgs`).
/// The image is the caller's, read by the load and kept by nothing: the module
/// is this process's own copy.
pub(super) fn sys_dlopen(
    ctx: &crate::user_ptr::SyscallContext,
    resolved: alloc::string::String,
    image: &crate::file_backing::SharedImage,
    init_out: Option<UserAddr>,
) -> u64 {
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

    let mut lib = match crate::elf::load_shared_lib(image) {
        Ok(lib) => lib,
        Err(msg) => {
            log!("dlopen: {}: {}", resolved, msg);
            return SyscallError::InvalidArgument.to_u64();
        }
    };

    let pt = process::current_address_space();
    // The module's own program headers decide which pages are writable and
    // which executable; mapping the whole image writable would make it
    // executable too.
    let Some(base) = lib.map_into(&pt) else {
        log!("dlopen: {}: out of virtual address space", resolved);
        return SyscallError::ResourceExhausted.to_u64();
    };
    if base != lib.user_base {
        lib.user_base = base;
        crate::elf::rebase_relative_relocs(&lib);
    }

    // The mapping is committed before the fallible copy-out below; this guard
    // takes it back if the copy-out refuses, so a refused dlopen leaves no
    // library mapped or registered.
    let span = lib.image.size() as u64;
    let mapping = crate::rollback::Rollback::new(move || pt.lock().release(base, span));

    let lib_tls = lib.tls().and_then(toyos_elf::TlsSegment::occupied);
    let data_arc = process::process_data();
    // References no module defines: a count in this load's one record, as a spawn's are.
    let mut unresolved;
    let init_info = {
        let data = data_arc.lock();
        unresolved = crate::elf::resolve_dlopen_relocs(&lib, &data.elf.loaded_libs);

        // Every TLS value is resolved here, before the point of no return: a
        // reference that leaves its module's segment refuses the whole load,
        // and the mapping guard takes the module back down.
        let tls_info = crate::elf::TlsModuleInfo {
            libs: &data.elf.loaded_libs,
            modules: &data.elf.tls_modules,
        };
        let initial_exec = if data.elf.tls.total_memsz() > 0 {
            crate::elf::apply_tpoff_relocs(&lib, 0, data.elf.tls, &tls_info)
        } else {
            Ok(0)
        };
        let general_dynamic = initial_exec.and_then(|left| match lib_tls {
            Some(_) => crate::elf::apply_dtpoff_relocs(&lib, &tls_info).map(|more| left + more),
            None => Ok(left),
        });
        match general_dynamic {
            Ok(left) => unresolved += left,
            Err(refused) => {
                log!("dlopen: {}: {}", resolved, refused.as_str());
                return SyscallError::InvalidArgument.to_u64();
            }
        }

        // init_info layout: [init_array address, init_array count].
        lib.init_array.map_or([0, 0], |array| {
            [(lib.user_base + array.range().start().get()).raw(), array.count()]
        })
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
    // Asked here for the same reason, so a load past the bound, or one whose
    // records the process cannot be charged, is refused having registered
    // nothing; its mapping goes down with the guard.
    if data.elf.loaded_libs.len() >= MAX_LIBRARIES || data.charge.grow(library_heap(&resolved)).is_err() {
        drop(data);
        return SyscallError::ResourceExhausted.to_u64();
    }
    mapping.commit();

    let idx = data.elf.loaded_libs.len();
    // Taken and bumped under the guard that registers, so two names loading at
    // once are two modules: the id a library's `DTPMOD64` relocations carry is
    // no other library's, and `dynamic_tls_blocks` is keyed on it.
    if let Some(lib_tls) = lib_tls {
        let module_id = data.elf.next_tls_module_id;
        data.elf.next_tls_module_id = module_id + 1;
        let tls_info = crate::elf::TlsModuleInfo {
            libs: &data.elf.loaded_libs,
            modules: &data.elf.tls_modules,
        };
        unresolved += crate::elf::apply_dtpmod_relocs(&lib, module_id, &tls_info);
        data.elf.tls_modules.push(crate::elf::TlsModule {
            template: lib.tls_template,
            memsz: lib_tls.memsz() as usize,
            base_offset: 0,
            module_id,
            is_static: false,
        });
    }
    log!("dlopen: {} pid={} unresolved={}", resolved, process::current_process(), unresolved);
    data.elf.lib_paths.push(resolved);
    data.elf.loaded_libs.push(lib);
    idx as u64
}

/// The heap a module's records take in its process, which keeps them for its
/// life: its place in the lists of libraries, paths and TLS modules, each of
/// which doubles, and its path.
fn library_heap(path: &str) -> usize {
    use core::mem::size_of;
    2 * (size_of::<crate::elf::LoadedLib>() + size_of::<alloc::string::String>() + size_of::<crate::elf::TlsModule>())
        + charge::array::<u8>(path.len())
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
        data.elf.dynamic_tls_blocks.get(&(tid, module_id)).map(|(b, _)| b.at)
    });

    let tls_vaddr = match existing {
        Some(vaddr) => vaddr,
        None => {
            let place = Charge::current(charge::map_entry::<(process::Tid, u64), (process::KernelMapping, Charge)>())?;
            let block = crate::object::shm::SharedMemObject::create(&crate::mm::charge::current(), tls_memsz.max(1) as u64)?;
            if let Some(template) = &tls_template {
                // SAFETY: the template is a bounds-checked view of the module's
                // own image, which its `LoadedLib` keeps alive.
                block.write(0, unsafe { template.as_slice() });
            }
            let pt = process::current_address_space();
            let len = block.size();
            let vaddr = pt.lock().map(block.clone(), 0, len, kernel::vm::Prot::ReadWrite, crate::mm::space::At::ANYWHERE)?;
            // A thread asks for its own blocks alone, so none raced this one in.
            process::with_process_data(|data| {
                data.elf.dynamic_tls_blocks.insert((tid, module_id), (process::KernelMapping { object: block, at: vaddr }, place))
            });
            vaddr
        }
    };

    // Through the thread's own TLS object, never by chasing a pointer out of
    // the FS base, which addresses user-writable memory.
    let tls = process::with_current_data(|data| {
        data.tls.as_ref().map(|tls| alloc::sync::Arc::clone(&tls.object)).expect("sys_tls_alloc_block: thread has no TLS block")
    });
    // `module_id` is bounded non-zero and at most `DTV_INITIAL_CAPACITY` at the
    // top of this function, so the slot is inside the DTV after its two
    // header words, which the block's first bytes hold.
    tls.write((2 + (module_id - 1)) * 8, &tls_vaddr.raw().to_ne_bytes());
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

        let (eh_addr, eh_size) = data.elf.exe_eh_frame_hdr;
        let (phdr, phnum) = data.elf.exe_phdrs;
        let exe_info = ModuleInfo {
            base: data.elf.elf_base.raw(),
            text_end: data.elf.exe_vaddr_max,
            eh_frame_hdr: eh_addr,
            eh_frame_hdr_size: eh_size,
            phdr,
            phnum: phnum.into(),
            path_offset,
            path_len: exe_path_bytes.len() as u32,
        };
        out.write_at(0, toyos_abi::usersafe::bytes(&exe_info));
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
                eh_frame_hdr: lib.eh_frame_hdr.map_or(0, |r| (lib.user_base + r.start().get()).raw()),
                eh_frame_hdr_size: lib.eh_frame_hdr.map_or(0, |r| r.len()),
                phdr: (lib.user_base + lib.phdrs.image().start().get()).raw(),
                phnum: lib.phdrs.count().into(),
                path_offset,
                path_len: lib_path_bytes.len() as u32,
            };
            out.write_at((1 + i) * info_size, toyos_abi::usersafe::bytes(&lib_info));
            out.write_at(path_offset as usize, lib_path_bytes);
            path_offset += lib_path_bytes.len() as u32;
        }

        required as u64
    })
}
