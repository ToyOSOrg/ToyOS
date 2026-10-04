//! Kernel threads: ordinary tasks that name `mm::paging::kernel` as their
//! address space, enter through `loader::kernel_start`, and hold a process-table
//! entry. One is preempted or stolen only at a preemption point its body reaches,
//! and a Ring 0 loop reaches none. [`ROWS`] holds every one.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::process::{
    ElfInfo, Endowments, PageFaultTrace, ProcessAccounting, ProcessData, ProcessEntry, ThreadData,
    ThreadEntry, PROCESS_TABLE, THREAD_NAME_LEN,
};
use crate::scheduler::{self, TaskId};
use crate::symbols::SymbolTable;
use crate::sync::Lock;
use kernel::proclife::Processes;

use super::payload::ThreadSched;

/// `klogd`.
const MAX_KERNEL_TASKS: usize = 1;

/// Collides with no packed id: neither id map issues `u32::MAX`.
const NO_TASK: u64 = u64::MAX;

/// A reserved row whose identity is not yet known; collides with no packed id.
const CLAIMING: u64 = u64::MAX - 1;

/// A row reserved before the table lock and published before `enqueue_new`.
struct Claim(&'static AtomicU64);

impl Claim {
    /// Reserve a row, or panic naming the thread.
    fn take(name: &str) -> Self {
        for row in &ROWS {
            if row
                .compare_exchange(NO_TASK, CLAIMING, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Self(row);
            }
        }
        panic!("kthread: {name} is the {}th kernel thread and there is room for {MAX_KERNEL_TASKS}", MAX_KERNEL_TASKS + 1);
    }

    fn publish(self, id: TaskId) {
        self.0.store(id.pack(), Ordering::Relaxed);
    }
}

/// Registered at spawn and never cleared.
static ROWS: [AtomicU64; MAX_KERNEL_TASKS] = [const { AtomicU64::new(NO_TASK) }; MAX_KERNEL_TASKS];

/// Is the task this CPU is running a kernel thread? Lock-free and fault-free,
/// so it may run with any lock held or preemption on.
pub fn current_is_kernel_thread() -> bool {
    let (Some(pid), Some(tid)) = (
        crate::arch::percpu::current_pid(),
        crate::arch::percpu::current_tid(),
    ) else {
        return false;
    };
    is_kernel_task(TaskId(pid, tid))
}

/// Is `id` a kernel thread?
// No lock: `drain_irqs` calls this on a machine already suspected of being
// stuck, where taking one could hang diagnostics.
pub fn is_kernel_task(id: TaskId) -> bool {
    let packed = id.pack();
    ROWS.iter().any(|row| row.load(Ordering::Relaxed) == packed)
}

/// Start a kernel thread running `body(arg)` on its own kernel stack and return its scheduler faces.
pub fn spawn(name: &str, body: extern "C" fn(u64) -> !, arg: u64) -> ThreadSched {
    let (stack, entry_sp) = crate::loader::alloc_kernel_stack(
        crate::loader::kernel_start,
        body as usize as u64,
        0,
        arg,
    )
    .unwrap_or_else(|| panic!("kthread: no kernel stack for {name}"));

    // Before the table lock: a panic holding the process table hangs the machine.
    let claim = Claim::take(name);
    let taken = PROCESS_TABLE.lock().as_mut().expect("kthread: spawned before process::init").pids().take();
    let pid = taken.unwrap_or_else(|| panic!("kthread: {name} found every pid issued"));

    let mut short = [0u8; THREAD_NAME_LEN];
    let len = name.len().min(THREAD_NAME_LEN - 1);
    short[..len].copy_from_slice(&name.as_bytes()[..len]);

    // No user half: the empty table, and frames resolve through `symbols::resolve_kernel`.
    let syms = Arc::new(SymbolTable::empty());

    // One hold across insert and place: a visible pid already has its thread scheduled.
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("kthread: spawned before process::init");
    table.insert(ProcessEntry::new(
        crate::object::process::ProcessObject::new(pid),
        short,
        Arc::new(Lock::new(kernel_process_data(name))),
        Arc::clone(&syms),
        ThreadEntry::new(Arc::new(Lock::new(kernel_thread_data()))),
        kernel::proclife::Node::root(),
    ));
    let tid = table.get(pid).expect("kthread: the entry just inserted is gone").main_tid();
    claim.publish(TaskId(pid, tid));
    // The kernel address space, named so one declaration decides every task's `cr3`.
    let (sched, _dst) = scheduler::enqueue_new(
        TaskId(pid, tid),
        stack,
        entry_sp,
        crate::mm::paging::kernel().clone(),
        0,
        syms,
    );
    table
        .get_mut(pid)
        .and_then(|p| p.threads_mut().get_mut(tid))
        .expect("kthread: the thread just inserted is gone")
        .set_sched(sched.clone());
    drop(guard);

    crate::log!("kthread: {name} pid={pid} tid={tid} runs in the kernel address space");
    sched
}

/// Every field is the empty value: a kernel thread has no user half.
fn kernel_process_data(name: &str) -> ProcessData {
    ProcessData {
        handles: crate::object::HandleTable::new(),
        cwd: String::from("/"),
        env: Vec::new(),
        elf: ElfInfo::none(),
        mmap_regions: Vec::new(),
        pipe_maps: Vec::new(),
        demand_pages: Vec::new(),
        fault_trace: PageFaultTrace::new(),
        peak_memory: 0,
        alloc_count: 0,
        free_count: 0,
        exe_path: String::from(name),
        spawn_ns: crate::clock::nanos_since_boot(),
        accounting: ProcessAccounting::default(),
        endowments: Endowments::empty(),
    }
}

fn kernel_thread_data() -> ThreadData {
    ThreadData {
        tls_pages: None,
        stack_pages: None,
        user_stack_base: crate::mm::UserAddr::new(0),
        user_stack_size: 0,
        syscall_counts: [0; toyos_abi::syscall::SYSCALL_PROFILE_BINS],
        syscall_total: 0,
        syscall_total_ns: 0,
    }
}
