//! Building a process out of an ELF file.
//!
//! The kernel reads an executable's program headers, its `PT_TLS` template
//! and its build-id note, copies its `PT_LOAD`s into memory of the new
//! process's own and jumps to its entry;
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

pub use start::{build_child_handles, Commit, PendingHandles};
pub(crate) use start::{alloc_kernel_stack, KernelStack, Start};
pub use tls::{TlsBlock, DTV_INITIAL_CAPACITY, VARIANT as TLS_VARIANT};

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::elf;
use crate::object::{ops, HandleTable, KObjectRef};
use crate::mm::space::CopySource;
use crate::mm::{PAGE_2M, PAGE_BYTES};
use crate::process::{
    Admission, ElfInfo, Endowments, OwnedAlloc, Parent, Pid, ProcessAccounting, ProcessData,
    ProcessEntry, ThreadData, ThreadEntry, UserImage, PROCESS_TABLE,
};
use crate::sync::Lock;
use crate::{scheduler, vfs, UserAddr};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::SyscallError;
use toyos_elf::{Layout, TlsSegment};
use toyos_symbols::frame::BuildId;

/// The main thread's stack: committed whole, populated as it is touched.
const USER_STACK_SIZE: usize = 8 * 1024 * 1024;

/// The reserved range below the main stack.
const STACK_GUARD: u64 = 1024 * 1024;

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


/// Commit one range of private memory per `PT_LOAD` segment at `image_start`
/// plus its offset, its file bytes and the rest of it zero, each page copied
/// out of `source` at its first touch.
///
/// `Err` when two segments would share a page, which their ranges cannot:
/// checked before the first range, and a refusal after it leaves ranges in a
/// space nothing runs in yet.
fn load_segments(
    space: &mut crate::mm::space::AddressSpace,
    layout: &Layout,
    image_start: UserAddr,
    source: &Arc<dyn CopySource>,
) -> Result<(), SyscallError> {
    use crate::mm::space::At;

    if let Some((a, b)) = layout.overlapping_load_pages(4096) {
        log!("spawn: PT_LOAD segments {} and {} contend for a page", a, b);
        return Err(SyscallError::InvalidArgument);
    }
    for seg in layout.segments() {
        let (lo, hi) = seg.page_range(4096);
        let (seg_start, seg_end) = ((image_start + lo).raw(), (image_start + hi).raw());
        if seg_end == seg_start {
            continue;
        }
        // The file's bytes from the segment's first page: the bytes of that
        // page before the segment are the file's too, as a linker lays them.
        // Past the file's end and the segment's file size, the range reads
        // zeros.
        let file_start = seg.file_offset() & !4095;
        let file_bytes = seg.filesz() + seg.file_offset() % 4096;
        let copied = file_bytes.min(source.size().saturating_sub(file_start));
        let at = At::Fixed(UserAddr::new(seg_start));
        space.copy(seg_end - seg_start, at, segment_prot(seg), Arc::clone(source), file_start, copied)?;
    }
    Ok(())
}

/// The bytes a program's stack starts with below `top`, and where its stack
/// pointer starts: argc, the argv pointers and a null, 16-aligned, then the
/// strings they point at.
fn argv_block(top: u64, args: &[&str]) -> (u64, Vec<u8>) {
    let strings: usize = args.iter().map(|a| a.len() + 1).sum();
    let words = args.len() + 2;
    let sp = (top - strings as u64 - words as u64 * 8) & !15;
    let mut block = alloc::vec![0u8; (top - sp) as usize];
    let mut word = |i: usize, value: u64| block[i * 8..i * 8 + 8].copy_from_slice(&value.to_ne_bytes());
    word(0, args.len() as u64);
    let mut at = top - strings as u64;
    let mut pointers = Vec::with_capacity(args.len());
    for arg in args {
        pointers.push(at);
        at += arg.len() as u64 + 1;
    }
    for (i, pointer) in pointers.iter().enumerate() {
        word(1 + i, *pointer);
    }
    for (arg, pointer) in args.iter().zip(&pointers) {
        let off = (pointer - sp) as usize;
        block[off..off + arg.len()].copy_from_slice(arg.as_bytes());
    }
    (sp, block)
}

/// What one `PT_LOAD` segment's pages may be used for.
///
/// `PF_W | PF_X` refuses the write, not the execution: taking `X` away would
/// let a hostile ELF run as data instead.
fn segment_prot(seg: &toyos_elf::Segment) -> kernel::vm::Prot {
    use kernel::vm::Prot;
    if seg.flags().executable() {
        Prot::ReadExec
    } else if seg.flags().writable() {
        Prot::ReadWrite
    } else {
        Prot::Read
    }
}


/// Load a program and place its main thread under `parent`, answering its pid
/// and what `commit` left its caller holding of it. Everything built for the
/// child is counted to [`Commit::job`], which it runs in, and `commit` builds
/// the child's handle table around its handle to itself once nothing is left
/// to refuse.
///
/// `path` is the program, opened here — or, with `image`, the bytes the caller
/// read from it itself, and then nothing opens it. `argv[0]` is only the name the child goes by.
///
/// `Refusal`, not `-> !`, is the error type: every failure below owns a
/// partly built process (address space, stack, kernel stack), and nothing
/// unwinds, so the error must travel out as a value rather than strand it.
pub fn spawn<C: Commit>(
    path: &str,
    argv: &[&str],
    commit: C,
    cwd: String,
    env: Vec<u8>,
    image: Option<Arc<dyn crate::file_backing::FileBacking>>,
    parent: Parent,
) -> Result<(Pid, C::Held), crate::object::Refusal> {
    let job = Arc::clone(commit.job());
    let pager = if commit.pager() { crate::process::PagerRole::May } else { crate::process::PagerRole::None };
    // An argv of only separators survives sys_spawn's split as an empty slice.
    let Some(&name) = argv.first() else {
        return Err(SyscallError::InvalidArgument.into());
    };
    // Before anything is built; dropped on every way out below but the insert.
    let admission = Admission::ask(parent)?;
    let mut charge = ProcessData::charge(&job, path, &env).map_err(SyscallError::from)?;
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

    // The program reads the mapped table to relocate itself, and
    // `SYS_QUERY_MODULES` answers with it: an image without one has neither.
    let Some(phdrs) = layout.program_headers() else {
        log!("spawn: {}: no PT_LOAD maps the program header table", path);
        return Err(SyscallError::InvalidArgument.into());
    };

    // Taken before the child's space is locked: a file's snapshot takes the
    // file cache's lock, which a user copy may hold while it takes a space's.
    let source = match backing.source() {
        Ok(source) => source,
        Err(e) => {
            log!("spawn: {}: {e}", path);
            return Err(e.into());
        }
    };

    let t1 = crate::clock::nanos_since_boot();

    let Some(child_pt) = crate::mm::space::Space::new_user(&job) else {
        log!("spawn: {}: no address space: every user PCID is held, or no frame is left for its root", path);
        return Err(SyscallError::ResourceExhausted.into());
    };
    let (image_start, base, stack_base, sp) = {
        use crate::mm::space::At;
        use kernel::vm::Prot;
        let mut space = child_pt.lock();
        crate::clock::map_page(&mut space);
        // The image's span is placed whole, so its segments keep the distances
        // the file gave them; the holes between them stay reserved.
        let extent = layout.extent();
        let Some(span) = layout.span().checked_next_multiple_of(crate::mm::PAGE_SIZE).filter(|&s| s != 0) else {
            log!("spawn: {}: an image spanning {:#x}", path, layout.span());
            return Err(SyscallError::InvalidArgument.into());
        };
        let image_start = space.reserve(span, At::Anywhere(PAGE_2M), Prot::None)?;
        // The rebase base is the file's numbers, so `rebase_base` refuses a
        // vaddr_min that underflows the subtraction or a span that leaves the
        // user half.
        let Some(base) = toyos_userbound::rebase_base(image_start.raw(), extent.min(), layout.span()) else {
            log!("spawn: {}: image at vaddr_min {:#x} spanning {:#x} cannot rebase to {image_start:#x}",
                path, extent.min(), layout.span());
            return Err(SyscallError::InvalidArgument.into());
        };
        load_segments(&mut space, &layout, image_start, &source)?;
        // Never executable, and a reserved guard below it, so running off the
        // stack's bottom faults instead of writing what lies there.
        let stack = space.reserve(STACK_GUARD + USER_STACK_SIZE as u64, At::ANYWHERE, Prot::None)?;
        let stack_base = stack + STACK_GUARD;
        space.reserve(USER_STACK_SIZE as u64, At::Fixed(stack_base), Prot::ReadWrite)?;
        let (sp, block) = argv_block(stack_base.raw() + USER_STACK_SIZE as u64, argv);
        if block.len() > USER_STACK_SIZE {
            return Err(SyscallError::InvalidArgument.into());
        }
        space.fill(sp, &block);
        (image_start, base, stack_base, sp)
    };

    let exe_tls_template = match layout.tls().and_then(TlsSegment::occupied) {
        Some(tls) => {
            let Some(tls_file_off) = layout.file_offset_of(tls.template()) else {
                log!("spawn: {}: PT_TLS's template is in no PT_LOAD's file bytes", path);
                return Err(SyscallError::InvalidArgument.into());
            };
            // Read directly into the `memsz`-sized buffer: `OwnedAlloc` zeroes
            // (no second pass for `.tbss`) and refuses a size past one heap
            // allocation itself.
            let template = core::alloc::Layout::from_size_align(tls.memsz() as usize, 16).map_err(|_| SyscallError::InvalidArgument)?;
            charge.grow(crate::mm::charge::heap(template)).map_err(SyscallError::from)?;
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

    let (ks_alloc, ks_sp) = match alloc_kernel_stack(&job, Start::Process { entry, sp }) {
        Some(ks) => ks,
        None => {
            log!("spawn: {}: failed to allocate kernel stack", path);
            return Err(SyscallError::ResourceExhausted.into());
        }
    };

    let pid = admission.pid();
    let object = crate::object::process::ProcessObject::new(pid).map_err(SyscallError::from)?;
    let notice = crate::object::notice::NoticeObject::new().map_err(SyscallError::from)?;
    // The point of no return: every failure above answers the caller with its
    // table untouched.
    let (handles, endowments, held) = commit.commit(start::Own::new(&object, &notice))?;
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
        space: Arc::clone(&child_pt),
        exe_path: String::from(path),
        spawn_ns: crate::clock::nanos_since_boot(),
        accounting: ProcessAccounting::default(),
        endowments,
        job: job.clone(),
        charge,
        pager,
    }));

    let thread_data = Arc::new(Lock::new(ThreadData {
        tls: Some(tls_pages),
        user_stack_base: stack_base,
        user_stack_size: USER_STACK_SIZE as u64,
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
            notice,
        ));
        let tid = table.get(pid).unwrap().main_tid();
        // Placed while still holding the table lock: kill_process claims teardown
        // under it, so a retire sweep can never see the pid before its thread is scheduled.
        let (sched, _placed) = scheduler::enqueue_new(scheduler::NewTask {
            id: scheduler::TaskId(pid, tid),
            kernel_stack: ks_alloc,
            entry_sp: ks_sp,
            address_space: child_pt.clone(),
            thread_pointer,
            job,
            image: Some(image),
            // The spawning thread's, as a new thread's is.
            mask: crate::sched::driver::current_mask(),
        });
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

/// The name the supervisor runs under: no file, since it runs from its boot image.
const SUPERVISOR: &str = "supervisor";

/// Start the supervisor from its boot image, holding the machine's one
/// full-rights `SysCap` and the file server's image and ROOT's
/// (`toyos_abi::syscall::FILESERVER_IMAGE_LABEL`, `ROOT_IMAGE_LABEL`).
///
/// Nothing else can construct one: what the supervisor endows is the entire set of
/// processes that can ever claim a device, enter the RT band, or power off.
/// Panics on failure: a boot that cannot start the supervisor has nowhere to report to.
pub fn spawn_supervisor(images: crate::bootimage::Objects) -> Pid {
    let job = crate::object::job::new_job();
    let mut handles = HandleTable::new(&job);
    let mut entries = Vec::new();
    let mut labels = Vec::new();
    // One console, the supervisor's descriptors 0 to 2.
    let console = KObjectRef::Console(crate::object::device::ConsoleObject::new(&job).expect("spawn_supervisor: no heap for its console"));
    for fd in ["0", "1", "2"] {
        let entry = crate::object::HandleEntry::new(console.clone(), ops::initial_rights(&console));
        let handle = handles.install(entry).expect("spawn_supervisor: three handles cannot exhaust an empty table");
        let label_off = labels.len() as u32;
        labels.extend_from_slice(toyos_abi::syscall::FD_PREFIX.as_bytes());
        labels.extend_from_slice(fd.as_bytes());
        let label_len = labels.len() as u32 - label_off;
        entries.push(toyos_abi::syscall::EndowEntry { label_off, label_len, handle, _pad: 0 });
    }
    let cap = KObjectRef::SysCap(crate::object::syscap::SysCap::new().expect("spawn_supervisor: no heap for the system capability"));
    // Every machine-wide authority the system has: rights only shrink from
    // here, so a bit absent here is a bit no manifest can ever name. LOG and
    // WAIT arrive together since SYS_LOG_READ never blocks on its own; ROSTER
    // needs no partner since SYS_SYSINFO never blocks either.
    let rights = Rights::DUP
        .union(Rights::TRANSFER)
        .union(Rights::DEVICE)
        .union(Rights::JOBS)
        .union(Rights::FLOOR)
        .union(Rights::RT)
        .union(Rights::WAKE_LATENCY)
        .union(Rights::LOG)
        .union(Rights::WAIT)
        .union(Rights::POWER)
        .union(Rights::ROSTER)
        .union(Rights::INVENTORY)
        .union(Rights::COUNTERS)
        .union(Rights::TRACE)
        .union(Rights::PAGER);
    let cap_handle = handles
        .install(crate::object::HandleEntry::new(cap, rights))
        .expect("spawn_supervisor: an empty table refused the system capability");
    let label = toyos_abi::syscall::SYSCAP_LABEL;
    entries.push(toyos_abi::syscall::EndowEntry {
        label_off: labels.len() as u32,
        label_len: label.len() as u32,
        handle: cap_handle,
        _pad: 0,
    });
    labels.extend_from_slice(label.as_bytes());
    for (label, object) in [
        (toyos_abi::syscall::FILESERVER_IMAGE_LABEL, &images.fileserver),
        (toyos_abi::syscall::ROOT_IMAGE_LABEL, &images.root),
    ] {
        let object = KObjectRef::SharedMem(Arc::clone(object));
        let rights = ops::initial_rights(&object);
        let handle = handles
            .install(crate::object::HandleEntry::new(object, rights))
            .expect("spawn_supervisor: a table of five handles refused a boot image");
        entries.push(toyos_abi::syscall::EndowEntry {
            label_off: labels.len() as u32,
            label_len: label.len() as u32,
            handle,
            _pad: 0,
        });
        labels.extend_from_slice(label.as_bytes());
    }
    let len = images.supervisor.immutable().expect("a boot image is immutable");
    let image: Arc<dyn crate::file_backing::FileBacking> = Arc::new(
        crate::file_backing::SharedImage::over(images.supervisor, len)
            .expect("spawn_supervisor: a boot image is ordinary memory, its length inside it"),
    );
    // Built by the kernel and owing nobody anything: no table but the supervisor's own holds it.
    let table = SupervisorTable { job, handles, entries, labels };
    match spawn(SUPERVISOR, &[SUPERVISOR], table, String::from("/"), Vec::new(), Some(image), Parent::Root) {
        Ok((pid, ())) => pid,
        Err(crate::object::Refusal::Error(e)) => panic!("spawn_supervisor: failed to spawn: {e:?}"),
        Err(crate::object::Refusal::Handle(e)) => panic!("spawn_supervisor: {e}"),
    }
}

/// The supervisor's table as the kernel lays it out, in a job of its own.
struct SupervisorTable {
    job: Arc<kernel::sched::job::Job>,
    handles: HandleTable,
    entries: Vec<toyos_abi::syscall::EndowEntry>,
    labels: Vec<u8>,
}

impl Commit for SupervisorTable {
    type Held = ();

    fn job(&self) -> &Arc<kernel::sched::job::Job> {
        &self.job
    }

    fn pager(&self) -> bool {
        false
    }

    fn commit(self, own: start::Own) -> Result<(HandleTable, Endowments, ()), crate::object::Refusal> {
        let Self { mut handles, mut entries, mut labels, .. } = self;
        own.endow(&mut handles, &mut entries, &mut labels);
        Ok((handles, Endowments::new(entries, labels), ()))
    }
}
