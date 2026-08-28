//! The kernel-facing scheduler API surface: no decision, state transition or
//! ordering-sensitive step happens here.
//!
//! Exception: [`Parkable`] and [`Operation`] live here because the park token
//! has no public constructor outside the two doors this module defines.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use hashbrown::HashMap;
use toyos_sched::fair::{ShareState, QUANTUM_NS};
use toyos_sched::hw::{CpuId, Machine, Nanos};
use toyos_sched::task::{WaitClass, WakeCause, WakeReason};

use crate::arch::percpu;
use crate::completion::{self, Cancel, Outcome, Subject};
use crate::hw::HW;
use crate::pipe::PipeId;
use crate::process::{self, Pid, Tid};
use crate::sched::driver::{self, cpus, preempt_off, Dispose, NewTask};
use crate::sched::payload::{KShare, KShared, KWaitQueue, KernelLock, TaskHandle, ThreadSched};
use crate::sched::reap_gate::ReapGate;
use crate::sched::waitqs;
use crate::sync::Lock;
use crate::time::{Cadence, Deadline, Duration, Tripwire};
use crate::DirectMap;

pub use crate::sched::driver::{
    current_address_space, enter_idle_loop, in_pass as in_schedule_self, total_cpu_ns,
    write_stack_canary, Ticket,
};
pub use crate::sched::MAX_CPUS;

/// Panics unless the preempt depth equals `baseline`: a mismatch means a
/// spinlock is held across a scheduler entry that switches.
#[track_caller]
fn assert_baseline(baseline: u32) {
    let depth = crate::preempt::count();
    assert!(
        depth == baseline,
        "scheduler entered while a lock is held: preempt depth {depth}, baseline {baseline}",
    );
}

/// Depth an unnested trap handler runs at: one level, raised by the entry asm
/// and lowered on the way out.
const BASELINE_TRAP: u32 = 1;

/// Depth the deferred-preempt poll runs at: zero, since all three entry paths
/// are past the trap entry level.
const BASELINE_IRQ_EXIT: u32 = 0;

/// Read from `sched::kthread`'s rows rather than the `CpuSched`, which a
/// preempting pass may be holding `&mut` at this point.
fn blocking_baseline() -> u32 {
    if crate::sched::kthread::current_is_kernel_thread() {
        0
    } else {
        BASELINE_TRAP
    }
}

/// Proof that the calling context may park; threaded by reference rather
/// than stored, with no public constructor beside [`Parkable::at_entry`] and
/// [`Operation::parkable`].
pub struct Parkable(());

impl Parkable {
    /// Asserts this context is a trap entry or a kernel thread's body, and
    /// nothing below one, then mints the proof.
    #[track_caller]
    pub fn at_entry() -> Parkable {
        assert!(
            !Operation::established(),
            "scheduler: a frame inside an established operation minted its own park \
             permission — a leaf receives one from the operation, it does not make one",
        );
        Parkable::mint()
    }

    #[track_caller]
    fn mint() -> Parkable {
        assert_baseline(blocking_baseline());
        Parkable(())
    }
}

/// One operation the running context is inside. Establishments nest; an
/// inner one may only narrow the deadline, and the guard restores what it
/// displaced on drop.
#[must_use = "an operation lasts exactly as long as this guard"]
pub struct Operation {
    /// Held rather than re-derived, so the drop restores the slot even if
    /// the task has migrated; `None` selects the per-CPU slot named by `cpu`.
    task: Option<Arc<TaskHandle>>,
    cpu: usize,
    /// What the slot held before this establishment; `None` is "no operation".
    outer: Option<u64>,
}

/// Two words rather than one sentinel: [`Deadline`] is total over its range
/// and has no value left to mean "none". Read as a pair only by the writer
/// that wrote them, so Relaxed ordering between the two is sound.
pub struct OperationSlot {
    live: core::sync::atomic::AtomicBool,
    until: AtomicU64,
}

impl OperationSlot {
    pub const fn new() -> Self {
        Self {
            live: core::sync::atomic::AtomicBool::new(false),
            until: AtomicU64::new(0),
        }
    }
}

impl Default for OperationSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a context with no task establishes: one per CPU, since boot and an
/// idle CPU's pass cannot be moved off theirs.
static NO_TASK_OPERATION: [OperationSlot; MAX_CPUS] =
    [const { OperationSlot::new() }; MAX_CPUS];

impl Operation {
    /// Declare the running context inside one operation, bounded by `until`
    /// or by whatever already bounds it, whichever comes first.
    pub fn begin(until: Deadline) -> Operation {
        let task = driver::current_handle();
        let cpu = percpu::cpu_id() as usize;
        let outer = {
            let slot = operation_slot(&task, cpu);
            let outer = slot
                .live
                .load(Ordering::Relaxed)
                .then(|| slot.until.load(Ordering::Relaxed));
            slot.until.store(
                outer.map_or(until.nanos(), |outer| outer.min(until.nanos())),
                Ordering::Relaxed,
            );
            slot.live.store(true, Ordering::Relaxed);
            outer
        };
        Operation { task, cpu, outer }
    }

    /// The deadline the operation this depth is part of has left to spend.
    /// Panics if no operation is established above this depth.
    #[track_caller]
    pub fn deadline() -> Deadline {
        let (live, until) = Self::read();
        assert!(
            live,
            "scheduler: a depth asked for its operation's deadline with no operation \
             established above it",
        );
        Deadline::at(crate::time::Instant::from_nanos_since_boot(until))
    }

    /// The park token of the operation this depth is part of. Panics if no
    /// operation is established above this depth. No caller yet:
    /// `xhci::wait_transfer` wants this, but the ticket locks above it fail
    /// [`Parkable::mint`]'s baseline assertion until they convert.
    #[allow(dead_code)]
    #[track_caller]
    pub fn parkable() -> Parkable {
        assert!(
            Self::established(),
            "scheduler: a depth asked to park with no operation established above it",
        );
        Parkable::mint()
    }

    /// Whether the running context is inside one.
    pub fn established() -> bool {
        Self::read().0
    }

    /// A borrow and not a clone: `Arc::clone`'s read-modify-write is too
    /// costly on this hot path.
    fn read() -> (bool, u64) {
        fn of(slot: &OperationSlot) -> (bool, u64) {
            (
                slot.live.load(Ordering::Relaxed),
                slot.until.load(Ordering::Relaxed),
            )
        }
        driver::with_current_handle(|task| of(task.operation()))
            .unwrap_or_else(|| of(&NO_TASK_OPERATION[percpu::cpu_id() as usize]))
    }

    fn slot(&self) -> &OperationSlot {
        operation_slot(&self.task, self.cpu)
    }
}

/// The slot a context establishes in: its task's, or its CPU's if it has none.
fn operation_slot(task: &Option<Arc<TaskHandle>>, cpu: usize) -> &OperationSlot {
    match task {
        Some(task) => task.operation(),
        None => &NO_TASK_OPERATION[cpu],
    }
}

impl Drop for Operation {
    fn drop(&mut self) {
        let slot = self.slot();
        match self.outer {
            Some(until) => slot.until.store(until, Ordering::Relaxed),
            None => slot.live.store(false, Ordering::Relaxed),
        }
    }
}

/// Process-scoped thread identity: tids are per-process only.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub Pid, pub Tid);

impl TaskId {
    pub fn pack(self) -> u64 {
        self.1.raw() as u64 | (self.0.raw() as u64) << 32
    }
    pub fn unpack(v: u64) -> Self {
        Self(Pid::from_raw((v >> 32) as u32), Tid::from_raw(v as u32))
    }
}

/// The running task, or `None` for boot and an idle CPU. No lock: called
/// with preemption still on, aliasing a preempting pass's `&mut CpuSched`.
pub fn current_task() -> Option<TaskId> {
    match (percpu::current_pid(), percpu::current_tid()) {
        (Some(pid), Some(tid)) => Some(TaskId(pid, tid)),
        _ => None,
    }
}

impl core::fmt::Display for TaskId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}:{}", self.0, self.1)
    }
}

/// Pid → share; the charge path reaches it through the task, never this lock.
static SHARES: Lock<Option<HashMap<Pid, Arc<KShare>>>> = Lock::new(None);

pub fn init() {
    *SHARES.lock() = Some(HashMap::new());
    driver::init();
}

/// The share a new task of `pid` joins, as `NonRunnable { lag: 0 }` so the
/// adopting CPU's `enter_runnable` reproduces `new_runnable(frontier)`'s state.
fn share_for(pid: Pid) -> Arc<KShare> {
    let mut guard = SHARES.lock();
    let map = guard.as_mut().expect("scheduler not initialized");
    map.entry(pid)
        .or_insert_with(|| {
            Arc::new(KShare::new(KernelLock::new(ShareState::NonRunnable {
                lag: 0,
            })))
        })
        .clone()
}

fn share_of(pid: Pid) -> Option<Arc<KShare>> {
    SHARES.lock().as_ref()?.get(&pid).cloned()
}

/// The process is gone from the table; live tasks keep their `Arc` alive.
pub fn remove_vruntime(pid: Pid) {
    if let Some(map) = SHARES.lock().as_mut() {
        map.remove(&pid);
    }
}

pub fn process_vruntime(pid: Pid) -> u64 {
    share_of(pid).map_or(0, |s| s.vruntime(driver::frontier()))
}

pub fn process_lag(pid: Pid) -> i64 {
    share_of(pid).map_or(0, |s| s.lag())
}

pub fn global_min_vruntime() -> u64 {
    driver::frontier().get()
}

/// Build and place a new task; returns the CPU it was placed on.
pub fn enqueue_new(
    id: TaskId,
    kernel_stack: crate::process::OwnedAlloc,
    entry_rsp: u64,
    address_space: crate::process::PageTables,
    fs_base: u64,
    symbols: alloc::sync::Arc<crate::symbols::SymbolTable>,
) -> (ThreadSched, CpuId) {
    driver::spawn(NewTask {
        id,
        kernel_stack,
        entry_rsp,
        address_space,
        fs_base,
        share: share_for(id.0),
        symbols,
    })
}

/// Phase 1 of the wait handshake: register the running thread on `queue`.
/// Registering before the caller re-checks its condition is what closes the
/// check-then-block window. Mints via [`Parkable::mint`], not
/// [`Parkable::at_entry`]: a context already inside an [`Operation`] must
/// receive the token here too, which `at_entry` would refuse.
#[must_use = "a wait ticket must be blocked on or cancelled"]
#[track_caller]
pub fn prepare_wait(queue: &KWaitQueue, cancel: Cancel, class: WaitClass) -> Ticket<'_> {
    let _parkable = Parkable::mint();
    Ticket::register(queue, cancel, class)
}

/// Phase 2: park the running thread on the queue it registered with. Takes
/// the ticket by value: a park that reaches the machine without a
/// registration behind it is the lost-wake window.
#[track_caller]
pub fn block_on(ticket: Ticket<'_>, deadline: Deadline) {
    // One level above the calling context's baseline: the ticket has held the
    // registration window's own level since `prepare_wait`.
    assert_baseline(blocking_baseline() + 1);
    driver::pass_block(ticket, (!deadline.is_never()).then(|| Nanos(deadline.nanos())));
}

/// Give the CPU up voluntarily, keeping the claim on it: the pass decides
/// whether anything else deserves the quantum. Asserts the calling context's
/// own baseline, not a flat trap level, since a kernel thread (`iod`'s
/// write-back retry) yields at zero and a flat assert would panic it.
#[track_caller]
pub fn yield_now() {
    assert_baseline(blocking_baseline());
    driver::pass(Dispose::Yield);
}

/// Unified preempt entry: the Ring 3 timer path, `kernel_exit_to_user_check`
/// and the `preempt::enable` slow path all funnel through here.
#[track_caller]
pub fn do_preempt() {
    if in_schedule_self() {
        return;
    }
    assert_baseline(BASELINE_IRQ_EXIT);
    crate::preempt::clear_need_resched();
    if percpu::current_tid().is_none() {
        // No thread on this CPU: the idle loop passes every iteration anyway,
        // and boot has no `CpuSched` yet — moot, not deferred, for an ISR that
        // raised this before either exists.
        return;
    }
    crate::trace::trace(crate::trace::Kind::Preempt, 0);
    driver::pass(Dispose::None);
}

/// A killed thread's last safe point: the return to Ring 3. Called from every
/// Ring 3 exit boundary, since a killed task is dispatched rather than reaped
/// and would otherwise run in userland unbounded.
#[track_caller]
pub fn exit_if_killed() {
    if !driver::current_kill_pending() {
        return;
    }
    assert_baseline(BASELINE_IRQ_EXIT);
    // The retirer owns teardown; a mark_thread_zombie here would race it.
    driver::pass(Dispose::Exit);
    unreachable!("exit_if_killed: returned from the exit pass");
}

#[track_caller]
pub fn exit_current(code: i32) -> ! {
    assert_baseline(BASELINE_TRAP);
    {
        let mut guard = process::PROCESS_TABLE.lock();
        let table = guard.as_mut().unwrap();
        let tid = percpu::current_tid().unwrap();
        let pid = percpu::current_pid().unwrap();
        process::mark_thread_zombie(table, pid, tid, code);
    }
    driver::pass(Dispose::Exit);
    unreachable!("exit_current: returned from the exit pass");
}

/// Claim one specific thread's rendezvous word and post its wake. Returns
/// `true` only if this call won the claim. No baseline assert: unlike every
/// parking entry above, a wake never switches, so posting from inside a lock
/// is the protocol here.
pub fn wake_sched(shared: &Arc<KShared>, boost: Option<Nanos>) -> bool {
    let cause = match boost {
        Some(until) => WakeCause::boosted(WakeReason::Woken, until),
        None => WakeCause::new(WakeReason::Woken),
    };
    preempt_off(|p| toyos_sched::waitq::wake_direct(shared, cause, cpus(), &HW, p))
}

/// Wake pipe readers, lending each an RT window if the writer holds one; the
/// pipe is also marked, so a runnable reader takes the window too.
pub fn wake_pipe_readers(pipe_id: PipeId) {
    let Some(end) = crate::pipe::readers_queue(pipe_id) else {
        return;
    };
    if driver::current_is_rt() {
        crate::pipe::set_rt_boost_pending(pipe_id);
        completion::post_boosted(Subject::of(&end.watch), Outcome::Ready, boost_window());
    } else {
        completion::post(Subject::of(&end.watch), Outcome::Ready);
    }
}

pub fn wake_pipe_writers(pipe_id: PipeId) {
    if let Some(end) = crate::pipe::writers_queue(pipe_id) {
        completion::post(Subject::of(&end.watch), Outcome::Ready);
    }
}

/// How long a lent RT priority lasts: one quantum, a wall-clock bound on time held.
pub fn boost_window() -> Nanos {
    HW.now().after(QUANTUM_NS)
}

/// Grant the running thread the window its producer left on a pipe.
pub fn boost_current_rt_inherited() {
    driver::boost_current(boost_window());
}

/// `SYS_RT_ENTER`. Gated at the dispatch site on `Rights::RT`, not here — this
/// must stay callable from kernel init.
pub fn set_current_rt(enable: bool) {
    driver::set_current_rt(enable);
}

/// Block on a futex word unless it already changed, and answer which of the
/// two things the ABI names ended the wait.
#[track_caller]
pub fn futex_wait(
    addr: crate::UserAddr,
    phys_addr: DirectMap,
    expected: u32,
    deadline: Deadline,
) -> FutexEnd {
    let parkable = Parkable::at_entry();
    // Re-translated rather than trusted: `munmap` clears the entry before
    // walking futex buckets, so a changed translation means this arm is stale.
    let read = || {
        let Some(pt) = current_address_space() else {
            return true;
        };
        let same_frame =
            pt.lock().translate(addr).is_some_and(|now| now.phys() == phys_addr.phys());
        if !same_frame {
            return true;
        }
        // SAFETY: `same_frame` re-translated `addr` and found it still names
        // `phys_addr`'s frame, so this is a live, mapped, syscall-checked
        // 4-byte-aligned word; volatile because this predicate may run more
        // than once.
        let word = unsafe { phys_addr.as_ptr::<u32>().read_volatile() };
        word != expected
    };
    let _ = completion::wait_until(
        &parkable,
        completion::Subject::of(waitqs::futex_watch(phys_addr)),
        completion::Token::new(phys_addr.phys()),
        WaitClass::Futex,
        deadline,
        read,
    );
    // `wait_until` answers `Ok(())` for a satisfied predicate and an expired
    // deadline alike, so the word itself is what tells them apart.
    if read() {
        FutexEnd::Changed
    } else {
        FutexEnd::Timeout
    }
}

/// Which of the two things `SYS_FUTEX_WAIT`'s ABI names ended a wait.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FutexEnd {
    /// The word no longer holds `expected`.
    Changed,
    /// The word still holds `expected`; the caller's own deadline ended it.
    Timeout,
}

/// Wake up to `count` waiters on this futex word, and answer how many.
pub fn futex_wake(phys_addr: DirectMap, count: usize) -> u64 {
    completion::post_n(
        completion::Subject::of(waitqs::futex_watch(phys_addr)),
        completion::Outcome::Ready,
        completion::Token::new(phys_addr.phys()),
        count,
    ) as u64
}

/// Retire a thread and wait until its record — kernel stack and
/// address-space reference — is released. The state word reading `Dead` is
/// not enough: that payload is freed by the pass after the one that publishes it.
#[track_caller]
pub fn retire_task(sched: &ThreadSched) {
    // Also on the early-return path below, where no park happens and the two
    // asserts inside the wait would never run.
    assert_baseline(BASELINE_TRAP);
    if let (Some(pid), Some(tid)) = (percpu::current_pid(), percpu::current_tid()) {
        if let Some(handle) = driver::current_shared() {
            assert!(
                !Arc::ptr_eq(&handle, &sched.shared),
                "retire_task: cannot retire self ({})",
                TaskId(pid, tid),
            );
        }
    }
    if sched.handle.released() {
        return;
    }
    preempt_off(|p| {
        toyos_sched::retire::begin(&sched.shared).post(cpus(), &HW, p);
    });
    /// Re-poll rate for the liveness backstop; the release wake is what
    /// actually ends the wait.
    const RECHECK: Cadence = Cadence::every(
        Duration::from_millis(50),
        "two hundred re-polls inside the tripwire, on a thread that is otherwise parked",
    );
    /// Superseded by the scheduling-reservations design; kept because a
    /// known-wrong constant is still what this kernel runs
    /// (`issues/kernel/scheduler-pass-blocks-in-xhci.md`).
    const GIVE_UP: Tripwire = Tripwire::absurd(
        Duration::from_secs(10),
        "four pass prologues on xHCI's own 2 s deadline, two quanta, and an unwind \
         the real-time band may stretch elevenfold; past this the wake was lost",
    );
    let give_up = Deadline::at(crate::clock::now() + GIVE_UP.duration());
    let parkable = Parkable::at_entry();
    // Uncancellable: a killed retirer cannot propagate a cancel with the
    // retire half done; the tripwire above bounds it instead.
    let Some(armed) = completion::arm(
        completion::Subject::of(sched.handle.watch()),
        completion::Token::new(sched.shared.key().0),
        WaitClass::Other,
    ) else {
        panic!("retire_task: no current task to park");
    };
    while !sched.handle.released() {
        if give_up.reached(crate::clock::now()) {
            panic!(
                "retire_task: task not released after {}: {:?}",
                GIVE_UP.duration(),
                sched.shared.state()
            );
        }
        let _record = completion::wait_uncancellable(
            &parkable,
            &armed,
            Deadline::at(crate::clock::now() + RECHECK.duration()),
        );
    }
}

/// Per-CPU hand-off slot for a thread that died in panic recovery: the panic
/// path may hold any lock, so it can only store here.
static POISONED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];

/// Whether [`reap_poisoned`] has anything to do; claimed by whichever idle
/// trip takes the work.
static REAP_GATE: ReapGate = ReapGate::new();

/// Tell the idle loop there is a table entry to collect. Call after the
/// object's `finished` flag is stored, so the gate's release publishes it.
pub fn note_reapable() {
    REAP_GATE.raise();
}

pub fn poison_tid(id: TaskId) {
    let cpu = percpu::cpu_id() as usize;
    let Some(slot) = POISONED.get(cpu) else {
        crate::log!("poison_tid: cpu {cpu} >= MAX_CPUS — {id} will never be reaped");
        return;
    };
    let prev = slot.swap(id.pack(), Ordering::Release);
    // After the slot is written, never before: the gate's release is what
    // carries it to the CPU that claims the work.
    REAP_GATE.raise();
    if prev != u64::MAX {
        crate::log!(
            "poison_tid: cpu {cpu} slot still held {} — its waiter is stranded",
            TaskId::unpack(prev)
        );
    }
}

/// Zombify threads that died in panic recovery, collect finished processes'
/// entries, and wake whoever was joining them. Called from the idle loop,
/// which holds none of the panicking thread's locks. Checked before locking
/// `PROCESS_TABLE` unconditionally: holding it on every idle trip would
/// starve a crash report's `try_lock` of that table.
pub(crate) fn reap_poisoned() {
    if !REAP_GATE.take() {
        return;
    }
    let mut wakes: [Option<process::PoisonWake>; MAX_CPUS] = [const { None }; MAX_CPUS];
    // Dropped after the guard: an entry's drop reaches `remove_vruntime`.
    let reaped;
    {
        let mut guard = process::PROCESS_TABLE.lock();
        let table = guard.as_mut().unwrap();
        // SAFETY: `reap_poisoned`'s one caller, `sched::driver::idle_loop`,
        // runs on the per-CPU idle stack, which is what `IdleProof` requires.
        reaped = process::reap_finished(table, unsafe { process::IdleProof::new_unchecked() });
        for (slot, wake) in POISONED.iter().zip(wakes.iter_mut()) {
            let raw = slot.load(Ordering::Relaxed);
            if raw == u64::MAX {
                continue;
            }
            let id = TaskId::unpack(raw);
            *wake = process::zombify_poisoned(table, id.0, id.1);
            slot.store(u64::MAX, Ordering::Relaxed);
        }
    }
    drop(reaped);
    for wake in wakes.into_iter().flatten() {
        match wake {
            process::PoisonWake::Joiner(pid, tid) => {
                if let Some(sched) = process::thread_sched(pid, tid) {
                    completion::post(
                        completion::Subject::of(sched.handle.watch()),
                        completion::Outcome::Gone(completion::Reason::Closed),
                    );
                }
            }
            // -1: nobody asked for this exit, and teardown never ran to account it.
            process::PoisonWake::Process(object) => {
                let stats = toyos_abi::syscall::ProcessStats {
                    pid: object.pid().raw(),
                    ..Default::default()
                };
                object.publish_exit(crate::object::process::Exit { code: -1, stats })
            }
        }
    }
}

/// The panic path's exit: the faulted thread's context is unusable, so it
/// dies where it stands. No baseline assert: a panicking thread may hold
/// any lock, and asserting would double-panic and lose the report.
pub fn schedule_no_return() -> ! {
    if in_schedule_self() {
        crate::log!("schedule_no_return: panicked inside a pass, cannot rejoin");
        crate::arch::apic::halt_all_cpus();
    }
    if percpu::current_tid().is_none() {
        enter_idle_loop();
    }
    driver::pass(Dispose::Exit);
    unreachable!("schedule_no_return: returned from the exit pass");
}

/// Cumulative CPU time; a running thread's live slice is added by the reader.
pub fn task_cpu_ns(sched: &ThreadSched) -> u64 {
    sched.handle.cpu_ns()
}

pub fn task_sched_state(sched: &ThreadSched) -> u8 {
    sched.sched_state()
}

/// Flush the running thread's blocked/runqueue counters into process accounting.
pub fn flush_current_stats(acct: &mut process::ProcessAccounting) {
    driver::with_current_acct(|a| crate::sched::payload::merge_accounting(a, acct));
}

/// How often an idle CPU may report occupancy: not a deadline, so it never
/// wakes a CPU with nothing to run — turning it into one would be an audio
/// change.
const SNAPSHOT_INTERVAL: Cadence = Cadence::every(
    Duration::from_secs(10),
    "one clock read and one relaxed compare per idle trip, on a CPU already awake",
);

/// `sched-fast-health`'s cadence: no guest test program this suite runs lives
/// past [`SNAPSHOT_INTERVAL`] once, let alone the two prints a comparison needs.
const FAST_SNAPSHOT_INTERVAL: Cadence = Cadence::every(
    Duration::from_millis(200),
    "an actuator no boot arms; a test that needs two prints buys them for one boot",
);

/// Which of the two cadences this boot took, read once per idle trip.
fn snapshot_interval_ns() -> u64 {
    if crate::actuator::sched_fast_health() {
        FAST_SNAPSHOT_INTERVAL.nanos()
    } else {
        SNAPSHOT_INTERVAL.nanos()
    }
}

/// When each CPU may next print its own line: per CPU, not global, so no
/// single CPU speaks for all of them.
static NEXT_HEALTH: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// How many times each CPU has passed through idle since boot, counted on
/// every trip rather than only the ones that print: `i8042_quarantine` needs
/// the raw rate to tell a halting CPU from a spinning one.
static IDLE_TRIPS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// A snapshot of this CPU's run queues, at most once per
/// [`snapshot_interval_ns`], plus the machine's page pools on the same
/// cadence. Called from the idle loop on every trip; the cadence is wall
/// clock rather than per-trip because a CPU that declines to sleep loops at
/// memory speed. Not a heartbeat: a busy CPU prints nothing, so a gap here
/// is not evidence of a hang.
pub fn log_health() {
    let now = crate::hw::now_ns();
    let cpu = percpu::cpu_id();
    let Some(next_health) = NEXT_HEALTH.get(cpu as usize) else { return };
    // Unconditional and every trip, unlike the print below.
    let trips = IDLE_TRIPS
        .get(cpu as usize)
        .map_or(0, |t| t.fetch_add(1, Ordering::Relaxed) + 1);
    if now >= next_health.load(Ordering::Relaxed) {
        next_health.store(now + snapshot_interval_ns(), Ordering::Relaxed);
        let ready = driver::ready_len() + usize::from(percpu::current_tid().is_some());
        let parked = driver::parked_len();
        let dying = driver::dying_len();
        crate::log!(
            "sched: cpu={} ready={} dying={} parked={} current={:?} trips={}",
            cpu,
            ready,
            dying,
            parked,
            percpu::current_tid(),
            trips,
        );
    }

    static NEXT_PMM_DUMP: AtomicU64 = AtomicU64::new(0);
    let next = NEXT_PMM_DUMP.load(Ordering::Relaxed);
    if next == 0 {
        NEXT_PMM_DUMP.store(now + snapshot_interval_ns(), Ordering::Relaxed);
    } else if now >= next
        && NEXT_PMM_DUMP
            .compare_exchange(
                next,
                now + snapshot_interval_ns(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
    {
        crate::mm::pmm::dump_stats();
    }
}

