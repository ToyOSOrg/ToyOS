//! Building a process out of the ranges its parent laid out.
//!
//! The kernel parses no program. A parent reads its child's program and names
//! the child's memory as ranges ([`Program`], `toyos_abi::syscall::SpawnMap`),
//! each checked as an ordinary mapping is, the entry point and the stack's
//! arguments; the kernel builds the space, places the stack and starts the
//! main thread. Everything else is the program's: it relocates itself
//! (`toyos::relocate`), lays out its own thread-local storage (`toyos::tls`)
//! and loads its own libraries (`toyos::dl`).
//!
//! A spawn that lands writes one record, `spawn: <path> pid=N (…ms)`, once
//! the process is in the table and placed; a spawn that is refused writes
//! one, naming why. Nothing is said on the way (`crate::process`'s header).

// `warn`, not `deny`: the rest of the kernel is not yet swept for undocumented unsafe blocks.
#![warn(clippy::undocumented_unsafe_blocks)]

mod start;

pub use start::{build_child_handles, Commit, PendingHandles};
pub(crate) use start::{alloc_kernel_stack, KernelStack, Start};

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::object::{ops, HandleTable, KObjectRef};
use crate::mm::space::CopySource;
use crate::process::{
    Admission, Endowments, Parent, Pid, ProcessAccounting, ProcessData,
    ProcessEntry, ThreadData, ThreadEntry, UserImage, PROCESS_TABLE,
};
use crate::sync::Lock;
use crate::{scheduler, UserAddr};
use toyos_abi::handle::Rights;
use crate::object::shm::SharedMemObject;
use toyos_abi::syscall::{SpawnMap, SyscallError};
use toyos_symbols::frame::BuildId;

/// The main thread's stack: committed whole, populated as it is touched.
const USER_STACK_SIZE: usize = 8 * 1024 * 1024;

/// The reserved range below the main stack.
const STACK_GUARD: u64 = 1024 * 1024;

/// One range of a child's memory, its numbers checked where the request was
/// read (`syscall::proc::spawn_program`): whole pages, and a copy's bytes inside
/// an object of ordinary memory.
pub struct Map {
    pub at: UserAddr,
    pub len: u64,
    pub prot: kernel::vm::Prot,
    /// The object its pages start as a copy of, from which page boundary, and
    /// how many bytes; zeros past them, and `None` for zeros alone.
    pub copy: Option<(Arc<dyn CopySource>, u64, u64)>,
}

impl Map {
    /// `raw`, checked as an ordinary mapping is: whole pages of the user half,
    /// a protection, and a copy only of ordinary memory `object` holds from a
    /// page boundary, `object` being `None` for zeros.
    pub fn checked(raw: &SpawnMap, object: Option<Arc<SharedMemObject>>) -> Result<Map, SyscallError> {
        let page = crate::mm::PAGE_SIZE;
        let prot = crate::syscall::vm::vm_prot(raw.prot)?;
        let at = UserAddr::checked(raw.addr).filter(|at| at.raw().is_multiple_of(page));
        let ends = raw.addr.checked_add(raw.len).is_some_and(|end| end <= toyos_userbound::USER_TOP);
        let (Some(at), true) = (at, ends && raw.len != 0 && raw.len.is_multiple_of(page) && raw.copied <= raw.len) else {
            return Err(SyscallError::InvalidArgument);
        };
        let copy = match object {
            None if raw.copied == 0 => None,
            None => return Err(SyscallError::InvalidArgument),
            Some(_) if !raw.offset.is_multiple_of(page) => return Err(SyscallError::InvalidArgument),
            // Ordinary memory alone: a device aperture is no program, and a
            // read of one is a device access. Each page is copied once, at
            // its first touch, so a holder that writes the object later
            // changes only pages not yet read: its own child's.
            Some(object) => {
                let held = raw.offset.checked_add(raw.copied).ok_or(SyscallError::InvalidArgument)?;
                if held > object.size() || !object.is_ordinary() {
                    return Err(SyscallError::InvalidArgument);
                }
                Some((object as Arc<dyn CopySource>, raw.offset, raw.copied))
            }
        };
        Ok(Map { at, len: raw.len, prot, copy })
    }
}

/// What a parent laid out of its child's program.
pub struct Program {
    pub maps: Vec<Map>,
    pub entry: u64,
    /// Where the program's address 0 lands: what a crash record names a
    /// frame's offset from.
    pub image_base: u64,
    pub build_id: Option<BuildId>,
}

/// Lay `maps` into `space`, a fresh one holding nothing but the clock page.
/// `InvalidArgument` for a range that meets another or the clock page; a
/// refusal after the first range leaves ranges in a space nothing runs in yet.
fn lay(space: &mut crate::mm::space::AddressSpace, maps: &[Map]) -> Result<(), SyscallError> {
    use crate::mm::space::At;
    for map in maps {
        if !space.vacant(map.at.raw(), map.len) {
            return Err(SyscallError::InvalidArgument);
        }
        let at = At::Fixed(map.at);
        match &map.copy {
            Some((source, offset, copied)) => space.copy(map.len, at, map.prot, Arc::clone(source), *offset, *copied)?,
            None => space.reserve(map.len, at, map.prot)?,
        };
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

/// Build the process `program` describes and place its main thread under `parent`, answering its pid
/// and what `commit` left its caller holding of it. Everything built for the
/// child is counted to [`Commit::job`], which it runs in, and `commit` builds
/// the child's handle table around its handle to itself once nothing is left
/// to refuse.
///
/// `path` labels the program; the kernel resolves no path. `argv[0]` is the
/// name the child goes by.
///
/// `Refusal`, not `-> !`, is the error type: every failure below owns a
/// partly built process (address space, stack, kernel stack), and nothing
/// unwinds, so the error must travel out as a value rather than strand it.
pub fn spawn<C: Commit>(
    path: &str,
    argv: &[&str],
    commit: C,
    env: Vec<u8>,
    program: Program,
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
    let charge = ProcessData::charge(&job, path, &env).map_err(SyscallError::from)?;
    let t0 = crate::clock::nanos_since_boot();

    let Program { maps, entry, image_base, build_id } = program;
    // Inside a range the child may execute, which `spawn_program` placed in the user half.
    let executes = |map: &Map| map.prot == kernel::vm::Prot::ReadExec && (map.at.raw()..map.at.raw() + map.len).contains(&entry);
    let Some(entry) = maps.iter().any(executes).then(|| toyos_userbound::Entry::new(entry)).flatten() else {
        log!("spawn: {}: the entry {:#x} is in no executable range", path, entry);
        return Err(SyscallError::InvalidArgument.into());
    };
    let start = maps.iter().map(|map| map.at.raw()).min().ok_or(SyscallError::InvalidArgument)?;
    let end = maps.iter().map(|map| map.at.raw() + map.len).max().ok_or(SyscallError::InvalidArgument)?;

    let Some(child_pt) = crate::mm::space::Space::new_user(&job) else {
        log!("spawn: {}: no address space: every user PCID is held, or no frame is left for its root", path);
        return Err(SyscallError::ResourceExhausted.into());
    };
    let (stack_base, sp) = {
        use crate::mm::space::At;
        use kernel::vm::Prot;
        let mut space = child_pt.lock();
        crate::clock::map_page(&mut space);
        if let Err(e) = lay(&mut space, &maps) {
            log!("spawn: {}: a range meets another or the clock page, or is refused: {:?}", path, e);
            return Err(e.into());
        }
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
        (stack_base, sp)
    };

    // What a crash record names a frame by.
    let image = Arc::new(UserImage { name: String::from(path), build_id, start, end, bias: image_base });

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
        env,
        space: Arc::clone(&child_pt),
        spawn_ns: crate::clock::nanos_since_boot(),
        accounting: ProcessAccounting::default(),
        endowments,
        job: job.clone(),
        _charge: charge,
        pager,
    }));

    let thread_data = Arc::new(Lock::new(ThreadData {
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
            thread_pointer: 0,
            job,
            image,
            // The spawning thread's, as a new thread's is.
            mask: crate::sched::driver::current_mask(),
            near: None,
        });
        table.get_mut(pid).unwrap().threads_mut().get_mut(&tid).unwrap().set_sched(sched);
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
    log!("spawn: {} pid={} (total={}ms)", path, pid, (t3 - t0) / 1_000_000);

    Ok((pid, held))
}

/// The name the supervisor runs under: no file, since it runs from its boot image.
const SUPERVISOR: &str = "supervisor";

/// Start the supervisor from its boot image as the build laid it out, holding the machine's one
/// full-rights `SysCap` and the file server's image and ROOT's
/// (`toyos_abi::syscall::FILESERVER_IMAGE_LABEL`, `ROOT_IMAGE_LABEL`).
///
/// Nothing else can construct one: what the supervisor endows is the entire set of
/// processes that can ever claim a device, enter the RT band, or power off.
/// Panics on failure: a boot that cannot start the supervisor has nowhere to report to.
pub fn spawn_supervisor(images: crate::bootimage::Objects, laid: &toyos_kimage::Supervisor<'_>) -> Pid {
    let job = crate::object::job::new_job();
    let mut handles = HandleTable::new(&job);
    let mut entries = Vec::new();
    let mut labels = Vec::new();
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
        .union(Rights::PAGER)
        .union(Rights::CONSOLE)
        .union(Rights::LAST_WORDS);
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
            .expect("spawn_supervisor: a table of three handles refused a boot image");
        entries.push(toyos_abi::syscall::EndowEntry {
            label_off: labels.len() as u32,
            label_len: label.len() as u32,
            handle,
            _pad: 0,
        });
        labels.extend_from_slice(label.as_bytes());
    }
    // Its ranges as the build laid them out of the supervisor section
    // (`toyos_kimage`), checked as a spawn's are.
    let maps = laid.maps().map(|raw| {
        let object = (raw.object == toyos_kimage::SUPERVISOR_OBJECT).then(|| Arc::clone(&images.supervisor));
        Map::checked(&raw, object).unwrap_or_else(|e| panic!("spawn_supervisor: the build's range {raw:?}: {e:?}"))
    });
    let program = Program {
        maps: maps.collect(),
        entry: laid.entry,
        image_base: laid.image_base,
        build_id: BuildId::new(laid.build_id),
    };
    // Built by the kernel and owing nobody anything: no table but the supervisor's own holds it.
    let table = SupervisorTable { job, handles, entries, labels };
    match spawn(SUPERVISOR, &[SUPERVISOR], table, Vec::new(), program, Parent::Root) {
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
