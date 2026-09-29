//! The kernel-facing scheduler API surface: no decision, state transition or
//! ordering-sensitive step happens here.
//!
//! Exception: [`Parkable`] and [`Operation`] live here because the park token
//! has no public constructor outside the two doors this module defines.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::hasher::HashMap;
use toyos_sched::fair::{ShareState, QUANTUM_NS};
use toyos_sched::hw::{CpuId, Machine, Nanos};
use toyos_sched::task::{Refused, SafePoint, WaitClass};

use crate::arch::percpu;
use crate::watch::{self, Cancel};
use crate::hw::HW;
use crate::pipe::PipeId;
use crate::process::{self, Pid, Tid};
use crate::sched::driver::{self, cpus, preempt_off, Dispose, NewTask};
use crate::sched::payload::{KShare, KernelLock, TaskHandle, ThreadSched};
use crate::sched::reap_gate::ReapGate;
use crate::sched::futex;
use crate::sync::Lock;
use crate::time::{Cadence, Deadline, Duration};
use crate::DirectMap;

pub use crate::sched::driver::{
    current_address_space, enter_idle_loop, in_pass as in_schedule_self, started, total_cpu_ns,
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
    *SHARES.lock() = Some(HashMap::default());
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
    entry_sp: u64,
    address_space: crate::process::PageTables,
    thread_pointer: u64,
    symbols: alloc::sync::Arc<crate::symbols::SymbolTable>,
) -> (ThreadSched, CpuId) {
    driver::spawn(NewTask {
        id,
        kernel_stack,
        entry_sp,
        address_space,
        thread_pointer,
        share: share_for(id.0),
        symbols,
    })
}

/// Phase 1 of the wait handshake on the running thread's own word, or
/// [`Refused`]: a post reached it since it registered on a watch, and the
/// caller re-reads its condition, or a revoke ended the registration, and the
/// wait is over. Mints via [`Parkable::mint`], not [`Parkable::at_entry`]: a
/// context already inside an [`Operation`] must receive the token here too,
/// which `at_entry` would refuse.
#[must_use = "a wait ticket must be blocked on or cancelled"]
#[track_caller]
pub fn prepare_wait(cancel: Cancel, class: WaitClass) -> Result<Ticket, Refused> {
    let _parkable = Parkable::mint();
    Ticket::register(cancel, class)
}

/// Phase 2: park the running thread. Takes the ticket by value: a park that
/// reaches the machine without phase 1 behind it is the lost-wake window.
#[track_caller]
pub fn block_on(ticket: Ticket, deadline: Deadline) {
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

/// Whether [`yield_now`] may be called where the running context stands.
#[cfg(feature = "boot-actuators")]
pub fn may_yield() -> bool {
    crate::preempt::count() == blocking_baseline()
}

/// Unified preempt entry: the user-mode timer path, [`exit_to_user`]
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

/// The last thing a thread does before returning to user mode, if either mark it
/// can carry says it never does. [`exit_to_user`] is the one caller;
/// `kernel/src/quiesce.rs`'s header says why that boundary is the safe point.
///
/// **One call and one match, so the two marks have no order to disagree
/// about**: `toyos_sched::task::SafePoint` ranks them, here and in
/// `CpuSched::place` alike.
#[track_caller]
pub fn leave_user_if_due() {
    let Some(due) = driver::current_safe_point(crate::quiesce::stops_this_thread()) else {
        return;
    };
    assert_baseline(BASELINE_IRQ_EXIT);
    match due {
        SafePoint::Stop => {
            driver::pass(Dispose::Stop);
            unreachable!("leave_user_if_due: a stopped task was dispatched again");
        }
        SafePoint::Exit => {
            // Interrupts open across the teardown, as a syscall's exit runs it: its
            // closes and address-space drop are no interrupt latency. The
            // depth stays this boundary's, which is `do_preempt`'s own.
            crate::arch::cpu::enable_interrupts();
            process::leave(None);
            crate::arch::cpu::disable_interrupts();
            driver::pass(Dispose::Exit);
            unreachable!("leave_user_if_due: returned from the exit pass");
        }
    }
}


/// The deferred-preempt epilogue every return to user mode runs last, with
/// interrupts masked on entry and on return: a killed or stopped thread leaves
/// here, and a reschedule owed since the entry is served before the thread
/// sees user mode again.
pub fn exit_to_user() {
    flush_kernel_timer_fires_to_trace();
    loop {
        // A killed or stopped thread returns to user mode exactly once more: never.
        leave_user_if_due();
        // `do_preempt` owns clearing `need_resched`; this function never clears it itself.
        if !crate::preempt::need_resched() {
            #[cfg(feature = "boot-actuators")]
            if crate::actuator::dump_in_blocking_pass() {
                crate::sched::dump::staged::note_return_to_user();
            }
            return;
        }
        assert!(!in_schedule_self(), "exit-to-user inside a scheduler pass");
        // Not an IrqGuard: both loop exits must open interrupts, not restore a saved value.
        crate::arch::cpu::enable_interrupts();
        do_preempt();
        crate::arch::cpu::disable_interrupts();
        flush_kernel_timer_fires_to_trace();
    }
}

fn flush_kernel_timer_fires_to_trace() {
    let cur = percpu::kernel_timer_fires();
    let missed = cur.wrapping_sub(percpu::last_seen_kernel_timer_fires());
    if missed > 0 {
        crate::trace::trace(crate::trace::Kind::TimerFireBurst, missed);
        percpu::set_last_seen_kernel_timer_fires(cur);
    }
}
/// The exit pass of a thread that has left its process (`process::leave`).
#[track_caller]
pub fn exit_current() -> ! {
    assert_baseline(BASELINE_TRAP);
    driver::pass(Dispose::Exit);
    unreachable!("exit_current: returned from the exit pass");
}

/// Wake pipe readers, lending each an RT window if the writer holds one; the
/// pipe is also marked, so a runnable reader takes the window too.
pub fn wake_pipe_readers(pipe_id: PipeId) {
    let Some(watch) = crate::pipe::read_watch(pipe_id) else {
        return;
    };
    if driver::current_is_rt() {
        crate::pipe::set_rt_boost_pending(pipe_id);
        watch.post_boosted(boost_window());
    } else {
        watch.post();
    }
}

/// Wake pipe writers, and complete every poll on the write end.
pub fn wake_pipe_writers(pipe_id: PipeId) {
    if let Some(watch) = crate::pipe::write_watch(pipe_id) {
        watch.post();
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
    let _ = watch::wait_until(
        &parkable,
        futex::watch_of(phys_addr),
        phys_addr.phys(),
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
    futex::watch_of(phys_addr).post_n(phys_addr.phys(), count) as u64
}

/// Set a thread's kill bit and ask its CPU for a safe point; returns at once.
/// The thread leaves at that safe point: `process::leave`.
pub fn post_retire(sched: &ThreadSched) {
    preempt_off(|p| {
        toyos_sched::retire::begin(&sched.shared).post(cpus(), &HW, p);
    });
}

/// Whether [`reap_finished`] has anything to do; claimed by whichever idle
/// trip takes the work.
static REAP_GATE: ReapGate = ReapGate::new();

/// Tell the idle loop there is a table entry to collect. Call after the
/// object's `finished` flag is stored, so the gate's release publishes it.
pub fn note_reapable() {
    REAP_GATE.raise();
}

/// Collect finished processes' entries. Called from the idle loop, and
/// checked before locking `PROCESS_TABLE` unconditionally: holding it on every
/// idle trip would starve a crash report's `try_lock` of that table.
pub(crate) fn reap_finished() {
    if !REAP_GATE.take() {
        return;
    }
    // Dropped after the guard: an entry's drop reaches `remove_vruntime`.
    let reaped = {
        let mut guard = process::PROCESS_TABLE.lock();
        let table = guard.as_mut().unwrap();
        // SAFETY: `reap_finished`'s one caller, `sched::driver::idle_loop`,
        // runs on the per-CPU idle stack, which is what `IdleProof` requires.
        process::reap_finished(table, unsafe { process::IdleProof::new_unchecked() })
    };
    drop(reaped);
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

/// When each CPU may next print its own line: per CPU, not global, so no
/// single CPU speaks for all of them.
static NEXT_HEALTH: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// How many times each CPU has passed through idle since boot.
static IDLE_TRIPS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// A snapshot of this CPU's run queues, at most once per
/// [`SNAPSHOT_INTERVAL`], plus the machine's page pools on the same
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
        next_health.store(now + SNAPSHOT_INTERVAL.nanos(), Ordering::Relaxed);
        let ready = driver::ready_len() + usize::from(percpu::current_tid().is_some());
        let parked = driver::parked_len();
        let dying = driver::dying_len();
        let stopped = driver::stopped_len();
        crate::log!(
            "sched: cpu={} ready={} dying={} stopped={} parked={} current={:?} trips={}",
            cpu,
            ready,
            dying,
            stopped,
            parked,
            percpu::current_tid(),
            trips,
        );
    }

    static NEXT_PMM_DUMP: AtomicU64 = AtomicU64::new(0);
    let next = NEXT_PMM_DUMP.load(Ordering::Relaxed);
    if next == 0 {
        NEXT_PMM_DUMP.store(now + SNAPSHOT_INTERVAL.nanos(), Ordering::Relaxed);
    } else if now >= next
        && NEXT_PMM_DUMP
            .compare_exchange(
                next,
                now + SNAPSHOT_INTERVAL.nanos(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .is_ok()
    {
        crate::mm::pmm::dump_stats();
    }
}

