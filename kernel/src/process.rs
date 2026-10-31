//! The process table, and what a process is made of.
//!
//! [`dump_crash_diagnostics`] never waits on [`PROCESS_TABLE`]: the faulting
//! thread may hold it. A crash report records a
//! user frame off the task's own record ([`record_user_frame`]), never off
//! this table.
//!
//! `toyos_proclife` decides lifecycle transitions; this file only performs them.
//!
//! **What a process writes into the log is two records**: one where its spawn
//! lands (`crate::loader`) and one where it ends, `exit: <name> pid=N
//! code=N|killed|fault=<kind> cpu=Nms …`, the verdict first and then what that process itself consumed.
//! Each is written every time and charged to the process it names: nothing
//! here is rate-limited, and a thread's end writes nothing. A reading of the
//! whole machine is `crate::census`'s, taken once, where the machine ends, by
//! its stop or by its death, and no process's start or end repeats one: the
//! log's volume is then a function of what ran, never of how many CPUs
//! watched it.
//!
//! Two records written where a process ends are another owner's and are
//! charged to it, not to the process: a device function whose ports the
//! process held says they went back (`crate::isa::process_ends`, the pair of
//! the record its claim wrote when it bound them), and a fault that ends a
//! process writes the fault's report ([`dump_crash_diagnostics`]), which is
//! several records because it is a crash and not an end.

use alloc::alloc::{alloc_zeroed, dealloc, Layout};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::NonNull;
use crate::arch::percpu;
use crate::mm::charge::{self, Charge, Exhausted};
use crate::object::{ops, HandleTable};
use crate::sync::Lock;
use crate::sched::payload::ThreadSched;
use crate::time::{Deadline, Duration};
use crate::{elf, pipe, scheduler};
use crate::object::shm::SharedMemObject;
use crate::UserAddr;
use crate::loader::{alloc_kernel_stack, Start, TlsBlock};

pub use toyos_abi::{Pid, Tid};
pub use crate::scheduler::TaskId;
use toyos_abi::end::{End, Fault};
use toyos_abi::syscall::{EndowEntry, SyscallError, MAX_LIBRARIES};

/// The lifecycle's decisions; this file only performs them.
pub use toyos_proclife::{ThreadLocation, Watch};
use toyos_proclife::quit::{self, Notice, Reason};
use toyos_proclife::{join, reap, spawn as proclife_spawn, teardown as proclife, tree, Lifecycle, Node, Processes};

/// One `EndowEntry` on the wire; `loader::start` and [`Endowments::encode`] both index by it.
pub const ENDOW_ENTRY_LEN: usize = core::mem::size_of::<EndowEntry>();

pub use crate::loader::{build_child_handles, spawn, spawn_supervisor};

/// A process's space, which every thread of it runs in.
pub use crate::mm::space::PageTables;

/// Move-only wrapper around a heap allocation, freed on drop; not for user-mapped pages.
pub struct OwnedAlloc {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl OwnedAlloc {
    /// `None` above `mm::MAX_HEAP_ALLOC` — not `PAGE_2M`, which dlmalloc's own overhead can still overrun for a `PT_TLS` block near that size.
    pub fn new(size: usize, align: usize) -> Option<Self> {
        if size > crate::mm::MAX_HEAP_ALLOC { return None; }
        let layout = Layout::from_size_align(size, align).ok()?;
        // SAFETY: `layout` has non-zero size (checked above); a null return becomes `None` via `NonNull::new` rather than a dangling `OwnedAlloc`.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(Self { ptr, layout })
    }

    pub fn ptr(&self) -> *mut u8 { self.ptr.as_ptr() }
    pub fn size(&self) -> usize { self.layout.size() }

    /// A bounds-checked view of the first `len` bytes; `len` is checked against the allocation, not trusted.
    pub fn slice(&self, len: usize) -> crate::mm::KernelSlice {
        crate::mm::KernelSlice::whole(self).subslice(0, len)
    }
}

// SAFETY: `ptr()`/`size()` read the same `Layout` `alloc_zeroed` used; the type is move-only and frees once in `Drop`.
unsafe impl crate::mm::Allocation for OwnedAlloc {
    fn ptr(&self) -> *mut u8 { OwnedAlloc::ptr(self) }
    fn size(&self) -> usize { OwnedAlloc::size(self) }
}

impl Drop for OwnedAlloc {
    fn drop(&mut self) {
        // SAFETY: same `ptr`/`layout` pair `new` produced; freed exactly once (move-only, no `Clone`).
        unsafe { dealloc(self.ptr.as_ptr(), self.layout); }
    }
}

// SAFETY: sole owner of the allocation moves with `self`; nothing else can reach `ptr`.
unsafe impl Send for OwnedAlloc {}


/// An object the kernel mapped into a process for it and takes back itself: a
/// thread's TLS block, a `dlopen`ed module's block for one thread, an inbox's
/// rings. The kernel writes what it owes the process into the object.
pub struct KernelMapping {
    pub object: Arc<SharedMemObject>,
    pub at: UserAddr,
}

impl KernelMapping {
    /// Take the mapping out of `space`, unless the process replaced it.
    pub fn release(&self, space: &PageTables) {
        space.release_object(self.at, self.object.size(), &self.object);
    }
}

pub const KERNEL_STACK_SIZE: usize = 128 * 1024;

/// How much of a process or thread name the table keeps, fixed by the `SYS_SYSINFO` record's name field.
pub const THREAD_NAME_LEN: usize = 28;

/// Per-thread metadata. Tid is the HashMap key in ProcessEntry.threads.
pub struct ThreadEntry {
    state: ThreadLocation,
    name: [u8; THREAD_NAME_LEN],
    thread_data: Arc<Lock<ThreadData>>,
    /// `None` only between the table insert allocating the tid and the `sched::spawn` that creates the task.
    sched: Option<ThreadSched>,
}

impl ThreadEntry {
    pub fn new(thread_data: Arc<Lock<ThreadData>>) -> Self {
        Self { state: ThreadLocation::Scheduled, name: [0u8; THREAD_NAME_LEN], thread_data, sched: None }
    }
    pub fn set_sched(&mut self, sched: ThreadSched) {
        assert!(self.sched.is_none(), "thread already has a scheduler record");
        self.sched = Some(sched);
    }
    pub fn sched(&self) -> Option<&ThreadSched> { self.sched.as_ref() }
    pub fn state(&self) -> ThreadLocation { self.state }
    pub fn name(&self) -> &[u8; THREAD_NAME_LEN] { &self.name }
    pub fn set_name(&mut self, name: &[u8]) {
        self.name = [0u8; THREAD_NAME_LEN];
        let len = name.len().min(THREAD_NAME_LEN);
        self.name[..len].copy_from_slice(&name[..len]);
    }
}

/// A process and all its threads. Removing a ProcessEntry removes all threads.
pub struct ProcessEntry {
    pid: Pid,
    /// The thing a handle to this process names; outlives the entry for as long as somebody still holds a handle.
    object: Arc<crate::object::process::ProcessObject>,
    name: [u8; THREAD_NAME_LEN],
    process_data: Arc<Lock<ProcessData>>,
    /// No `Lock`: written once by the loader, then read-only — a crash report reaches it through the task it is reporting on, never through this table. `None` for a kernel thread's.
    image: Option<Arc<UserImage>>,
    main_tid: Tid,
    threads: crate::id_map::IdMap<Tid, ThreadEntry>,
    /// Set once, with the end, by the exit or kill that claims teardown; checked by `spawn_thread` so no thread appears after the retire set.
    teardown_end: Option<End>,
    /// Its parent, children and depth, answered by no syscall; `toyos_proclife::tree` reads and writes it.
    node: Node,
    /// The exit its teardown made, kept until every end below it is published.
    exit: Option<crate::object::process::Exit>,
    /// What a quit asks it, which its own table holds the one handle to.
    notice: Arc<crate::object::notice::NoticeObject>,
}

impl ProcessEntry {
    /// Create a new process with its main thread (always `Tid(0)`), at `object`'s pid.
    pub fn new(
        object: Arc<crate::object::process::ProcessObject>,
        name: [u8; THREAD_NAME_LEN],
        process_data: Arc<Lock<ProcessData>>,
        image: Option<Arc<UserImage>>,
        main_thread: ThreadEntry,
        node: Node,
        notice: Arc<crate::object::notice::NoticeObject>,
    ) -> Self {
        let mut threads = crate::id_map::IdMap::new();
        let main_tid = threads.insert(main_thread);
        Self {
            pid: object.pid(),
            object,
            name,
            process_data,
            image,
            main_tid,
            threads,
            teardown_end: None,
            node,
            exit: None,
            notice,
        }
    }
    pub fn pid(&self) -> Pid { self.pid }
    pub fn name(&self) -> &[u8; THREAD_NAME_LEN] { &self.name }
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name).unwrap_or("?").trim_end_matches('\0')
    }
    pub fn process_data(&self) -> &Arc<Lock<ProcessData>> { &self.process_data }
    pub fn main_tid(&self) -> Tid { self.main_tid }
    pub fn threads(&self) -> &crate::id_map::IdMap<Tid, ThreadEntry> { &self.threads }
    pub fn threads_mut(&mut self) -> &mut crate::id_map::IdMap<Tid, ThreadEntry> { &mut self.threads }
}

impl ProcessEntry {
    /// Mirrors [`Lifecycle::tearing_down`], usable without the trait in scope.
    pub fn tearing_down(&self) -> bool { self.teardown_end.is_some() }
}

/// The lifecycle face of an entry: only the fields `toyos_proclife` decides against, and nothing else this type carries.
impl Lifecycle for ProcessEntry {
    fn main_tid(&self) -> Tid { self.main_tid }
    fn teardown_end(&self) -> Option<End> { self.teardown_end }
    fn begin_teardown(&mut self, end: End) { self.teardown_end = Some(end); }
    fn location(&self, tid: Tid) -> Option<ThreadLocation> {
        self.threads.get(tid).map(|t| t.state)
    }
    fn set_location(&mut self, tid: Tid, to: ThreadLocation) {
        if let Some(thread) = self.threads.get_mut(tid) {
            thread.state = to;
        }
    }
    fn forget_thread(&mut self, tid: Tid) {
        self.threads.remove(tid);
    }
    fn each_thread(&self, f: &mut dyn FnMut(Tid, ThreadLocation)) {
        for (tid, thread) in self.threads.iter() {
            f(tid, thread.state);
        }
    }
    fn thread_count(&self) -> usize {
        self.threads.len()
    }
    fn node(&self) -> &Node { &self.node }
    fn node_mut(&mut self) -> &mut Node { &mut self.node }
}

/// The lifecycle face of the table; `published_exit` comes off the [`ProcessObject`](crate::object::process::ProcessObject) rather than the table, since it outlives the entry.
impl Processes for ProcessTable {
    type Proc = ProcessEntry;

    fn get(&self, pid: Pid) -> Option<&ProcessEntry> {
        self.entries.get(&pid)
    }
    fn get_mut(&mut self, pid: Pid) -> Option<&mut ProcessEntry> {
        self.entries.get_mut(&pid)
    }
    fn published_exit(&self, pid: Pid) -> bool {
        self.entries.get(&pid).is_some_and(|p| p.object.finished())
    }
    fn each_pid(&self, f: &mut dyn FnMut(Pid)) {
        for &pid in self.entries.keys() {
            f(pid);
        }
    }
    fn pids(&mut self) -> &mut toyos_proclife::Pids {
        &mut self.pids
    }
}


/// ELF loading artifacts and TLS state.
pub struct ElfInfo {
    pub elf_alloc: Option<OwnedAlloc>,
    pub tls_modules: Vec<crate::elf::TlsModule>,
    /// Every static module's TLS, as a thread's block is laid out from it.
    pub tls: toyos_elf::tls::Static,
    /// Next module ID to assign on dlopen (1-based, exe=1).
    pub next_tls_module_id: u64,
    /// Dynamically allocated TLS blocks for dlopen'd modules, keyed by
    /// (Tid, module_id), each with the charge of its place here.
    pub dynamic_tls_blocks: alloc::collections::BTreeMap<(Tid, u64), (KernelMapping, Charge)>,
    /// Dynamically loaded shared libraries (indexed by dlopen handle).
    pub loaded_libs: Vec<elf::LoadedLib>,
    pub elf_base: UserAddr,
    /// The executable's `.eh_frame_hdr` as (address, size), `(0, 0)` without one.
    pub exe_eh_frame_hdr: (u64, u64),
    /// One past the executable's last byte.
    pub exe_vaddr_max: u64,
    /// The executable's program header table as (address, count), `(0, 0)` with no executable.
    pub exe_phdrs: (u64, u16),
    /// Paths of dlopen'd libraries (parallel to loaded_libs).
    pub lib_paths: Vec<String>,
}

// The ledger stops at `MAX_LIBRARIES`, and a doubling from below it stays inside one heap allocation.
const _: () = assert!(2 * MAX_LIBRARIES * core::mem::size_of::<elf::LoadedLib>() <= crate::mm::MAX_HEAP_ALLOC);

impl ElfInfo {
    /// The state of a process with no ELF at all (a kernel thread). Not `Default`: `next_tls_module_id`'s only honest default is 1, not 0; written here, not at the one call site, so a field added to [`ElfInfo`] stops this build too.
    pub fn none() -> Self {
        Self {
            elf_alloc: None,
            tls_modules: Vec::new(),
            tls: toyos_elf::tls::Static::empty(crate::loader::TLS_VARIANT),
            next_tls_module_id: 1,
            dynamic_tls_blocks: alloc::collections::BTreeMap::new(),
            loaded_libs: Vec::new(),
            elf_base: UserAddr::new(0),
            exe_eh_frame_hdr: (0, 0),
            exe_vaddr_max: 0,
            exe_phdrs: (0, 0),
            lib_paths: Vec::new(),
        }
    }
}

/// Process-level data shared across all threads via `Arc<Lock<ProcessData>>`.
pub struct ProcessData {
    /// Every kernel object this process can name; stdio is handles `0`, `1`, `2`.
    pub handles: HandleTable,
    pub cwd: String,
    /// Inherited environment variables (KEY=VALUE\0KEY2=VALUE2\0...)
    pub env: Vec<u8>,

    pub elf: ElfInfo,

    /// The space every thread of it runs in.
    pub space: PageTables,
    /// Executable path (for SYS_QUERY_MODULES).
    pub exe_path: String,

    pub spawn_ns: u64,
    pub accounting: ProcessAccounting,
    /// What this process's parent called each handle it moved in at spawn.
    pub endowments: Endowments,
    /// The job its threads, and its children unless a spawn names another,
    /// run in.
    pub job: Arc<kernel::sched::job::Job>,
    /// What [`ProcessData::charge`] charged, and its executable's TLS beside.
    pub charge: Charge,
    /// Whether it was started as a pager, which no call changes, and whether
    /// it made its one pager object.
    pub pager: PagerRole,
}

/// A process's part in paged objects (`object::paged`): set at its spawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PagerRole {
    None,
    /// Started as a pager; it has not made its pager object.
    May,
    /// It made it.
    Made,
}

impl PagerRole {
    pub fn is_pager(self) -> bool {
        self != PagerRole::None
    }
}

impl ProcessData {
    /// The heap a process of the program at `path` takes beside what is
    /// charged on its own — its handles, threads, ranges and objects: its
    /// records, its place in the process table and among its parent's
    /// children, its strings at their bound, and its endowments at theirs.
    pub fn charge(job: &Arc<kernel::sched::job::Job>, path: &str, env: &[u8]) -> Result<Charge, Exhausted> {
        // The process table doubles, and its control byte beside each.
        let entry = 2 * (core::mem::size_of::<(Pid, ProcessEntry)>() + 1);
        // A thread table of one thread is a whole leaf.
        let threads = charge::heap(Layout::array::<(Tid, ThreadEntry)>(11).expect("process: a leaf's layout"));
        let records = charge::arc::<Lock<ProcessData>>()
            + charge::arc::<UserImage>()
            + charge::arc::<crate::mm::space::Space>()
            + charge::arc::<crate::mm::space::Usage>()
            + entry
            + threads
            + 2 * core::mem::size_of::<Pid>();
        let strings = 2 * charge::array::<u8>(path.len())
            + charge::array::<u8>(toyos_abi::syscall::MAX_PATH)
            + charge::array::<u8>(env.len());
        let endowments = charge::array::<EndowEntry>(toyos_abi::syscall::MAX_ENDOWMENTS)
            + charge::array::<u8>(toyos_abi::syscall::MAX_LABELS_LEN);
        Charge::new(job, records + strings + endowments)
    }
}

/// The labels a parent put on the handles it endowed — names, not authority: the handle is in the table whether or not the child ever asks.
pub struct Endowments {
    entries: alloc::boxed::Box<[EndowEntry]>,
    labels: alloc::boxed::Box<[u8]>,
}

impl Endowments {
    pub fn empty() -> Self {
        Self { entries: alloc::boxed::Box::new([]), labels: alloc::boxed::Box::new([]) }
    }

    pub fn new(entries: Vec<EndowEntry>, labels: Vec<u8>) -> Self {
        Self { entries: entries.into_boxed_slice(), labels: labels.into_boxed_slice() }
    }

    /// Bytes `SYS_ENDOWMENTS` answers with: a `u64` count, the entries, then the blob their offsets index.
    pub fn encoded_len(&self) -> usize {
        8 + self.entries.len() * ENDOW_ENTRY_LEN + self.labels.len()
    }

    /// Render into `out`, sized at [`Self::encoded_len`]. Field by field, not a slice transmute: `EndowEntry`'s padding word is written as zero here, so nothing of the kernel's reaches a child in it.
    pub fn encode(&self, out: &mut [u8]) {
        out[..8].copy_from_slice(&(self.entries.len() as u64).to_ne_bytes());
        for (i, entry) in self.entries.iter().enumerate() {
            let at = 8 + i * ENDOW_ENTRY_LEN;
            out[at..at + 4].copy_from_slice(&entry.label_off.to_ne_bytes());
            out[at + 4..at + 8].copy_from_slice(&entry.label_len.to_ne_bytes());
            out[at + 8..at + 12].copy_from_slice(&entry.handle.0.to_ne_bytes());
            out[at + 12..at + 16].copy_from_slice(&0u32.to_ne_bytes());
        }
        let blob = 8 + self.entries.len() * ENDOW_ENTRY_LEN;
        out[blob..blob + self.labels.len()].copy_from_slice(&self.labels);
    }
}

/// Per-process accounting counters. Accumulated from all threads on exit.
#[derive(Default)]
pub struct ProcessAccounting {
    pub blocked_io_ns: u64,
    pub blocked_futex_ns: u64,
    pub blocked_pipe_ns: u64,
    pub blocked_ipc_ns: u64,
    pub blocked_other_ns: u64,
    pub runqueue_wait_ns: u64,
}

/// Per-thread data, unique to each thread via `Arc<Lock<ThreadData>>`.
pub struct ThreadData {
    pub tls: Option<KernelMapping>,
    pub user_stack_base: UserAddr,
    pub user_stack_size: u64,
}

/// Zero-sized proof of running on the per-CPU idle stack; required by `collect_orphan_zombies` so it never drops the thread entry it runs on.
#[derive(Clone, Copy)]
pub struct IdleProof(());

impl IdleProof {
    /// # Safety
    /// Caller must be running on the idle stack.
    pub(crate) unsafe fn new_unchecked() -> Self { Self(()) }
}


/// Every process by pid, and the pids not yet issued.
pub struct ProcessTable {
    entries: crate::hasher::HashMap<Pid, ProcessEntry>,
    pids: toyos_proclife::Pids,
}

impl ProcessTable {
    fn new() -> Self {
        Self { entries: crate::hasher::HashMap::default(), pids: toyos_proclife::Pids::default() }
    }

    pub fn get(&self, pid: Pid) -> Option<&ProcessEntry> {
        self.entries.get(&pid)
    }

    pub fn get_mut(&mut self, pid: Pid) -> Option<&mut ProcessEntry> {
        self.entries.get_mut(&pid)
    }

    pub fn remove(&mut self, pid: Pid) -> Option<ProcessEntry> {
        self.entries.remove(&pid)
    }

    pub fn iter(&self) -> impl Iterator<Item = (Pid, &ProcessEntry)> {
        self.entries.iter().map(|(&pid, proc)| (pid, proc))
    }

    /// `entry`, at the pid it was admitted with, which no entry has held.
    pub fn insert(&mut self, entry: ProcessEntry) {
        let pid = entry.pid;
        assert!(self.entries.insert(pid, entry).is_none(), "ProcessTable::insert: pid {pid} held twice");
    }
}

pub static PROCESS_TABLE: Lock<Option<ProcessTable>> = Lock::new(None);

pub fn init() {
    *PROCESS_TABLE.lock() = Some(ProcessTable::new());
}

/// Accounting for a process.
pub fn stats_of(
    object: &crate::object::process::ProcessObject,
) -> Option<toyos_abi::syscall::ProcessStats> {
    if let Some(stats) = object.final_stats() {
        return Some(stats);
    }
    let pid = object.pid();
    // The other two locks are taken after this one is dropped, so no ordering edge.
    let (data_arc, cpu_ns, syscall_total, syscall_total_ns) = {
        let guard = PROCESS_TABLE.lock();
        let proc = guard.as_ref()?.get(pid)?;
        let (mut cpu_ns, mut syscall_total, mut syscall_total_ns) = (0, 0, 0);
        for sched in proc.threads().iter().filter_map(|(_, thread)| thread.sched()) {
            cpu_ns += scheduler::task_cpu_ns(sched);
            syscall_total += sched.handle.syscalls().calls();
            syscall_total_ns += sched.handle.syscalls().ns();
        }
        (Arc::clone(proc.process_data()), cpu_ns, syscall_total, syscall_total_ns)
    };
    let data = data_arc.lock();
    Some(stats_from(&data, pid, cpu_ns, syscall_total, syscall_total_ns))
}

/// Take every entry whose process has published its exit.
/// Entries come back rather than being dropped here: the caller holds the table lock, and an entry's drop releases what its process held.
#[must_use = "the reaped entries must be dropped outside the table lock"]
pub fn reap_finished(table: &mut ProcessTable, _proof: IdleProof) -> Vec<ProcessEntry> {
    reap::finished_pids(table).into_iter().filter_map(|pid| table.remove(pid)).collect()
}

pub fn current_tid() -> Tid {
    percpu::current_tid().expect("current_tid() called during idle (no thread running)")
}

pub fn current_process() -> Pid {
    percpu::current_pid().expect("current_process() called during idle (no thread running)")
}

pub fn current_address_space() -> PageTables {
    scheduler::current_address_space().expect("current_address_space: no address space")
}


/// Get the current thread's ThreadData Arc (brief table lock).
pub fn current_data() -> Arc<Lock<ThreadData>> {
    let guard = PROCESS_TABLE.lock();
    let thread = guard.as_ref().unwrap()
        .get(current_process())
        .and_then(|p| p.threads.get(current_tid()))
        .expect("current_data: an entry outlives every thread that has not left it");
    Arc::clone(&thread.thread_data)
}

/// Set the name of the currently running thread.
pub fn set_current_thread_name(name: &[u8]) {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().unwrap();
    if let Some(proc) = table.get_mut(current_process()) {
        if let Some(thread) = proc.threads.get_mut(current_tid()) {
            thread.set_name(name);
        }
    }
}

/// Get the process-level ProcessData Arc (brief table lock); shared by every thread of the process.
pub fn process_data() -> Arc<Lock<ProcessData>> {
    let guard = PROCESS_TABLE.lock();
    let proc = guard.as_ref().unwrap()
        .get(current_process())
        .expect("process_data: an entry outlives every thread that has not left it");
    Arc::clone(&proc.process_data)
}

/// Access the current thread's ThreadData mutably; the table lock is not held during the closure.
pub fn with_current_data<R>(f: impl FnOnce(&mut ThreadData) -> R) -> R {
    let arc = current_data();
    let mut guard = arc.lock();
    f(&mut guard)
}

/// Access the process-level ProcessData mutably; the table lock is not held during the closure.
pub fn with_process_data<R>(f: impl FnOnce(&mut ProcessData) -> R) -> R {
    let arc = process_data();
    let mut guard = arc.lock();
    f(&mut guard)
}

/// Whom a new process is placed under.
#[derive(Clone, Copy)]
pub enum Parent {
    /// No process: the supervisor, which the kernel starts.
    Root,
    /// The spawner, or the process whose `self` its place named.
    Under(Pid),
}

/// A spawn admitted under its parent, holding the pid its child will have and the parent's publication: [`Admission::land`] hands both to the child, and a drop — every way a spawn's build fails — gives both back.
pub struct Admission(Option<tree::Admitted>);

impl Admission {
    /// The question at the top of a spawn, before anything is built: `Gone` for a parent being torn down, and `ResourceExhausted`, which the log names, for a child more than `MAX_DEPTH` below the supervisor or no pid left to issue.
    pub fn ask(parent: Parent) -> Result<Self, SyscallError> {
        let place = match parent {
            Parent::Root => None,
            Parent::Under(place) => Some(place),
        };
        let admit = {
            let mut guard = PROCESS_TABLE.lock();
            tree::admit_child(guard.as_mut().unwrap(), place)
        };
        match admit {
            tree::Admit::Yes(admitted) => Ok(Self(Some(admitted))),
            tree::Admit::Gone => Err(SyscallError::Gone),
            tree::Admit::TooDeep { depth } => {
                log!("spawn: refused under pid {} at depth {depth}, more than {} below the supervisor",
                    place.expect("spawn: the supervisor is admitted at depth 0"), toyos_proclife::MAX_DEPTH);
                Err(SyscallError::ResourceExhausted)
            }
            tree::Admit::NoPid => {
                log!("spawn: refused, every pid below {} is issued", Pid::MAX);
                Err(SyscallError::ResourceExhausted)
            }
        }
    }

    pub fn pid(&self) -> Pid {
        self.0.as_ref().expect("Admission: taken only by land, which consumes it").pid()
    }

    /// Land the child in the `PROCESS_TABLE` hold `insert` puts its entry in and schedules its thread in. A parent claimed since the admission has walked its children, so the child is claimed in this hold too, and the retires answered are the spawner's to post with the lock given up.
    pub fn land<R>(
        mut self,
        table: &mut ProcessTable,
        insert: impl FnOnce(&mut ProcessTable, Node) -> R,
    ) -> (R, Vec<ThreadSched>) {
        let admitted = self.0.take().expect("Admission: landed once");
        let pid = admitted.pid();
        let (inserted, retire) = tree::land_child(table, admitted, End::Killed, insert);
        let child = table.get(pid).expect("Admission::land: the child inserted in this hold");
        if let Some(parent) = child.node.parent() {
            let held = table.get(parent).expect("Admission::land: a landed child's parent is unpublished").notice.with(|n| *n);
            child.notice.with(|n| *n = Notice::under(&held));
        }
        (inserted, scheds(table, pid, retire))
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        let Some(admitted) = self.0.take() else { return };
        let ready = {
            let mut guard = PROCESS_TABLE.lock();
            tree::refuse_child(guard.as_mut().unwrap(), admitted)
        };
        publish_climb(ready);
    }
}

/// Spawn a thread within the current process.
pub fn spawn_thread(
    entry: toyos_userbound::Entry,
    stack_ptr: u64,
    arg: u64,
    stack_base: u64,
) -> Option<Tid> {
    // Phase 1: parent's data + address space (table lock dropped after).
    let parent_process = current_process();
    let (parent_addr_space, process_data_arc) = {
        let guard = PROCESS_TABLE.lock();
        let table = guard.as_ref().unwrap();
        // Not `is_yes()`: a missing entry here means this thread's own process was reaped out from under it, which panics rather than refuses.
        match proclife_spawn::admit_thread_start(table, parent_process) {
            proclife_spawn::Admit::Yes => {}
            proclife_spawn::Admit::TearingDown | proclife_spawn::Admit::Full => return None,
            proclife_spawn::Admit::NoSuchProcess => {
                panic!("spawn_thread: pid {parent_process} is spawning and is not in the table")
            }
        }
        let proc = table.get(parent_process)
            .expect("spawn_thread: the entry the admission just answered for");
        let addr_space = scheduler::current_address_space()
            .expect("spawn_thread: the spawning thread runs in an address space");
        (addr_space, Arc::clone(&proc.process_data))
    };
    let (tls_modules, tls, job) = {
        let data = process_data_arc.lock();
        (data.elf.tls_modules.clone(), data.elf.tls, data.job.clone())
    };

    // Phase 2: build the TLS block outside any lock, then publish it into the
    // running parent — whose other threads may already be touching memory the
    // block's address is chosen from, so every pointer in it is final before
    // the mapping exists.
    let block = TlsBlock::build(&tls_modules, tls)?;
    // Placement or memory refused is a resource failure the process caused,
    // not a kernel bug.
    let (tls_alloc, thread_pointer, tp_offset) = block.publish(&parent_addr_space)?;

    let (ks_alloc, ks_sp) = match alloc_kernel_stack(&job, Start::Thread { entry, sp: stack_ptr, arg }) {
        Some(ks) => ks,
        None => {
            tls_alloc.release(&parent_addr_space);
            return None;
        }
    };

    // Phase 3: insert into table (brief table lock); threads share the parent's ProcessData Arc.
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().unwrap();
    // Re-checked under the insert lock: a thread refused here would otherwise be invisible to a retire sweep already under way.
    if !proclife_spawn::admit_thread_insert(table, parent_process).is_yes() {
        // The unmap cannot run under the table lock (its shootdown is a wait a
        // sibling can be spinning against), so the mapping is
        // still solely this scope's here — built into a ThreadData only past
        // the admission, where the table owns its release.
        drop(guard);
        tls_alloc.release(&parent_addr_space);
        return None;
    }
    let tcb = Arc::clone(&tls_alloc.object);
    let thread_data = Arc::new(Lock::new(ThreadData {
        tls: Some(tls_alloc),
        user_stack_base: UserAddr::new(stack_base),
        user_stack_size: if stack_base > 0 { stack_ptr - stack_base } else { 0 },
    }));
    let proc = table.get_mut(parent_process)
        .expect("spawn_thread: the entry the insert admission just answered for");
    // Every thread carries the same image, so a crash report never asks this table.
    let image = proc.image.clone();
    let tid = proc.threads.insert(ThreadEntry::new(thread_data));
    // Before the thread's first instruction, which is the enqueue below: the
    // thread reads its own id here without a syscall (`toyos_abi::TCB_TID`).
    tcb.write((tp_offset + toyos_abi::TCB_TID) as u64, &tid.raw().to_ne_bytes());

    // Enqueue while still holding the table lock: this thread is fully visible to the scheduler before any retire sweep can start.
    let (sched, _dst) = scheduler::enqueue_new(
        TaskId(parent_process, tid),
        ks_alloc,
        ks_sp,
        parent_addr_space,
        thread_pointer,
        image,
        job,
    );
    proc.threads.get_mut(tid).unwrap().set_sched(sched);
    drop(guard);
    Some(tid)
}


/// What a process's exit record says it consumed beside its CPU time: its memory, and its main thread's syscalls.
struct Consumed {
    peak_memory: u64,
    alloc_count: u64,
    free_count: u64,
    syscall_total: u64,
    syscall_total_ns: u64,
    syscall_counts: [u32; toyos_abi::syscall::SYSCALL_PROFILE_BINS],
}

impl core::fmt::Display for Consumed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "peak={}MB allocs={} frees={} syscalls={} syscall_wall={}ms",
            self.peak_memory / (1024 * 1024),
            self.alloc_count,
            self.free_count,
            self.syscall_total,
            self.syscall_total_ns / 1_000_000,
        )?;
        for (number, &count) in self.syscall_counts.iter().enumerate() {
            if count > 0 {
                write!(f, " {number}={count}")?;
            }
        }
        Ok(())
    }
}

/// Frees an exiting process's resources (mappings, handles, ELF state), and answers what it consumed.
/// Says nothing of the machine: a reading of every CPU is `crate::census`'s, never one process's end.
fn teardown_resources(
    process_data_arc: &Arc<Lock<ProcessData>>,
    main_thread: &(Arc<Lock<ThreadData>>, Option<ThreadSched>),
    pid: Pid,
) -> Consumed {
    let (thread_data_arc, sched) = main_thread;
    let syscalls = sched.as_ref().map(|sched| sched.handle.syscalls());
    let syscall_total = syscalls.map_or(0, |s| s.calls());
    let syscall_total_ns = syscalls.map_or(0, |s| s.ns());
    let syscall_counts = syscalls.map_or([0; toyos_abi::syscall::SYSCALL_PROFILE_BINS], |s| s.by_number());
    // Never hold ThreadData + ProcessData at once.
    thread_data_arc.lock().tls.take();

    // Before the teardown below: a report spans its process up to here, and the next one this teardown.
    #[cfg(feature = "mask-windows")]
    crate::windows::report();

    let mut data = process_data_arc.lock();
    ops::close_all(&mut data.handles);
    // Every thread has left, and none returns to Ring 3 to use them.
    crate::isa::process_ends(pid);
    data.elf.elf_alloc.take();
    data.elf.loaded_libs.clear();

    let usage = data.space.usage();
    Consumed {
        peak_memory: usage.peak_bytes(),
        alloc_count: usage.ranges_made(),
        free_count: usage.ranges_taken(),
        syscall_total,
        syscall_total_ns,
        syscall_counts,
    }
}

/// Table-side teardown bookkeeping: drop the image record, total the CPU time of every thread still in the table, and write the one record of the process's end.
/// Caller must hold `PROCESS_TABLE`, be the last thread out and have freed the resources.
fn teardown_bookkeeping(table: &mut ProcessTable, process_pid: Pid, end: End, consumed: &Consumed) -> u64 {
    let proc = table.get_mut(process_pid)
        .expect("teardown_bookkeeping: process not found");
    proc.image = None;
    let cpu_ns: u64 = proc.threads.iter().map(|(_, t)| t.sched().map_or(0, scheduler::task_cpu_ns)).sum();
    let name = proc.name_str();
    // The verdict first: a record past `MAX_RECORD_MESSAGE` loses its end.
    log!("exit: {name} pid={process_pid} {end} cpu={}ms {consumed}", cpu_ns / 1_000_000);
    cpu_ns
}

/// Publish `first`, then every parent each publication lets go, child before parent: at most `toyos_proclife::MAX_DEPTH` + 1 publications, each with the table lock given up.
/// Once published an entry is reapable, so only its parent's entry is read after.
fn publish_climb(first: Option<tree::Publish>) {
    let mut ready = first;
    while let Some(publish) = ready {
        let (object, exit) = {
            let mut guard = PROCESS_TABLE.lock();
            let proc = guard.as_mut().unwrap().get_mut(publish.pid)
                .expect("publish_climb: a process is in the table until its exit is published");
            let exit = proc.exit.take()
                .expect("publish_climb: a count reaches zero only after the teardown kept its exit");
            (Arc::clone(&proc.object), exit)
        };
        object.publish_exit(exit);
        ready = publish.parent.and_then(|parent| {
            let mut guard = PROCESS_TABLE.lock();
            tree::published(guard.as_mut().unwrap(), parent, publish.pid)
        });
    }
}

/// One `ProcessStats`, from a process's own data; written once, since `SYS_PROCESS_STATS` samples a live process through the same fields the teardown snapshots.
pub fn stats_from(
    data: &ProcessData,
    pid: Pid,
    cpu_ns: u64,
    syscall_total: u64,
    syscall_total_ns: u64,
) -> toyos_abi::syscall::ProcessStats {
    use toyos_abi::syscall::ProcessStats;
    let acct = &data.accounting;
    ProcessStats {
        wall_ns: crate::clock::nanos_since_boot().saturating_sub(data.spawn_ns),
        cpu_ns,
        syscall_total,
        syscall_total_ns,
        faults: data.space.usage().faults(),
        fault_ns: data.space.usage().fault_ns(),
        pid: pid.raw(),
        _pad: 0,
        blocked_io_ns: acct.blocked_io_ns,
        blocked_futex_ns: acct.blocked_futex_ns,
        blocked_pipe_ns: acct.blocked_pipe_ns,
        blocked_ipc_ns: acct.blocked_ipc_ns,
        blocked_other_ns: acct.blocked_other_ns,
        runqueue_wait_ns: acct.runqueue_wait_ns,
        peak_memory: data.space.usage().peak_bytes(),
        alloc_count: data.space.usage().ranges_made(),
    }
}

/// The last thread out's teardown of its process, on its own stack: free what the process holds, run every release its handles owed, then keep its exit until every end below it is published. Every other thread has left, so none can run in what is freed.
/// The thread is in the process until the last table section marks it, so a machine stop waits for every record and release here.
/// Waits on nothing: it runs on a killed thread whose one cancel may be spent, and at the exit boundary's preempt depth, where a park asserts.
/// The section that marks it also lets the teardown's own hold on the publication go: the exit is published here when nothing below it is left, and otherwise by the last end below it.
fn teardown(pid: Pid, tid: Tid, end: End, mark: i32, process_data: &Arc<Lock<ProcessData>>) {
    let main_thread = {
        let guard = PROCESS_TABLE.lock();
        let proc = guard.as_ref().unwrap().get(pid).expect("teardown: the process its last thread is in");
        let main = proc.threads.get(proc.main_tid).expect("teardown: a claimed process gives up no thread");
        (Arc::clone(&main.thread_data), main.sched().cloned())
    };
    let releases = crate::object::Releases::open();
    let consumed = teardown_resources(process_data, &main_thread, pid);
    let released = releases.run();
    let cpu_ns = {
        let mut guard = PROCESS_TABLE.lock();
        teardown_bookkeeping(guard.as_mut().unwrap(), pid, end, &consumed)
    };
    let stats = stats_from(&process_data.lock(), pid, cpu_ns, consumed.syscall_total, consumed.syscall_total_ns);
    let ready = {
        let mut guard = PROCESS_TABLE.lock();
        let table = guard.as_mut().unwrap();
        proclife::torn_down(table, pid, tid, mark);
        table.get_mut(pid).expect("teardown: the process its last thread is in").exit =
            Some(crate::object::process::Exit::new(end, stats, released));
        tree::teardown_done(table, pid)
    };
    // The table says zombie now, which a sweep counts as nothing left to stop.
    crate::quiesce::note_progress();
    publish_climb(ready);
}

/// Take the running thread out of its process: out of its address space, its accounting into the process's, and its own mark in the table. The last thread out of a claimed process tears the process down here.
/// Returns rather than diverging: the exit pass never comes back, and the live `Arc`s here would leak with no scope left to drop them.
pub fn leave(chosen: Option<i32>) {
    let (pid, tid) = (current_process(), current_tid());
    crate::mm::paging::activate_kernel();
    let process_data = process_data();
    scheduler::flush_current_stats(&mut process_data.lock().accounting);
    let out = {
        let mut guard = PROCESS_TABLE.lock();
        proclife::leave(guard.as_mut().unwrap(), pid, tid, chosen)
    };
    match out {
        // The table says zombie now, which a sweep counts as nothing left to stop.
        proclife::Leave::NotLast => crate::quiesce::note_progress(),
        proclife::Leave::Last { end, mark } => teardown(pid, tid, end, mark, &process_data),
    }
}

/// Claim `pid`'s teardown for `end`, in one hold of the table lock: the winner gets the threads it retires, every one still in the process but `caller`, and owes `pid`'s children to its walk; a loser, or a process already gone, gets none.
fn claim(pid: Pid, end: End, caller: Option<Tid>, owed: &mut Vec<Pid>) -> Vec<ThreadSched> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().unwrap();
    if !tree::claim(table, pid, end, owed) {
        return Vec::new();
    }
    let proc = table.get(pid).expect("claim: a claimed process is in the table");
    scheds(table, pid, proclife::retire_set(proc, caller))
}

/// The scheduler records of `pid`'s threads `tids`, which a claim of it retires.
fn scheds(table: &ProcessTable, pid: Pid, tids: Vec<Tid>) -> Vec<ThreadSched> {
    let proc = table.get(pid).expect("scheds: a claimed process is in the table");
    tids.into_iter()
        .map(|tid| {
            proc.threads.get(tid).and_then(ThreadEntry::sched).cloned()
                .expect("scheds: a thread in the table has its scheduler record")
        })
        .collect()
}

/// End every process an end owes, on the ending or killing thread: one claim per hold of the table lock, each claim's retires posted with it given up, and a reschedule owed between two claims served with nothing held.
fn walk(mut owed: Vec<Pid>) {
    while let Some(pid) = owed.pop() {
        for sched in claim(pid, End::Killed, None, &mut owed) {
            scheduler::post_retire(&sched);
        }
        if !owed.is_empty() && crate::preempt::need_resched() {
            scheduler::yield_now();
        }
    }
}

/// End the calling thread's process as `end` says, and everything below it.
pub fn exit(end: End) -> ! {
    let tid = current_tid();
    let mut owed = Vec::new();
    for sched in claim(current_process(), end, Some(tid), &mut owed) {
        scheduler::post_retire(&sched);
    }
    walk(owed);
    leave(None);
    scheduler::exit_current();
}

/// Exit the current thread. If this is the main thread, it is the process's exit. For a child thread, frees its own mappings and leaves.
pub fn thread_exit(code: i32) -> ! {
    let process_pid = current_process();
    let tid = current_tid();
    let route = {
        let guard = PROCESS_TABLE.lock();
        let table = guard.as_ref().unwrap();
        proclife::route_thread_exit(table, process_pid, tid)
    };
    let post = match route {
        proclife::ThreadExit::Process => exit(End::Exited(code)),
        proclife::ThreadExit::Sibling { post } => post,
    };

    release_thread(tid);
    leave(Some(code));
    // Whoever joined this thread armed on it; post before the exit pass — after it this thread never runs again.
    if let Some(handle) = crate::sched::driver::current_handle() {
        // Held together by assertion, not shared state: the decision names the subject, this performs it via the CPU's own handle.
        debug_assert_eq!(
            post,
            Watch::Thread(process_pid, tid),
            "thread_exit posts through its own task handle, so the decision must have \
             named its own watch",
        );
        handle.watch().post();
    }
    scheduler::exit_current();
}

/// A child thread's own mappings, released before it leaves; returns rather than diverging for [`leave`]'s reason.
fn release_thread(tid: Tid) {
    let addr_space = current_address_space();
    crate::mm::paging::activate_kernel();

    let tls = current_data().lock().tls.take();
    let blocks: Vec<KernelMapping> = {
        let owner_arc = process_data();
        let mut owner = owner_arc.lock();
        let keys: Vec<(Tid, u64)> = owner.elf.dynamic_tls_blocks.keys().filter(|&&(t, _)| t == tid).copied().collect();
        keys.iter().filter_map(|key| owner.elf.dynamic_tls_blocks.remove(key)).map(|(block, _charge)| block).collect()
    };
    // With no process lock held: a release takes the space's.
    for block in tls.iter().chain(&blocks) {
        block.release(&addr_space);
    }
}

/// A thread's scheduler record, cloned out of the table so wake/retire never hold the table lock while they post.
pub fn thread_sched(pid: Pid, tid: Tid) -> Option<ThreadSched> {
    let guard = PROCESS_TABLE.lock();
    let table = guard.as_ref()?;
    table.get(pid)?.threads.get(tid)?.sched().cloned()
}

/// The physical address of a futex word, its page populated like any other user access. Alignment matters: the scheduler dereferences this directly on every wake check, so an unaligned word could straddle into the next physical page.
/// Not a lease: nothing pins the frame, and a sibling's `munmap` can free it under a parked wait. Made safe by an unmap ending the waits on the frames it frees (`sched::futex::revoke_frames`), and `scheduler::futex_wait` re-deriving the translation on every check.
fn futex_word(addr: UserAddr) -> Option<crate::mm::DirectMap> {
    if !addr.raw().is_multiple_of(4) {
        return None;
    }
    crate::user_ptr::translate_user(addr, crate::user_ptr::Access::Read)
}

/// Atomically check a user futex word and block if it matches `expected`. Returns 0 if woken/never blocked, 1 if timed out, an error if `addr` names no word this process may have.
/// Takes [`UserAddr`], not `u64`: the bound check is the whole safety of the call, done once here rather than repeated by every caller.
pub fn futex_wait(addr: UserAddr, expected: u32, timeout_ns: u64) -> u64 {
    // The ABI's relative `u64::MAX` means no timeout; every other value is a relative span, turned absolute exactly once, here.
    let deadline = if timeout_ns != u64::MAX {
        Deadline::at(crate::clock::now() + Duration::from_nanos(timeout_ns))
    } else {
        Deadline::never()
    };

    // Physical, so a futex in shared memory works across processes.
    let Some(phys_addr) = futex_word(addr) else {
        return toyos_abi::syscall::SyscallError::BadAddress.to_u64();
    };

    // 0 covers both woken and never-blocked (the caller re-checks the word either way); 1 means only the caller's own deadline ended it.
    match scheduler::futex_wait(addr, phys_addr, expected, deadline) {
        scheduler::FutexEnd::Changed => 0,
        scheduler::FutexEnd::Timeout => 1,
    }
}

/// Wake up to `count` threads blocked on the same physical address as `addr`.
pub fn futex_wake(addr: UserAddr, count: u64) -> u64 {
    let Some(phys_addr) = futex_word(addr) else {
        return toyos_abi::syscall::SyscallError::BadAddress.to_u64();
    };
    scheduler::futex_wake(phys_addr, count as usize)
}

/// Wake processes blocked on reading from a pipe that now has data.
pub fn wake_pipe_readers(pipe: &pipe::PipeRef) {
    scheduler::wake_pipe_readers(pipe);
}

/// Wake processes blocked on writing to a pipe that now has space.
pub fn wake_pipe_writers(pipe: &pipe::PipeRef) {
    scheduler::wake_pipe_writers(pipe);
}

/// Ask `join` once more; an unsettled one collects under the table lock, which is what makes validating the parent-thread relationship and collecting the zombie one act.
pub fn ask_join(join: &join::Join, tid: Tid, parent_pid: Pid) -> Option<Result<i32, join::JoinRefused>> {
    join.ask(|| join::collect_zombie(PROCESS_TABLE.lock().as_mut().unwrap(), parent_pid, tid))
}

/// Serve a fault at `fault_addr` for `access` in the running thread's space:
/// whether its page was populated, or already held a leaf allowing the access.
/// Takes the space's lock and nothing else.
pub fn handle_page_fault(fault_addr: u64, access: kernel::vm::Access) -> bool {
    let Some(space) = scheduler::current_address_space() else { return false };
    let began = crate::clock::nanos_since_boot();
    let served = space.lock().fault(fault_addr, access);
    if served {
        space.usage().faulted(crate::clock::nanos_since_boot().saturating_sub(began));
    }
    served
}

/// Dump memory around `fault_addr` and the instruction pointer for the
/// current process; called from the exception handler on user-mode crashes.
pub fn dump_crash_diagnostics(fault_addr: u64, rip: u64) {
    let Some(space) = scheduler::current_address_space() else { return };
    let Some(guard) = space.try_lock() else {
        log!("  [crash diagnostics: the address space is locked, skipping]");
        return;
    };

    // Reads via the direct map (no USER bit) to avoid SMAP faults.
    let read_user = |virt: u64| -> Option<u64> {
        if !virt.is_multiple_of(8) || !toyos_userbound::is_user_addr(virt) {
            return None;
        }
        let (phys, _) = guard.get(virt)?;
        // SAFETY: `get` answered a present leaf's frame, so `phys` is mapped
        // in the direct map; `virt % 8 == 0` keeps the `u64` inside its page.
        // `read_volatile`: this is a crash report, and two folded reads would
        // print a value never fetched.
        Some(unsafe { crate::mm::DirectMap::from_phys(phys).as_ptr::<u64>().read_volatile() })
    };

    let dump_region = |label: &str, addr: u64| {
        if read_user(addr & !0x7).is_none() { return; }
        let start = (addr & !0x7).saturating_sub(32);
        log!("  Memory around {} ({:#x}):", label, addr);
        for i in 0..8u64 {
            let a = start + i * 8;
            let Some(val) = read_user(a) else { break };
            let marker = if a == (addr & !0x7) { " <--" } else { "" };
            log!("    [{:#x}] = {:#018x}{}", a, val, marker);
        }
    };

    if fault_addr != 0 {
        dump_region("fault_addr", fault_addr);
    }
    dump_region("rip", rip);

    let tp = crate::arch::cpu::thread_pointer();
    if tp != 0 {
        log!("  Thread pointer: {:#x}", tp);
        if let Some(first) = read_user(tp) {
            if matches!(crate::loader::TLS_VARIANT, toyos_elf::tls::Variant::II) {
                log!("  [TP] = {:#x} (expected {:#x}, variant II's self-pointer)", first, tp);
            }
            for i in 0..8u64 {
                let addr = tp + i * 8;
                let Some(val) = read_user(addr) else { break };
                log!("    TP+{:#x} = {:#018x}", i * 8, val);
            }
            log!("  TLS data before TP:");
            for i in 1..=4u64 {
                let addr = tp - i * 8;
                let Some(val) = read_user(addr) else { break };
                log!("    TP-{:#x} = {:#018x}", i * 8, val);
            }
        } else {
            log!("  Thread pointer {:#x} NOT MAPPED!", tp);
        }
    }
}

/// The executable a process runs, as its crash report records a frame in it
/// (`toyos_symbols::frame`): the file the kernel opened, the build-id that
/// file carries, and where it is mapped.
pub struct UserImage {
    pub name: String,
    pub build_id: Option<toyos_symbols::frame::BuildId>,
    /// `[start, end)`: the image's span in the process.
    pub start: u64,
    pub end: u64,
    /// What a file address is moved by: an address `a` in the file is at `a + bias`.
    pub bias: u64,
}

/// What a crash report did with a user address. Not a `bool`: "no frame was recorded" would conflate an address the image does not cover with one nothing looked up because a lock was held.
#[must_use = "an address with no frame line still has to be printed"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameRecord {
    /// Recorded as file and offset: the line is already logged.
    Recorded,
    /// The running task's image does not hold the address.
    Outside,
    /// Nothing was read: a scheduler pass already held this CPU's task record when the report began, so the running task's image was unreachable.
    InPass,
    /// Nothing was read: no task is running on this CPU (idle, or before the first task).
    NoTask,
}

impl FrameRecord {
    /// Log the bare address for an address with no frame line, saying why; [`Recorded`](Self::Recorded) logs nothing — its line is already out.
    pub fn log_bare(self, addr: u64) {
        match self {
            Self::Recorded => {}
            Self::Outside => log!("    {:#x}", addr),
            Self::InPass => {
                log!("    {:#x}  <frame unread: a scheduler pass held this CPU's task record>", addr)
            }
            Self::NoTask => {
                log!("    {:#x}  <frame unread: no task is running on this CPU>", addr)
            }
        }
    }
}

/// Record the user address `pc` as a frame of the running process's image; see [`with_current_image`].
pub fn record_user_frame(pc: u64) -> FrameRecord {
    with_current_image(|image| record(image, pc, Some(pc)))
}

/// [`record_user_frame`] for a backtrace frame's return address: its offset is the call's last byte, so a call in tail position names its caller.
pub fn record_user_frame_return(return_addr: u64) -> FrameRecord {
    with_current_image(|image| record(image, return_addr, return_addr.checked_sub(1)))
}

/// Log `pc`'s frame, its offset that of `at`, when `image` holds `at`.
fn record(image: &UserImage, pc: u64, at: Option<u64>) -> bool {
    let offset = at.and_then(|at| toyos_symbols::frame::frame_offset(at, image.start, image.end, image.bias));
    let Some(offset) = offset else { return false };
    log!("{}", toyos_symbols::frame::UserFrame { pc, name: &image.name, offset, build_id: image.build_id });
    true
}

/// Run `f` against the image of the task this CPU is running, and say what happened.
/// No lock: this runs from fault/panic reports, where the faulting thread may hold `PROCESS_TABLE`, so a task carries its own process's image on its own record ([`sched::driver::current_image`]) instead.
/// `pid` is not a parameter: a report is always about the process whose CPU is producing it.
fn with_current_image(f: impl FnOnce(&UserImage) -> bool) -> FrameRecord {
    let Some(image) = crate::sched::driver::current_image() else {
        // This CPU cannot switch between the two causes mid-report: a report is not reschedulable while `PerCpu::fault_state` is non-zero.
        return if crate::sched::driver::in_pass() {
            FrameRecord::InPass
        } else {
            FrameRecord::NoTask
        };
    };
    match image {
        Some(image) if f(&image) => FrameRecord::Recorded,
        _ => FrameRecord::Outside,
    }
}

/// Kill the process an object names, and everything below it.
/// The handle is the whole authorization, not the parent relationship: a `Process` handle carrying `Rights::MANAGE` says who may, and it can be narrowed away or handed on. `Ok` for an already-gone process: the caller asked for it to be dead and it is.
/// Returns once every retire is posted, never waiting on a victim: one may be killing this caller. The object's exit is published once every end below it is.
pub fn kill_process(object: &crate::object::process::ProcessObject) -> u64 {
    kill(object.pid());
    0
}

fn kill(pid: Pid) {
    let mut owed = Vec::new();
    for sched in claim(pid, End::Killed, None, &mut owed) {
        scheduler::post_retire(&sched);
    }
    walk(owed);
}

/// Ask the process an object names, and everything below it, to quit for
/// `reason`, on the asking thread: one ask per hold of the table lock, each
/// claim's retires and each notice's post made with it given up, then the
/// kill walk of what it claimed. The authorization is the handle's `MANAGE`,
/// as a kill's.
pub fn quit_process(object: &crate::object::process::ProcessObject, reason: Reason) {
    let mut owed = quit::Owed::of(object.pid());
    while let Some(pid) = owed.ask.pop() {
        let (retire, notified) = {
            let mut guard = PROCESS_TABLE.lock();
            ask(guard.as_mut().unwrap(), pid, reason, &mut owed)
        };
        for sched in &retire {
            scheduler::post_retire(sched);
        }
        if let Some(notice) = notified {
            notice.watch().post();
        }
        if !owed.ask.is_empty() && crate::preempt::need_resched() {
            scheduler::yield_now();
        }
    }
    walk(owed.kill);
}

/// One ask of a quit, under `table`'s hold and `pid`'s notice's: the threads a
/// claim of it retires, or the notice a reason was put in.
fn ask(table: &mut ProcessTable, pid: Pid, reason: Reason, owed: &mut quit::Owed) -> (Vec<ThreadSched>, Option<Arc<crate::object::notice::NoticeObject>>) {
    let Some(notice) = table.get(pid).map(|proc| Arc::clone(&proc.notice)) else { return (Vec::new(), None) };
    match notice.with(|held| quit::ask(table, pid, held, reason, owed)) {
        quit::Asked::Gone => (Vec::new(), None),
        quit::Asked::Killed => {
            let proc = table.get(pid).expect("ask: a claimed process is in the table");
            (scheds(table, pid, proclife::retire_set(proc, None)), None)
        }
        quit::Asked::Notified => (Vec::new(), Some(notice)),
    }
}

/// The process whose next spawn `debug_action::KILL_PLACE_AS_SPAWN_LANDS` marked; `Pid::MAX`, which is never issued, for none.
#[cfg(feature = "test-actuators")]
static MARKED_SPAWNER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(Pid::MAX.0);

/// Mark the calling process's next spawn that reaches its commit.
#[cfg(feature = "test-actuators")]
pub fn debug_mark_spawn() {
    MARKED_SPAWNER.store(current_process().0, core::sync::atomic::Ordering::Relaxed);
}

/// Kill the place of a spawn its caller marked, and take the mark. The loader calls it between the spawn's commit and its landing.
#[cfg(feature = "test-actuators")]
pub fn debug_kill_marked_place(parent: Parent) {
    use core::sync::atomic::Ordering::Relaxed;
    let Parent::Under(place) = parent else { return };
    if MARKED_SPAWNER.compare_exchange(current_process().0, Pid::MAX.0, Relaxed, Relaxed).is_ok() {
        kill(place);
    }
}

/// The process whose next landed spawn `debug_action::HOLD_SPAWN_UNTIL_CHILD_ENDS` marked; `Pid::MAX` for none.
#[cfg(feature = "test-actuators")]
static HELD_SPAWNER: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(Pid::MAX.0);

/// Mark the calling process's next spawn whose child lands.
#[cfg(feature = "test-actuators")]
pub fn debug_mark_spawn_hold() {
    HELD_SPAWNER.store(current_process().0, core::sync::atomic::Ordering::Relaxed);
}

/// Park the thread of a spawn its caller marked until `child`'s exit is published, and take the mark. The loader calls it once the child has landed.
#[cfg(feature = "test-actuators")]
pub fn debug_hold_marked_spawn(parent: Parent, child: &crate::object::process::ProcessObject) {
    use core::sync::atomic::Ordering::Relaxed;
    let Parent::Under(_) = parent else { return };
    if HELD_SPAWNER.compare_exchange(current_process().0, Pid::MAX.0, Relaxed, Relaxed).is_ok() {
        let parkable = scheduler::Parkable::at_entry();
        // A cancelled wait is the spawner's own end, which the spawn's return meets.
        let _ = crate::watch::wait_until(
            &parkable,
            child.watch(),
            0,
            kernel::sched::task::WaitClass::Other,
            crate::time::Deadline::never(),
            || child.finished(),
        );
    }
}

/// End a process that named a handle it does not hold — userland's bug, not the kernel's, so the process dies alone rather than panicking the kernel.
/// Must be reached with nothing held: this calls `exit`, which takes the process's own lock, the table lock, and whatever a released object reaches for.
pub fn handle_fault(error: crate::object::HandleError) -> ! {
    log!(
        "handle fault: pid={} tid={} syscall={} {error}",
        current_process(),
        current_tid(),
        percpu::syscall_num(),
    );
    exit(End::Fault(Fault::Handle))
}

/// An AP's way into the idle loop on either architecture, once it has answered
/// the BSP: it waits for the machine's release, then joins the scheduler.
pub fn ap_idle() -> ! {
    while !crate::smp::is_ready() {
        core::hint::spin_loop();
    }

    // Only a committed CPU may join: an uncommitted AP has no scheduler slot and
    // no shootdown targets it, so it halts. The acquire above makes the count visible.
    let me = percpu::cpu_id();
    if me >= crate::smp::cpu_count() {
        log!("CPU {me}: bring-up did not commit; halting");
        crate::arch::cpu::halt();
    }

    // Before touching anything not self-mapped: the acquire on the release
    // makes the BSP's mappings visible, and this settles every TLB
    // invalidation the wait could not take.
    crate::arch::tlb::join();

    crate::counters::bring_up();

    // Once this CPU is committed and about to run something: the counter is
    // per CPU, so a CPU nobody arms here is one the hard-lockup bound does not cover.
    crate::hardlockup::arm_this_cpu();

    log!("CPU {me}: joining scheduler");
    scheduler::enter_idle_loop();
}
