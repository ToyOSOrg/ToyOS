//! The kernel-facing scheduler API surface: no decision, state transition or
//! ordering-sensitive step happens here.
//!
//! Exception: [`Parkable`] lives here because the park token has no public
//! constructor outside the door this module defines.

use kernel::sched::hw::{CpuId, Machine, Nanos};
use kernel::sched::task::{Refused, SafePoint, WaitClass, QUANTUM_NS};

use crate::arch::percpu;
use crate::watch::{self, Cancel, Watch};
use crate::hw::HW;
use crate::pipe::PipeRef;
use crate::process::{self, Pid, Tid};
use crate::sched::driver::{self, cpus, preempt_off, Dispose};
use crate::sched::payload::ThreadSched;
use crate::sched::reap_gate::ReapGate;
use crate::sched::futex;
use crate::time::Deadline;
use crate::DirectMap;

pub use crate::sched::driver::{
    current_address_space, enter_idle_loop, in_pass as in_schedule_self, started, total_cpu_ns,
    write_stack_canary, NewTask, Ticket,
};

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
/// and lowered on the way out (x86_64's `syscall_entry` and `common_entry`,
/// AArch64's `trap::syscall` and `trap::user_abort`). A user copy runs at it
/// and no deeper (`user_ptr`).
pub(crate) const BASELINE_TRAP: u32 = 1;

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
/// than stored, with no public constructor beside [`Parkable::at_entry`].
pub struct Parkable(());

impl Parkable {
    /// Asserts this context is a trap entry or a kernel thread's body, and
    /// nothing below one, then mints the proof.
    #[track_caller]
    pub fn at_entry() -> Parkable {
        Parkable::mint()
    }

    #[track_caller]
    fn mint() -> Parkable {
        assert_baseline(blocking_baseline());
        Parkable(())
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

pub fn init() {
    driver::init();
}

/// Build and place a new task; returns the CPU it was placed on.
pub fn enqueue_new(new: NewTask) -> (ThreadSched, CpuId) {
    driver::spawn(new)
}

/// Phase 1 of the wait handshake on the running thread's own word, or
/// [`Refused`]: a post reached it since it registered on a watch, and the
/// caller re-reads its condition, or a revoke ended the registration, and the
/// wait is over.
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
/// own baseline, not a flat trap level, since a kernel thread yields at zero
/// and a flat assert would panic it.
#[track_caller]
pub fn yield_now() {
    assert_baseline(blocking_baseline());
    driver::pass(Dispose::Yield);
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
/// about**: `kernel::sched::task::SafePoint` ranks them, here and in
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
            // Every caller's next unmask is the return to user mode, or a
            // kernel thread's first.
            #[cfg(feature = "mask-windows")]
            crate::windows::irqs_unmasking();
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
    // Masked into the pass, as `leave_user_if_due`'s exit masks into it.
    crate::arch::cpu::disable_interrupts();
    driver::pass(Dispose::Exit);
    unreachable!("exit_current: returned from the exit pass");
}

/// Wake pipe readers, lending each an RT window if the writer holds one.
pub fn wake_pipe_readers(pipe: &PipeRef) {
    post_lending(pipe.read_watch(), || pipe.set_rt_boost_pending());
}

/// Post `watch`, lending whoever it wakes one [`boost_window`] if the poster
/// is real-time; `mark` records the loan for a consumer already runnable,
/// which takes it with [`boost_current_rt_inherited`].
pub fn post_lending(watch: &Watch, mark: impl FnOnce()) {
    if driver::current_is_rt() {
        mark();
        watch.post_boosted(boost_window());
    } else {
        watch.post();
    }
}

/// Wake pipe writers, and complete every poll on the write end.
pub fn wake_pipe_writers(pipe: &PipeRef) {
    pipe.write_watch().post();
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
        let same_frame = pt.lock().get(addr.raw()).is_some_and(|(now, _)| now == phys_addr.phys());
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
    // deadline alike, so the word itself is what tells them apart. With no
    // deadline the word or its frame changed, or a kill ended it, whose answer
    // never reaches Ring 3.
    if deadline.is_never() || read() {
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
        kernel::sched::retire::begin(&sched.shared).post(cpus(), &HW, p);
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
    // Dropped after the guard: an entry's drop releases what it held.
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
