//! Which state an idle CPU waits in: the states the `acpi` claim's holder
//! declared for it, checked at the boundary (`toyos_userbound::idle`), chosen
//! among once per idle period by `kernel::sched::idle::choose`, and the
//! machine-wide wake bound that `object::wake_latency` holds set.
//!
//! **A CPU with nothing declared waits in C1**, `hlt`, as it always has: that
//! is the machine before its AML server has spoken, not a failure.
//!
//! **Everything here on the idle path is the CPU's own and lock-free**: the
//! declaration is a seqlock its CPU reads, the bound one atomic word, and what
//! the CPU learned of its wakes two words that CPU alone writes.
//!
//! **A bound holds from the moment its hold is made.** A CPU publishes the
//! exit of the state it is about to wait in, fences, and reads the bound
//! again; a new hold stores the bound, fences, and wakes every CPU whose
//! published exit exceeds it. So either the CPU sees the bound, or the hold
//! sees the CPU.

use core::sync::atomic::{fence, AtomicU64, Ordering};

use kernel::sched::hw::Nanos;
use kernel::sched::idle::{self, Bounds, Cost, History, MAX_STATES};
use toyos_abi::acpi::IdleDeclaration;
use toyos_abi::syscall::SyscallError;
use toyos_userbound::idle::{Entry, Table, WORDS};

use crate::seqlock::Published;
use crate::sync::Lock;

/// Tells one declaration of a CPU from the next, so the CPU forgets what it
/// learned of the states it replaced.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// A CPU's part.
pub struct Local {
    /// Published by [`declare`], under the claim's lock; read by the CPU.
    table: Published<WORDS>,
    /// What the CPU can enter, read by it at its first idle; `None` before.
    facts: Lock<Option<toyos_userbound::idle::CpuIdle>>,
    /// What the CPU learned of its wakes (`History::bits`), and the
    /// declaration it learned it under.
    history: AtomicU64,
    learned_under: AtomicU64,
    /// Idle periods spent in a declared state.
    deep: AtomicU64,
}

impl Local {
    pub fn new() -> Self {
        Self {
            table: Published::new(),
            facts: Lock::new(None),
            history: AtomicU64::new(0),
            learned_under: AtomicU64::new(0),
            deep: AtomicU64::new(0),
        }
    }

    /// Idle periods this CPU spent in a declared state.
    pub fn deep(&self) -> u64 {
        self.deep.load(Ordering::Relaxed)
    }
}

/// Read what this CPU can enter: called by each CPU as it first enters its
/// idle loop, before any declaration for it is checked.
pub fn init_here() {
    *crate::cpu_local::here().idle.facts.lock() = Some(crate::arch::idle::facts());
}

/// Take a processor's declaration from the `acpi` claim's holder: `NotFound`
/// where its UID names no CPU this kernel runs, which a machine declaring
/// more processor objects than it has CPUs gives often and is not said;
/// `InvalidArgument`, said by name, where the CPU cannot enter it as declared.
/// `readable` says whether the holder could read a port itself. The holder's
/// lock is held: one declaration at a time.
pub fn declare(declaration: &IdleDeclaration, readable: impl Fn(u16) -> bool) -> Result<(), SyscallError> {
    let uid = declaration.processor_uid;
    let cpu = crate::arch::idle::cpu_of(uid).ok_or(SyscallError::NotFound)?;
    let local = &crate::cpu_local::of(cpu).idle;
    let facts = (*local.facts.lock()).ok_or(SyscallError::NotFound)?;
    let table = toyos_userbound::idle::table(declaration, &facts, readable).map_err(|why| {
        log!("idle: cpu{cpu}'s declaration (processor UID {uid}) refused, so it waits in C1: {why:?}");
        SyscallError::InvalidArgument
    })?;
    local.table.publish(toyos_userbound::idle::words(&table, GENERATION.fetch_add(1, Ordering::Relaxed)));
    log!("idle: cpu{cpu} (processor UID {uid}) waits in C1 or {}", Said(&table));
    Ok(())
}

struct Said<'a>(&'a Table);

impl core::fmt::Display for Said<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (i, state) in self.0.states().enumerate() {
            let sep = if i == 0 { "" } else { ", " };
            match state.entry {
                Entry::Mwait { hint } => write!(f, "{sep}MWAIT {hint:#x}")?,
                Entry::Port { port } => write!(f, "{sep}port {port:#x}")?,
            }
            write!(f, " (exit {} µs, residency {} µs)", state.exit_ns / 1000, state.residency_ns / 1000)?;
        }
        if self.0.is_empty() {
            f.write_str("nothing deeper")?;
        }
        Ok(())
    }
}

/// The least bound held, in nanoseconds; `u64::MAX` is none.
static LIMIT: AtomicU64 = AtomicU64::new(u64::MAX);
static BOUNDS: Lock<Bounds> = Lock::new(Bounds::new());

fn limit() -> Option<u64> {
    Some(LIMIT.load(Ordering::Relaxed)).filter(|&l| l != u64::MAX)
}

/// This CPU's declared states, read whole, and the declaration they came
/// from; `None` with nothing declared or a declaration mid-publication, which
/// this idle period treats as nothing declared and the next reads.
struct Declared {
    generation: u64,
    len: usize,
    entries: [Entry; MAX_STATES],
    costs: [Cost; MAX_STATES],
}

fn declared(local: &Local) -> Option<Declared> {
    let read = toyos_userbound::idle::read(&local.table.snapshot()?);
    let mut d = Declared { generation: read.generation, len: 0, entries: [Entry::Mwait { hint: 0 }; MAX_STATES], costs: [Cost { exit: 0, residency: 0 }; MAX_STATES] };
    for (i, state) in read.states.iter().flatten().enumerate() {
        d.entries[i] = state.entry;
        d.costs[i] = Cost { exit: state.exit_ns, residency: state.residency_ns };
        d.len = i + 1;
    }
    Some(d)
}

/// What this CPU learned of its wakes, forgotten where its declaration is
/// not the one it learned under.
fn history(local: &Local, generation: u64) -> History {
    let learned = History::from_bits(local.history.load(Ordering::Relaxed));
    if generation == local.learned_under.load(Ordering::Relaxed) { learned } else { History::default() }
}

/// The idle state this CPU waits in once its pass ends, chosen with the
/// scheduler's timer due at `deadline`: the kernel's `Machine::idle_plan`.
/// The pass folds the plan's own wake into its one timer write.
pub fn plan(deadline: Option<Nanos>, now: Nanos) -> idle::Plan {
    let (deadline, now) = (deadline.map(|d| d.0), now.0);
    let local = &crate::cpu_local::here().idle;
    let Some(d) = declared(local).filter(|d| d.len > 0) else { return idle::Plan::c1(deadline, now) };
    let costs = &d.costs[..d.len];
    let choice = idle::choose(costs, &history(local, d.generation), deadline.map(|at| at.saturating_sub(now)), limit());
    idle::Plan { choice, exit: choice.state.map_or(0, |state| costs[state].exit), declaration: d.generation, chosen_at: now, deadline }
}

/// Wait, with interrupts masked and the timer armed by the pass, in the
/// state its `plan` chose; returns once the CPU has left it, for an
/// interrupt or, from `MWAIT`, for an event of the processor's own (SDM Vol.
/// 2B, MWAIT), which is a short unforeseen period. The kernel's
/// `Machine::idle_wait`.
///
/// The plan's exit is published, fenced, and the bound read again: a hold
/// made since the pass either is read here, and the CPU waits in C1 under
/// the timer the plan armed, or sees the exit and wakes it.
pub fn wait(plan: &idle::Plan) {
    let local = &crate::cpu_local::here().idle;
    let d = declared(local).filter(|d| d.len > 0 && d.generation == plan.declaration);
    crate::sched::driver::publish_idle_exit(plan.exit);
    fence(Ordering::SeqCst);
    let state = plan.choice.state.filter(|_| d.is_some() && limit().is_none_or(|bound| plan.exit <= bound));
    if state.is_none() && plan.exit != 0 {
        // Falling back to C1: what this CPU publishes is what it waits in.
        crate::sched::driver::publish_idle_exit(0);
    }
    match (state, &d) {
        (Some(state), Some(d)) => crate::arch::idle::enter(d.entries[state]),
        _ => crate::arch::hw::halt(),
    }
    // Awake: no hold has a reason to wake this CPU until it waits again.
    crate::sched::driver::publish_idle_exit(0);
    let woke = crate::clock::nanos_since_boot();
    if state.is_some() {
        local.deep.fetch_add(1, Ordering::Relaxed);
    }
    let Some(d) = d else { return };
    let costs = &d.costs[..d.len];
    let mut history = history(local, d.generation);
    plan.learn(&mut history, costs, woke);
    local.learned_under.store(d.generation, Ordering::Relaxed);
    local.history.store(history.bits(), Ordering::Relaxed);
}

/// A `WakeLatencyObject`'s bound, held until it goes: from when this returns
/// no CPU enters a state slower to leave than it, and each CPU waiting in one
/// has been sent a wake, which pays that state's exit once.
pub fn hold(bound: u64) {
    let least = {
        let mut bounds = BOUNDS.lock();
        bounds.hold(bound);
        let least = bounds.least().expect("a bound was just held");
        LIMIT.store(least, Ordering::Relaxed);
        least
    };
    fence(Ordering::SeqCst);
    crate::sched::driver::wake_slower_than(least);
}

pub fn release(bound: u64) {
    let mut bounds = BOUNDS.lock();
    bounds.release(bound);
    LIMIT.store(bounds.least().unwrap_or(u64::MAX), Ordering::Relaxed);
}
