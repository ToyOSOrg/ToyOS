//! Building a process out of an ELF file.
//!
//! The kernel reads an executable's program headers, its `PT_TLS` template
//! and its build-id note, demand-pages its `PT_LOAD`s and jumps to its entry;
//! it reads no dynamic section, applies no relocation and loads no library at
//! spawn. The program relocates itself (`toyos::relocate`).
//!
//! Every number the file names is untrusted: a refusal is
//! `SyscallError::{InvalidArgument, ResourceExhausted}`, never a panic.
//!
//! A spawn that lands writes one record, `spawn: <path> pid=N (…ms)`, once
//! the process is in the table and placed; a spawn that is refused writes
//! one, naming why. Nothing is said on the way, by this file or by
//! `crate::elf` under it (`crate::process`'s header).

// `warn`, not `deny`: the rest of the kernel is not yet swept for undocumented unsafe blocks.
#![warn(clippy::undocumented_unsafe_blocks)]

mod start;
mod tls;

pub use start::{build_child_handles, PendingHandles, SLOT_PAIR_LEN};
pub(crate) use start::{alloc_kernel_stack, Start};
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
    Pid, ProcessAccounting, ProcessData, ProcessEntry, ThreadData, ThreadEntry, UserImage,
    UserStack, PROCESS_TABLE,
};
use crate::sync::Lock;
use crate::{scheduler, vfs, UserAddr};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::SyscallError;
use toyos_elf::{Layout, TlsSegment};
use toyos_symbols::frame::BuildId;

const USER_STACK_SIZE: usize = 4 * PAGE_2M as usize; // 8 MB

/// User virtual address space starts at 1 TB, above any direct-mapped physical RAM.
const USER_VM_BASE: u64 = 0x100_0000_0000;

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


/// Load a program and place its main thread under `parent`, answering its pid
/// and what `commit` left its caller holding of it. `commit` builds the
/// child's handle table around the child's handle to itself, once nothing is
/// left to refuse.
///
/// `path` is the program, opened here — or, with `image`, the bytes the caller
/// read from it itself, and then nothing opens it. `argv[0]` is only the name the child goes by.
///
/// `Refusal`, not `-> !`, is the error type: every failure below owns a
/// partly built process (address space, stack, kernel stack), and nothing
/// unwinds, so the error must travel out as a value rather than strand it.
pub fn spawn<H>(
    path: &str,
    argv: &[&str],
    commit: impl FnOnce(crate::object::HandleEntry) -> Result<(HandleTable, Endowments, H), crate::object::Refusal>,
    cwd: String,
    env: Vec<u8>,
    image: Option<Arc<dyn crate::file_backing::FileBacking>>,
    parent: Parent,
) -> Result<(Pid, H), crate::object::Refusal> {
    // An argv of only separators survives sys_spawn's split as an empty slice.
    let Some(&name) = argv.first() else {
        return Err(SyscallError::InvalidArgument.into());
    };
    // Before anything is built; dropped on every way out below but the insert.
    let admission = Admission::ask(parent)?;
    let t0 = crate::clock::nanos_since_boot();

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
    // The program reads the mapped table to relocate itself, and
    // `SYS_QUERY_MODULES` answers with it: an image without one has neither.
    let Some(phdrs) = layout.program_headers() else {
        log!("spawn: {}: no PT_LOAD maps the program header table", path);
        return Err(SyscallError::InvalidArgument.into());
    };

    let t1 = crate::clock::nanos_since_boot();

    // ELF segments are demand-faulted; the address space starts with the clock page alone.
    let Some(mut space) = crate::mm::paging::AddressSpace::new_user() else {
        log!("spawn: {}: no user PCID free — too many live address spaces", path);
        return Err(SyscallError::ResourceExhausted.into());
    };
    crate::clock::map_page(&mut space);
    let child_pt: PageTables = Arc::new(Lock::new(space));
    insert_elf_regions(&mut child_pt.lock(), &layout, image_start, &backing)?;

    // Mapped eagerly, not demand-paged: every process touches the stack immediately.
    let stack_pages = match PageAlloc::new(USER_STACK_SIZE) {
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
            let Some(tls_file_off) = layout.file_offset_of(tls.template()) else {
                log!("spawn: {}: PT_TLS's template is in no PT_LOAD's file bytes", path);
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

    let Some((tls_modules, tls)) = tls::build_tls_layout(&layout, exe_tls_template.as_ref()) else {
        log!("spawn: {}: the TLS module does not fit one block", path);
        return Err(SyscallError::ResourceExhausted.into());
    };

    let Some((tls_pages, thread_pointer, _)) =
        tls::TlsBlock::build(&tls_modules, tls).and_then(|b| b.publish(&child_pt))
    else {
        log!("spawn: {}: failed to allocate TLS ({} bytes)", path, tls.total_memsz());
        return Err(SyscallError::ResourceExhausted.into());
    };

    // Inside the image, which `rebase_base` placed inside the user half.
    let Some(entry) = toyos_userbound::Entry::new((image_start + layout.entry().get()).raw())
    else {
        log!("spawn: {}: the entry is outside the user half", path);
        return Err(SyscallError::InvalidArgument.into());
    };
    let image_end = (image_start + layout.span()).raw();
    let sp = user_stack.write_argv(argv);
    let t_tls = crate::clock::nanos_since_boot();

    // What a crash record names a frame by. The build-id is read at each
    // `PT_NOTE`'s own offset, a page of it at most.
    let image = Arc::new(UserImage {
        name: String::from(path),
        build_id: BuildId::find(&header_data, |note| {
            Some(read_file_range(backing.as_ref(), note.offset, note.filesz.min(PAGE_BYTES as u64) as usize))
        }),
        start: image_start.raw(),
        end: image_end,
        bias: base,
    });

    let (ks_alloc, ks_sp) = match alloc_kernel_stack(Start::Process { entry, sp }) {
        Some(ks) => ks,
        None => {
            log!("spawn: {}: failed to allocate kernel stack", path);
            return Err(SyscallError::ResourceExhausted.into());
        }
    };

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
            next_tls_module_id: tls::FIRST_DLOPEN_MODULE,
            dynamic_tls_blocks: alloc::collections::BTreeMap::new(),
            loaded_libs: Vec::new(),
            elf_base: UserAddr::new(base),
            exe_eh_frame_hdr: layout
                .eh_frame_hdr()
                .map_or((0, 0), |r| ((image_start + r.start().get()).raw(), r.len())),
            exe_vaddr_max: image_end,
            exe_phdrs: ((image_start + phdrs.image().start().get()).raw(), phdrs.count()),
            lib_paths: Vec::new(),
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

    #[cfg(feature = "test-actuators")]
    crate::process::debug_kill_marked_place(parent);

    let mut guard = PROCESS_TABLE.lock();
    let ((), retire) = admission.land(guard.as_mut().unwrap(), |table, node| {
        table.insert(ProcessEntry::new(
            Arc::clone(&object),
            start::make_name(name),
            proc_data,
            // Two holders: a crash report on this thread reads it without the process table.
            Some(Arc::clone(&image)),
            ThreadEntry::new(thread_data),
            node,
        ));
        let tid = table.get(pid).unwrap().main_tid();
        // Placed while still holding the table lock: kill_process claims teardown
        // under it, so a retire sweep can never see the pid before its thread is scheduled.
        let (sched, _placed) = scheduler::enqueue_new(
            scheduler::TaskId(pid, tid),
            ks_alloc,
            ks_sp,
            child_pt.clone(),
            thread_pointer,
            Some(image),
        );
        table.get_mut(pid).unwrap().threads_mut().get_mut(tid).unwrap().set_sched(sched);
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
    log!("spawn: {} pid={} (layout={}ms tls={}ms total={}ms)",
        path, pid, (t1 - t0) / 1_000_000, (t_tls - t1) / 1_000_000, (t3 - t0) / 1_000_000);

    Ok((pid, held))
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
    match spawn(SUPERVISOR_PATH, &[SUPERVISOR_PATH], commit, String::from("/"), Vec::new(), None, Parent::Root) {
        Ok((pid, ())) => pid,
        Err(crate::object::Refusal::Error(e)) => panic!("spawn_supervisor: failed to spawn: {e:?}"),
        Err(crate::object::Refusal::Handle(e)) => panic!("spawn_supervisor: {e}"),
    }
}
