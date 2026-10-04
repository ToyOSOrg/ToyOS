//! Building a process out of an ELF file.
//!
//! Segment contents are demand-paged through a relocation index the fault
//! handler applies per page; shared libraries are the exception and are
//! mapped eagerly since they are shared between processes.
//!
//! Every number the file names is untrusted: a refusal is
//! `SyscallError::{InvalidArgument, ResourceExhausted}`, never a panic.

// `warn`, not `deny`: the rest of the kernel is not yet swept for undocumented unsafe blocks.
#![warn(clippy::undocumented_unsafe_blocks)]

mod start;
mod symbols;
mod tls;

pub use start::{build_child_handles, PendingHandles, SLOT_PAIR_LEN};
pub(crate) use start::alloc_kernel_stack;
pub(crate) use crate::arch::entry::{kernel_start, process_start, thread_start};
pub use tls::{TlsBlock, DTV_INITIAL_CAPACITY, VARIANT as TLS_VARIANT};

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::elf;
use crate::object::{ops, HandleTable, KObjectRef};
use crate::mm::policy::{CachePolicy, Prot};
use crate::mm::{PAGE_2M, PAGE_BYTES};
use crate::process::{
    Admission, ElfInfo, Endowments, OwnedAlloc, PageAlloc, PageFaultTrace, PageTables, Parent,
    Pid, ProcessAccounting, ProcessData, ProcessEntry, ThreadData, ThreadEntry, UserStack,
    PROCESS_TABLE,
};
use crate::sync::Lock;
use crate::{scheduler, vfs, UserAddr};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::SyscallError;
use toyos_elf::section::SectionTable;
use toyos_elf::sym::{self, SymTab};
use toyos_elf::rela::{FillLattice, Rules, FILL_GRANULE};
use toyos_elf::{Layout, RelocError, TlsSegment};

const USER_STACK_SIZE: usize = 4 * PAGE_2M as usize; // 8 MB

/// User virtual address space starts at 1 TB, above any direct-mapped physical RAM.
const USER_VM_BASE: u64 = 0x100_0000_0000;

/// The most of a `.gnu.hash` table the loader will read off an executable.
///
/// A table exceeding this yields no symbol count at all — the loader falls
/// back to the `DT_SYMTAB`/`DT_STRTAB` gap rather than a short count nothing
/// downstream could tell from a real one.
const MAX_GNU_HASH_BYTES: u64 = 64 * 1024;

/// Read a byte range from a file through the page cache.
///
/// Returns only the part of the request the file actually holds — callers
/// treat a short result as truncated and length-check before indexing; `len`
/// comes from untrusted ELF fields, so the read is clamped rather than sized
/// by it.
pub(crate) fn read_file_range(
    backing: &dyn crate::file_backing::FileBacking,
    offset: u64,
    len: usize,
) -> Vec<u8> {
    let available = backing.file_size().saturating_sub(offset);
    let len = len.min(available as usize);
    let mut result = Vec::with_capacity(len);
    let mut remaining = len;
    let mut file_off = offset;
    let mut page_buf = [0u8; PAGE_BYTES];

    while remaining > 0 {
        let off_in_block = (file_off % 4096) as usize;
        let chunk = (4096 - off_in_block).min(remaining);

        // A page the store refuses ends the read here rather than filling zeros.
        if backing.read_page(file_off - off_in_block as u64, &mut page_buf).is_err() {
            break;
        }
        result.extend_from_slice(&page_buf[off_in_block..off_in_block + chunk]);

        file_off += chunk as u64;
        remaining -= chunk;
    }

    result
}

/// [`read_file_range`] for a length the ELF declared.
///
/// `None` above [`crate::mm::MAX_HEAP_ALLOC`]: past that the allocation would
/// assert in the heap's page source rather than fail, so this refuses instead
/// of clamping to a table nothing downstream could tell was short.
fn read_elf_table(
    backing: &dyn crate::file_backing::FileBacking,
    offset: u64,
    len: usize,
) -> Option<Vec<u8>> {
    if len > crate::mm::MAX_HEAP_ALLOC {
        return None;
    }
    Some(read_file_range(backing, offset, len))
}

/// Insert one demand-paged region per `PT_LOAD` segment.
///
/// `Err` when two segments would share a page: page-rounded regions at one
/// address would trip `insert_region`'s assert. Checked before the first
/// insert, so a refusal leaves the address space untouched.
fn insert_elf_regions(
    addr_space: &mut crate::mm::paging::AddressSpace,
    layout: &Layout,
    image_start: UserAddr,
    backing: &Arc<dyn crate::file_backing::FileBacking>,
) -> Result<(), SyscallError> {
    use crate::vma::{Region, RegionKind};

    if let Some((a, b)) = layout.overlapping_load_pages(4096) {
        log!("spawn: PT_LOAD segments {} and {} contend for a page", a, b);
        return Err(SyscallError::InvalidArgument);
    }

    for seg in layout.segments() {
        let (lo, hi) = seg.page_range(4096);
        let (seg_start, seg_end) = ((image_start + lo).raw(), (image_start + hi).raw());
        // A zero-size region would sit in the map where `find_region` can't see past it.
        if seg_end == seg_start {
            continue;
        }
        let prot = segment_prot(seg);

        let file_block_start = seg.file_offset() / 4096;
        let file_blocks_needed = (seg.filesz() + (seg.file_offset() % 4096)).div_ceil(4096);
        let file_backed_end = seg_start + file_blocks_needed * 4096;

        if file_blocks_needed > 0 {
            addr_space.insert_region(
                UserAddr::new(seg_start),
                Region {
                    size: file_backed_end.min(seg_end) - seg_start,
                    kind: RegionKind::FileBacked {
                        backing: Arc::clone(backing),
                        file_offset: file_block_start * 4096,
                        file_size: seg.filesz() + (seg.file_offset() % 4096),
                        prot,
                    },
                },
            );
        }

        if file_backed_end < seg_end {
            let anon_start = file_backed_end.max(seg_start);
            addr_space.insert_region(
                UserAddr::new(anon_start),
                Region {
                    size: seg_end - anon_start,
                    kind: RegionKind::Anonymous { prot },
                },
            );
        }
    }
    Ok(())
}

/// What one `PT_LOAD` segment's pages may be used for.
///
/// `PF_W | PF_X` refuses the write, not the execution: taking `X` away would
/// let a hostile ELF run as data instead.
fn segment_prot(seg: &toyos_elf::Segment) -> crate::mm::policy::Prot {
    use crate::mm::policy::Prot;
    if seg.flags().executable() {
        Prot::ReadExec
    } else if seg.flags().writable() {
        Prot::ReadWrite
    } else {
        Prot::Read
    }
}

/// Everything the executable's `PT_DYNAMIC` names, resolved to file offsets
/// and read.
struct ExeTables {
    /// `DT_NEEDED` values, as offsets into `dynstr`.
    needed: Vec<u64>,
    dynstr: Vec<u8>,
    dynsym: Vec<u8>,
    relas: elf::ParsedRelaEntries,
}

impl ExeTables {
    fn symbols(&self) -> SymTab<'_> {
        SymTab::new(&self.dynsym, &self.dynstr)
    }
}

/// Turn a `DT_*` vaddr into a file offset, or refuse the binary.
///
/// `Err` when the vaddr lies below every `PT_LOAD`: there is no file offset for it.
fn file_off(layout: &Layout, path: &str, what: &str, vaddr: u64) -> Result<u64, SyscallError> {
    match layout.vaddr_to_file_offset(vaddr) {
        Some(off) => Ok(off),
        None => {
            log!("spawn: {}: {} vaddr {:#x} is in or near no PT_LOAD segment", path, what, vaddr);
            Err(SyscallError::InvalidArgument)
        }
    }
}

/// Read a table whose length the file declared, or refuse the binary.
///
/// `Err` above one kernel allocation, where the `Vec` would instead panic in
/// the heap's page source.
fn table(
    backing: &dyn crate::file_backing::FileBacking,
    path: &str,
    what: &str,
    off: u64,
    len: usize,
) -> Result<Vec<u8>, SyscallError> {
    match read_elf_table(backing, off, len) {
        Some(v) => Ok(v),
        None => {
            log!("spawn: {}: {} declares {} bytes, past one kernel allocation", path, what, len);
            Err(SyscallError::ResourceExhausted)
        }
    }
}

fn read_exe_tables(
    backing: &dyn crate::file_backing::FileBacking,
    layout: &Layout,
    path: &str,
) -> Result<ExeTables, SyscallError> {
    let (dyn_info, needed) = match layout.dynamic() {
        Some(dynamic) => {
            let data = table(backing, path, "PT_DYNAMIC", dynamic.file_offset(), dynamic.image().len() as usize)?;
            let mut needed = Vec::new();
            needed.reserve_exact(data.len() / toyos_elf::dynamic::ENTRY_SIZE);
            needed.extend(toyos_elf::Dynamic::needed(&data));
            (toyos_elf::Dynamic::parse(&data), needed)
        }
        None => (toyos_elf::Dynamic::default(), Vec::new()),
    };

    let rela_data = match dyn_info.rela {
        Some(t) => {
            let off = file_off(layout, path, "DT_RELA", t.vaddr)?;
            table(backing, path, "DT_RELASZ", off, t.size as usize)?
        }
        // No PT_DYNAMIC: `.rela.dyn` is found through section headers by shape, not name.
        None if layout.dynamic().is_none() => rela_dyn_from_sections(backing, layout, path)?,
        None => Vec::new(),
    };
    let jmprel_data = match dyn_info.jmprel {
        Some(t) => {
            let off = file_off(layout, path, "DT_JMPREL", t.vaddr)?;
            table(backing, path, "DT_PLTRELSZ", off, t.size as usize)?
        }
        None => Vec::new(),
    };
    let symtab_file_off = match dyn_info.symtab {
        Some(vaddr) => Some(file_off(layout, path, "DT_SYMTAB", vaddr)?),
        None => None,
    };
    let dynstr = match dyn_info.strtab_table() {
        Some(t) => {
            let off = file_off(layout, path, "DT_STRTAB", t.vaddr)?;
            table(backing, path, "DT_STRSZ", off, t.size as usize)?
        }
        None => Vec::new(),
    };
    let dynsym = match (symtab_file_off, exe_sym_count(backing, layout, &dyn_info, path)?) {
        (Some(off), n) if n > 0 => table(backing, path, "symbol count", off, n * sym::ENTRY_SIZE)?,
        _ => Vec::new(),
    };

    // Parsed like a library's but for the window and the fill page: the
    // executable's writes land anywhere in its own image, one demand-fault page
    // at a time, so a crossing write is refused, not silently dropped.
    let extent = layout.extent();
    let rules = Rules {
        extent,
        window: (extent.min(), extent.max()),
        fill: Some(FillLattice { base: extent.min(), granule: FILL_GRANULE }),
        tls: layout.tls(),
    };
    let relas = elf::parse_rela_entries(&rela_data, &jmprel_data, &rules, SymTab::new(&dynsym, &dynstr)).map_err(|refused| {
        log!("spawn: {}: {}", path, refused.as_str());
        refused.error()
    })?;

    Ok(ExeTables { needed, dynstr, dynsym, relas })
}

/// `.dynsym`'s entry count, by the rule `Dynamic::sym_count` states.
fn exe_sym_count(
    backing: &dyn crate::file_backing::FileBacking,
    layout: &Layout,
    dyn_info: &toyos_elf::Dynamic,
    path: &str,
) -> Result<usize, SyscallError> {
    let gnu_hash = match dyn_info.gnu_hash {
        Some(vaddr) => {
            let off = file_off(layout, path, "DT_GNU_HASH", vaddr)?;
            let len = layout.file_bytes_from(vaddr).unwrap_or(0).min(MAX_GNU_HASH_BYTES) as usize;
            Some(read_file_range(backing, off, len))
        }
        None => None,
    };
    Ok(dyn_info.sym_count(gnu_hash.as_deref()).unwrap_or_else(|gap| {
        log!("spawn: {}: .gnu.hash does not describe a symbol count in its {} bytes", path, gnu_hash.as_ref().map_or(0, |t| t.len()));
        gap
    }))
}

/// `.rela.dyn` located through section headers, for a file with no `PT_DYNAMIC`.
fn rela_dyn_from_sections(
    backing: &dyn crate::file_backing::FileBacking,
    layout: &Layout,
    path: &str,
) -> Result<Vec<u8>, SyscallError> {
    let Some(sections) = layout.section_headers() else {
        return Ok(Vec::new());
    };
    let shdrs = table(backing, path, "e_shnum", sections.file_offset, sections.byte_len())?;
    let mut first = |off: u64| {
        let head = read_file_range(backing, off, toyos_elf::rela::ENTRY_SIZE);
        toyos_elf::RelaTable::new(&head, crate::arch::ELF_MACHINE).get(0)
    };
    match SectionTable::new(&shdrs).rela_dyn(&mut first) {
        Some((off, size)) => table(backing, path, "SHT_RELA sh_size", off, size as usize),
        None => Ok(Vec::new()),
    }
}

/// Load a program and place its main thread under `parent`, answering its pid
/// and what `commit` left its caller holding of it. `commit` builds the
/// child's handle table around the child's handle to itself, once nothing is
/// left to refuse.
///
/// `image` is the program's bytes when the caller read them itself, and then
/// `argv[0]` is only its name: nothing opens it, and its libraries come from
/// `/system/lib` alone, since the directory it names is no volume of this
/// kernel's.
///
/// `Refusal`, not `-> !`, is the error type: every failure below owns a
/// partly built process (address space, stack, kernel stack), and nothing
/// unwinds, so the error must travel out as a value rather than strand it.
pub fn spawn<H>(
    argv: &[&str],
    commit: impl FnOnce(crate::object::HandleEntry) -> Result<(HandleTable, Endowments, H), crate::object::Refusal>,
    cwd: String,
    env: Vec<u8>,
    image: Option<Arc<dyn crate::file_backing::FileBacking>>,
    parent: Parent,
) -> Result<(Pid, H), crate::object::Refusal> {
    // An argv of only separators survives sys_spawn's split as an empty slice.
    let Some(&path) = argv.first() else {
        return Err(SyscallError::InvalidArgument.into());
    };
    // Before anything is built; dropped on every way out below but the insert.
    let admission = Admission::ask(parent)?;
    let t0 = crate::clock::nanos_since_boot();

    let from_image = image.is_some();
    let backing: Arc<dyn crate::file_backing::FileBacking> = match image {
        Some(image) => image,
        None => {
            // Scoped, not held across the match: dropping `commit` on any `return` here takes the VFS lock.
            let opened = vfs::lock().open_backing(path);
            match opened {
                Ok(b) => b,
                Err(e) => {
                    log!("spawn: {}: {e}", path);
                    return Err(e.into());
                }
            }
        }
    };

    let header_size = 4096.min(backing.file_size() as usize);
    let header_data = read_file_range(backing.as_ref(), 0, header_size);
    let layout = match elf::parse_layout(&header_data) {
        Ok(l) => l,
        Err(msg) => {
            log!("spawn: {}: {}", path, msg);
            return Err(SyscallError::InvalidArgument.into());
        }
    };

    // The rebase base is the file's numbers, so `rebase_base` refuses a vaddr_min
    // that underflows the subtraction or a span that leaves the user half.
    let extent = layout.extent();
    let Some(base) = toyos_userbound::rebase_base(USER_VM_BASE, extent.min(), layout.span())
    else {
        log!("spawn: {}: image at vaddr_min {:#x} spanning {:#x} cannot rebase to {:#x}",
            path, extent.min(), layout.span(), USER_VM_BASE);
        return Err(SyscallError::InvalidArgument.into());
    };
    // Where the image's first byte lands: every `ImageOffset` the file's
    // numbers were parsed into is added to it, and its span fits above it.
    let image_start = UserAddr::new(USER_VM_BASE);
    // `SYS_QUERY_MODULES` answers with the mapped table, so an image without
    // one has no true answer to give.
    let Some(phdrs) = layout.program_headers() else {
        log!("spawn: {}: no PT_LOAD maps the program header table", path);
        return Err(SyscallError::InvalidArgument.into());
    };

    let exe = read_exe_tables(backing.as_ref(), &layout, path)?;
    let t1 = crate::clock::nanos_since_boot();

    // Reserved from the counts, not grown: these are exact upper bounds on `add_u64` calls.
    let u64_writes =
        exe.relas.relative.len() + exe.relas.glob_dat.len() + exe.relas.tpoff64.len();
    let Some(mut reloc_index) =
        elf::RelocationIndex::with_capacity(u64_writes, exe.relas.tpoff32.len())
    else {
        log!("spawn: {}: {} relocations do not fit one index", path, u64_writes);
        return Err(SyscallError::ResourceExhausted.into());
    };
    for &(r_offset, target) in &exe.relas.relative {
        reloc_index.add_u64(r_offset, (image_start + target.get()).raw());
    }

    let t2 = crate::clock::nanos_since_boot();
    let mut loaded_libs = load_needed_libs(&exe, path, from_image)?;
    let t_deps = crate::clock::nanos_since_boot();

    // ELF segments are demand-faulted; the address space starts with the clock page alone.
    let Some(mut space) = crate::mm::paging::AddressSpace::new_user() else {
        log!("spawn: {}: no user PCID free — too many live address spaces", path);
        return Err(SyscallError::ResourceExhausted.into());
    };
    crate::clock::map_page(&mut space);
    let child_pt: PageTables = Arc::new(Lock::new(space));
    insert_elf_regions(&mut child_pt.lock(), &layout, image_start, &backing)?;

    // Libraries get user addresses before any relocation is written: RELATIVE
    // and GLOB_DAT compute a GOT value as `user_base + addend`/`st_value`.
    map_libs(&child_pt, &mut loaded_libs, path)?;
    for lib in &loaded_libs.libs {
        if lib.user_base.raw() != lib.phys_base {
            elf::rebase_relative_relocs(lib);
        }
    }

    if !loaded_libs.libs.is_empty() {
        // A PIE without `--export-dynamic` exports nothing through `.dynsym`;
        // read `.symtab` only when that lookup came back empty.
        let fallback = exe
            .symbols()
            .defined()
            .next()
            .is_none()
            .then(|| symbols::read_symtab(backing.as_ref(), &layout))
            .flatten();
        let exe_sym_map = match &fallback {
            Some((syms, strs)) => {
                symbols::static_map(&SymTab::new(syms, strs), image_start, extent)
            }
            None => symbols::dynamic_map(&exe.symbols(), image_start, extent),
        };
        let exe_sym_map = exe_sym_map.map_err(|refused| {
            log!("spawn: {}: {}", path, refused.as_str());
            SyscallError::InvalidArgument
        })?;
        log!("dynamic: {} exe symbols available to libraries", exe_sym_map.len());
        for lib in &loaded_libs.libs {
            elf::resolve_lib_bind_relocs(lib, &exe_sym_map, &loaded_libs.libs);
        }

        for &(r_offset, r_sym) in &exe.relas.glob_dat {
            if r_sym.get() == 0 {
                continue;
            }
            let name = elf::relocated_symbol(exe.symbols(), r_sym).name_in(&exe.dynstr);
            match loaded_libs.libs.iter().find_map(|lib| lib.resolve(name)) {
                Some(addr) => reloc_index.add_u64(r_offset, addr.raw()),
                None => log!("dynamic: unresolved exe symbol: {}", name),
            }
        }
    }

    // Mapped eagerly, not demand-paged: every process touches the stack immediately.
    let stack_pages = match PageAlloc::new(USER_STACK_SIZE, crate::mm::pmm::Category::Stack) {
        Some(a) => a,
        None => {
            log!("spawn: {}: failed to allocate user stack ({} bytes)", path, USER_STACK_SIZE);
            return Err(SyscallError::ResourceExhausted.into());
        }
    };
    let stack_vaddr = UserAddr::new(crate::vma::STACK_BASE);
    // `USER_STACK_SIZE` is named once, at the `PageAlloc::new` above; argv writes bound against the actual allocation.
    let user_stack = UserStack::new(stack_vaddr, stack_pages.window());
    {
        let mut pt = child_pt.lock();
        // `Prot::ReadWrite`, never executable: a fixed-address W+X stack is the
        // shape stack-smashing payloads target.
        pt.map_range(stack_vaddr, stack_pages.phys(), USER_STACK_SIZE as u64,
            Prot::ReadWrite, CachePolicy::Normal);
        pt.insert_region(stack_vaddr, crate::vma::Region {
            size: USER_STACK_SIZE as u64,
            kind: crate::vma::RegionKind::Anonymous { prot: Prot::ReadWrite },
        });
    }

    let exe_tls_template = match layout.tls().and_then(TlsSegment::occupied) {
        Some(tls) => {
            let Some(tls_file_off) = layout.file_offset_of(tls.template().start()) else {
                log!("spawn: {}: PT_TLS is in or near no PT_LOAD segment", path);
                return Err(SyscallError::InvalidArgument.into());
            };
            // Read directly into the `memsz`-sized buffer: `OwnedAlloc` zeroes
            // (no second pass for `.tbss`) and refuses a size past one heap
            // allocation itself.
            let Some(tls_buf) = OwnedAlloc::new(tls.memsz() as usize, 16) else {
                log!("spawn: {}: cannot allocate a {}-byte TLS template", path, tls.memsz());
                return Err(SyscallError::ResourceExhausted.into());
            };
            // `slice` bounds `filesz` against the `memsz` allocation, re-checking what `Layout::parse` already refused.
            if elf::read_backing_into(
                backing.as_ref(),
                tls_file_off,
                tls_buf.slice(tls.template().len() as usize),
            )
            .is_err()
            {
                log!("spawn: {}: the TLS template could not be read off the device", path);
                return Err(SyscallError::NotFound.into());
            }
            Some(tls_buf)
        }
        None => None,
    };

    let Some((tls_modules, tls, next_tls_module_id)) =
        tls::build_tls_layout(&loaded_libs.libs, &layout, exe_tls_template.as_ref())
    else {
        log!("spawn: {}: the TLS modules do not fit one block", path);
        return Err(SyscallError::ResourceExhausted.into());
    };

    if let Err(refused) = apply_tls_relocs(&exe, &layout, &loaded_libs.libs, &tls_modules, tls,
        &mut reloc_index)
    {
        log!("spawn: {}: {}", path, refused.as_str());
        return Err(SyscallError::InvalidArgument.into());
    }

    reloc_index.finalize();
    let reloc_index = if reloc_index.len() > 0 {
        log!("ELF: {} relocations indexed (RELATIVE + GLOB_DAT + TPOFF)", reloc_index.len());
        Some(Arc::new(reloc_index))
    } else {
        None
    };

    log!("spawn: TLS {} modules, total_memsz={}", tls_modules.len(), tls.total_memsz());
    let Some((tls_pages, thread_pointer, _)) =
        tls::TlsBlock::build(&tls_modules, tls).and_then(|b| b.publish(&child_pt))
    else {
        log!("spawn: {}: failed to allocate TLS ({} bytes)", path, tls.total_memsz());
        return Err(SyscallError::ResourceExhausted.into());
    };

    let entry = (image_start + layout.entry().get()).raw();
    let image_end = (image_start + layout.span()).raw();
    let sp = user_stack.write_argv(argv);
    let t_tls = crate::clock::nanos_since_boot();

    let syms = symbols::read_backtrace_table(
        backing.as_ref(), &layout, path, base,
        image_start.raw(), image_end,
        user_stack.base().raw(), user_stack.top(),
    );
    let sym_bytes = syms.resident_bytes();

    let (ks_alloc, ks_sp) = match alloc_kernel_stack(process_start, entry, sp, 0) {
        Some(ks) => ks,
        None => {
            log!("spawn: {}: failed to allocate kernel stack", path);
            return Err(SyscallError::ResourceExhausted.into());
        }
    };


    let NeededLibs { libs: loaded_libs, paths: lib_paths } = loaded_libs;
    let pid = admission.pid();
    let object = crate::object::process::ProcessObject::new(pid);
    // The point of no return: every failure above answers the caller with its
    // table untouched.
    let (handles, endowments, held) = commit(start::own_handle(&object))?;
    let proc_data = Arc::new(Lock::new(ProcessData {
        handles,
        cwd,
        env,
        elf: ElfInfo {
            elf_alloc: exe_tls_template,
            tls_modules,
            tls,
            next_tls_module_id,
            dynamic_tls_blocks: alloc::collections::BTreeMap::new(),
            loaded_libs,
            reloc_index,
            elf_base: UserAddr::new(base),
            exe_eh_frame_hdr: layout
                .eh_frame_hdr()
                .map_or((0, 0), |r| ((image_start + r.start().get()).raw(), r.len())),
            exe_vaddr_max: image_end,
            exe_phdrs: ((image_start + phdrs.image().start().get()).raw(), phdrs.count()),
            lib_paths,
        },
        mmap_regions: Vec::new(),
        pipe_maps: Vec::new(),
        demand_pages: Vec::new(),
        fault_trace: PageFaultTrace::new(),
        peak_memory: 0,
        alloc_count: 0,
        free_count: 0,
        exe_path: String::from(path),
        spawn_ns: crate::clock::nanos_since_boot(),
        accounting: ProcessAccounting::default(),
        endowments,
    }));

    let thread_data = Arc::new(Lock::new(ThreadData {
        tls_pages: Some(tls_pages),
        stack_pages: Some(stack_pages),
        user_stack_base: user_stack.base(),
        user_stack_size: user_stack.size(),
        syscall_counts: [0; toyos_abi::syscall::SYSCALL_PROFILE_BINS],
        syscall_total: 0,
        syscall_total_ns: 0,
    }));

    // One table, two holders: cloned so a crash report on this thread reads names without the process table.
    let syms = Arc::new(syms);

    #[cfg(feature = "test-actuators")]
    crate::process::debug_kill_marked_place(parent);

    let mut guard = PROCESS_TABLE.lock();
    let ((tid, dst), retire) = admission.land(guard.as_mut().unwrap(), |table, node| {
        table.insert(ProcessEntry::new(
            Arc::clone(&object),
            start::make_name(path),
            proc_data,
            Arc::clone(&syms),
            ThreadEntry::new(thread_data),
            node,
        ));
        let tid = table.get(pid).unwrap().main_tid();
        // Placed while still holding the table lock: kill_process claims teardown
        // under it, so a retire sweep can never see the pid before its thread is scheduled.
        let (sched, dst) = scheduler::enqueue_new(
            scheduler::TaskId(pid, tid),
            ks_alloc,
            ks_sp,
            child_pt.clone(),
            thread_pointer,
            syms,
        );
        table.get_mut(pid).unwrap().threads_mut().get_mut(tid).unwrap().set_sched(sched);
        (tid, dst)
    });
    drop(guard);
    // Its parent was claimed while it was built, and its walk has passed: the
    // child is ended as that walk would have ended it, and the spawn answers it.
    for sched in &retire {
        scheduler::post_retire(sched);
    }

    #[cfg(feature = "test-actuators")]
    crate::process::debug_hold_marked_spawn(parent, &object);

    let t3 = crate::clock::nanos_since_boot();
    log!("spawn: {} pid={} tid={} dst={} base={:#x} entry={:#x} root={:#x} symbols={}KiB (layout={}ms relocs={}ms deps={}ms tls={}ms total={}ms)",
        path, pid, tid, dst.0, base, entry, child_pt.lock().root().phys(), sym_bytes / 1024,
        (t1 - t0) / 1_000_000, (t2 - t1) / 1_000_000, (t_deps - t2) / 1_000_000,
        (t_tls - t_deps) / 1_000_000, (t3 - t0) / 1_000_000);

    Ok((pid, held))
}

/// The libraries an executable's `DT_NEEDED` entries name, and the paths they were found at.
struct NeededLibs {
    libs: Vec<elf::LoadedLib>,
    paths: Vec<String>,
}

/// The most distinct libraries one executable may pull in; each is a private
/// 2 MiB window, so a `DT_NEEDED` list naming more is refused rather than loaded.
const MAX_NEEDED_LIBS: usize = 64;

/// Load each distinct `DT_NEEDED` library, from the executable's own directory
/// first and `/system/lib` second; an image's from `/system/lib` alone.
fn load_needed_libs(exe: &ExeTables, path: &str, from_image: bool) -> Result<NeededLibs, SyscallError> {
    let mut out = NeededLibs { libs: Vec::new(), paths: Vec::new() };
    if exe.needed.is_empty() {
        return Ok(out);
    }
    let exe_dir = match from_image {
        true => "/system/lib",
        false => path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or(""),
    };

    // Collapse duplicates to the distinct set and bound it: a repeat resolves to
    // one library `elf/cache.rs` holds one window for, so it buys no second one.
    let mut distinct: Vec<&str> = Vec::new();
    for &name_offset in &exe.needed {
        // An offset outside the string table yields an empty name, not a bounds failure.
        let lib_name = toyos_elf::cstr(&exe.dynstr, name_offset);
        if lib_name.is_empty() || distinct.contains(&lib_name) {
            continue;
        }
        if distinct.len() == MAX_NEEDED_LIBS {
            log!("spawn: {}: more than {} distinct DT_NEEDED libraries", path, MAX_NEEDED_LIBS);
            return Err(SyscallError::ResourceExhausted);
        }
        distinct.push(lib_name);
    }
    out.libs.reserve_exact(distinct.len());
    out.paths.reserve_exact(distinct.len());

    for lib_name in distinct {
        let t_load0 = crate::clock::nanos_since_boot();

        // Which spelling opens is decided before the cache is consulted, and is
        // the key from here on: keyed by the exe-dir string it never found, a
        // library loaded through the fallback was mapped a second time by any
        // later `dlopen("/system/lib/…")`. Fallback only for NotFound — any other error
        // would repeat on `/system/lib` too and produce a misleading second log line.
        let (so_backing, id, lib_path) = {
            let in_exe_dir = alloc::format!("{}/{}", exe_dir, lib_name);
            let opened = vfs::lock().open_backing_identified(&in_exe_dir);
            match opened {
                Ok((b, id)) => (b, id, in_exe_dir),
                Err(SyscallError::NotFound) => {
                    let fallback = alloc::format!("/system/lib/{}", lib_name);
                    match vfs::lock().open_backing_identified(&fallback) {
                        Ok((b, id)) => (b, id, fallback),
                        Err(e) => {
                            log!("spawn: {}: failed to load {}: {e}", path, lib_name);
                            return Err(e);
                        }
                    }
                }
                Err(e) => {
                    log!("spawn: {}: failed to load {}: {e}", path, lib_name);
                    return Err(e);
                }
            }
        };

        match elf::try_clone_cached(&lib_path, id) {
            Ok(Some(lib)) => {
                out.paths.push(lib_path);
                out.libs.push(lib);
                continue;
            }
            Err(e) => {
                log!(
                    "spawn: {}: {}: the cached image is stale — the file behind this path changed \
                     since it was loaded, and the image cannot be replaced while a process has it \
                     mapped",
                    path, lib_path
                );
                return Err(e);
            }
            Ok(None) => {}
        }

        match elf::load_shared_lib(so_backing.as_ref()) {
            Ok((lib, rw_offset, rw_size)) => {
                let t_load1 = crate::clock::nanos_since_boot();
                log!("dynamic: loaded {} base={:#x} ({} syms, {}ms)",
                    lib_name, lib.phys_base, lib.symbols().count(), (t_load1 - t_load0) / 1_000_000);
                out.libs.push(elf::cache_loaded_lib(&lib_path, id, lib, rw_offset, rw_size)?);
                out.paths.push(lib_path);
            }
            Err(e) => {
                log!("spawn: {}: failed to load {}: {}", path, lib_name, e);
                return Err(SyscallError::NotFound);
            }
        }
    }
    Ok(out)
}

/// Give every library a virtual address in the child and map its pages there.
///
/// A cached library's read-only pages are the cache's own; only its writable
/// window is private to this process.
fn map_libs(
    child_pt: &PageTables,
    loaded: &mut NeededLibs,
    path: &str,
) -> Result<(), SyscallError> {
    for lib in &mut loaded.libs {
        let Some(vaddr) = lib.map_into(child_pt) else {
            log!("spawn: {}: out of virtual address space for a library", path);
            return Err(SyscallError::ResourceExhausted);
        };
        lib.user_base = vaddr;
    }
    Ok(())
}

/// Apply the TLS relocations of every startup library, and index the executable's.
///
/// Libraries' land directly; the executable's go into the relocation index
/// because its pages do not exist yet.
fn apply_tls_relocs(
    exe: &ExeTables,
    layout: &Layout,
    loaded_libs: &[elf::LoadedLib],
    tls_modules: &[elf::TlsModule],
    tls: toyos_elf::tls::Static,
    reloc_index: &mut elf::RelocationIndex,
) -> Result<(), RelocError> {
    let tls_info = elf::TlsModuleInfo { libs: loaded_libs, modules: tls_modules };
    for lib in loaded_libs {
        // Matched by template pointer, unique per lib; a lib without TLS matches nothing.
        let module = tls_modules.iter().find(|m| m.template == lib.tls_template);
        let base_offset = module.map_or(0, |m| m.base_offset);
        // Initial-exec: references to TLS in the static block.
        elf::apply_tpoff_relocs(lib, base_offset, tls, &tls_info)?;
        // General-dynamic: this lib's own TLS, reached through the DTV.
        if let Some(m) = module {
            elf::apply_dtpoff_relocs(lib, &tls_info)?;
            elf::apply_dtpmod_relocs(lib, m.module_id, &tls_info);
        }
    }

    let exe_base_offset = tls_modules
        .iter()
        .find(|m| m.module_id == 1)
        .map_or(0, |m| m.base_offset);
    let exe_tpoff =
        |r| elf::compute_tpoff(r, exe_base_offset, layout.tls(), exe.symbols(), tls, &tls_info);
    for &(r_offset, r) in &exe.relas.tpoff64 {
        reloc_index.add_u64(r_offset, exe_tpoff(r)? as u64);
    }
    for &(r_offset, r) in &exe.relas.tpoff32 {
        reloc_index.add_i32(r_offset, elf::tpoff32_value(exe_tpoff(r)?)?);
    }
    Ok(())
}

/// The one program the kernel starts. `src/build.rs` puts this binary in every
/// image, so a missing one is a bad build, not a different boot.
pub const SUPERVISOR_PATH: &str = "/system/bin/supervisor";

/// Start `/system/bin/supervisor`, holding the machine's one full-rights `SysCap`.
///
/// Nothing else can construct one: what the supervisor endows is the entire set of
/// processes that can ever claim a device, enter the RT band, or power off.
/// Panics on failure: a boot that cannot start the supervisor has nowhere to report to.
pub fn spawn_supervisor() -> Pid {
    let mut handles = HandleTable::new();
    let console = KObjectRef::Console(crate::object::device::ConsoleObject::new());
    for slot in 0..3 {
        let entry = crate::object::HandleEntry::new(
            console.clone(),
            ops::initial_rights(&console),
        );
        let (_, displaced) = handles
            .install_at(slot, entry)
            .expect("spawn_supervisor: three slots cannot exhaust an empty table");
        assert!(displaced.is_none(), "an empty table had something at slot {slot}");
    }
    let cap = KObjectRef::SysCap(crate::object::syscap::SysCap::new());
    // Every machine-wide authority the system has: rights only shrink from
    // here, so a bit absent here is a bit no manifest can ever name. LOG and
    // WAIT arrive together since SYS_LOG_READ never blocks on its own; ROSTER
    // needs no partner since SYS_SYSINFO never blocks either.
    let rights = Rights::DUP
        .union(Rights::TRANSFER)
        .union(Rights::DEVICE)
        .union(Rights::RT)
        .union(Rights::LOG)
        .union(Rights::WAIT)
        .union(Rights::POWER)
        .union(Rights::ROSTER)
        .union(Rights::INVENTORY)
        .union(Rights::COUNTERS)
        .union(Rights::TRACE);
    let cap_handle = handles
        .install(crate::object::HandleEntry::new(cap, rights))
        .expect("spawn_supervisor: an empty table refused the system capability");
    let label = toyos_abi::syscall::SYSCAP_LABEL;
    let mut entries = alloc::vec![toyos_abi::syscall::EndowEntry {
        label_off: 0,
        label_len: label.len() as u32,
        handle: cap_handle,
        _pad: 0,
    }];
    let mut labels = label.as_bytes().to_vec();
    // Built by the kernel and owing nobody anything: no table but the supervisor's own holds it.
    let commit = |own| {
        start::endow_self(&mut handles, &mut entries, &mut labels, own);
        Ok((handles, Endowments::new(entries, labels), ()))
    };
    match spawn(&[SUPERVISOR_PATH], commit, String::from("/"), Vec::new(), None, Parent::Root) {
        Ok((pid, ())) => pid,
        Err(crate::object::Refusal::Error(e)) => panic!("spawn_supervisor: failed to spawn: {e:?}"),
        Err(crate::object::Refusal::Handle(e)) => panic!("spawn_supervisor: {e}"),
    }
}
