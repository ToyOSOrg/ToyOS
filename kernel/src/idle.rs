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

use core::sync::atomic::{AtomicU64, Ordering};

use kernel::sched::hw::Nanos;
use kernel::sched::idle::{self, Bounds, Cost, History, MAX_STATES};
use toyos_abi::acpi::IdleDeclaration;
use toyos_abi::syscall::SyscallError;
use toyos_userbound::idle::{Entry, Table};

use crate::seqlock::Published;
use crate::sync::Lock;

/// A declaration's words: a generation and the count, then two per state.
const WORDS: usize = 1 + 2 * MAX_STATES;

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
/// The holder's lock is held: one declaration at a time.
pub fn declare(declaration: &IdleDeclaration) -> Result<(), SyscallError> {
    let uid = declaration.processor_uid;
    let cpu = crate::arch::idle::cpu_of(uid).ok_or(SyscallError::NotFound)?;
    let local = &crate::cpu_local::of(cpu).idle;
    let facts = (*local.facts.lock()).ok_or(SyscallError::NotFound)?;
    let table = toyos_userbound::idle::table(declaration, &facts).map_err(|why| {
        log!("idle: cpu{cpu}'s declaration (processor UID {uid}) refused, so it waits in C1: {why:?}");
        SyscallError::InvalidArgument
    })?;
    local.table.publish(encode(&table, GENERATION.fetch_add(1, Ordering::Relaxed)));
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

const MWAIT: u64 = 1;
const PORT: u64 = 2;

fn encode(table: &Table, generation: u64) -> [u64; WORDS] {
    let mut words = [0; WORDS];
    words[0] = generation << 8 | table.len() as u64;
    for (i, state) in table.states().enumerate() {
        let (kind, address) = match state.entry {
            Entry::Mwait { hint } => (MWAIT, u64::from(hint)),
            Entry::Port { port } => (PORT, u64::from(port)),
        };
        words[1 + 2 * i] = kind | address << 8;
        // Microseconds, as declared: each fits its half.
        words[2 + 2 * i] = (state.exit_ns / 1000) | ((state.residency_ns / 1000) << 32);
    }
    words
}

/// A CPU's states as [`encode`] wrote them, entry and cost each.
struct Declared {
    generation: u64,
    len: usize,
    entries: [Entry; MAX_STATES],
    costs: [Cost; MAX_STATES],
}

fn decode(words: &[u64; WORDS]) -> Declared {
    let len = (words[0] & 0xFF) as usize;
    let mut entries = [Entry::Mwait { hint: 0 }; MAX_STATES];
    let mut costs = [Cost { exit: 0, residency: 0 }; MAX_STATES];
    for i in 0..len {
        let (head, cost) = (words[1 + 2 * i], words[2 + 2 * i]);
        entries[i] = match head & 0xFF {
            MWAIT => Entry::Mwait { hint: (head >> 8) as u32 },
            PORT => Entry::Port { port: (head >> 8) as u16 },
            kind => panic!("idle: a published state of kind {kind}, which encode writes none of"),
        };
        costs[i] = Cost { exit: (cost & 0xFFFF_FFFF) * 1000, residency: (cost >> 32) * 1000 };
    }
    Declared { generation: words[0] >> 8, len, entries, costs }
}

/// The least bound held, in nanoseconds; `u64::MAX` is none.
static LIMIT: AtomicU64 = AtomicU64::new(u64::MAX);
static BOUNDS: Lock<Bounds> = Lock::new(Bounds::new());

/// Wait, with interrupts masked and the scheduler's timer armed for `armed`,
/// in the state this idle period chooses; returns once an interrupt has been
/// taken. The kernel's `Machine::idle_wait`.
pub fn wait(armed: Option<Nanos>) {
    let local = &crate::cpu_local::here().idle;
    let mut history = History::from_bits(local.history.load(Ordering::Relaxed));
    let now = crate::clock::nanos_since_boot();
    // A declaration mid-publication is this period in C1, and the next reads it.
    let declared = local.table.snapshot().map(|words| decode(&words));
    let (len, costs) = declared.as_ref().map_or((0, &[][..]), |d| (d.len, &d.costs[..d.len]));
    if let Some(d) = declared.as_ref().filter(|d| d.generation != local.learned_under.load(Ordering::Relaxed)) {
        history.forget();
        local.learned_under.store(d.generation, Ordering::Relaxed);
    }
    let horizon = armed.map(|at| at.0.saturating_sub(now));
    let limit = Some(LIMIT.load(Ordering::Relaxed)).filter(|&l| l != u64::MAX);
    let choice = idle::choose(costs, &history, horizon, limit);
    crate::sched::driver::publish_idle_exit(choice.state.map_or(0, |state| costs[state].exit));
    // The deadline met rather than made late by the state's exit: the timer
    // fires that much early, and the pass it brings finds nothing due and
    // waits again, shallower. A CPU its history held shallow wakes to look
    // again sooner still, where that is sooner.
    let fires = armed.map(|at| at.0.saturating_sub(choice.wake_early_by));
    let early = fires.filter(|_| choice.wake_early_by > 0);
    let revisit = choice.revisit_after.map(|after| now.saturating_add(after));
    if let Some(at) = early.into_iter().chain(revisit).min() {
        crate::arch::irqchip::arm_within(at.saturating_sub(now));
    }
    match (choice.state, &declared) {
        (Some(state), Some(d)) => crate::arch::idle::enter(d.entries[state]),
        _ => crate::arch::hw::halt(),
    }
    let slept = crate::clock::nanos_since_boot().saturating_sub(now);
    if choice.state.is_some() {
        local.deep.fetch_add(1, Ordering::Relaxed);
        local.deep_nanos.fetch_add(slept, Ordering::Relaxed);
    }
    // The scheduler's own timer ended the period where it fired at or after
    // the instant it was armed for; a revisit is a period the history learns.
    let foreseen = fires.is_some_and(|fires| now + slept >= fires);
    if !foreseen && len > 0 {
        history.unforeseen(costs, slept);
    }
    local.history.store(history.bits(), Ordering::Relaxed);
}

/// A `WakeLatencyObject`'s bound, held until it goes.
pub fn hold(bound: u64) {
    let mut bounds = BOUNDS.lock();
    bounds.hold(bound);
    LIMIT.store(bounds.least().unwrap_or(u64::MAX), Ordering::Relaxed);
}

pub fn release(bound: u64) {
    let mut bounds = BOUNDS.lock();
    bounds.release(bound);
    LIMIT.store(bounds.least().unwrap_or(u64::MAX), Ordering::Relaxed);
}
