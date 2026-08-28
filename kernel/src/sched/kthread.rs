//! Kernel threads: ordinary tasks that name `mm::paging::kernel` as their
//! address space, enter through `loader::kernel_start`, and hold a process-table
//! entry. One is preempted or stolen only at a preemption point its body reaches,
//! and a Ring 0 loop reaches none. [`ROWS`] holds every one, and
//! [`panic_recovers_here`] says what a panic inside one means.

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

use super::payload::ThreadSched;

/// `klogd`, `usbd` and `iod`, plus one `log-storm` thread per shard in the actuator build.
#[cfg(not(feature = "boot-actuators"))]
const MAX_KERNEL_TASKS: usize = 3;
#[cfg(feature = "boot-actuators")]
const MAX_KERNEL_TASKS: usize = 3 + toyos_abi::log::MAX_LOG_SHARDS;

/// Collides with no packed id: neither id map issues `u32::MAX`.
const NO_TASK: u64 = u64::MAX;

/// Distinct from a published identity, so the identity can be stored last,
/// after the policy word, without another claimant matching the same row.
const CLAIMING: u64 = u64::MAX - 1;

/// `task` is stored `Release` after `recoverable` and loaded `Acquire` before it,
/// so a row found by identity never answers with an unwritten policy.
struct Row {
    task: AtomicU64,
    // `recoverable` exists because `percpu::in_syscall()` is never true for a kernel
    // thread, which would otherwise make every kernel-thread panic halt the machine by default.
    recoverable: AtomicU64,
}

/// A row reserved before the table lock and published before `enqueue_new`.
struct Claim(&'static Row);

impl Claim {
    /// Reserve a row, or panic naming the thread.
    fn take(name: &str) -> Self {
        for row in &ROWS {
            if row
                .task
                .compare_exchange(NO_TASK, CLAIMING, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return Self(row);
            }
        }
        panic!("kthread: {name} is the {}th kernel thread and there is room for {MAX_KERNEL_TASKS}", MAX_KERNEL_TASKS + 1);
    }

    /// Payload first, then the identity `Release`.
    fn publish(self, id: TaskId, on_panic: OnPanic) {
        self.0
            .recoverable
            .store(u64::from(on_panic == OnPanic::Recover), Ordering::Relaxed);
        self.0.task.store(id.pack(), Ordering::Release);
    }
}

/// Registered at spawn and never cleared: a dead `Recover` thread's row stays.
static ROWS: [Row; MAX_KERNEL_TASKS] =
    [const { Row { task: AtomicU64::new(NO_TASK), recoverable: AtomicU64::new(0) } };
        MAX_KERNEL_TASKS];

/// What a kernel thread's panic does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OnPanic {
    /// Ask for `Recover` only when the thread's absence is both survivable and visible.
    Recover,
    /// `Halt` is `klogd`'s answer: it is the machine's only console drainer,
    /// so killing it would leave the machine silently mute.
    Halt,
}

/// Lock-free and fault-free, so it may run with any lock held or preemption on.
fn current_row() -> Option<&'static Row> {
    let (Some(pid), Some(tid)) = (
        crate::arch::percpu::current_pid(),
        crate::arch::percpu::current_tid(),
    ) else {
        return None;
    };
    let packed = TaskId(pid, tid).pack();
    // Pairs with `Claim::publish`'s `Release`: a relaxed load could read an unwritten `recoverable`.
    ROWS.iter().find(|row| row.task.load(Ordering::Acquire) == packed)
}

/// Is the task this CPU is running a kernel thread?
pub fn current_is_kernel_thread() -> bool {
    current_row().is_some()
}

/// Is `id` a kernel thread?
// No lock: `drain_irqs` calls this on a machine already suspected of being
// stuck, where taking one could hang diagnostics.
pub fn is_kernel_task(id: TaskId) -> bool {
    let packed = id.pack();
    ROWS.iter().any(|row| row.task.load(Ordering::Acquire) == packed)
}

/// Whether a panic on the running task recovers; `None` unless it is a kernel thread.
pub fn panic_recovers_here() -> Option<bool> {
    Some(current_row()?.recoverable.load(Ordering::Relaxed) != 0)
}

/// Start a kernel thread running `body(arg)` on its own kernel stack and return its scheduler faces.
pub fn spawn(name: &str, body: extern "C" fn(u64) -> !, arg: u64, on_panic: OnPanic) -> ThreadSched {
    let (stack, entry_rsp) = crate::loader::alloc_kernel_stack(
        crate::loader::kernel_start,
        body as usize as u64,
        0,
        arg,
    )
    .unwrap_or_else(|| panic!("kthread: no kernel stack for {name}"));

    // Before the table lock: a panic holding the process table hangs the machine.
    let claim = Claim::take(name);

    let mut short = [0u8; THREAD_NAME_LEN];
    let len = name.len().min(THREAD_NAME_LEN - 1);
    short[..len].copy_from_slice(&name.as_bytes()[..len]);

    // No user half: the empty table, and frames resolve through `symbols::resolve_kernel`.
    let syms = Arc::new(SymbolTable::empty());

    // One hold across insert and place: a visible pid already has its thread scheduled.
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("kthread: spawned before process::init");
    let pid = table.insert_with(|pid| {
        ProcessEntry::new(
            pid,
            short,
            Arc::new(Lock::new(kernel_process_data(name))),
            Arc::clone(&syms),
            ThreadEntry::new(Arc::new(Lock::new(kernel_thread_data()))),
        )
    });
    let tid = table.get(pid).expect("kthread: the entry just inserted is gone").main_tid();
    // Before `enqueue_new`: from that call the task can run and panic.
    claim.publish(TaskId(pid, tid), on_panic);
    // The kernel address space, named so one declaration decides every task's `cr3`.
    let (sched, _dst) = scheduler::enqueue_new(
        TaskId(pid, tid),
        stack,
        entry_rsp,
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

    crate::log!(
        "kthread: {name} pid={} tid={} runs in the kernel address space; a panic in it {}",
        pid,
        tid,
        match on_panic {
            OnPanic::Halt => "halts the machine",
            OnPanic::Recover => "kills the thread",
        }
    );
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
