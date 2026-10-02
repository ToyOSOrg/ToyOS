//! The process table, and what a process is made of.
//!
//! [`dump_crash_diagnostics`] and [`try_for_each_thread`] never wait on
//! [`PROCESS_TABLE`]: the faulting thread may hold it. A crash report resolves
//! symbols off the task's own record ([`resolve_user_symbol`]), never off
//! this table.
//!
//! `toyos-proclife` decides lifecycle transitions; this file only performs them.

use alloc::alloc::{alloc_zeroed, dealloc, Layout};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::ptr::NonNull;
use crate::arch::percpu;
use crate::mm::policy::{CachePolicy, Prot, WindowProt};
use crate::mm::{PAGE_2M, PAGE_BYTES};
use crate::object::{ops, HandleTable};
use crate::sync::Lock;
use crate::symbols::SymbolTable;
use crate::sched::payload::ThreadSched;
use crate::time::{Deadline, Duration};
use crate::{elf, pipe, scheduler};
use crate::UserAddr;
use crate::loader::{alloc_kernel_stack, thread_start, TlsBlock};

pub use toyos_abi::{Pid, Tid};
pub use crate::scheduler::TaskId;
use toyos_abi::syscall::{EndowEntry, SyscallError};

/// The lifecycle's decisions; this file only performs them.
pub use toyos_proclife::{ThreadLocation, Watch};
use toyos_proclife::{join, reap, spawn as proclife_spawn, teardown as proclife, tree, Lifecycle, Node, Processes};

/// One `EndowEntry` on the wire; `loader::start` and [`Endowments::encode`] both index by it.
pub const ENDOW_ENTRY_LEN: usize = core::mem::size_of::<EndowEntry>();

pub use crate::loader::{build_child_handles, spawn, spawn_init, INIT_PATH};

/// Page tables shared between a process and all its threads.
pub type PageTables = Arc<Lock<crate::mm::paging::AddressSpace>>;

/// Allocate a virtual region and map physical memory into it, or `None` if out of address space.
pub fn vma_map(
    pt: &Lock<crate::mm::paging::AddressSpace>,
    phys: u64,
    size: u64,
    prot: Prot,
) -> Option<(UserAddr, u64)> {
    pt.lock().alloc_and_map(phys, size, prot, CachePolicy::Normal)
}


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


/// Contiguous 2MB-aligned physical pages from PMM, zeroed on allocation, freed on drop.
pub struct PageAlloc(Vec<crate::mm::pmm::PhysPage>);

impl PageAlloc {
    /// Allocate `size` bytes as contiguous 2MB pages.
    pub fn new(size: usize, cat: crate::mm::pmm::Category) -> Option<Self> {
        let count = size.div_ceil(PAGE_2M as usize);
        Some(Self(crate::mm::pmm::alloc_contiguous(count, cat)?))
    }

    /// Kernel pointer to the start of the allocation (via direct map).
    pub fn ptr(&self) -> *mut u8 {
        self.0[0].direct_map().as_mut_ptr()
    }

    /// This allocation as a bounds-checked window; a `&mut [u8]` here would carry `noalias`/`dereferenceable` into LLVM over pages that become a user mapping.
    pub fn window(&self) -> crate::mm::KernelSlice {
        crate::mm::KernelSlice::whole(self)
    }

    /// Total size in bytes (always a multiple of 2MB).
    pub fn size(&self) -> usize {
        self.0.len() * PAGE_2M as usize
    }

    /// Physical address of the start.
    pub fn phys(&self) -> u64 {
        self.0[0].direct_map().phys()
    }
}

// SAFETY: `ptr()`/`size()` come from the same `Vec<PhysPage>` this type owns; pages return to the PMM when that `Vec` drops, which is when `self` does.
unsafe impl crate::mm::Allocation for PageAlloc {
    fn ptr(&self) -> *mut u8 { PageAlloc::ptr(self) }
    fn size(&self) -> usize { PageAlloc::size(self) }
}


/// Frames no user mapping reaches yet.
///
/// The kernel builds what a process will see in these while they are its
/// alone, and [`publish`](Self::publish) is the only way to map them: it
/// consumes this, and runs the caller's last fix-up — the one that needs the
/// address the frames will have — under the address-space lock that then maps
/// them.
pub struct Unpublished(PageAlloc);

impl Unpublished {
    pub fn new(frames: PageAlloc) -> Self {
        Self(frames)
    }

    /// Kernel pointer to the start, via the direct map; the frames are this
    /// value's alone, so writes through it race nothing.
    pub fn ptr(&self) -> *mut u8 {
        self.0.ptr()
    }

    pub fn phys(&self) -> u64 {
        self.0.phys()
    }

    /// Choose an address in `pt`, hand it to `fix`, then map the frames there
    /// at `prot` — all under one hold of `pt`'s lock, so no thread of the
    /// process can reach the frames before `fix` returns. `None`, with the
    /// frames freed, when `pt` has no room.
    pub fn publish(self, pt: &PageTables, prot: Prot, fix: impl FnOnce(&Self, UserAddr)) -> Option<MappedPages> {
        let size = self.0.size() as u64;
        let phys = self.0.phys();
        // `alloc_and_map` fuses the reserve and the map with no seam for `fix`,
        // whose whole point is to run between them under one lock.
        assert!(phys & (PAGE_2M - 1) == 0, "publish: phys {phys:#x} not 2MB-aligned");
        let mut space = pt.lock();
        let at = space.alloc_region(size, crate::vma::RegionKind::Mapped)?;
        fix(&self, at);
        space.map_range(at, phys, size, prot, CachePolicy::Normal);
        drop(space);
        Some(MappedPages::new(at, self.0))
    }
}

/// Pages handed to userland through [`vma_map`], paired with the mapping address so the two are unmapped together; dropping without unmapping is sound only when the address space itself is being destroyed.
pub struct MappedPages {
    vaddr: UserAddr,
    pages: PageAlloc,
}

impl MappedPages {
    pub fn new(vaddr: UserAddr, pages: PageAlloc) -> Self {
        Self { vaddr, pages }
    }

    pub fn vaddr(&self) -> UserAddr { self.vaddr }

    /// Kernel pointer to the start, via the direct map.
    pub fn ptr(&self) -> *mut u8 { self.pages.ptr() }

    pub fn size(&self) -> usize { self.pages.size() }

    /// Take the mapping out of `addr_space`; the pages stay reachable from other CPUs until the returned wrapper drops (`invlpg` reaches only this CPU).
    fn unmap_from(
        self,
        addr_space: &mut crate::mm::paging::AddressSpace,
    ) -> crate::mm::Unmapped<PageAlloc> {
        addr_space.free_and_unmap(self.vaddr);
        crate::mm::Unmapped::new(self.pages)
    }

    pub fn release(self, pt: &PageTables) {
        // Two statements: the shootdown in drop must not run under the address-space lock, which a sibling's page fault spins on with IF clear.
        let pages = self.unmap_from(&mut pt.lock());
        drop(pages);
    }
}

/// Release every user mapping an exiting thread owns (its TLS block, and any dlopen'd modules it touched), so none is left as a live writable window onto memory the PMM has already reissued; not called by process teardown, which has no address space left to unmap from.
#[must_use]
fn release_thread_mappings(
    data: &mut ProcessData,
    tls: Option<MappedPages>,
    pt: &PageTables,
    tid: Tid,
) -> Vec<crate::mm::Unmapped<PageAlloc>> {
    let keys: Vec<(Tid, u64)> = data.elf.dynamic_tls_blocks.keys()
        .filter(|&&(t, _)| t == tid)
        .copied()
        .collect();
    let mut pages = Vec::with_capacity(keys.len() + 1);
    {
        let mut addr_space = pt.lock();
        pages.extend(tls.map(|t| t.unmap_from(&mut addr_space)));
        for key in keys {
            let block = data.elf.dynamic_tls_blocks.remove(&key).unwrap();
            pages.push(block.unmap_from(&mut addr_space));
            data.free_count += 1;
        }
    }
    pages
}

pub const KERNEL_STACK_SIZE: usize = 128 * 1024;

/// Type-safe user stack: couples the user-visible virtual address with the kernel window onto the same pages.
pub struct UserStack {
    vaddr: UserAddr,
    window: crate::mm::KernelSlice,
}

impl UserStack {
    pub fn new(vaddr: UserAddr, window: crate::mm::KernelSlice) -> Self {
        Self { vaddr, window }
    }

    /// User-visible virtual address of the stack top (highest address).
    pub fn top(&self) -> u64 { self.vaddr.raw() + self.size() }

    /// User-visible virtual base address.
    pub fn base(&self) -> UserAddr { self.vaddr }

    pub fn size(&self) -> u64 { self.window.size() as u64 }

    /// Copy `src` onto this stack at user address `user_addr`; bounds-checked against the whole write, not just `user_addr`.
    /// Panics rather than refuses — both arguments are the kernel's own arithmetic, never boundary-crossed input.
    fn write_at(&self, user_addr: u64, src: &[u8]) {
        let offset = user_addr.checked_sub(self.vaddr.raw())
            .expect("UserStack: address below stack base");
        // SAFETY: `copy_from` bounds `offset + src.len()` against the stack's own allocation; the destination is kernel-only until `loader::start` hands the address space to Ring 3, so nothing else can be reading it.
        unsafe { self.window.copy_from(offset as usize, src) };
    }

    /// Write argc, argv pointers, and string data onto this stack; returns the new user-visible stack pointer.
    pub fn write_argv(&self, args: &[&str]) -> u64 {
        let mut sp = self.top();
        let mut argv_ptrs: Vec<u64> = Vec::with_capacity(args.len());
        for arg in args.iter().rev() {
            sp -= (arg.len() + 1) as u64;
            self.write_at(sp, arg.as_bytes());
            self.write_at(sp + arg.len() as u64, &[0u8]);
            argv_ptrs.push(sp);
        }
        argv_ptrs.reverse();
        let metadata_qwords = args.len() + 2;
        sp = (sp - metadata_qwords as u64 * 8) & !15;
        self.write_at(sp, &(args.len() as u64).to_ne_bytes());
        for (i, ptr) in argv_ptrs.iter().enumerate() {
            self.write_at(sp + (1 + i as u64) * 8, &ptr.to_ne_bytes());
        }
        self.write_at(sp + (1 + args.len() as u64) * 8, &0u64.to_ne_bytes());
        sp
    }
}


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
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name).unwrap_or("?").trim_end_matches('\0')
    }
    pub fn set_name(&mut self, name: &[u8]) {
        self.name = [0u8; THREAD_NAME_LEN];
        let len = name.len().min(THREAD_NAME_LEN);
        self.name[..len].copy_from_slice(&name[..len]);
    }
    pub fn thread_data(&self) -> &Arc<Lock<ThreadData>> { &self.thread_data }
}

/// A process and all its threads. Removing a ProcessEntry removes all threads.
pub struct ProcessEntry {
    pid: Pid,
    /// The thing a handle to this process names; outlives the entry for as long as somebody still holds a handle.
    object: Arc<crate::object::process::ProcessObject>,
    name: [u8; THREAD_NAME_LEN],
    process_data: Arc<Lock<ProcessData>>,
    /// No `Lock`: written once by the loader, then read-only, so teardown releases it by dropping this `Arc` — a crash report then reaches names through the task it is reporting on, never through this table.
    symbols: Arc<SymbolTable>,
    main_tid: Tid,
    threads: crate::id_map::IdMap<Tid, ThreadEntry>,
    /// Set once, with the exit's code, by the exit or kill that claims teardown; checked by `spawn_thread` so no thread appears after the retire set.
    teardown_code: Option<i32>,
    /// Its parent, children and depth, answered by no syscall; `toyos_proclife::tree` reads and writes it.
    node: Node,
    /// The exit its teardown made, kept until every end below it is published.
    exit: Option<crate::object::process::Exit>,
}

impl ProcessEntry {
    /// Create a new process with its main thread (always `Tid(0)`), at `object`'s pid.
    pub fn new(
        object: Arc<crate::object::process::ProcessObject>,
        name: [u8; THREAD_NAME_LEN],
        process_data: Arc<Lock<ProcessData>>,
        symbols: Arc<SymbolTable>,
        main_thread: ThreadEntry,
        node: Node,
    ) -> Self {
        let mut threads = crate::id_map::IdMap::new();
        let main_tid = threads.insert(main_thread);
        Self {
            pid: object.pid(),
            object,
            name,
            process_data,
            symbols,
            main_tid,
            threads,
            teardown_code: None,
            node,
            exit: None,
        }
    }
    pub fn pid(&self) -> Pid { self.pid }
    pub fn object(&self) -> &Arc<crate::object::process::ProcessObject> { &self.object }
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
    pub fn tearing_down(&self) -> bool { self.teardown_code.is_some() }
}

/// The lifecycle face of an entry: only the fields `toyos-proclife` decides against, and nothing else this type carries.
impl Lifecycle for ProcessEntry {
    fn main_tid(&self) -> Tid { self.main_tid }
    fn teardown_code(&self) -> Option<i32> { self.teardown_code }
    fn begin_teardown(&mut self, code: i32) { self.teardown_code = Some(code); }
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

impl Drop for ProcessEntry {
    fn drop(&mut self) {
        // vruntime must live as long as the table entry: a zombified process's threads still yield inside their own exit path and read it.
        scheduler::remove_vruntime(self.pid);
    }
}


/// Record of a single demand-paged fault, stored in a ring buffer for crash diagnostics.
#[derive(Clone, Copy)]
pub struct PageFaultRecord {
    pub fault_addr: u64,
    pub page_elf_offset: u64,
    pub block_idx: u32,
    pub reloc_count: u16,
    // bit 0 writable, 1 has_relocs, 2 anonymous, 3 beyond_extent, 4 executable; 0 and 4 come from one `Prot` and are never both set (W^X).
    pub flags: u16,
    pub duration_us: u16, // microseconds spent handling this fault
}

/// Fixed-size ring buffer of recent page fault events for crash diagnostics.
pub struct PageFaultTrace {
    entries: [PageFaultRecord; 32],
    write_pos: usize,
    total: u64,
}

impl PageFaultTrace {
    pub fn new() -> Self {
        Self {
            entries: [PageFaultRecord {
                fault_addr: 0, page_elf_offset: 0, block_idx: 0,
                reloc_count: 0, flags: 0, duration_us: 0,
            }; 32],
            write_pos: 0,
            total: 0,
        }
    }

    pub fn record(&mut self, rec: PageFaultRecord) {
        self.entries[self.write_pos] = rec;
        self.write_pos = (self.write_pos + 1) % 32;
        self.total += 1;
    }

    /// Iterate entries in chronological order (oldest first).
    pub fn iter_chronological(&self) -> impl Iterator<Item = &PageFaultRecord> {
        let count = self.total.min(32) as usize;
        let start = if self.total >= 32 { self.write_pos } else { 0 };
        (0..count).map(move |i| &self.entries[(start + i) % 32])
    }

    pub fn total(&self) -> u64 { self.total }
}

/// ELF loading artifacts and TLS state.
pub struct ElfInfo {
    pub elf_alloc: Option<OwnedAlloc>,
    pub tls_modules: Vec<crate::elf::TlsModule>,
    /// Every static module's TLS, as a thread's block is laid out from it.
    pub tls: toyos_elf::tls::Static,
    /// Next module ID to assign on dlopen (1-based, exe=1).
    pub next_tls_module_id: u64,
    /// Dynamically allocated TLS blocks for dlopen'd modules, keyed by (Tid, module_id).
    pub dynamic_tls_blocks: alloc::collections::BTreeMap<(Tid, u64), MappedPages>,
    /// Dynamically loaded shared libraries (indexed by dlopen handle).
    pub loaded_libs: Vec<elf::LoadedLib>,
    /// RELATIVE relocation index for demand-paged ELF (applied per-page on fault).
    pub reloc_index: Option<Arc<elf::RelocationIndex>>,
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
            reloc_index: None,
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

    pub mmap_regions: Vec<MmapRegion>,
    /// Live `SYS_PIPE_MAP` windows. See [`PipeMap`].
    pub pipe_maps: Vec<PipeMap>,
    /// 2MB allocations for demand-paged pages. Freed on process exit.
    pub demand_pages: Vec<PageAlloc>,
    /// Ring buffer of recent page faults for crash diagnostics.
    pub fault_trace: PageFaultTrace,
    /// Peak memory usage in bytes (high-water mark)
    pub peak_memory: u64,
    /// Total allocations (demand pages + mmap + TLS blocks)
    pub alloc_count: u64,
    /// Total frees (munmap)
    pub free_count: u64,
    /// Executable path (for SYS_QUERY_MODULES).
    pub exe_path: String,

    pub spawn_ns: u64,
    pub accounting: ProcessAccounting,
    /// What this process's parent called each handle it moved in at spawn.
    pub endowments: Endowments,
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
    pub fault_demand_count: u32,
    pub fault_zero_count: u32,
    pub fault_ns: u64,
    pub io_read_ops: u32,
    pub io_read_bytes: u64,
    pub blocked_io_ns: u64,
    pub blocked_futex_ns: u64,
    pub blocked_pipe_ns: u64,
    pub blocked_ipc_ns: u64,
    pub blocked_other_ns: u64,
    pub runqueue_wait_ns: u64,
}

/// Per-thread data, unique to each thread via `Arc<Lock<ThreadData>>`.
pub struct ThreadData {
    pub tls_pages: Option<MappedPages>,
    /// Main thread's user stack; freed only with its address space, so it needs no address here.
    pub stack_pages: Option<PageAlloc>,
    pub user_stack_base: UserAddr,
    pub user_stack_size: u64,
    /// Syscall counts per syscall number (for profiling)
    pub syscall_counts: [u32; toyos_abi::syscall::SYSCALL_PROFILE_BINS],
    pub syscall_total: u64,
    /// Wall-clock nanoseconds spent in syscall dispatch (includes preemption time)
    pub syscall_total_ns: u64,
}

/// One process's window onto a pipe's ring page (`SYS_PIPE_MAP`); recorded so it can be revoked before the page outlives the process's handle to it.
pub struct PipeMap {
    pub pipe: pipe::PipeId,
    pub addr: UserAddr,
}

/// Take back every window onto `pipe`; called when the process's last handle to it goes.
pub fn revoke_pipe_maps(maps: &mut Vec<PipeMap>, pt: &PageTables, pipe: pipe::PipeId) {
    if !maps.iter().any(|m| m.pipe == pipe) {
        return;
    }
    {
        let mut addr_space = pt.lock();
        maps.retain(|m| {
            if m.pipe != pipe {
                return true;
            }
            addr_space.free_and_unmap(m.addr);
            false
        });
    }
    // Outside the block: it waits, and a sibling can be spinning on this lock with IF clear.
    crate::arch::tlb::shootdown(crate::invalidation::Origin::Pipe);
}

/// One live `mmap` and its physical pages; the range's registration in the address space's `regions` is separate (placement search, `munmap`).
pub struct MmapRegion {
    pub addr: UserAddr,
    pub size: usize,
    /// `None` for a `MmapProt::NONE` mapping: the range is reserved, but no physical page backs an access whose whole purpose is to fault.
    pub _pages: Option<PageAlloc>,
}


/// Zero-sized proof of running on the per-CPU idle stack; required by `collect_orphan_zombies` so it never drops the thread entry it runs on.
#[derive(Clone, Copy)]
pub struct IdleProof(());

impl IdleProof {
    /// # Safety
    /// Caller must be running on the idle stack.
    pub(crate) unsafe fn new_unchecked() -> Self { Self(()) }
}


/// Every process by pid, and the pids not yet issued (`toyos_proclife::pids`).
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

/// One thread as a diagnostic sees it: who it is, and what the scheduler's cross-CPU-readable face says about it.
pub struct ThreadCensus<'a> {
    pub pid: Pid,
    pub tid: Tid,
    pub process: &'a str,
    pub thread: &'a str,
    pub zombie: Option<i32>,
    /// One of `sched::payload`'s `SCHED_*`; `None` between the table insert (tid allocated) and `sched::spawn` (task minted).
    pub sched: Option<u8>,
    /// Zero means the thread has never executed a user instruction.
    pub cpu_ns: u64,
}

/// Walk every thread in the table, or answer `false` because the table is held; `try_lock`, not `lock`, since the one time anybody asks is when the machine is stuck and whatever holds this lock is a candidate for what is stuck.
pub fn try_for_each_thread(mut f: impl FnMut(ThreadCensus<'_>)) -> bool {
    let Some(guard) = PROCESS_TABLE.try_lock() else {
        return false;
    };
    let Some(table) = guard.as_ref() else {
        return false;
    };
    for (pid, proc) in table.iter() {
        for (tid, thread) in proc.threads().iter() {
            let sched = thread.sched();
            f(ThreadCensus {
                pid,
                tid,
                process: proc.name_str(),
                thread: thread.name_str(),
                zombie: match thread.state() {
                    ThreadLocation::Zombie(code) => Some(code),
                    ThreadLocation::Scheduled => None,
                },
                sched: sched.map(|s| s.sched_state()),
                cpu_ns: sched.map_or(0, |s| s.handle.cpu_ns()),
            });
        }
    }
    true
}

/// The object a handle to `pid` would name, for a process still in the table.
/// A kernel thread's pid names none: it has no Ring 3 boundary a kill could end it at.
pub fn process_object(pid: Pid) -> Option<Arc<crate::object::process::ProcessObject>> {
    let guard = PROCESS_TABLE.lock();
    let proc = guard.as_ref()?.get(pid)?;
    if crate::sched::kthread::is_kernel_task(TaskId(pid, proc.main_tid())) {
        return None;
    }
    Some(Arc::clone(proc.object()))
}

/// Accounting for a process. `None` only in the window between a live process and its published exit (the process being torn down right now).
pub fn stats_of(
    object: &crate::object::process::ProcessObject,
) -> Option<toyos_abi::syscall::ProcessStats> {
    if let Some(stats) = object.final_stats() {
        return Some(stats);
    }
    let pid = object.pid();
    // The other two locks are taken after this one is dropped, so no ordering edge.
    let (data_arc, cpu_ns, threads) = {
        let guard = PROCESS_TABLE.lock();
        let proc = guard.as_ref()?.get(pid)?;
        let mut cpu_ns = 0;
        let mut threads = Vec::new();
        for (_, thread) in proc.threads().iter() {
            cpu_ns += thread.sched().map_or(0, scheduler::task_cpu_ns);
            threads.push(Arc::clone(thread.thread_data()));
        }
        (Arc::clone(proc.process_data()), cpu_ns, threads)
    };
    let (mut syscall_total, mut syscall_total_ns) = (0, 0);
    for tdata in threads {
        let tdata = tdata.lock();
        syscall_total += tdata.syscall_total;
        syscall_total_ns += tdata.syscall_total_ns;
    }
    let data = data_arc.lock();
    Some(stats_from(&data, pid, cpu_ns, syscall_total, syscall_total_ns))
}

/// Take every entry whose process has published its exit.
/// Entries come back rather than being dropped here: the caller holds the table lock, and an entry's drop reaches `remove_vruntime`.
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
    /// No process: init, which the kernel starts.
    Root,
    /// The spawner, or the process whose `self` its place named.
    Under(Pid),
}

/// A spawn admitted under its parent, holding the pid its child will have and the parent's publication: [`Admission::land`] hands both to the child, and a drop — every way a spawn's build fails — gives both back.
pub struct Admission(Option<tree::Admitted>);

impl Admission {
    /// The question at the top of a spawn, before anything is built: `Gone` for a parent being torn down, and `ResourceExhausted`, which the log names, for a child more than `MAX_DEPTH` below init or no pid left to issue.
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
                log!("spawn: refused under pid {} at depth {depth}, more than {} below init",
                    place.expect("spawn: init is admitted at depth 0"), toyos_proclife::MAX_DEPTH);
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
        let (inserted, retire) = tree::land_child(table, admitted, KILLED_EXIT_CODE, insert);
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
pub fn spawn_thread(entry: u64, stack_ptr: u64, arg: u64, stack_base: u64) -> Option<Tid> {
    // Phase 1: parent's data + address space (table lock dropped after).
    let parent_process = current_process();
    let (parent_addr_space, process_data_arc) = {
        let guard = PROCESS_TABLE.lock();
        let table = guard.as_ref().unwrap();
        // Not `is_yes()`: a missing entry here means this thread's own process was reaped out from under it, which panics rather than refuses.
        match proclife_spawn::admit_thread_start(table, parent_process) {
            proclife_spawn::Admit::Yes => {}
            proclife_spawn::Admit::TearingDown => return None,
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
    let (tls_modules, tls) = {
        let data = process_data_arc.lock();
        (data.elf.tls_modules.clone(), data.elf.tls)
    };

    // Phase 2: build the TLS block outside any lock, then publish it into the
    // running parent — whose other threads may already be touching memory the
    // block's address is chosen from, so every pointer in it is final before
    // the mapping exists.
    let block = TlsBlock::build(&tls_modules, tls)?;
    let (tls_alloc, thread_pointer, tp_offset) = {
        let parent_data = process_data_arc.lock();
        // VA exhaustion is a resource failure the process caused, not a kernel bug; the block drops on the way out, returning its pages.
        let published = block.publish(&parent_addr_space)?;
        drop(parent_data);
        published
    };
    let tls_alloc_tcb = tls_alloc.ptr().wrapping_add(tp_offset);

    let (ks_alloc, ks_sp) = match alloc_kernel_stack(thread_start, entry, stack_ptr, arg) {
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
        // sibling can be spinning against with IF clear), so the mapping is
        // still solely this scope's here — built into a ThreadData only past
        // the admission, where the table owns its release.
        drop(guard);
        tls_alloc.release(&parent_addr_space);
        return None;
    }
    let thread_data = Arc::new(Lock::new(ThreadData {
        tls_pages: Some(tls_alloc),
        stack_pages: None,
        user_stack_base: UserAddr::new(stack_base),
        user_stack_size: if stack_base > 0 { stack_ptr - stack_base } else { 0 },
        syscall_counts: [0; toyos_abi::syscall::SYSCALL_PROFILE_BINS],
        syscall_total: 0,
        syscall_total_ns: 0,
    }));
    let proc = table.get_mut(parent_process)
        .expect("spawn_thread: the entry the insert admission just answered for");
    // Every thread names the same symbols, so a crash report never asks this table.
    let symbols = Arc::clone(&proc.symbols);
    let tid = proc.threads.insert(ThreadEntry::new(thread_data));
    // Before the thread's first instruction, which is the enqueue below: the
    // thread reads its own id here without a syscall (`toyos_abi::TCB_TID`).
    // SAFETY: `tp_offset` is this thread's TCB inside the TLS block the table
    // now owns, which no thread has run on yet; a 4-byte volatile store inside
    // the TCB the builder reserved, and a store is all the kernel does there.
    unsafe {
        core::ptr::write_volatile(
            tls_alloc_tcb.add(toyos_abi::TCB_TID).cast::<u32>(),
            tid.raw(),
        );
    }

    // Enqueue while still holding the table lock: this thread is fully visible to the scheduler before any retire sweep can start.
    let (sched, _dst) = scheduler::enqueue_new(
        TaskId(parent_process, tid),
        ks_alloc,
        ks_sp,
        parent_addr_space,
        thread_pointer,
        symbols,
    );
    proc.threads.get_mut(tid).unwrap().set_sched(sched);
    drop(guard);
    Some(tid)
}


/// Frees an exiting process's resources (mappings, handles, ELF state). Returns (syscall_total, syscall_total_ns) for the main thread, for the accounting snapshot.
fn teardown_resources(
    process_data_arc: &Arc<Lock<ProcessData>>,
    thread_data_arc: &Arc<Lock<ThreadData>>,
    pid: Pid,
) -> (u64, u64) {
    // Never hold ThreadData + ProcessData at once.
    let (syscall_total, syscall_total_ns, syscall_counts) = {
        let mut tdata = thread_data_arc.lock();
        let stats = (tdata.syscall_total, tdata.syscall_total_ns, tdata.syscall_counts);
        tdata.tls_pages.take();
        tdata.stack_pages.take();
        stats
    };

    let mut data = process_data_arc.lock();

    if syscall_total > 0 {
        use alloc::string::String;
        use core::fmt::Write;
        let mut profile = String::new();
        for (i, &count) in syscall_counts.iter().enumerate() {
            if count > 0 {
                let _ = write!(profile, " {}={}", i, count);
            }
        }
        let wall_ms = syscall_total_ns / 1_000_000;
        log!("syscalls: pid={pid} total={} syscall_wall={wall_ms}ms{profile}", syscall_total);
        // Printed here, not at shutdown: process exit is the one recurring moment a running guest reaches (the harness kills QEMU).
        if syscall_counts[toyos_abi::syscall::SYS_FSYNC as usize] > 0 {
            crate::block::census::print_if_moved();
        }
    }

    if data.peak_memory > 0 || data.alloc_count > 0 {
        log!("memory: pid={pid} peak={}MB allocs={} frees={}",
            data.peak_memory / (1024 * 1024), data.alloc_count, data.free_count);
    }

    // Machine-wide, cumulative counters, printed here (not at shutdown) because process exit is the one recurring moment every boot reaches.
    crate::irq_census::log_census();
    // After the irq lines: the tlb conservation check reads deliveries first, issues second.
    crate::arch::tlb::log_census();
    crate::arch::trap::log_unclaimed();

    ops::close_all(&mut data.handles);
    data.elf.elf_alloc.take();
    data.elf.loaded_libs.clear();
    data.mmap_regions.clear();
    data.pipe_maps.clear();
    data.demand_pages.clear();
    data.elf.reloc_index = None;

    (syscall_total, syscall_total_ns)
}

/// Table-side teardown bookkeeping: drop the symbol table and total the CPU time of every thread still in the table.
/// Caller must hold `PROCESS_TABLE`, be the last thread out and have freed the resources.
fn teardown_bookkeeping(table: &mut ProcessTable, process_pid: Pid, code: i32) -> u64 {
    let proc = table.get_mut(process_pid)
        .expect("teardown_bookkeeping: process not found");
    proc.symbols = Arc::new(SymbolTable::empty());
    let cpu_ns: u64 = proc.threads.iter().map(|(_, t)| t.sched().map_or(0, scheduler::task_cpu_ns)).sum();
    let name = proc.name_str();
    log!("exit: {name} pid={process_pid} code={code} cpu={}ms", cpu_ns / 1_000_000);
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
        fault_demand_count: acct.fault_demand_count,
        fault_zero_count: acct.fault_zero_count,
        fault_ns: acct.fault_ns,
        io_read_ops: acct.io_read_ops,
        pid: pid.raw(),
        io_read_bytes: acct.io_read_bytes,
        blocked_io_ns: acct.blocked_io_ns,
        blocked_futex_ns: acct.blocked_futex_ns,
        blocked_pipe_ns: acct.blocked_pipe_ns,
        blocked_ipc_ns: acct.blocked_ipc_ns,
        blocked_other_ns: acct.blocked_other_ns,
        runqueue_wait_ns: acct.runqueue_wait_ns,
        peak_memory: data.peak_memory,
        alloc_count: data.alloc_count,
    }
}

/// The last thread out's teardown of its process, on its own stack: free what the process holds, then keep its exit until every end below it is published. Every other thread has left, so none can run in what is freed.
/// The thread is in the process until the last table section marks it, so a machine stop waits for every record and release here.
/// Waits on nothing: it runs on a killed thread whose one cancel may be spent, and at the exit boundary's preempt depth, where a park asserts.
/// The section that marks it also lets the teardown's own hold on the publication go: the exit is published here when nothing below it is left, and otherwise by the last end below it.
fn teardown(pid: Pid, tid: Tid, code: i32, mark: i32, process_data: &Arc<Lock<ProcessData>>) {
    let main_thread_data = {
        let guard = PROCESS_TABLE.lock();
        let proc = guard.as_ref().unwrap().get(pid).expect("teardown: the process its last thread is in");
        Arc::clone(&proc.threads.get(proc.main_tid).expect("teardown: a claimed process gives up no thread").thread_data)
    };
    let (syscall_total, syscall_total_ns) = teardown_resources(process_data, &main_thread_data, pid);
    let cpu_ns = {
        let mut guard = PROCESS_TABLE.lock();
        teardown_bookkeeping(guard.as_mut().unwrap(), pid, code)
    };
    let stats = stats_from(&process_data.lock(), pid, cpu_ns, syscall_total, syscall_total_ns);
    let ready = {
        let mut guard = PROCESS_TABLE.lock();
        let table = guard.as_mut().unwrap();
        proclife::torn_down(table, pid, tid, mark);
        table.get_mut(pid).expect("teardown: the process its last thread is in").exit =
            Some(crate::object::process::Exit { code, stats });
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
        proclife::Leave::Last { code, mark } => teardown(pid, tid, code, mark, &process_data),
    }
}

/// Claim `pid`'s teardown for `code`, in one hold of the table lock: the winner gets the threads it retires, every one still in the process but `caller`, and owes `pid`'s children to its walk; a loser, or a process already gone, gets none.
fn claim(pid: Pid, code: i32, caller: Option<Tid>, owed: &mut Vec<Pid>) -> Vec<ThreadSched> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().unwrap();
    if !tree::claim(table, pid, code, owed) {
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
        for sched in claim(pid, KILLED_EXIT_CODE, None, &mut owed) {
            scheduler::post_retire(&sched);
        }
        if !owed.is_empty() && crate::preempt::need_resched() {
            scheduler::yield_now();
        }
    }
}

/// End the calling thread's process with `code`, and everything below it.
pub fn exit(code: i32) -> ! {
    let tid = current_tid();
    let mut owed = Vec::new();
    for sched in claim(current_process(), code, Some(tid), &mut owed) {
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
        proclife::ThreadExit::Process => exit(code),
        proclife::ThreadExit::Sibling { post } => post,
    };

    release_thread(process_pid, tid, code);
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
fn release_thread(process_pid: Pid, tid: Tid, code: i32) {
    let addr_space = current_address_space();
    crate::mm::paging::activate_kernel();

    let tls = {
        let tdata_arc = current_data();
        let mut tdata = tdata_arc.lock();
        tdata.tls_pages.take()
    };
    let released = {
        let owner_arc = process_data();
        let mut owner_data = owner_arc.lock();
        release_thread_mappings(&mut owner_data, tls, &addr_space, tid)
    };
    // After the block: dropping waits for every other CPU, and the page-fault handler takes this same lock with IF clear.
    drop(released);

    let guard = PROCESS_TABLE.lock();
    let proc = guard.as_ref().unwrap().get(process_pid).expect("release_thread: a thread's process is not in the table");
    let cpu_ms = proc.threads.get(tid).and_then(|t| t.sched()).map_or(0, scheduler::task_cpu_ns) / 1_000_000;
    let name = proc.name_str();
    crate::log_limited!("exit: {name} tid={tid} code={code} cpu={cpu_ms}ms");
}

/// A thread's scheduler record, cloned out of the table so wake/retire never hold the table lock while they post.
pub fn thread_sched(pid: Pid, tid: Tid) -> Option<ThreadSched> {
    let guard = PROCESS_TABLE.lock();
    let table = guard.as_ref()?;
    table.get(pid)?.threads.get(tid)?.sched().cloned()
}

/// The physical address of a futex word, demand-paged in like any other user access. Alignment matters: the scheduler dereferences this directly on every wake check, so an unaligned word could straddle into the next physical page.
/// Not a lease: nothing pins the frame, and a sibling's `munmap` can free it under a parked wait. Made safe by `AddressSpace::unmap` ending orphaned waits, and `scheduler::futex_wait` re-deriving the translation on every check.
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
pub fn wake_pipe_readers(pipe_id: pipe::PipeId) {
    scheduler::wake_pipe_readers(pipe_id);
}

/// Wake processes blocked on writing to a pipe that now has space.
pub fn wake_pipe_writers(pipe_id: pipe::PipeId) {
    scheduler::wake_pipe_writers(pipe_id);
}

/// Ask `join` once more; an unsettled one collects under the table lock, which is what makes validating the parent-thread relationship and collecting the zombie one act.
pub fn ask_join(join: &join::Join, tid: Tid, parent_pid: Pid) -> Option<Result<i32, join::JoinRefused>> {
    join.ask(|| join::collect_zombie(PROCESS_TABLE.lock().as_mut().unwrap(), parent_pid, tid))
}

/// Handle a page fault at `fault_addr` by looking up the current process's VMAs. Returns whether the fault was resolved.
/// A 2 MiB window may be filled by several racing threads; only the one that wins [`AddressSpace::map_window_if_absent`]'s critical section installs it, and a loser drops its frame but is still charged for it — `fault_demand_count + fault_zero_count` exceeds `alloc_count` by exactly the fills thrown away.
pub fn handle_page_fault(fault_addr: u64, _error_code: u64) -> bool {
    let t0 = crate::clock::nanos_since_boot();
    let tid = current_tid();
    if tid == Tid::MAX {
        return false;
    }
    let (data_arc, addr_space) = {
        let Some(addr_space) = scheduler::current_address_space() else { return false };
        let guard = PROCESS_TABLE.lock();
        let Some(table) = guard.as_ref() else { return false };
        let pid = current_process();
        let Some(proc) = table.get(pid) else { return false };
        let data = Arc::clone(&proc.process_data);
        (data, addr_space)
    };

    let page_2m = PAGE_2M;
    let region_start = fault_addr & !(page_2m - 1);
    let region_end_full = region_start.saturating_add(page_2m);

    // Snapshot region info (brief address-space lock) so the fill below can run without holding it.
    struct RegionSnap {
        start: u64,
        end: u64,
        prot: Prot,
        kind: RegionSnapKind,
    }
    enum RegionSnapKind {
        Anonymous,
        FileBacked { backing: Arc<dyn crate::file_backing::FileBacking>, file_offset: u64, file_size: u64 },
    }

    let (window_prot, regions) = {
        let as_guard = addr_space.lock();

        match as_guard.find_region(UserAddr::new(fault_addr)) {
            None => return false,
            // A `Mapped` region's pages exist from creation, so a fault inside one (a `MmapProt::NONE` reservation) must refuse, not fill.
            // Tested on the faulting address, not the overlap set below: a shared window still contributes zeros to a demand-paged one.
            Some((_, region)) if matches!(region.kind, crate::vma::RegionKind::Mapped) => {
                return false;
            }
            Some(_) => {}
        }

        // Fast answer, not the decision: the lock drops before the fill, so a racing sibling can see the same "not mapped".
        // `map_window_if_absent` re-checks inside the install's critical section; this just skips a fill for the common already-filled case.
        if as_guard.translate(UserAddr::new(region_start)).is_some() {
            return true;
        }

        // Per-4-KiB protection, not one verdict for all 512: `PT_LOAD` segments align to 4 KiB, so a window can hold both `.text` and `.data`, and OR-ing their flags would make it writable and executable at once; padding past the image gets the least of the three.
        let mut window_prot = WindowProt::uniform(Prot::Read);
        let mut snaps = Vec::new();
        for (&start_addr, region) in as_guard.overlapping_regions(UserAddr::new(region_start), UserAddr::new(region_end_full)) {
            let (snap_kind, prot) = match &region.kind {
                crate::vma::RegionKind::Anonymous { prot } => (RegionSnapKind::Anonymous, *prot),
                crate::vma::RegionKind::FileBacked { backing, file_offset, file_size, prot } => (
                    RegionSnapKind::FileBacked {
                        backing: Arc::clone(backing),
                        file_offset: *file_offset,
                        file_size: *file_size,
                    },
                    *prot,
                ),
                // Already mapped eagerly; contributes zeros and the least protection.
                crate::vma::RegionKind::Mapped => (RegionSnapKind::Anonymous, Prot::Read),
            };
            let start = start_addr.raw();
            let end = start + region.size;
            let mut page = region_start.max(start) & !0xFFF;
            while page < region_end_full.min(end) {
                window_prot.set(page - region_start, prot);
                page += 4096;
            }
            snaps.push(RegionSnap { start, end, prot, kind: snap_kind });
        }
        (window_prot, snaps)
    };

    let mut data = data_arc.lock();

    let reloc_index = data.elf.reloc_index.clone();
    let elf_base = data.elf.elf_base.raw();

    let page_alloc = match PageAlloc::new(page_2m as usize, crate::mm::pmm::Category::DemandPage) {
        Some(a) => a,
        None => return false,
    };
    // A bounds-checked window, not a bare `*mut u8`: both fills below are the kernel's own arithmetic, unchecked until something bounds it.
    let page = page_alloc.window();

    // Multiple segments (e.g. .text/.rodata) can share a 2MB range.
    let mut io_reads: u32 = 0;
    for region in &regions {
        match &region.kind {
            RegionSnapKind::Anonymous => {
                // Already zeroed by PageAlloc::new
            }
            RegionSnapKind::FileBacked { backing, file_offset, file_size } => {
                let fill_start = region_start.max(region.start);
                let fill_end = region_end_full.min(region.end);
                let mut vaddr = fill_start & !0xFFF;

                while vaddr < fill_end {
                    let vma_offset = vaddr - region.start;
                    let page_offset = (vaddr - region_start) as usize;

                    if vma_offset < *file_size {
                        let byte_offset = vma_offset + file_offset;
                        let mut page_buf = [0u8; PAGE_BYTES];
                        // Unhandled, not zero-filled: zeros here would be instructions/constants the program never had; `page_alloc` is a local, so returning drops it.
                        if backing.read_page(byte_offset, &mut page_buf).is_err() {
                            log!("fault: {:#x} is backed by a file byte {byte_offset} that the \
                                 device would not read; leaving the fault unhandled",
                                fault_addr);
                            return false;
                        }
                        io_reads += 1;
                        let valid = if vma_offset + 4096 <= *file_size { 4096 } else { (*file_size - vma_offset) as usize };
                        // SAFETY: `copy_from` bounds `page_offset + valid` against `page_alloc`'s own size; the frame is unmapped in any address space until `map_window` below, so nothing else can read it, and `page_buf` is this loop's own stack array.
                        unsafe { page.copy_from(page_offset, &page_buf[..valid]) };
                    }
                    vaddr += 4096;
                }
            }
        }
    }


    let mut total_relocs = 0u16;
    if let Some(ref ri) = reloc_index {
        let mut offset = 0u64;
        while offset < page_2m {
            let page_elf_offset = (region_start + offset).wrapping_sub(elf_base);
            if ri.has_relocs_in_page(page_elf_offset) {
                // `subslice` bounds `offset + 4096` against the frame's own size; `apply_to_page` bounds every write against the window it's handed.
                total_relocs = total_relocs.saturating_add(
                    ri.apply_to_page(page_elf_offset, page.subslice(offset as usize, 4096)) as u16,
                );
            }
            offset += 4096;
        }
    }


    // No invalidation: the fault means the PDE was absent, so nothing is cached from it (`map_window` derives that from the entry it replaces).
    // Claimed under the install lock, not the decide-to-fill one: a racing sibling can reach here too and lose the install race; the loser's fill is thrown away rather than both being installed, since no shootdown is issued from a fault.
    let installed = addr_space.lock().map_window_if_absent(
        UserAddr::new(region_start),
        page_alloc.phys(),
        &window_prot,
    );

    if installed {
        data.demand_pages.push(page_alloc);

        data.alloc_count += 1;
        let current_mem = data.demand_pages.len() as u64 * PAGE_2M;
        if current_mem > data.peak_memory {
            data.peak_memory = current_mem;
        }
    } else {
        // Wasted fill: dropped now, since nothing but this local knows it exists.
        drop(page_alloc);
    }

    // Charged whether or not the fill was kept, so a losing race doesn't vanish from accounting — the gap vs `alloc_count` is the only reading of how often this races.
    let fault_elapsed = crate::clock::nanos_since_boot() - t0;
    data.accounting.fault_ns += fault_elapsed;
    if io_reads > 0 {
        data.accounting.fault_demand_count += 1;
        data.accounting.io_read_ops += io_reads;
        data.accounting.io_read_bytes += io_reads as u64 * 4096;
    } else {
        data.accounting.fault_zero_count += 1;
    }

    // The faulting page's own protection, not the window's: a window can hold code and data pages together.
    let fault_prot = regions
        .iter()
        .find(|r| fault_addr >= r.start && fault_addr < r.end)
        .map_or(Prot::Read, |r| r.prot);
    let elapsed_us = (fault_elapsed / 1000).min(u16::MAX as u64) as u16;
    data.fault_trace.record(PageFaultRecord {
        fault_addr,
        page_elf_offset: region_start.wrapping_sub(elf_base),
        block_idx: (region_start / PAGE_2M) as u32,
        reloc_count: total_relocs,
        flags: match fault_prot {
            Prot::Read => 0,
            Prot::ReadWrite => 1,
            Prot::ReadExec => 16,
        },
        duration_us: elapsed_us,
    });

    true
}

/// Dump the page fault trace and memory around `fault_addr` for the current process; called from the exception handler on user-mode crashes.
pub fn dump_crash_diagnostics(fault_addr: u64, rip: u64) {
    let Some(pid) = percpu::current_pid() else { return };

    let data_arc = {
        let Some(guard) = PROCESS_TABLE.try_lock() else {
            log!("  [crash diagnostics: PROCESS_TABLE locked, skipping]");
            return;
        };
        let Some(table) = guard.as_ref() else { return };
        match table.get(pid) {
            Some(proc) => Arc::clone(&proc.process_data),
            None => return,
        }
    };
    let Some(data) = data_arc.try_lock() else {
        log!("  [crash diagnostics: ProcessData locked, skipping]");
        return;
    };

    let trace = &data.fault_trace;
    let count = trace.total().min(32);
    if count > 0 {
        log!("  Page fault trace ({} total, last {}):", trace.total(), count);
        for rec in trace.iter_chronological() {
            if rec.fault_addr == 0 { continue; }
            let mut flag_str = [b' '; 5];
            if rec.flags & 1 != 0 { flag_str[0] = b'W'; } // writable
            if rec.flags & 2 != 0 { flag_str[1] = b'R'; } // has_relocs
            if rec.flags & 4 != 0 { flag_str[2] = b'A'; } // anonymous
            if rec.flags & 8 != 0 { flag_str[3] = b'Z'; } // beyond extent (zero)
            // Never beside `W` (one `Prot`'s variants); both set means lost W^X.
            if rec.flags & 16 != 0 { flag_str[4] = b'X'; } // executable
            let flags = core::str::from_utf8(&flag_str).unwrap_or("????");
            log!("    fault={:#x} elf_off={:#x} blk={} relocs={} {}us [{}]",
                rec.fault_addr, rec.page_elf_offset, rec.block_idx,
                rec.reloc_count, rec.duration_us, flags);
        }
    }

    let Some(addr_space) = scheduler::current_address_space() else { return };

    // Reads via the direct map (no USER bit) to avoid SMAP faults.
    let read_user = |virt: u64| -> Option<u64> {
        if !virt.is_multiple_of(8) { return None; }
        let phys = addr_space.lock().translate(UserAddr::new(virt))?;
        // SAFETY: `translate` answered `Some`, so `phys` is `virt`'s current direct-map address; the `virt % 8` guard keeps the `u64` within the one 2 MiB page the translation answered for. `read_volatile`: this is a crash report, and two folded reads would print a value never fetched.
        Some(unsafe { phys.as_ptr::<u64>().read_volatile() })
    };

    let dump_region = |label: &str, addr: u64| {
        if read_user(addr).is_none() { return; }
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

/// What a crash report learned when it asked for a user address's symbol. Not a `bool`: "no symbol was logged" would conflate an address the tables genuinely don't cover with one nothing looked up because a lock was held.
#[must_use = "an address with no symbol line still has to be printed"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SymbolLookup {
    /// Resolved: the line naming it has already been logged.
    Named,
    /// The symbol table was read and covers no such address.
    Unnamed,
    /// Nothing was read: a scheduler pass already held this CPU's task record when the report began, so the running task's symbols were unreachable.
    InPass,
    /// Nothing was read: no task is running on this CPU (idle, or before the first task).
    NoTask,
}

impl SymbolLookup {
    /// Log the bare address for a lookup with no symbol line, saying why; [`Named`](Self::Named) logs nothing — its line is already out.
    pub fn log_bare(self, addr: u64) {
        match self {
            Self::Named => {}
            Self::Unnamed => log!("    {:#x}", addr),
            Self::InPass => {
                log!("    {:#x}  <symbol unread: a scheduler pass held this CPU's task record>", addr)
            }
            Self::NoTask => {
                log!("    {:#x}  <symbol unread: no task is running on this CPU>", addr)
            }
        }
    }
}

/// Resolve and log a user-mode address against the running process's symbol table; see [`with_current_symbols`].
pub fn resolve_user_symbol(addr: u64) -> SymbolLookup {
    with_current_symbols(|syms| crate::symbols::resolve_user(syms, addr))
}

/// [`resolve_user_symbol`] for a backtrace frame's return address — see [`crate::symbols::SymbolTable::resolve_return`].
pub fn resolve_user_symbol_return(return_addr: u64) -> SymbolLookup {
    with_current_symbols(|syms| crate::symbols::resolve_user_return(syms, return_addr))
}

/// Run `f` against the symbol table of the task this CPU is running, and say what happened.
/// Three-way, not two: `f` decides between [`Named`](SymbolLookup::Named) and [`Unnamed`](SymbolLookup::Unnamed); every other answer means the address was never looked up, and the caller must print it via [`SymbolLookup::log_bare`].
/// No lock: this runs from fault/panic reports, where the faulting thread may hold `PROCESS_TABLE`, so a task carries its own process's symbols on its own record ([`sched::driver::current_symbols`]) instead.
/// `pid` is not a parameter: a report is always about the process whose CPU is producing it.
fn with_current_symbols(f: impl FnOnce(&crate::symbols::SymbolTable) -> bool) -> SymbolLookup {
    let Some(syms) = crate::sched::driver::current_symbols() else {
        // This CPU cannot switch between the two causes mid-report: a report is not reschedulable while `PerCpu::fault_state` is non-zero.
        return if crate::sched::driver::in_pass() {
            SymbolLookup::InPass
        } else {
            SymbolLookup::NoTask
        };
    };
    if f(&syms) {
        SymbolLookup::Named
    } else {
        SymbolLookup::Unnamed
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
    for sched in claim(pid, KILLED_EXIT_CODE, None, &mut owed) {
        scheduler::post_retire(&sched);
    }
    walk(owed);
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

/// The shell convention for "died on SIGKILL"; kept because every test that reads one already spells it.
pub const KILLED_EXIT_CODE: i32 = 137;

/// The shell convention for "died on SIGSEGV" (same mistake class, pointer instead of handle).
pub const HANDLE_FAULT_EXIT_CODE: i32 = 139;

/// End a process that named a handle it does not hold — userland's bug, not the kernel's, so the process dies alone rather than panicking the kernel.
/// Must be reached with nothing held: this calls `exit`, which takes the process's own lock, the table lock, and whatever a released object reaches for.
pub fn handle_fault(error: crate::object::HandleError) -> ! {
    log!(
        "handle fault: pid={} tid={} syscall={} {error}",
        current_process(),
        current_tid(),
        percpu::syscall_num(),
    );
    exit(HANDLE_FAULT_EXIT_CODE)
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

    // Once this CPU is committed and about to run something: the counter is
    // per CPU, so a CPU nobody arms here is one the hard-lockup bound does not cover.
    crate::hardlockup::arm_this_cpu();

    log!("CPU {me}: joining scheduler");
    scheduler::enter_idle_loop();
}
