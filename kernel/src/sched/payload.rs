//! The kernel's `SchedPayload`: [`KernelCtx`] is what `Hw::switch` loads
//! through the raw context pointer; [`KernelPayload`] is what must be
//! released exactly once, by `Hw::release`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use toyos_sched::fair::{FairShare, ShareState};
use toyos_sched::hw::Nanos;
use toyos_sched::msg::Msg;
use toyos_sched::sync::LeafLock;
use toyos_sched::task::{SchedPayload, TaskAccounting, TaskShared, WaitClass};
use toyos_sched::waitq::{WaitList, WaitQueue, WaitTicket};

use crate::completion::{Inbox, Watch};
use crate::mm::paging::Cr3;
use crate::scheduler::OperationSlot;
use crate::process::{OwnedAlloc, PageTables, ProcessAccounting, TaskId};
use crate::symbols::SymbolTable;
use crate::sync::Lock;

/// The environment's leaf lock; holding it raises the preempt count, making a wake path a legal mailbox producer.
pub struct KernelLock<T>(Lock<T>);

impl<T> KernelLock<T> {
    pub const fn new(value: T) -> Self {
        Self(Lock::new(value))
    }
}

impl<T: Send> LeafLock<T> for KernelLock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.lock())
    }
}

pub type KMsg = Msg<KernelPayload>;
pub type KShared = TaskShared<KMsg>;
pub type KShare = FairShare<KernelLock<ShareState>>;
pub type KWaitList = KernelLock<WaitList<KMsg>>;
pub type KWaitQueue = WaitQueue<KMsg, KWaitList>;
/// The core's wait ticket; blocking sites use `driver::Ticket`, which wraps it in the needed preempt guard.
pub type RawTicket<'q> = WaitTicket<'q, KMsg, KWaitList>;

/// A queue in a `static` — the device queues and the futex/park buckets.
pub const fn static_queue(class: WaitClass) -> KWaitQueue {
    KWaitQueue::new(class, KernelLock::new(WaitList::new()))
}

/// The saved callee context; everything `Hw::switch` must load without dereferencing anything else.
pub struct KernelCtx {
    /// Saved kernel stack pointer, written by the `context_switch` asm.
    pub rsp: u64,
    pub cr3: Cr3,
    pub fs_base: u64,
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
    /// This task's process's symbol table; kept here, not looked up via the process table, so a crash report never takes that lock.
    /// On the payload, not the handle: the handle outlives the task until reaped, and this is megabytes of process pages.
    pub symbols: Arc<SymbolTable>,
}

impl SchedPayload for KernelPayload {
    type Ctx = KernelCtx;
    type ShareLock = KernelLock<ShareState>;
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
    acct: Lock<TaskAccounting>,
    /// Set by `Hw::release`; the one fact a retirer needs, that the thread is off every CPU.
    released: AtomicBool,
    /// Where this thread parks: its own one-waiter queue, never woken as a queue — a post claims the rendezvous word directly.
    /// One list of one, not shared hashed buckets — those would make `Registration::finish` scan past every unrelated sleeper.
    park: KWaitQueue,
    /// What another thread arms on to be told this one moved: its exit, for `SYS_THREAD_JOIN`, and its release, for the retirer.
    watch: Watch,
    /// Cancels reported to this thread; a second one means a caller swallowed the first, so it panics rather than spinning.
    cancels: AtomicU32,
    /// This thread's completions inbox; kept on the cross-CPU handle so a post never asks the process table.
    inbox: Inbox,
    /// The operation this thread is inside, if any; kept here so it survives a mid-operation migration. `scheduler::Operation` owns the rules.
    operation: OperationSlot,
}

impl TaskHandle {
    pub fn new() -> Self {
        Self {
            cpu_ns: AtomicU64::new(0),
            running_since: AtomicU64::new(0),
            acct: Lock::new(TaskAccounting::default()),
            released: AtomicBool::new(false),
            park: static_queue(WaitClass::Other),
            watch: Watch::new(),
            cancels: AtomicU32::new(0),
            inbox: Inbox::new(),
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
        *self.acct.lock() = acct;
    }

    /// Announces the death only after the payload is dropped; that ordering is the guarantee a retirer's park buys.
    pub(crate) fn publish_released(&self) {
        self.released.store(true, Ordering::Release);
        // The retirer arms on this thread's own watch, the same subject a joiner uses.
        crate::completion::post(
            crate::completion::Subject::of(&self.watch),
            crate::completion::Outcome::Gone(crate::completion::Reason::Closed),
        );
    }

    /// Has `Hw::release` run for this thread? The retire wait's condition.
    pub fn released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    pub fn inbox(&self) -> &Inbox {
        &self.inbox
    }

    /// Where this thread's establishment lives; `scheduler::Operation` owns every rule about it.
    pub fn operation(&self) -> &OperationSlot {
        &self.operation
    }

    pub fn park_queue(&self) -> &KWaitQueue {
        &self.park
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
            "completion: a second cancel reported to one thread — the first was \
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

    pub fn merge_into(&self, target: &mut ProcessAccounting) {
        let acct = self.acct.lock();
        merge_accounting(&acct, target);
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
        use toyos_sched::task::TaskState;
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
