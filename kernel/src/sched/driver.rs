//! The kernel driver for the scheduler core: plumbing only — percpu,
//! the asm switch, the idle loop, the trampoline. Every scheduling
//! decision lives in `toyos-sched`.
//!
//! A pass is not complete when it returns: restoring a context before its last `switch` instruction runs puts two CPUs on the same stack.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::arch::{asm, naked_asm};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

use toyos_sched::cpu::{Action, Balance, CpuHandle, CpuHandles, CpuSched, Env, SchedPass};
use toyos_sched::fair::Frontier;
use toyos_sched::hw::{CpuId, Hw, Kicker, Machine, Nanos};
use toyos_sched::mailbox::{mailbox, Kick, PreemptGuard, Urgency};
use toyos_sched::msg::Msg;
use toyos_sched::task::{RtState, TaskBuilder, TaskKey, WaitClass};
use toyos_sched::waitq::{Cancel, Cancelled, Commit, CurrentTask};

use crate::arch::percpu;
use crate::hw::HW;
use crate::process::{OwnedAlloc, PageTables, TaskId, KERNEL_STACK_SIZE};

use super::payload::{
    KMsg, KShare, KShared, KWaitQueue, KernelCtx, KernelPayload, RawTicket, TaskHandle, ThreadSched,
};
use super::MAX_CPUS;

/// Proof that preemption is disabled for as long as the borrow lasts.
pub struct PreemptOff(());

// SAFETY: every constructor holds the preempt count raised for the value's whole lifetime, so it cannot be descheduled while alive.
unsafe impl PreemptGuard for PreemptOff {}

/// Run `f` in a preempt-disabled region.
pub fn preempt_off<R>(f: impl FnOnce(&PreemptOff) -> R) -> R {
    crate::preempt::disable();
    let result = f(&PreemptOff(()));
    crate::preempt::enable();
    result
}

/// The same proof, bought with `cli`: `log::emit` must pay neither of [`preempt_off`]'s locked read-modify-writes.
pub struct IrqOff(());

// SAFETY: `do_preempt` needs `IF` set or a voluntary pass; `irq_off` masks `IF` and calls nothing that yields one.
unsafe impl PreemptGuard for IrqOff {}

/// Run `f` with interrupts masked, holding [`IrqOff`] for exactly that region.
pub fn irq_off<R>(f: impl FnOnce(&IrqOff) -> R) -> R {
    let _guard = crate::hw::IrqGuard::close();
    f(&IrqOff(()))
}

static CPUS: AtomicPtr<CpuHandles<KMsg>> = AtomicPtr::new(ptr::null_mut());
static FRONTIER: Frontier = Frontier::new();
static NEXT_KEY: AtomicU64 = AtomicU64::new(1);

/// Per-CPU CPU-time counters, for `total_cpu_ns`. Cache-line padded.
#[repr(align(64))]
struct CpuTime(AtomicU64);
static CPU_TIME_NS: [CpuTime; MAX_CPUS] = [const { CpuTime(AtomicU64::new(0)) }; MAX_CPUS];

/// Nanoseconds between two pass-cost reports — wall-clock, not per-pass, since `klogd`'s own wake is itself a pass.
#[cfg(feature = "sched-check")]
const PASS_COST_REPORT_EVERY_NS: u64 = 200_000_000;

/// When each CPU last reported, owned by that CPU alone.
#[cfg(feature = "sched-check")]
static PASS_COST_REPORTED: [CpuTime; MAX_CPUS] = [const { CpuTime(AtomicU64::new(0)) }; MAX_CPUS];

/// Publish this CPU's pass-cost distribution, at most once every
/// [`PASS_COST_REPORT_EVERY_NS`], outside the pass: `log::emit` may take no lock and does not.
#[cfg(feature = "sched-check")]
fn report_pass_costs(now: Nanos) {
    let cpu = current_cpu();
    let last = &PASS_COST_REPORTED[cpu.0 as usize].0;
    if now.0 < last.load(Ordering::Relaxed) + PASS_COST_REPORT_EVERY_NS {
        return;
    }
    last.store(now.0, Ordering::Relaxed);
    crate::log!("{}", cpus().get(cpu).pass_costs().report(cpu));
}

/// How often the heap sweep walks every live band — guest time, not passes, which are not a unit of wall-clock latency.
#[cfg(feature = "heap-sweep")]
const SWEEP_EVERY_NS: u64 = 25_000_000;

#[cfg(feature = "heap-sweep")]
static NEXT_SWEEP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Take the sweep if this CPU is the one that claims the slot, outside `with_cpu`: the sweep takes `dlmalloc`'s lock,
/// which wedges the machine if taken inside the driver's exclusive region.
#[cfg(feature = "heap-sweep")]
fn maybe_sweep(now: Nanos) {
    let due = NEXT_SWEEP.load(Ordering::Relaxed);
    if now.0 < due {
        return;
    }
    // A compare-exchange claim: concurrent CPUs run one sweep between them, not several.
    if NEXT_SWEEP
        .compare_exchange(due, now.0 + SWEEP_EVERY_NS, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    crate::mm::sweep_heap_bands("pass");
}

/// How long a [`maybe_hold`] visit spends on the pass path, and how often.
#[cfg(feature = "pass-spin")]
const HOLD_NS: u64 = 1_000_000;
#[cfg(feature = "pass-spin")]
const HOLD_EVERY_NS: u64 = 25_000_000;
#[cfg(feature = "pass-spin")]
static NEXT_HOLD: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Spend [`HOLD_NS`] on the pass path, and — under `heap-lockspin` — holding `dlmalloc`'s lock. Outside `with_cpu`, as [`maybe_sweep`].
#[cfg(feature = "pass-spin")]
fn maybe_hold(now: Nanos) {
    let due = NEXT_HOLD.load(Ordering::Relaxed);
    if now.0 < due {
        return;
    }
    if NEXT_HOLD
        .compare_exchange(due, now.0 + HOLD_EVERY_NS, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    #[cfg(feature = "heap-lockspin")]
    crate::mm::hold_heap_lock(HOLD_NS);
    #[cfg(not(feature = "heap-lockspin"))]
    spin_for(HOLD_NS);
}

/// Burn `ns` of guest time and nothing else.
#[cfg(feature = "pass-spin")]
pub(crate) fn spin_for(ns: u64) {
    let until = crate::clock::nanos_since_boot() + ns;
    while crate::clock::nanos_since_boot() < until {
        core::hint::spin_loop();
    }
}

pub fn cpus() -> &'static CpuHandles<KMsg> {
    let ptr = CPUS.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "scheduler used before sched::init");
    // SAFETY: set once by `init` from a leaked Box, never cleared.
    unsafe { &*ptr }
}

pub fn frontier() -> &'static Frontier {
    &FRONTIER
}

/// Monotonic and never reused, so a message names a dead task unambiguously. Not `TaskId`: pids and tids are
/// recycled.
fn next_key() -> TaskKey {
    TaskKey(NEXT_KEY.fetch_add(1, Ordering::Relaxed))
}

pub fn total_cpu_ns() -> u64 {
    (0..crate::arch::smp::cpu_count() as usize)
        .map(|i| CPU_TIME_NS[i].0.load(Ordering::Relaxed))
        .sum()
}

struct SchedSlot(UnsafeCell<Option<CpuSched<KernelPayload>>>);

// SAFETY: reached only through `with_cpu`, indexed by the calling CPU's own id, so no other CPU can alias this cell.
unsafe impl Sync for SchedSlot {}

static SCHEDS: [SchedSlot; MAX_CPUS] = [const { SchedSlot(UnsafeCell::new(None)) }; MAX_CPUS];
static IN_PASS: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// Is this CPU inside a pass? A nested pass is a bug, never deferred.
pub fn in_pass() -> bool {
    IN_PASS[percpu::cpu_id() as usize].load(Ordering::Relaxed)
}

/// The only accessor. Panics on reentry: a nested pass would alias `&mut`.
///
/// Nothing else in the kernel writes `SCHEDS` between one exit from here and the next entry — the window `sched-tripwire` holds to.
fn with_cpu<R>(f: impl FnOnce(&mut CpuSched<KernelPayload>) -> R) -> R {
    let cpu = percpu::cpu_id() as usize;
    assert!(
        !IN_PASS[cpu].swap(true, Ordering::Acquire),
        "nested scheduler pass on cpu {cpu}",
    );
    // SAFETY: exclusive by the flag above, and by CpuId — no other CPU
    // indexes this slot.
    let sched = unsafe { (*SCHEDS[cpu].0.get()).as_mut() }
        .unwrap_or_else(|| panic!("cpu {cpu} has no CpuSched"));
    #[cfg(feature = "sched-tripwire")]
    tripwire::verify(cpu, sched);
    let result = f(&mut *sched);
    #[cfg(feature = "sched-tripwire")]
    tripwire::record(cpu, sched);
    IN_PASS[cpu].store(false, Ordering::Release);
    result
}

/// The stray-write tripwire's storage: per CPU, touched by that CPU alone inside `with_cpu`'s exclusive region.
#[cfg(feature = "sched-tripwire")]
mod tripwire {
    use super::{CpuSched, KernelPayload, MAX_CPUS};
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    /// Words of shadow per CPU, checked against the record it shadows below.
    const WORDS: usize = 96;
    const _: () = assert!(
        core::mem::size_of::<CpuSched<KernelPayload>>().div_ceil(8) <= WORDS,
        "the CpuSched outgrew its shadow: raise WORDS",
    );

    struct Shadow {
        words: UnsafeCell<[u64; WORDS]>,
        taken: AtomicBool,
    }

    // SAFETY: each element is read and written by the CPU whose index it is and
    // by no other, inside `with_cpu`'s exclusive region.
    unsafe impl Sync for Shadow {}

    static SHADOW: [Shadow; MAX_CPUS] = [const {
        Shadow {
            words: UnsafeCell::new([0; WORDS]),
            taken: AtomicBool::new(false),
        }
    }; MAX_CPUS];

    /// The record's bytes as little-endian words, with the remotely-written field's words read back as zero.
    /// Volatile: a non-volatile read could be folded away instead of catching a stray write.
    fn snapshot(sched: &CpuSched<KernelPayload>, out: &mut [u64; WORDS]) {
        let base = (sched as *const CpuSched<KernelPayload>).cast::<u8>();
        let size = core::mem::size_of::<CpuSched<KernelPayload>>();
        let (lo, hi) = CpuSched::<KernelPayload>::tripwire_remote_range();
        for (i, word) in out.iter_mut().enumerate().take(size.div_ceil(8)) {
            let off = i * 8;
            if off < hi && off + 8 > lo {
                *word = 0;
                continue;
            }
            let mut bytes = [0u8; 8];
            for (k, byte) in bytes.iter_mut().enumerate() {
                if off + k >= size {
                    break;
                }
                // SAFETY: `off + k < size`, so this addresses a byte of the very
                // record `sched` borrows.
                *byte = unsafe { core::ptr::read_volatile(base.add(off + k)) };
            }
            *word = u64::from_le_bytes(bytes);
        }
    }

    /// Leaving the exclusive region: what the record must look like at the next entry.
    pub fn record(cpu: usize, sched: &CpuSched<KernelPayload>) {
        // Walked at both ends: which end first disagrees says whether
        // the pass or something outside it broke the record.
        walk(cpu, sched);
        // SAFETY: this CPU's own slot, inside the exclusive region.
        let slot = unsafe { &mut *SHADOW[cpu].words.get() };
        snapshot(sched, slot);
        SHADOW[cpu].taken.store(true, Ordering::Relaxed);
    }

    /// Entering it: anything that differs was written by something with no business writing it.
    pub fn verify(cpu: usize, sched: &CpuSched<KernelPayload>) {
        if !SHADOW[cpu].taken.load(Ordering::Relaxed) {
            return;
        }
        let mut now = [0u64; WORDS];
        snapshot(sched, &mut now);
        // SAFETY: as `record`.
        let was = unsafe { &*SHADOW[cpu].words.get() };
        let words = CpuSched::<KernelPayload>::tripwire_words();
        let mut hit = false;
        for i in 0..words {
            if was[i] == now[i] {
                continue;
            }
            if !hit {
                crate::log!("A STRAY WRITE REACHED cpu{cpu}'s CpuSched while no pass held it:");
                hit = true;
            }
            crate::log!(
                "  +{:#05x} {} was {:#018x}, is {:#018x}",
                i * 8,
                CpuSched::<KernelPayload>::tripwire_field(i * 8),
                was[i],
                now[i],
            );
        }
        if hit {
            let here = 0u64;
            crate::hw::report_contexts(core::ptr::addr_of!(here) as u64, None);
            panic!("cpu {cpu}: a stray write reached its CpuSched");
        }
        walk(cpu, sched);
    }

    /// The shadow covers the record's own bytes only; containers are walked here too, since a broken `BTreeMap`
    /// node is as likely as a broken header.
    fn walk(cpu: usize, sched: &CpuSched<KernelPayload>) {
        let mut walked = 0usize;
        let mut fingerprint = 0u64;
        // Counted by hand, not `.count()`: a chained `ExactSizeIterator`'s count can fold to
        // its length without touching a node.
        for task in sched.rq().tasks() {
            walked += 1;
            fingerprint ^= task.key().0;
        }
        assert_eq!(
            walked,
            sched.rq().len(),
            "cpu {cpu}: the ready band walks {walked} tasks and calls itself {} long",
            sched.rq().len(),
        );
        // Walked for the walk's sake: a corrupt node fails inside the
        // iterator, and neither publishes a second length to disagree with.
        for parked in sched.parked() {
            fingerprint ^= parked.key().0;
        }
        for dying in sched.dying() {
            fingerprint ^= dying.key().0;
        }
        core::hint::black_box(fingerprint);
    }
}

/// A read-only peek for diagnostics that must not fail while a pass runs.
fn try_with_cpu<R>(f: impl FnOnce(&CpuSched<KernelPayload>) -> R) -> Option<R> {
    let cpu = percpu::cpu_id() as usize;
    if IN_PASS[cpu].load(Ordering::Relaxed) {
        return None;
    }
    // SAFETY: as `with_cpu`, and shared rather than exclusive.
    let sched = unsafe { (*SCHEDS[cpu].0.get()).as_ref() }?;
    Some(f(sched))
}

/// Build every CPU's mailbox and handle, and the BSP's `CpuSched`. Called once, before any task exists.
pub fn init() {
    let count = crate::arch::smp::cpu_count() as usize;
    assert!(count <= MAX_CPUS, "cpu count {count} exceeds MAX_CPUS");
    let mut handles = Vec::with_capacity(count);
    // A CPU number, not a walk of `SCHEDS`: `SCHEDS` is `MAX_CPUS` long whatever `count` is.
    #[allow(clippy::needless_range_loop)]
    for cpu in 0..count {
        let (tx, rx) = mailbox::<KMsg>();
        handles.push(CpuHandle::new(CpuId(cpu as u32), tx));
        // SAFETY: single-threaded boot; the APs have not joined yet.
        unsafe {
            *SCHEDS[cpu].0.get() = Some(CpuSched::new(CpuId(cpu as u32), rx, idle_ctx()));
        }
    }
    CPUS.store(
        Box::into_raw(Box::new(CpuHandles::new(handles))),
        Ordering::Release,
    );
}

/// The context a CPU runs on when idle — never a dead task's stack, so a pass can free the previous zombie.
fn idle_ctx() -> KernelCtx {
    KernelCtx {
        rsp: 0,
        cr3: crate::mm::paging::kernel_cr3(),
        fs_base: 0,
        kernel_stack_top: 0,
        id: None,
        // Never read: the idle loop is entered by jump, not switch, and
        // a switch away from idle always writes the real depth first.
        preempt: 0,
    }
}

/// Where a spawn goes: the rule is [`CpuHandles::place`]'s; this supplies the rotating start, load-bearing at
/// boot since every init program is spawned before any CPU has published a load.
fn placement(now: Nanos) -> CpuId {
    static ROTATE: AtomicU64 = AtomicU64::new(0);
    let count = crate::arch::smp::cpu_count() as u64;
    let start = CpuId((ROTATE.fetch_add(1, Ordering::Relaxed) % count) as u32);
    cpus().place(start, now)
}

/// Everything a new thread needs. `entry_rsp` points at the trampoline frame `alloc_kernel_stack` built;
/// `address_space` is not `Option` — every kernel thread uses the kernel address space, so one declaration
/// decides `cr3`.
pub struct NewTask {
    pub id: TaskId,
    pub kernel_stack: OwnedAlloc,
    pub entry_rsp: u64,
    pub address_space: PageTables,
    pub fs_base: u64,
    pub share: Arc<KShare>,
    /// The process's symbol table; a kernel thread names an empty one.
    pub symbols: Arc<crate::symbols::SymbolTable>,
}

/// Place a new task by message — never by reaching into the destination's queue.
pub fn spawn(new: NewTask) -> (ThreadSched, CpuId) {
    // A kernel thread's is the kernel address space, the one every CPU
    // sits in between user threads — why `idle_ctx` names the same `cr3`.
    // Nothing is released at teardown: this `Arc` clones a leaked, permanent kernel mapping.
    let cr3 = new.address_space.lock().cr3();
    let kernel_stack_top = new.kernel_stack.ptr() as u64 + KERNEL_STACK_SIZE as u64;
    let ctx = KernelCtx {
        rsp: new.entry_rsp,
        cr3,
        fs_base: new.fs_base,
        kernel_stack_top,
        id: Some(new.id),
        // The one level `trampoline_entry` discharges before the first `iretq`.
        preempt: 1,
    };
    // One clock read for the placement and the build: `HW.now` is a divide.
    let now = HW.now();
    let handle = Arc::new(TaskHandle::new());
    let task = TaskBuilder {
        key: next_key(),
        share: new.share,
        ctx,
        ext: KernelPayload {
            id: new.id,
            kernel_stack: new.kernel_stack,
            address_space: new.address_space,
            handle: handle.clone(),
            symbols: new.symbols,
        },
        rt: RtState::default(),
    }
    .build(placement(now), now);
    let sched = ThreadSched {
        handle,
        shared: task.shared().clone(),
    };
    let dst = match task.shared().state() {
        toyos_sched::task::TaskState::InTransit(cpu) => cpu,
        state => panic!("a freshly built task is not in transit: {state:?}"),
    };
    preempt_off(|p| {
        if cpus()
            .get(dst)
            .post_owned(Msg::Adopt { task }, Msg::adopt_node, Urgency::Normal, p)
            == Kick::Send
        {
            HW.kick(dst);
        }
    });
    (sched, dst)
}

pub enum Dispose {
    /// An IRQ-exit poll: the pass decides whether the running task keeps the CPU.
    None,
    Yield,
    Exit,
}

/// The environment every pass runs against.
///
/// `balance` is [`Balance::PushOnSurplus`]: pull is the core's own, push closes its one hole — a CPU halted
/// before any sibling published surplus.
///
/// `preempt` is borrowed, not owned: its lifetime is the pass's, and it belongs to the caller that raised
/// the count.
fn env(preempt: &PreemptOff) -> Env<'_, crate::hw::KernelHw, PreemptOff> {
    Env {
        hw: &HW,
        cpus: cpus(),
        frontier: &FRONTIER,
        preempt,
        balance: Balance::PushOnSurplus {
            threshold: toyos_sched::cpu::PUSH_THRESHOLD,
        },
    }
}

/// Run one scheduler pass and execute its action. The preempt count travels with whichever context resumes, not per call.
pub fn pass(dispose: Dispose) {
    // The witness's negative control: sets DF one instruction before the reader that must refuse it.
    #[cfg(feature = "df-witness-mutate")]
    // SAFETY: a build that exists to stage the defect, and the reader below
    // panics before any `rep movs` can run. Nothing runs in between, so no
    // string op ever executes with it set.
    unsafe {
        core::arch::asm!("std", options(nomem, nostack))
    };
    #[cfg(feature = "df-witness")]
    crate::arch::cpu::df_witness("a scheduler pass");
    crate::preempt::disable();
    // Must clear before it drains, so a wake from this pass's own drain survives into the next poll.
    crate::preempt::clear_need_resched();
    drain_irqs();
    // After `drain_irqs` and before the pass picks, so a wake this
    // posts is in the run queue by the time the pass chooses.
    crate::object::drain_zero_handles();
    let now = HW.now();
    #[cfg(feature = "heap-sweep")]
    maybe_sweep(now);
    #[cfg(feature = "pass-spin")]
    maybe_hold(now);
    let action = with_cpu(|cpu| {
        let pass = SchedPass::begin(cpu, env(&PreemptOff(())), now);
        if let Some(current) = pass.cpu().running() {
            check_stack_canary(current.ext());
            current.ext().handle.publish(current.acct(), None);
        }
        let disposed = match dispose {
            Dispose::None => pass.dispose_none(),
            Dispose::Yield => pass.dispose_yield(),
            Dispose::Exit => pass.dispose_exit(),
        };
        disposed.finish()
    });
    charge_cpu_time(now);
    with_cpu(|cpu| {
        if let Some(current) = cpu.running() {
            current.ext().handle.publish(current.acct(), Some(now));
        }
    });
    // The one report site: every CPU reaches here on a timer tick or
    // while idling, so `pass_block` need not be a second one.
    #[cfg(feature = "sched-check")]
    report_pass_costs(now);
    execute(action);
    crate::preempt::enable_no_resched();
}

/// A wait registration, holding preemption off for the whole window between phase 1 and phase 2 of the wait handshake.
///
/// The window must stay closed: a preemption here lets a waker find `Committing` instead of `Ready` and report a lost wake.
#[must_use = "a wait ticket must be blocked on or cancelled"]
pub struct Ticket<'q>(RawTicket<'q>);

impl<'q> Ticket<'q> {
    /// Phase 1: register the running thread on `queue`. The count goes up before the task is read, or a
    /// preemption in between leaves `CurrentTask` naming a CPU it no longer runs on.
    pub fn register(queue: &'q KWaitQueue, cancel: Cancel, class: WaitClass) -> Self {
        crate::preempt::disable();
        let shared = current_shared().expect("prepare_wait: no running thread");
        let current = CurrentTask::new(&shared, current_cpu());
        // The class is the wait's, not the queue's — the queue is this
        // thread's own parking place and has no subject of its own.
        Self(queue.prepare_wait_as(&current, cancel, class))
    }

    /// The condition became true after registering: withdraw, and take the deferred preemption.
    pub fn cancel(self) -> Cancelled {
        let outcome = self.0.cancel();
        crate::preempt::enable();
        outcome
    }

    /// Hand the registration to the blocking pass. The count stays raised — see [`pass_block`].
    fn into_raw(self) -> RawTicket<'q> {
        self.0
    }
}

/// The blocking pass: commit the wait ticket **inside** the pass, after the mailbox drain, and park on the same pass.
///
/// Committing after the drain puts a remote waker's claim on one side of it or the other, so neither a lost
/// wake nor a double park.
pub fn pass_block(ticket: Ticket<'_>, deadline: Option<Nanos>) {
    // No `preempt::disable()` of its own: the ticket has held the count raised since registration; that guard is this bracket.
    let ticket = ticket.into_raw();
    crate::preempt::clear_need_resched();
    drain_irqs();
    let now = HW.now();
    let (action, registration) = with_cpu(|cpu| {
        let pass = SchedPass::begin(cpu, env(&PreemptOff(())), now);
        if let Some(current) = pass.cpu().running() {
            check_stack_canary(current.ext());
            current.ext().handle.publish(current.acct(), None);
        }
        match ticket.commit() {
            Commit::Parked(committed, registration) => (
                pass.dispose_block(committed, deadline).finish(),
                Some(registration),
            ),
            // A wake landed between registration and commit: do not park; the quantum may still have expired.
            Commit::AlreadyWoken => (pass.dispose_none().finish(), None),
            // A retire landed while deciding to park: the thread keeps running and unwinds rather than exit through a dead switch.
            Commit::Killed => (pass.dispose_none().finish(), None),
        }
    });
    charge_cpu_time(now);
    with_cpu(|cpu| {
        if let Some(current) = cpu.running() {
            current.ext().handle.publish(current.acct(), Some(now));
        }
    });
    execute(action);
    crate::preempt::enable_no_resched();
    if let Some(registration) = registration {
        // The node must leave the queue before this thread registers
        // anywhere else, or a later `wake_one` finds a waiter not waiting.
        registration.finish();
    }
}

/// Per-CPU busy time, for `sysinfo`, derived from the pass's own `now`.
fn charge_cpu_time(now: Nanos) {
    let cpu = percpu::cpu_id() as usize;
    static LAST: [CpuTime; MAX_CPUS] = [const { CpuTime(AtomicU64::new(0)) }; MAX_CPUS];
    let last = LAST[cpu].0.swap(now.0, Ordering::Relaxed);
    if last != 0 && percpu::current_tid().is_some() {
        CPU_TIME_NS[cpu]
            .0
            .fetch_add(now.0.saturating_sub(last), Ordering::Relaxed);
    }
}

fn execute(action: Action<KernelPayload>) {
    match action {
        // SAFETY: the token's task record outlives the switch — the
        // only way to free one is `Hw::release`, which runs in a later pass.
        Action::Run(token) => unsafe { HW.switch(token) },
        Action::Resume => {}
        Action::Idle(token) => {
            // The final look, with interrupts off, so a message that landed after the pass's own check is not lost to the halt.
            // Not `Machine::irq_guard`: both exits must set `IF`.
            crate::arch::cpu::disable_interrupts();
            let cpu = CpuId(percpu::cpu_id());
            let awake = cpus().get(cpu).doorbell().kick_pending()
                || crate::preempt::need_resched()
                || crate::irq_ring::any_pending_self()
                || !with_cpu(|c| c.mailbox_is_empty())
                // The i8042 verdict needs a pass to notice its deadline; a quiet machine after boot runs none otherwise.
                || crate::drivers::i8042::verdict_due()
                // No log condition here: a log to write means a runnable process, covered above. A pending
                // root-hub port needs a pass too — no interrupt is coming.
                || crate::drivers::xhci::port_work_pending();
            if awake {
                crate::arch::cpu::enable_interrupts();
                drop(token);
                return;
            }
            HW.idle_wait(token);
        }
    }
}

/// Consume this CPU's `irq_ring` records into wakes, before the mailbox drain, so a wake posted here reaches this pass's pick.
fn drain_irqs() {
    // First in the function, so the stamp means "this CPU reached a
    // pass" and not "this CPU got all the way through one".
    #[cfg(feature = "boot-actuators")]
    crate::heartbeat::note_pass();
    crate::drivers::xhci::poll_if_pending();
    crate::drivers::i8042::service();
    // Here, not at the keystroke: the keystroke's decoding driver's guard is done by this point.
    if crate::keyboard::take_dump_request() {
        super::dump::request();
    }
    // A CPU cannot read a sibling's `CpuSched`, so the dump reaches every CPU
    // by asking, and this is where each one answers.
    super::dump::serve_if_owed();
    // Repaints the panel if whoever owns the screen has drawn over the report.
    crate::drivers::panic_console::hold_report();

    if crate::irq_ring::take(crate::irq_ring::IrqSource::Net).is_some() {
        crate::net::wake_waiters();
        let watchers = crate::net::inbox_watchers();
        if !watchers.is_empty() {
            crate::inbox::complete_pending_for_event(
                &watchers,
                crate::inbox::Source::Network,
            );
        }
    }
    if crate::irq_ring::take(crate::irq_ring::IrqSource::Audio).is_some() {
        // One wait queue for both backends: a second queue would need
        // the parking side to know which driver bound, which it doesn't.
        crate::sched::waitqs::wake_device(&crate::sched::waitqs::AUDIO_WATCH);
        for (watchers, source) in [
            (
                crate::drivers::virtio_sound::inbox_watchers(),
                crate::inbox::Source::VirtioSound,
            ),
            (crate::drivers::hda::inbox_watchers(), crate::inbox::Source::Hda),
        ] {
            if !watchers.is_empty() {
                crate::inbox::complete_pending_for_event(&watchers, source);
            }
        }
    }
}

/// Leave the current stack for this CPU's idle stack and never come back.
pub fn enter_idle_loop() -> ! {
    percpu::set_current_tid(None);
    percpu::set_current_pid(None);
    // SAFETY: `set_kernel_stack` requires the caller be the CPU its GS base belongs to — true here, on that CPU, after its base was set.
    unsafe { percpu::set_kernel_stack(percpu::idle_stack_top()) };
    // SAFETY: `kernel_cr3` is the space this function's own code and stack already run in, so the write cannot unmap what executes it.
    unsafe { crate::mm::paging::kernel_cr3().activate() };
    let sp = percpu::idle_stack_top();
    // SAFETY: nothing on the outgoing stack is live past this — the function returns `!`, and `sp` is this CPU's own idle stack top.
    unsafe {
        asm!(
            "mov rsp, {sp}",
            // Zeroes the frame chain, so a panic here can backtrace instead of walking off the top of this stack;
            // `push` also leaves `rsp` where a function entry expects it.
            "xor ebp, ebp",
            "push rbp",
            "jmp {func}",
            sp = in(reg) sp,
            func = in(reg) idle_loop as *const () as usize,
            options(noreturn),
        );
    }
}

extern "C" fn idle_loop() -> ! {
    loop {
        // The idle loop, not a pass: the state it stages is a CPU that
        // never reaches one.
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::dump_deaf_cpu() {
            super::dump::deaf_window();
        }
        // From the other side: the storming CPU has nothing to run,
        // while the one under observation spins on `syscall` from Ring 3.
        #[cfg(feature = "boot-actuators")]
        if crate::actuator::syscall_window_nmi() {
            crate::nmi_gate::storm();
        }
        // Here, not from a syscall: the panic handler recovers, not paints, when a userland
        // thread is current, and the idle loop has none.
        #[cfg(feature = "boot-actuators")]
        if crate::drivers::panic_console::probe_due() {
            panic!("metal-panic-probe: a fatal report over a desktop that owns the screen");
        }
        crate::scheduler::log_health();
        crate::scheduler::reap_poisoned();
        // `pass` below covers this too; here as well so a CPU that
        // halts immediately has still run every hook first.
        crate::object::drain_zero_handles();
        // A heartbeat is a record like any other; the idle loop touches no filesystem itself.
        #[cfg(feature = "boot-actuators")]
        crate::heartbeat::poll();
        pass(Dispose::None);
    }
}

/// The running task's rendezvous word, cloned so the caller can hold it without borrowing `CpuSched`.
pub fn current_shared() -> Option<Arc<KShared>> {
    try_with_cpu(|cpu| cpu.running().map(|t| t.shared().clone())).flatten()
}

/// The running task's cross-CPU face, where its completion inbox lives. `None` off-task.
pub fn current_handle() -> Option<Arc<crate::sched::payload::TaskHandle>> {
    try_with_cpu(|cpu| cpu.running().map(|t| t.ext().handle.clone())).flatten()
}

/// The same face, borrowed rather than cloned, for a peek that does not outlive it — [`current_handle`]'s clone is a hot-path RMW.
pub fn with_current_handle<R>(f: impl FnOnce(&crate::sched::payload::TaskHandle) -> R) -> Option<R> {
    try_with_cpu(|cpu| cpu.running().map(|t| f(t.ext().handle.as_ref())))?
}

/// The symbol table of the task this CPU is running.
///
/// Takes no lock: the table is immutable and the `Arc` outlives teardown. `None`: a pass already holds this
/// record, or none is running.
///
/// No pass can start underneath the read: a fault handler runs with preemption declined.
pub fn current_symbols() -> Option<Arc<crate::symbols::SymbolTable>> {
    try_with_cpu(|cpu| cpu.running().map(|t| t.ext().symbols.clone())).flatten()
}

/// Whether the running task has been killed — one relaxed load, no clone, since an `Arc` refcount here is too costly on this path.
pub fn current_kill_pending() -> bool {
    try_with_cpu(|cpu| cpu.running().is_some_and(|t| t.shared().kill_pending())).unwrap_or(false)
}

pub fn current_cpu() -> CpuId {
    CpuId(percpu::cpu_id())
}

/// The address space the running task runs in. `None` means "no task is running" — `KernelPayload::address_space` is not `Option`.
pub fn current_address_space() -> Option<PageTables> {
    try_with_cpu(|cpu| cpu.running().map(|t| t.ext().address_space.clone())).flatten()
}

pub fn with_current_acct<R>(
    f: impl FnOnce(&toyos_sched::task::TaskAccounting) -> R,
) -> Option<R> {
    try_with_cpu(|cpu| cpu.running().map(|t| f(t.acct()))).flatten()
}

pub fn set_current_rt(permanent: bool) {
    with_cpu(|cpu| cpu.set_current_rt(permanent));
}

pub fn boost_current(until: Nanos) {
    with_cpu(|cpu| cpu.boost_current(until));
}

pub fn current_is_rt() -> bool {
    try_with_cpu(|cpu| cpu.running().is_some_and(|t| t.rt().is_rt())).unwrap_or(false)
}

pub fn ready_len() -> usize {
    try_with_cpu(|cpu| cpu.ready_len()).unwrap_or(0)
}

pub fn parked_len() -> usize {
    try_with_cpu(|cpu| cpu.parked().count()).unwrap_or(0)
}

/// Killed threads on this CPU that are unwinding or waiting to.
///
/// The dump's fourth container — without it a dying task is invisible to `unheld = claimed − scheduled`.
pub fn dying_len() -> usize {
    try_with_cpu(|cpu| cpu.dying_len()).unwrap_or(0)
}

/// Every dying thread on this CPU, in the order the pick will take them.
pub fn for_each_dying(mut f: impl FnMut(TaskId)) -> bool {
    try_with_cpu(|cpu| {
        for task in cpu.dying() {
            f(task.ext().id);
        }
    })
    .is_some()
}

/// The thread this CPU has loaded, if any.
pub fn running_id() -> Option<TaskId> {
    try_with_cpu(|cpu| cpu.running().map(|t| t.ext().id)).flatten()
}

/// One parked task, flattened because a `ParkedView` borrows the `CpuSched`, which nothing outside this file may hold.
pub struct ParkedInfo {
    pub id: TaskId,
    pub class: toyos_sched::task::WaitClass,
    pub deadline: Option<u64>,
    /// When the park began.
    pub since: u64,
    pub rt: bool,
}

/// Walk this CPU's parked tasks. `false` means a pass owns the state right now.
pub fn for_each_parked(mut f: impl FnMut(ParkedInfo)) -> bool {
    try_with_cpu(|cpu| {
        for parked in cpu.parked() {
            f(ParkedInfo {
                id: parked.ext().id,
                class: parked.class(),
                deadline: parked.deadline().map(|n| n.0),
                since: parked.since().0,
                rt: parked.is_rt(),
            });
        }
    })
    .is_some()
}

/// Tail of the first switch into a fresh task. No lock to release, no outgoing task to park: only the
/// preempt-count bracket's other half is owed.
pub extern "sysv64" fn trampoline_entry() {
    crate::preempt::enable_no_resched();
    crate::arch::idt::kernel_exit_to_user_check();
}

const STACK_CANARY: u64 = 0xDEAD_BEEF_CAFE_BABE;

/// What every untouched stack word holds under `heap-tripwire` — not zero, which a stack legitimately writes.
#[cfg(feature = "heap-tripwire")]
const STACK_FILL: u8 = 0xA5;
#[cfg(feature = "heap-tripwire")]
const STACK_FILL_WORD: u64 = u64::from_ne_bytes([STACK_FILL; 8]);

/// The layout `alloc_kernel_stack` asks for, named once so the tripwire reads
/// back the bands that request was given.
#[cfg(feature = "heap-tripwire")]
fn stack_layout() -> core::alloc::Layout {
    core::alloc::Layout::from_size_align(KERNEL_STACK_SIZE, 4096)
        .expect("the kernel stack layout")
}

/// Rungs of the depth ladder, in bytes above the bottom of the stack, descending so the depths they report
/// ascend. Touched is monotone up the stack, so the first still-filled rung ends the walk. Read rather than
/// walked: nine bounded reads stand in for an exact `partition_point`.
#[cfg(feature = "heap-tripwire")]
const DEPTH_RUNGS: [usize; 9] = [
    124 * 1024, 120 * 1024, 112 * 1024, 96 * 1024, 64 * 1024,
    32 * 1024, 16 * 1024, 8 * 1024, 4 * 1024,
];

/// The deepest any task kernel stack has been, in bytes used.
///
/// Never logged from where it's written: that runs inside `with_cpu`'s exclusive region, which a log re-enters and wedges.
/// `sched-tripwire`'s own `log!` is the one exception, since the `panic!` after it never returns.
#[cfg(feature = "heap-tripwire")]
static DEEPEST: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// The deepest any task kernel stack has been and the stack it's measured against, or `None` with no ladder.
#[cfg(feature = "heap-tripwire")]
pub fn stack_high_water() -> Option<(usize, usize)> {
    Some((DEEPEST.load(Ordering::Relaxed), KERNEL_STACK_SIZE))
}

/// See the `heap-tripwire` arm above; no ladder means no reading, not a reading of zero.
#[cfg(not(feature = "heap-tripwire"))]
pub fn stack_high_water() -> Option<(usize, usize)> {
    None
}

pub fn write_stack_canary(stack: &OwnedAlloc) {
    // Painted before the canary word, so the canary survives the fill.
    // SAFETY: `stack` is a fresh, exclusively-owned allocation of
    // exactly this size, so this writes only its own bytes.
    #[cfg(feature = "heap-tripwire")]
    unsafe { core::ptr::write_bytes(stack.ptr(), STACK_FILL, KERNEL_STACK_SIZE) };
    // SAFETY: the same fresh allocation; eight bytes at offset zero are
    // its own and 4096-aligned.
    unsafe { *(stack.ptr() as *mut u64) = STACK_CANARY };
}

fn check_stack_canary(payload: &KernelPayload) {
    // SAFETY: `payload.kernel_stack` is the allocation `write_stack_canary` painted, alive as long as the task is.
    let canary = unsafe { *(payload.kernel_stack.ptr() as *const u64) };
    if canary != STACK_CANARY {
        panic!(
            "KERNEL STACK OVERFLOW: tid={} canary={:#x} expected={:#x}",
            payload.id.1, canary, STACK_CANARY
        );
    }
    #[cfg(feature = "stack-witness")]
    check_stack_ownership(payload);
    #[cfg(feature = "heap-tripwire")]
    stack_depth(payload);
}

/// Does every Ring 3 → Ring 0 entry land on the stack of the task this CPU is running, and is this CPU standing on it?
///
/// A stray `kernel_rsp` or `tss.rsp0` aims a future entry at a stack it did not grow; this catches it before
/// that entry, not after.
#[cfg(feature = "stack-witness")]
fn check_stack_ownership(payload: &KernelPayload) {
    let bottom = payload.kernel_stack.ptr() as u64;
    let top = bottom + KERNEL_STACK_SIZE as u64;
    // SAFETY: a pass runs on the CPU whose GS base is its own `PerCpu`.
    let (kernel_rsp, rsp0) = unsafe { percpu::entry_stacks() };
    let rsp = crate::arch::cpu::read_rsp();
    if kernel_rsp == top && rsp0 == top && rsp <= top && rsp > bottom {
        return;
    }
    panic!(
        "STACK WITNESS: cpu{} is passing on tid={} whose stack is \
         [{bottom:#018x}, {top:#018x}) — kernel_rsp={kernel_rsp:#018x} \
         (off by {}), tss.rsp0={rsp0:#018x} (off by {}), rsp={rsp:#018x} \
         ({} bytes below the top). A Ring 3 entry takes its stack from one of \
         those two words, so one that is not this task's top aims the next \
         entry's return addresses into memory another execution owns.",
        percpu::cpu_id(),
        payload.id.1,
        kernel_rsp.wrapping_sub(top) as i64,
        rsp0.wrapping_sub(top) as i64,
        top.wrapping_sub(rsp) as i64,
    );
}

/// The heap bands around this task's kernel stack, and how deep it has been. Both ends: the canary catches
/// a write at the bottom, this reads the head and tail band words for a frame that stepped over it.
#[cfg(feature = "heap-tripwire")]
fn stack_depth(payload: &KernelPayload) {
    let bottom = payload.kernel_stack.ptr();
    // SAFETY: the running task's own stack, allocated with exactly
    // `stack_layout()`; running on this CPU, so nothing is freeing it.
    unsafe { crate::mm::check_heap_bands(bottom, stack_layout(), "kernel stack") };
    let mut used = 0;
    for rung in DEPTH_RUNGS {
        // SAFETY: `rung < KERNEL_STACK_SIZE`, so this reads a word of that
        // same stack.
        let word = unsafe { core::ptr::read_volatile(bottom.add(rung).cast::<u64>()) };
        if word == STACK_FILL_WORD {
            break;
        }
        used = KERNEL_STACK_SIZE - rung;
    }
    // Recorded and not logged. See [`DEEPEST`].
    DEEPEST.fetch_max(used, Ordering::Relaxed);
}

/// The outgoing half of [`context_switch`]. A macro, not inlined twice, so both builds share one instruction sequence.
macro_rules! switch_save {
    () => {
        "pushfq
         push rbp
         push rbx
         push r12
         push r13
         push r14
         push r15
         mov [rdi], rsp
         mov rsp, rsi"
    };
}

/// The incoming half: the seven words a resumed context stands on, ending in `ret`.
macro_rules! switch_restore {
    () => {
        "pop r15
         pop r14
         pop r13
         pop r12
         pop rbx
         pop rbp
         popfq
         ret"
    };
}

/// Callee-saved register save/restore.
#[cfg(not(feature = "switch-witness"))]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64) {
    naked_asm!(switch_save!(), switch_restore!());
}

/// The same switch with [`crate::hw::switch_witness_verify`] between the stack move and the first `pop`; never fired.
///
/// Placed after `mov rsp, rsi`, reading the incoming frame through the register the machine will use. Sound
/// to `call`: the return lands inside the incoming task's own stack, and every register `verify` may clobber
/// is caller-saved and already dead here.
#[cfg(feature = "switch-witness")]
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64) {
    naked_asm!(
        switch_save!(),
        "mov rdi, rsp",
        "call {verify}",
        switch_restore!(),
        verify = sym crate::hw::switch_witness_verify,
    );
}
