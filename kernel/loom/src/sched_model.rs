//! Scaffolding shared by the loom models: the message type, the modelled
//! preempt count, the cell lock, the kick recorder, and what a model that runs
//! whole `CpuSched`s is built against.

#![deny(unsafe_code)]

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Mutex, MutexGuard};

use crate::sched::affinity::CpuMask;
use crate::sched::cpu::{CpuHandles, RunToken};
use crate::sched::hw::{CpuId, Hw, Kicker, Machine, Nanos, TraceEvent};
use crate::sched::job::{Job, JobKey};
use crate::sched::mailbox::{PreemptGuard, SchedMsg, Urgency};
use crate::sched::sync::Arc;
use crate::sched::task::{
    RtState, SchedPayload, TaskAccounting, TaskBuilder, TaskKey, TaskShared, WakeCause, WakeReason,
};
use crate::sched::sync::CellLock;
use crate::sched::watch::Waiters;

pub const CPU0: CpuId = CpuId(0);
pub const CPU1: CpuId = CpuId(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Msg {
    Wake(TaskKey, WakeReason),
    Retire(TaskKey),
    /// Stands in for the ownership-carrying `Adopt` traffic;
    /// the mailbox models only need distinguishable payloads.
    Probe(u32),
}

impl SchedMsg for Msg {
    type Parked = ();
    fn wake(shared: Arc<TaskShared<Self>>, cause: WakeCause) -> Self {
        Msg::Wake(shared.key(), cause.reason)
    }

    fn retire(shared: Arc<TaskShared<Self>>) -> Self {
        Msg::Retire(shared.key())
    }
}

/// The modelled preempt count of one CPU.
///
/// A preempt-disabled region and that CPU's IRQ-exit scheduler pass are
/// mutually exclusive — which is precisely what the kernel's preempt count
/// buys: an IRQ may interrupt the region, but its exit path refuses to run a
/// pass while the count is nonzero, so the interrupted context resumes and
/// finishes what it started.
///
/// With `--features no-preempt-guard` the guard is **modelled away**:
/// [`PreemptModel::disable`] returns a guard that excludes nothing, so the
/// pass may run inside a half-finished push. That is the forbidden
/// interleaving — a push torn by a pass on its own CPU — and
/// `tests/loom_mailbox.rs` asserts it is detected.
pub struct PreemptModel {
    section: Mutex<()>,
}

impl PreemptModel {
    pub fn new() -> Self {
        Self {
            section: Mutex::new(()),
        }
    }

    pub fn disable(&self) -> ThreadGuard<'_> {
        if cfg!(feature = "no-preempt-guard") {
            ThreadGuard(None)
        } else {
            ThreadGuard(Some(self.section.lock().unwrap()))
        }
    }

    /// IRQ exit: may a scheduler pass run here? `None` means preemption is
    /// disabled in the interrupted context.
    pub fn enter_pass(&self) -> Option<PassGuard<'_>> {
        self.section.try_lock().ok().map(PassGuard)
    }
}

impl Default for PreemptModel {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-context guard: what every producer must hold to push (N3).
pub struct ThreadGuard<'a>(#[allow(dead_code)] Option<MutexGuard<'a, ()>>);

// SAFETY: the guard owns the modelled CPU's preempt-disabled section, so the
// pushing context cannot be descheduled while it lives — except under
// `--features no-preempt-guard`, where the model deliberately lies in order
// to prove that the lie is caught.
#[allow(unsafe_code)]
unsafe impl PreemptGuard for ThreadGuard<'_> {}

/// The scheduler pass's exclusion against preempt-disabled sections. A pass
/// runs with preemption disabled itself, so it is also what the
/// consumer's stub re-push pushes under.
pub struct PassGuard<'a>(#[allow(dead_code)] MutexGuard<'a, ()>);

// SAFETY: a pass owns the CPU's preempt-disabled section for its duration.
#[allow(unsafe_code)]
unsafe impl PreemptGuard for PassGuard<'_> {}

/// IRQ context: not preemptible by construction, so it needs no exclusion.
pub struct IrqGuard;

// SAFETY: an interrupt handler cannot be preempted.
#[allow(unsafe_code)]
unsafe impl PreemptGuard for IrqGuard {}

/// A remote CPU's thread context: it has its own preempt count, which this
/// CPU's model does not track.
pub struct RemoteGuard;

// SAFETY: models a preempt-disabled region on another CPU; the exclusion that
// matters here is the local one.
#[allow(unsafe_code)]
unsafe impl PreemptGuard for RemoteGuard {}

/// `CellLock` over loom's mutex, so the watch models exercise the real
/// critical sections.
pub struct LoomLock<T>(Mutex<T>);

impl<T> LoomLock<T> {
    pub fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }
}

impl<T: Send> CellLock<T> for LoomLock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }
}

pub fn watch_list<M: SchedMsg, R>() -> LoomLock<Waiters<M, R>>
where
    Waiters<M, R>: Send,
{
    LoomLock::new(Waiters::new())
}

/// Counts targeted IPIs. In these models an IPI's only observable effect is
/// "the target cannot stay halted", which is what the sleep model asserts.
#[derive(Default)]
pub struct Kicks {
    count: AtomicUsize,
}

impl Kicks {
    pub fn new() -> Self {
        Self {
            count: AtomicUsize::new(0),
        }
    }

    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}

impl Kicker for Kicks {
    fn trace(&self, _ev: crate::sched::hw::TraceEvent) {}
    fn now(&self) -> crate::sched::hw::Nanos {
        crate::sched::hw::Nanos::ZERO
    }
    fn kick(&self, _target: CpuId) {
        self.count.fetch_add(1, Ordering::AcqRel);
    }

    fn runs_on(&self, _cpu: CpuId) -> bool {
        false
    }
}

/// Loom explores every interleaving of the modelled threads with at most
/// three preemptions, which keeps the three-thread models in the seconds
/// range; it is stated explicitly rather than left to loom's default.
pub fn model(f: impl Fn() + Sync + Send + 'static) {
    model_bounded(3, f);
}

/// [`model`] with at most `preemptions`, for a model of more threads.
pub fn model_bounded(preemptions: usize, f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(preemptions);
    builder.check(f);
}

/// The smallest payload a `CpuSched` can be built over: no address space,
/// and a context that is only its own saved-ness.
pub struct Payload;

impl SchedPayload for Payload {
    type Ctx = ModelCtx;

    fn name(&self) -> u64 {
        0
    }
}

/// The full message set, as opposed to the reduced [`Msg`] the primitive
/// models use: a pass consumes `Adopt` and `Retire` too.
pub type TaskMsg = crate::sched::msg::Msg<Payload>;

/// A context as the models see it: whether it is saved, in a cell loom checks
/// at every access, so that a resume no save happens before is a causality
/// violation and not a lucky read.
pub struct ModelCtx(loom::cell::UnsafeCell<Frame>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Frame {
    /// A new task's initial frame.
    Fresh,
    /// A CPU stands on it.
    Live,
    Saved,
}

impl ModelCtx {
    /// A new task's.
    pub fn fresh() -> Self {
        Self(loom::cell::UnsafeCell::new(Frame::Fresh))
    }

    /// A CPU's idle context, which the CPU starts on.
    pub fn live() -> Self {
        Self(loom::cell::UnsafeCell::new(Frame::Live))
    }
}

/// An `Hw` that records nothing, and whose switch saves and resumes
/// [`ModelCtx`]s as the kernel's does: the outgoing context saved, then its
/// task's on-CPU word cleared with release, then the incoming one resumed. A
/// model that drops a pass's action switches nothing.
pub struct Silent;

impl Kicker for Silent {
    fn trace(&self, _ev: TraceEvent) {}
    fn now(&self) -> Nanos {
        NOW
    }
    fn kick(&self, _target: CpuId) {}

    fn runs_on(&self, _cpu: CpuId) -> bool {
        false
    }
}

impl Machine for Silent {
    fn set_timer(&self, _deadline: Nanos) {}
    fn stop_timer(&self) {}
    fn halt(&self) {}
    fn need_resched(&self, _cpu: CpuId) {}
}

impl Hw for Silent {
    type Payload = Payload;
    #[allow(unsafe_code)]
    unsafe fn switch(&self, token: RunToken<Payload>) {
        // SAFETY: the trait's: both contexts are live, the outgoing one is
        // this CPU's, and the word is null or the outgoing task's.
        unsafe {
            (*token.save_ptr()).0.with_mut(|frame| *frame = Frame::Saved);
            if let Some(left) = token.left_ptr().as_ref() {
                left.store(false, Ordering::Release);
            }
            (*token.restore_ptr()).0.with_mut(|frame| {
                assert_ne!(*frame, Frame::Live, "resumed a context a CPU still stands on");
                *frame = Frame::Live;
            });
        }
    }
    fn release(&self, _key: TaskKey, _payload: Payload, _acct: TaskAccounting) {}
}

/// The instant every pass in these models runs at. Constant on purpose: a
/// request that never ends keeps `preempt_if_due` out of the interleavings,
/// which have nothing to do with it.
pub const NOW: Nanos = Nanos(1_000);

/// Everything a pass touches that is **not** a `CpuSched`. A thread that
/// stands for a remote CPU gets a handle to this and to nothing else: it may
/// post and ring, and there is no second way in.
pub struct Owner {
    pub cpus: CpuHandles<TaskMsg>,
    pub hw: Silent,
}

impl Owner {
    /// A new task, placed on `cpu` the way every task is: by a message.
    pub fn spawn(&self, cpu: CpuId, key: TaskKey) -> Arc<TaskShared<TaskMsg>> {
        let task = TaskBuilder {
            key,
            job: Arc::new(Job::new(JobKey(key.0))),
            ctx: ModelCtx::fresh(),
            ext: Payload,
            rt: RtState::default(),
            mask: CpuMask::ANY,
        }
        .build(cpu, NOW);
        let shared = task.shared().clone();
        let _ = self.cpus.get(cpu).post_owned(TaskMsg::Adopt { task }, TaskMsg::adopt_node, Urgency::Normal, &IrqGuard);
        shared
    }
}
