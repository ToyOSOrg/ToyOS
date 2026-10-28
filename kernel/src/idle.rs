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
    /// Idle periods spent in a declared state, and their nanoseconds.
    deep: AtomicU64,
    deep_nanos: AtomicU64,
}

impl Local {
    pub fn new() -> Self {
        Self {
            table: Published::new(),
            facts: Lock::new(None),
            history: AtomicU64::new(0),
            learned_under: AtomicU64::new(0),
            deep: AtomicU64::new(0),
            deep_nanos: AtomicU64::new(0),
        }
    }

    /// Idle periods this CPU spent in a declared state, and their nanoseconds.
    pub fn deep(&self) -> (u64, u64) {
        (self.deep.load(Ordering::Relaxed), self.deep_nanos.load(Ordering::Relaxed))
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

/// Wait, with interrupts masked and the scheduler's timer armed for `armed`,
/// in the state this idle period chooses; returns once the CPU has left it,
/// for an interrupt or, from `MWAIT`, for an event of the processor's own
/// (SDM Vol. 2B, MWAIT), which is a short unforeseen period. The kernel's
/// `Machine::idle_wait`.
pub fn wait(armed: Option<Nanos>) {
    let local = &crate::cpu_local::here().idle;
    let mut history = History::from_bits(local.history.load(Ordering::Relaxed));
    let now = crate::clock::nanos_since_boot();
    // A declaration mid-publication is this period in C1, and the next reads it.
    let declared = local.table.snapshot().map(|words| toyos_userbound::idle::read(&words));
    let mut entries = [Entry::Mwait { hint: 0 }; MAX_STATES];
    let mut costs = [Cost { exit: 0, residency: 0 }; MAX_STATES];
    let mut len = 0;
    if let Some(d) = &declared {
        for (i, state) in d.states.iter().flatten().enumerate() {
            entries[i] = state.entry;
            costs[i] = Cost { exit: state.exit_ns, residency: state.residency_ns };
            len = i + 1;
        }
        if d.generation != local.learned_under.load(Ordering::Relaxed) {
            history.forget();
            local.learned_under.store(d.generation, Ordering::Relaxed);
        }
    }
    let costs = &costs[..len];
    let horizon = armed.map(|at| at.0.saturating_sub(now));
    // Chosen under the bound read after the exit is published: a hold made
    // meanwhile either is read here or sees this exit and wakes this CPU.
    let mut bound = limit();
    let choice = loop {
        let choice = idle::choose(costs, &history, horizon, bound);
        let exit = choice.state.map_or(0, |state| costs[state].exit);
        crate::sched::driver::publish_idle_exit(exit);
        fence(Ordering::SeqCst);
        let now_bound = limit();
        if now_bound.is_none_or(|b| exit <= b) {
            break choice;
        }
        bound = now_bound;
    };
    let armed = armed.map(|at| at.0);
    // The pass just armed the timer for `armed`, or stopped it (`SleepToken`),
    // and `wake_at` is never later: one write moves it earlier, with no read
    // of the timer, which on an xAPIC is an uncached access on every idle.
    if let Some(at) = idle::wake_at(&choice, armed, now) {
        crate::arch::irqchip::arm_one_shot(at.saturating_sub(now));
    }
    match choice.state {
        Some(state) => crate::arch::idle::enter(entries[state]),
        None => crate::arch::hw::halt(),
    }
    // Awake: no hold has a reason to wake this CPU until it waits again.
    crate::sched::driver::publish_idle_exit(0);
    let woke = crate::clock::nanos_since_boot();
    let slept = woke.saturating_sub(now);
    if choice.state.is_some() {
        local.deep.fetch_add(1, Ordering::Relaxed);
        local.deep_nanos.fetch_add(slept, Ordering::Relaxed);
    }
    if len > 0 {
        match idle::ended(&choice, armed, now, woke) {
            idle::Ended::Deadline => {}
            idle::Ended::Revisit => history.quiet(costs, slept),
            idle::Ended::Unforeseen => history.unforeseen(costs, slept),
        }
    }
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
