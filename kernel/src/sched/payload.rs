//! The kernel's `SchedPayload`: [`KernelCtx`] is what `Hw::switch` loads
//! through the raw context pointer; [`KernelPayload`] is what must be
//! released exactly once, by `Hw::release`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use kernel::sched::fair::{FairShare, ShareState};
use kernel::sched::hw::Nanos;
use kernel::sched::msg::Msg;
use kernel::sched::sync::CellLock;
use kernel::sched::task::{SchedPayload, TaskAccounting, TaskShared, WaitClass};
use kernel::sched::park::WaitTicket;

use crate::watch::Watch;
use crate::mm::paging::Root;
use crate::scheduler::OperationSlot;
use crate::process::{OwnedAlloc, PageTables, ProcessAccounting, TaskId, UserImage};
use crate::sync::Lock;

/// The environment's leaf lock; holding it raises the preempt count, making a wake path a legal mailbox producer.
pub struct KernelLock<T>(Lock<T>);

impl<T> KernelLock<T> {
    pub const fn new(value: T) -> Self {
        Self(Lock::new(value))
    }
}

impl<T: Send> CellLock<T> for KernelLock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.lock())
    }
}

pub type KMsg = Msg<KernelPayload>;
pub type KShared = TaskShared<KMsg>;
pub type KShare = FairShare<KernelLock<ShareState>>;
/// The core's wait ticket; blocking sites use `driver::Ticket`, which wraps it in the needed preempt guard.
pub type RawTicket = WaitTicket<KMsg>;

/// The saved callee context; everything `Hw::switch` must load without dereferencing anything else, named
/// by role because every architecture's switch loads it.
pub struct KernelCtx {
    /// Saved kernel stack pointer, written by the `context_switch` asm.
    pub sp: u64,
    pub root: Root,
    pub thread_pointer: u64,
    pub kernel_stack_top: u64,
    /// `None` is this CPU's idle context.
    pub id: Option<TaskId>,
    /// Swapped with the per-CPU word at every switch; contexts don't all owe the same `enable` count.
    pub preempt: u32,
}

/// Everything the kernel owns per task, released exactly once (the address-space `Arc` cannot double-drop).
pub struct KernelPayload {
    pub id: TaskId,
    pub kernel_stack: OwnedAlloc,
    /// The address space this task runs in; never `Option` — a kernel thread runs in the kernel's own.
    pub address_space: PageTables,
    /// The cross-CPU-readable face of this task; a `CpuSched` is `!Sync` and cannot be walked remotely.
    pub handle: Arc<TaskHandle>,
    /// This task's process's image, as a crash report records a frame in it; kept here, not looked up via the process table, so a crash report never takes that lock. `None` for a kernel thread.
    pub image: Option<Arc<UserImage>>,
}

impl SchedPayload for KernelPayload {
    type Ctx = KernelCtx;
    type ShareLock = KernelLock<ShareState>;

    /// The thread, as `TaskId::pack` packs it: what a diary record names.
    fn name(&self) -> u64 {
        self.id.pack()
    }
}

/// State word values for `task_sched_state` (the `ps` column).
pub const SCHED_RUNNING: u8 = 0;
pub const SCHED_READY: u8 = 1;
pub const SCHED_BLOCKED: u8 = 2;
pub const SCHED_UNKNOWN: u8 = 3;

/// What a thread other than the one running can be asked about; published here since a `CpuSched` is `!Sync` and unreachable remotely.
pub struct TaskHandle {
    cpu_ns: AtomicU64,
    /// Dispatch timestamp while running, 0 otherwise; a reader adds the live slice itself.
    running_since: AtomicU64,
    /// What another thread arms on to be told this one moved: its exit, for `SYS_THREAD_JOIN`.
    watch: Watch,
    /// Cancels reported to this thread; a second one means a caller swallowed the first, so it panics rather than spinning.
    cancels: AtomicU32,
    /// The operation this thread is inside, if any; kept here so it survives a mid-operation migration. `scheduler::Operation` owns the rules.
    operation: OperationSlot,
}

impl TaskHandle {
    pub fn new() -> Self {
        Self {
            cpu_ns: AtomicU64::new(0),
            running_since: AtomicU64::new(0),
            watch: Watch::new(),
            cancels: AtomicU32::new(0),
            operation: OperationSlot::new(),
        }
    }

    pub(crate) fn publish(&self, acct: &TaskAccounting, running_since: Option<Nanos>) {
        self.cpu_ns.store(acct.cpu_ns, Ordering::Relaxed);
        self.running_since
            .store(running_since.map_or(0, |n| n.0), Ordering::Relaxed);
    }

    /// Called once, by `Hw::release`; from here on the thread's numbers are frozen.
    pub(crate) fn finalize(&self, acct: TaskAccounting) {
        self.cpu_ns.store(acct.cpu_ns, Ordering::Relaxed);
        self.running_since.store(0, Ordering::Relaxed);
    }

    /// Where this thread's establishment lives; `scheduler::Operation` owns every rule about it.
    pub fn operation(&self) -> &OperationSlot {
        &self.operation
    }

    /// What this thread's own transitions are posted to.
    pub fn watch(&self) -> &Watch {
        &self.watch
    }

    /// Report a cancel to this thread, once. `false` means it is not killed.
    #[track_caller]
    pub fn take_cancel(&self, killed: bool) -> bool {
        if !killed {
            return false;
        }
        let reported = self.cancels.fetch_add(1, Ordering::Relaxed);
        assert!(
            reported == 0,
            "watch: a second cancel reported to one thread — the first was \
             swallowed by a caller that waited again instead of returning",
        );
        true
    }

    pub fn cpu_ns(&self) -> u64 {
        let base = self.cpu_ns.load(Ordering::Relaxed);
        match self.running_since.load(Ordering::Relaxed) {
            0 => base,
            since => base + crate::hw::now_ns().saturating_sub(since),
        }
    }

}

/// A thread's two scheduler-visible faces, kept by the process table; created at different instants.
#[derive(Clone)]
pub struct ThreadSched {
    pub handle: Arc<TaskHandle>,
    pub shared: Arc<KShared>,
}

impl ThreadSched {
    pub fn sched_state(&self) -> u8 {
        use kernel::sched::task::TaskState;
        match self.shared.state() {
            TaskState::Running(_) => SCHED_RUNNING,
            TaskState::Ready(_) | TaskState::WakeQueued(_) | TaskState::InTransit(_) => SCHED_READY,
            TaskState::Blocked(_) | TaskState::Committing(..) => SCHED_BLOCKED,
            TaskState::Dead => SCHED_UNKNOWN,
        }
    }
}

/// The core's per-class blocked-time array, spread over the kernel's named counters.
pub fn merge_accounting(acct: &TaskAccounting, target: &mut ProcessAccounting) {
    target.blocked_io_ns += acct.blocked_ns[WaitClass::Io.index()];
    target.blocked_futex_ns += acct.blocked_ns[WaitClass::Futex.index()];
    target.blocked_pipe_ns += acct.blocked_ns[WaitClass::Pipe.index()];
    target.blocked_ipc_ns += acct.blocked_ns[WaitClass::Ipc.index()];
    target.blocked_other_ns += acct.blocked_ns[WaitClass::Other.index()];
    target.runqueue_wait_ns += acct.runqueue_wait_ns;
}
