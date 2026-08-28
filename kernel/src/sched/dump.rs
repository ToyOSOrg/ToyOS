//! Ctrl+Alt+D: what every CPU is holding, and what nothing is holding at all.
//!
//! No CPU reads another's scheduler state: the asking CPU marks and kicks
//! every sibling, and each prints its own tasks from `drain_irqs` next pass.
//! Nothing here allocates, waits on a lock it could find held, or leaves a
//! list unbounded. A CPU that misses the budget is named silent. Asking is
//! itself an interrupt, so a machine frozen on an unfired deadline reports
//! `0 OVERDUE`: summoning the report repairs the deadline it names.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{apic, percpu, smp};
use crate::sched::payload::{SCHED_BLOCKED, SCHED_READY, SCHED_RUNNING};
use crate::time::{Budget, Duration, Floor};

use super::driver;
use super::MAX_CPUS;

/// A [`Floor`]: nothing waits for this value, it only bounds another duration.
const ABSURD_HORIZON: Floor = Floor::policy(
    Duration::from_secs(3_600),
    "no kernel site parks for an hour, and an overflowed saturating_add lands 584 years out",
);

/// How long the asking CPU spins with preemption off before naming the silent ones.
const ANSWER_BUDGET: Budget = Budget::of(
    Duration::from_millis(250),
    "the silent CPUs are named and their part of the report is missing",
);

/// Cap on ordinary parked lines per CPU; anomaly lines are never truncated.
const LINES_PER_CPU: u32 = 16;

/// Census lines, which carry only the threads the parked lists do not.
const CENSUS_LINES: u32 = 16;

/// How long the census retries a held process table before giving up.
const TABLE_BUDGET: Budget = Budget::of(
    Duration::from_millis(20),
    "the summary says the census is missing rather than naming the threads no CPU has",
);

/// How long a silent CPU gets to answer the NMI: far less than
/// [`ANSWER_BUDGET`] since an NMI needs no scheduler pass.
const NMI_BUDGET: Budget = Budget::of(
    Duration::from_millis(1),
    "the CPU is reported silent with no instruction pointer beside it",
);

static IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static OWES: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];

/// NMI handshake: the handler (`arch/idt/nmi.rs`) may not allocate, log, or
/// lock, so it only stores and clears; the asking CPU reads.
static NMI_OWES: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static NMI_RIP: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// What the CPUs report, summed as each one answers; reset by `request`.
mod tally {
    use core::sync::atomic::AtomicU32;

    pub static PARKED: AtomicU32 = AtomicU32::new(0);
    pub static READY: AtomicU32 = AtomicU32::new(0);
    /// Killed threads unwinding; counted separately since the census reads
    /// their state word as `Ready`.
    pub static DYING: AtomicU32 = AtomicU32::new(0);
    pub static RUNNING: AtomicU32 = AtomicU32::new(0);
    pub static NO_DEADLINE: AtomicU32 = AtomicU32::new(0);
    pub static PENDING: AtomicU32 = AtomicU32::new(0);
    pub static OVERDUE: AtomicU32 = AtomicU32::new(0);
    pub static ABSURD: AtomicU32 = AtomicU32::new(0);
    pub static UNPRINTED: AtomicU32 = AtomicU32::new(0);

    pub const ALL: [&AtomicU32; 9] = [
        &PARKED,
        &READY,
        &DYING,
        &RUNNING,
        &NO_DEADLINE,
        &PENDING,
        &OVERDUE,
        &ABSURD,
        &UNPRINTED,
    ];
}

/// What a parked task's deadline says about it.
#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    /// No deadline: only an event ends this wait, which is what a server does.
    Event,
    /// A deadline still in the future.
    Pending,
    /// A deadline that passed without firing.
    Overdue,
    /// A deadline no wait could have meant.
    Absurd,
}

impl Verdict {
    fn of(deadline: Option<u64>, now: u64) -> Self {
        match deadline {
            None => Self::Event,
            Some(at) if at <= now => Self::Overdue,
            Some(at) if at - now > ABSURD_HORIZON.nanos() => Self::Absurd,
            Some(_) => Self::Pending,
        }
    }

    /// Whether this line must survive truncation.
    fn is_anomaly(self) -> bool {
        matches!(self, Self::Overdue | Self::Absurd)
    }

    fn count(self) {
        match self {
            Self::Event => &tally::NO_DEADLINE,
            Self::Pending => &tally::PENDING,
            Self::Overdue => &tally::OVERDUE,
            Self::Absurd => &tally::ABSURD,
        }
        .fetch_add(1, Ordering::Relaxed);
    }
}

fn online_cpus() -> usize {
    (smp::cpu_count() as usize).min(MAX_CPUS)
}

/// Ctrl+Alt+D. Called from `drain_irqs` on the CPU that decoded the key, from nowhere else.
pub fn request() {
    // Runs holding nothing: a `Lock` guard would raise preempt depth above 1.
    let depth = crate::preempt::count();
    assert!(depth <= 1, "the blocked-task dump ran under a lock: preempt depth {depth}");
    if IN_PROGRESS.swap(true, Ordering::AcqRel) {
        return;
    }
    for counter in tally::ALL {
        counter.store(0, Ordering::Relaxed);
    }

    let cpus = online_cpus();
    let me = percpu::cpu_id() as usize;
    // Two instants, not byte positions: there is no single stream across CPUs.
    let from = crate::clock::nanos_since_boot();
    log!("=== blocked-task dump: {cpus} cpu(s), and this report takes the screen ===");

    // Indexed by cpu id: `OWES` is `MAX_CPUS` long regardless of `cpus`.
    #[allow(clippy::needless_range_loop)]
    for cpu in 0..cpus {
        if cpu != me {
            OWES[cpu].store(true, Ordering::Release);
        }
    }
    // Every flag set before any kick, so an instant answer can't race its own flag.
    for cpu in 0..cpus {
        if cpu != me {
            apic::kick_cpu(cpu as u32);
        }
    }

    report_this_cpu();

    // A pass reached by preemption holds no `Lock`, so spinning can't block a sibling.
    let deadline = crate::clock::nanos_since_boot().saturating_add(ANSWER_BUDGET.nanos());
    let mut silent = 0;
    loop {
        if (0..cpus).all(|cpu| !OWES[cpu].load(Ordering::Acquire)) {
            break;
        }
        if crate::clock::nanos_since_boot() >= deadline {
            let mut asked = [false; MAX_CPUS];
            for cpu in 0..cpus {
                if OWES[cpu].swap(false, Ordering::AcqRel) {
                    silent += 1;
                    asked[cpu] = true;
                    log!("  cpu{cpu} !! no answer: it did not reach a scheduler pass");
                }
            }
            probe_silent(&asked, cpus);
            break;
        }
        core::hint::spin_loop();
    }

    let census = census();
    summary(cpus, silent, census);
    crate::drivers::panic_console::paint_report(from, crate::clock::nanos_since_boot());
    IN_PROGRESS.store(false, Ordering::Release);
}

/// Sends an NMI to each CPU that ignored its kick: unlike a kick, an NMI
/// reaches a CPU regardless of IF, halt, or where it is wedged.
fn probe_silent(asked: &[bool; MAX_CPUS], cpus: usize) {
    let any = (0..cpus).any(|cpu| asked[cpu]);
    if !any {
        return;
    }
    for cpu in 0..cpus {
        if asked[cpu] {
            NMI_RIP[cpu].store(0, Ordering::Relaxed);
            NMI_OWES[cpu].store(true, Ordering::Release);
        }
    }
    // Every flag set before any NMI, so an instant answer can't race its own flag.
    // Indexed by cpu id: `cpu` doubles as the APIC id the NMI goes to.
    #[allow(clippy::needless_range_loop)]
    for cpu in 0..cpus {
        if asked[cpu] {
            apic::send_nmi(cpu as u32);
        }
    }

    let deadline = crate::clock::nanos_since_boot().saturating_add(NMI_BUDGET.nanos());
    while (0..cpus).any(|cpu| asked[cpu] && NMI_OWES[cpu].load(Ordering::Acquire)) {
        if crate::clock::nanos_since_boot() >= deadline {
            break;
        }
        core::hint::spin_loop();
    }

    for cpu in 0..cpus {
        if !asked[cpu] {
            continue;
        }
        if NMI_OWES[cpu].swap(false, Ordering::AcqRel) {
            log!("  cpu{cpu} !! no NMI answer either: wedged below the interrupt layer");
        } else {
            log!("  cpu{cpu} NMI answered, it is here:");
            crate::symbols::resolve_kernel(NMI_RIP[cpu].load(Ordering::Acquire));
        }
    }
}

/// Stages a CPU that ignores its kick: QEMU cannot make a guest CPU deaf on
/// its own, so a kernel actuator disables interrupts and spins on the victim.
/// Bounded and self-healing: the window is longer than [`ANSWER_BUDGET`] and
/// short enough that the guest still shuts down cleanly.
#[cfg(feature = "boot-actuators")]
pub(super) fn deaf_window() {
    /// Late enough that the machine is up and every CPU has joined.
    const ARM_AT_NS: u64 = 3_000_000_000;
    /// Comfortably past [`ANSWER_BUDGET`], so silence isn't a race, and
    /// bounded so the guest still shuts down.
    const DEAF_NS: u64 = 400_000_000;
    /// How long cpu0 waits for the victim to reach its idle loop; generous
    /// because `drain_irqs` may reach USB enumeration first, leaving arrival unbounded.
    const ACK_BUDGET_NS: u64 = 1_000_000_000;

    const IDLE: u64 = 0;
    const ASKED: u64 = 1;
    const DEAF: u64 = 2;

    static STAGE: AtomicU64 = AtomicU64::new(IDLE);
    static FIRED: AtomicBool = AtomicBool::new(false);

    let cpus = online_cpus();
    if cpus < 2 {
        return;
    }
    let me = percpu::cpu_id() as usize;
    let victim = cpus - 1;

    if me == victim {
        if STAGE
            .compare_exchange(ASKED, DEAF, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let began = crate::clock::nanos_since_boot();
            // `rdtsc`, not `nanos_since_boot`: the latter calls into
            // `compiler_builtins`, which would misname where a stuck CPU is.
            let until = crate::clock::tsc_deadline(DEAF_NS);
            // Not an `IrqGuard`: this must unconditionally set IF on exit, and
            // panic recovery may already have left IF clear.
            crate::arch::cpu::disable_interrupts();
            while crate::arch::cpu::rdtsc() < until {
                core::hint::spin_loop();
            }
            crate::arch::cpu::enable_interrupts();
            // The victim's own log line is what proves the NMI interrupted it
            // rather than killed it.
            let deaf_ms = (crate::clock::nanos_since_boot() - began) / 1_000_000;
            log!("dump-deaf-cpu: cpu{me} rejoined after {deaf_ms}ms deaf");
        }
        return;
    }
    if me != 0 || crate::clock::nanos_since_boot() < ARM_AT_NS {
        return;
    }
    if FIRED.swap(true, Ordering::AcqRel) {
        return;
    }
    // Driven from here, not idle-loop iterations: cpu0 may halt between them.
    STAGE.store(ASKED, Ordering::Release);
    apic::kick_cpu(victim as u32);
    let deadline = crate::clock::nanos_since_boot().saturating_add(ACK_BUDGET_NS);
    while STAGE.load(Ordering::Acquire) != DEAF {
        if crate::clock::nanos_since_boot() >= deadline {
            // The CAS takes the ask back before giving up: if it fails, the
            // victim just went deaf and the window is still open.
            if STAGE
                .compare_exchange(ASKED, IDLE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                log!("dump-deaf-cpu: cpu{victim} did not reach its idle loop in time; asking again");
                FIRED.store(false, Ordering::Release);
                return;
            }
            break;
        }
        core::hint::spin_loop();
    }
    request();
}

/// Where this CPU was, for the NMI probe. Called only from `arch/idt/nmi.rs`.
/// Stores unconditionally: reading the flag first would race the requester that owns it.
pub fn note_nmi(rip: u64) {
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS {
        return;
    }
    NMI_RIP[me].store(rip, Ordering::Release);
    NMI_OWES[me].store(false, Ordering::Release);
}

/// Prints this CPU's own tasks if asked. Called from `drain_irqs` every pass.
pub fn serve_if_owed() {
    let me = percpu::cpu_id() as usize;
    if me >= MAX_CPUS || !OWES[me].load(Ordering::Acquire) {
        return;
    }
    report_this_cpu();
    OWES[me].store(false, Ordering::Release);
}

fn report_this_cpu() {
    let cpu = percpu::cpu_id();
    let now = crate::clock::nanos_since_boot();

    match driver::running_id() {
        Some(id) => {
            tally::RUNNING.fetch_add(1, Ordering::Relaxed);
            log!("  cpu{cpu} running pid={} tid={}", id.0.raw(), id.1.raw());
        }
        None => log!("  cpu{cpu} running nothing"),
    }
    let ready = driver::ready_len() as u32;
    tally::READY.fetch_add(ready, Ordering::Relaxed);

    // Read separately: a killed thread's state word reads `Ready(cpu)`, so
    // without this list every teardown in flight is an `unheld` false positive.
    let mut dying = 0u32;
    let read_dying = driver::for_each_dying(|id| {
        dying += 1;
        tally::DYING.fetch_add(1, Ordering::Relaxed);
        log!("  cpu{cpu} pid={} tid={} unwinding (killed)", id.0.raw(), id.1.raw());
    });
    if !read_dying {
        log!("  cpu{cpu} !! a pass owns its scheduler state; nothing read from it");
    }

    let mut ordinary = 0u32;
    let read = driver::for_each_parked(|task| {
        tally::PARKED.fetch_add(1, Ordering::Relaxed);
        let verdict = Verdict::of(task.deadline, now);
        verdict.count();
        if !verdict.is_anomaly() {
            ordinary += 1;
            if ordinary > LINES_PER_CPU {
                tally::UNPRINTED.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
        log!(
            "  cpu{cpu} {}pid={} tid={} {} parked {} {}{}",
            if verdict.is_anomaly() { "!! " } else { "" },
            task.id.0.raw(),
            task.id.1.raw(),
            task.class.name(),
            Ms(now.saturating_sub(task.since)),
            Deadline { verdict, deadline: task.deadline, now },
            if task.rt { " rt" } else { "" },
        );
    });
    if !read {
        log!("  cpu{cpu} !! a pass owns its scheduler state; nothing read from it");
    }
    if ready > 0 {
        log!("  cpu{cpu} {ready} task(s) queued and not running");
    }
}

/// What the census found, for the summary to compare against the CPUs.
#[derive(Default, Clone, Copy)]
struct Census {
    read: bool,
    threads: u32,
    running: u32,
    ready: u32,
    blocked: u32,
    zombie: u32,
    unscheduled: u32,
    never_ran: u32,
}

/// Every thread the process table knows, one line for each the CPUs' parked
/// lists do not carry.
fn census() -> Census {
    let mut c = Census::default();
    let mut printed = 0u32;
    let deadline = crate::clock::nanos_since_boot().saturating_add(TABLE_BUDGET.nanos());
    c.read = walk_threads(deadline, |thread| {
        c.threads += 1;
        if thread.zombie.is_some() {
            c.zombie += 1;
            return;
        }
        // Kernel threads are named even though usually blocked: their parked
        // lines carry only a pid, and a frozen machine needs to know which.
        let kernel = crate::sched::kthread::is_kernel_task(crate::scheduler::TaskId(
            thread.pid,
            thread.tid,
        ));
        let (bucket, tag) = match thread.sched {
            Some(SCHED_RUNNING) => (&mut c.running, kernel.then_some("kernel")),
            Some(SCHED_BLOCKED) => (&mut c.blocked, kernel.then_some("kernel")),
            // `cpu_ns == 0` on a Ready thread means it has never run.
            Some(SCHED_READY) if thread.cpu_ns == 0 => {
                c.never_ran += 1;
                (&mut c.ready, Some("!! ready and has never run"))
            }
            Some(SCHED_READY) => (&mut c.ready, Some("ready")),
            _ => (&mut c.unscheduled, Some("!! no scheduler record")),
        };
        *bucket += 1;
        // Blocked and running threads are already the CPUs' lines; skip them here.
        let Some(tag) = tag else { return };
        // Kernel threads don't count against the budget: `MAX_KERNEL_TASKS`
        // bounds them at three, so counting them can't push these lines off the page.
        if !kernel {
            printed += 1;
            if printed > CENSUS_LINES {
                return;
            }
        }
        log!(
            "  {tag}: pid={} tid={} {} cpu={}",
            thread.pid.raw(),
            thread.tid.raw(),
            Named { process: thread.process, thread: thread.thread },
            Ms(thread.cpu_ns),
        );
    });
    c
}

/// Retries `try_for_each_thread` until `deadline`.
fn walk_threads(deadline: u64, mut f: impl FnMut(crate::process::ThreadCensus<'_>)) -> bool {
    loop {
        if crate::process::try_for_each_thread(&mut f) {
            return true;
        }
        if crate::clock::nanos_since_boot() >= deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

/// The verdict, printed last since it needs every CPU to have answered.
fn summary(cpus: usize, silent: u32, c: Census) {
    let answered = cpus - silent as usize;
    // Needs nothing from the CPU it describes: the counters are `PerCpu`'s own, read by a sibling.
    crate::irq_census::log_census();
    // Each count is written before the word it counts, so the gate parses by word, not position.
    if !c.read {
        log!("== census: the process table is held; no thread census this dump");
    } else {
        log!(
            "== census: {} thread(s) — {} running, {} ready, {} blocked, {} zombie, {} unscheduled",
            c.threads, c.running, c.ready, c.blocked, c.zombie, c.unscheduled,
        );
    }
    log!(
        "== sched: {answered}/{cpus} cpu(s) answered — {} running, {} queued, {} unwinding, \
         {} parked",
        tally::RUNNING.load(Ordering::Relaxed),
        tally::READY.load(Ordering::Relaxed),
        tally::DYING.load(Ordering::Relaxed),
        tally::PARKED.load(Ordering::Relaxed),
    );
    log!(
        "== deadlines: {} event-only, {} pending, {} OVERDUE, {} ABSURD",
        tally::NO_DEADLINE.load(Ordering::Relaxed),
        tally::PENDING.load(Ordering::Relaxed),
        tally::OVERDUE.load(Ordering::Relaxed),
        tally::ABSURD.load(Ordering::Relaxed),
    );
    // `unheld` compares what the state words claim against what the CPUs
    // actually hold: a thread claimed but not held will never run.
    let overdue = tally::OVERDUE.load(Ordering::Relaxed);
    let absurd = tally::ABSURD.load(Ordering::Relaxed);
    if c.read {
        // `DYING` is summed with `READY`: the census counts those threads as ready too.
        let scheduled = tally::PARKED.load(Ordering::Relaxed)
            + tally::READY.load(Ordering::Relaxed)
            + tally::DYING.load(Ordering::Relaxed)
            + tally::RUNNING.load(Ordering::Relaxed);
        let claimed = c.running + c.ready + c.blocked;
        log!(
            "== VERDICT: {overdue} overdue, {absurd} absurd, {} unheld, {} never ran{}",
            claimed.saturating_sub(scheduled),
            c.never_ran,
            if answered == cpus { "" } else { " (of the cpus that answered)" },
        );
    } else {
        log!(
            "== VERDICT: {overdue} overdue, {absurd} absurd — the process table would not \
             open, so nothing here can say what no cpu holds",
        );
    }
    // `lost` is the one count a console reader could never derive on their own.
    let (drained, lost, parks) = crate::log::console::stats();
    log!("== klogd: {drained} record(s) drained, {lost} lost, {parks} park(s)");
    let unprinted = tally::UNPRINTED.load(Ordering::Relaxed);
    if unprinted > 0 {
        log!("== {unprinted} ordinary parked task(s) not listed; every anomaly is");
    }
    log!("=== end of dump ===");
}

/// A thread by its two names; empty `thread` falls back to `process` alone.
struct Named<'a> {
    process: &'a str,
    thread: &'a str,
}

impl core::fmt::Display for Named<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.thread.is_empty() {
            true => write!(f, "{}", self.process),
            false => write!(f, "{}:{}", self.process, self.thread),
        }
    }
}

/// Milliseconds: every duration in this report is one.
struct Ms(u64);

impl core::fmt::Display for Ms {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}ms", self.0 / 1_000_000)
    }
}

struct Deadline {
    verdict: Verdict,
    deadline: Option<u64>,
    now: u64,
}

impl core::fmt::Display for Deadline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match (self.verdict, self.deadline) {
            (Verdict::Event, _) => write!(f, "no deadline"),
            (Verdict::Pending, Some(at)) => write!(f, "due in {}", Ms(at - self.now)),
            (Verdict::Overdue, Some(at)) => {
                write!(f, "OVERDUE by {}", Ms(self.now.saturating_sub(at)))
            }
            (Verdict::Absurd, Some(at)) => write!(f, "ABSURD, due in {}", Ms(at - self.now)),
            // Unreachable in practice: `Verdict::of` reads the same `Option`.
            (_, None) => write!(f, "no deadline"),
        }
    }
}
