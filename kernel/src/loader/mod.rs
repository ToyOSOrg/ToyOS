//! Building a process out of the ranges its parent laid out.
//!
//! The kernel parses no program. A parent reads its child's program and names
//! the child's memory as ranges ([`Program`], `toyos_abi::syscall::SpawnMap`),
//! each checked as an ordinary mapping is, and the entry point; the kernel
//! builds the space, places an empty stack and starts the main thread. What
//! the parent tells the child is bytes of a range, which the kernel never
//! reads. Everything else is the program's: it relocates itself
//! (`toyos::relocate`), lays out its own thread-local storage (`toyos::tls`)
//! and loads its own libraries (`toyos::dl`).
//!
//! A spawn that lands writes one record, `spawn: pid=N (total=Nms) <path>`,
//! once the process is in the table and placed, and nothing on the way
//! (`crate::process`'s header). The path is its caller's word, held to one
//! line (`toyos_abi::syscall::spawn_path`) and last, so neither what it says
//! nor a record's bound cutting it reaches the kernel's fields. A spawn that
//! is refused writes none: its error is the answer, and a caller chooses how
//! often it is refused.

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
    Admission, Parent, Pid, ProcessAccounting, ProcessData,
    ProcessEntry, ThreadData, ThreadEntry, UserImage, PROCESS_TABLE,
};
use crate::sync::Lock;
use crate::{scheduler, UserAddr};
use toyos_abi::handle::{RawHandle, Rights};
use crate::object::shm::SharedMemObject;
use toyos_abi::syscall::{SpawnBacking, SpawnMap, SyscallError};
use toyos_symbols::frame::BuildId;

/// The main thread's stack: committed whole, populated as it is touched.
const USER_STACK_SIZE: usize = 8 * 1024 * 1024;

/// The reserved range below the main stack.
const STACK_GUARD: u64 = 1024 * 1024;

/// One range of a child's memory, its numbers checked where the request was
/// read (`syscall::proc::spawn_program`): whole pages, of ordinary memory
/// where an object backs it. What a shared range's object gives is checked as
/// it is mapped (`SharedMemObject::mappable`).
pub struct Map {
    pub at: UserAddr,
    pub len: u64,
    pub prot: kernel::vm::Prot,
    pub backing: Backing,
}

/// What a range's pages are.
pub enum Backing {
    Zeros,
    /// Private, each starting as a copy of the object's from this page
    /// boundary for this many bytes, and zeros past them.
    Copy(Arc<dyn CopySource>, u64, u64),
    /// The object's own, from this page boundary.
    Share(Arc<SharedMemObject>, u64),
}

impl Map {
    /// The rights the handle naming `raw`'s object must carry: `MAP`, and
    /// `WRITE` beside it for a writable share. `InvalidArgument` for a word
    /// that names no protection or backing.
    pub fn rights(raw: &SpawnMap) -> Result<Rights, SyscallError> {
        let prot = crate::syscall::vm::vm_prot(raw.prot)?;
        match SpawnBacking::from_u64(raw.backing).ok_or(SyscallError::InvalidArgument)? {
            SpawnBacking::Copy => Ok(Rights::MAP),
            SpawnBacking::Share => Ok(crate::syscall::vm::map_rights(prot)),
        }
    }

    /// `raw`, checked as an ordinary mapping is: whole pages of the user half,
    /// a protection, and an object of ordinary memory from a page boundary,
    /// `None` for zeros.
    pub fn checked(raw: &SpawnMap, object: Option<Arc<SharedMemObject>>) -> Result<Map, SyscallError> {
        let page = crate::mm::PAGE_SIZE;
        let prot = crate::syscall::vm::vm_prot(raw.prot)?;
        let backing = SpawnBacking::from_u64(raw.backing).ok_or(SyscallError::InvalidArgument)?;
        let at = UserAddr::checked(raw.addr).filter(|at| at.raw().is_multiple_of(page));
        let ends = raw.addr.checked_add(raw.len).is_some_and(|end| end <= toyos_userbound::USER_TOP);
        let (Some(at), true) = (at, ends && raw.len != 0 && raw.len.is_multiple_of(page) && raw.copied <= raw.len) else {
            return Err(SyscallError::InvalidArgument);
        };
        let backing = match (object, backing) {
            (None, SpawnBacking::Copy) if raw.copied == 0 => Backing::Zeros,
            (None, _) => return Err(SyscallError::InvalidArgument),
            // Ordinary memory alone: a device aperture is no program, a read
            // of one is a device access, and its last handle takes it back
            // only from where its mapper said it is.
            (Some(object), _) if !raw.offset.is_multiple_of(page) || !object.is_ordinary() => {
                return Err(SyscallError::InvalidArgument);
            }
            // Each page is copied once, at its first touch, so a holder that
            // writes the object later changes only pages not yet read: its
            // own child's.
            (Some(object), SpawnBacking::Copy) => {
                let held = raw.offset.checked_add(raw.copied).ok_or(SyscallError::InvalidArgument)?;
                if held > object.size() {
                    return Err(SyscallError::InvalidArgument);
                }
                Backing::Copy(object as Arc<dyn CopySource>, raw.offset, raw.copied)
            }
            (Some(_), SpawnBacking::Share) if raw.copied != 0 => return Err(SyscallError::InvalidArgument),
            (Some(object), SpawnBacking::Share) => Backing::Share(object, raw.offset),
        };
        Ok(Map { at, len: raw.len, prot, backing })
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
        match &map.backing {
            Backing::Zeros => space.reserve(map.len, at, map.prot)?,
            Backing::Copy(source, offset, copied) => space.copy(map.len, at, map.prot, Arc::clone(source), *offset, *copied)?,
            Backing::Share(object, offset) => space.map(Arc::clone(object), *offset, map.len, map.prot, at)?,
        };
    }
    Ok(())
}

/// Build the process `program` describes and place its main thread under `parent`, answering its pid
/// and what `commit` left its caller holding of it. Everything built for the
/// child is counted to [`Commit::job`], which it runs in, and `commit` builds
/// the child's handle table around its handle to itself once nothing is left
/// to refuse.
///
/// `path` labels the program and `name` the child; the kernel resolves no path.
///
/// `Refusal`, not `-> !`, is the error type: every failure below owns a
/// partly built process (address space, stack, kernel stack), and nothing
/// unwinds, so the error must travel out as a value rather than strand it.
pub fn spawn<C: Commit>(
    path: &str,
    name: [u8; crate::process::THREAD_NAME_LEN],
    commit: C,
    program: Program,
    parent: Parent,
) -> Result<(Pid, C::Held), crate::object::Refusal> {
    let job = Arc::clone(commit.job());
    let pager = commit.pager();
    // Before anything is built; dropped on every way out below but the insert.
    let admission = Admission::ask(parent)?;
    let charge = ProcessData::charge(&job, path).map_err(SyscallError::from)?;
    let t0 = crate::clock::nanos_since_boot();

    let Program { maps, entry, image_base, build_id } = program;
    // Inside a range the child may execute, which `spawn_program` placed in the user half.
    let executes = |map: &Map| map.prot == kernel::vm::Prot::ReadExec && (map.at.raw()..map.at.raw() + map.len).contains(&entry);
    let Some(entry) = maps.iter().any(executes).then(|| toyos_userbound::Entry::new(entry)).flatten() else {
        return Err(SyscallError::InvalidArgument.into());
    };
    let start = maps.iter().map(|map| map.at.raw()).min().ok_or(SyscallError::InvalidArgument)?;
    let end = maps.iter().map(|map| map.at.raw() + map.len).max().ok_or(SyscallError::InvalidArgument)?;

    let Some(child_pt) = crate::mm::space::Space::new_user(&job) else {
        return Err(SyscallError::ResourceExhausted.into());
    };
    let (stack_base, sp) = {
        use kernel::vm::Prot;
        let mut space = child_pt.lock();
        crate::clock::map_page(&mut space);
        lay(&mut space, &maps)?;
        // Never executable, and a reserved guard below it, so running off the
        // stack's bottom faults instead of writing what lies there.
        let stack_base = space.reserve_above_guard(STACK_GUARD, USER_STACK_SIZE as u64, Prot::ReadWrite)?;
        (stack_base, stack_base.raw() + USER_STACK_SIZE as u64)
    };

    // What a crash record names a frame by.
    let image = Arc::new(UserImage { name: String::from(path), build_id, start, end, bias: image_base });

    let (ks_alloc, ks_sp) = match alloc_kernel_stack(&job, Start::Process { entry, sp }) {
        Some(ks) => ks,
        None => return Err(SyscallError::ResourceExhausted.into()),
    };

    let pid = admission.pid();
    let object = crate::object::process::ProcessObject::new(pid).map_err(SyscallError::from)?;
    let notice = crate::object::notice::NoticeObject::new().map_err(SyscallError::from)?;
    // The point of no return: every failure above answers the caller with its
    // table untouched.
    let (handles, held) = commit.commit(start::Own::new(&object, &notice))?;
    let handles = crate::process::Handles::new(handles, pager);
    let proc_data = Arc::new(Lock::new(ProcessData {
        handles: Arc::clone(&handles),
        space: Arc::clone(&child_pt),
        spawn_ns: crate::clock::nanos_since_boot(),
        accounting: ProcessAccounting::default(),
        job: job.clone(),
        _charge: charge,
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
            name,
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
            handles,
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
    log!("spawn: pid={} (total={}ms) {}", pid, (t3 - t0) / 1_000_000, path);

    Ok((pid, held))
}

/// The name the supervisor runs under and its label: no file, since it runs from its boot image.
const SUPERVISOR: &str = "supervisor";

/// Start the supervisor from its boot image as the build laid it out, holding the machine's one
/// full-rights `SysCap`, the file server's image, ROOT's and the loader's boot names, at the handles
/// `toyos_abi::boot::supervisor` states.
///
/// Nothing else can construct one: what the supervisor endows is the entire set of
/// processes that can ever claim a device, enter the RT band, or power off.
/// Panics on failure: a boot that cannot start the supervisor has nowhere to report to.
pub fn spawn_supervisor(images: crate::bootimage::Objects, laid: &toyos_kimage::Supervisor<'_>) -> Pid {
    use toyos_abi::boot::supervisor as at;
    let job = crate::object::job::new_job();
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
    let memory = |object: &Arc<SharedMemObject>, keeps: Option<Rights>| {
        let object = KObjectRef::SharedMem(Arc::clone(object));
        let rights = keeps.unwrap_or_else(|| ops::initial_rights(&object));
        crate::object::HandleEntry::new(object, rights)
    };
    // The two images go on to the file server; the names are the supervisor's to read.
    let endowed = [
        (at::SYSCAP, crate::object::HandleEntry::new(cap, rights)),
        (at::FILESERVER_IMAGE, memory(&images.fileserver, None)),
        (at::ROOT_IMAGE, memory(&images.root, None)),
        (at::BOOT_NAMES, memory(&images.names, Some(Rights::MAP))),
    ];
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
    let table = SupervisorTable { handles: HandleTable::new(&job), job, endowed };
    let name = toyos_abi::syscall::process_name(SUPERVISOR.as_bytes()).expect("spawn_supervisor: its own name");
    match spawn(SUPERVISOR, name, table, program, Parent::Root) {
        Ok((pid, ())) => pid,
        Err(crate::object::Refusal::Error(e)) => panic!("spawn_supervisor: failed to spawn: {e:?}"),
        Err(crate::object::Refusal::Handle(e)) => panic!("spawn_supervisor: {e}"),
    }
}

/// The supervisor's table as the kernel lays it out, in a job of its own.
struct SupervisorTable {
    job: Arc<kernel::sched::job::Job>,
    handles: HandleTable,
    /// What it holds beside its own two, each at the handle the ABI states.
    endowed: [(RawHandle, crate::object::HandleEntry); 4],
}

impl Commit for SupervisorTable {
    type Held = ();

    fn job(&self) -> &Arc<kernel::sched::job::Job> {
        &self.job
    }

    fn pager(&self) -> bool {
        false
    }

    fn commit(self, own: start::Own) -> Result<(HandleTable, ()), crate::object::Refusal> {
        let Self { mut handles, endowed, .. } = self;
        own.install(&mut handles);
        for (at, entry) in endowed {
            let handle = handles.install(entry).expect("spawn_supervisor: an empty table refused what the supervisor starts with");
            assert_eq!(handle, at, "spawn_supervisor: a new table issued its handles out of order");
        }
        Ok((handles, ()))
    }
}
