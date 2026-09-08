//! The boot deadline: the one bound on this machine that nothing running on it
//! can hold off.
//!
//! Every other bound a boot runs under is fed or checked by the thing it
//! bounds: the firmware's watchdog dies at `ExitBootServices`, the chipset's TCO
//! does not count on every PCH, the runner's job bound is a check made *between*
//! jobs, and the panic path's bound exists only once something has panicked. A
//! kernel that stops making progress without panicking is bounded by none of
//! them, and on a machine with no power switch that is a hand on the button.
//!
//! So this one is armed off the parameter line and polled from the timer
//! interrupt entry, on every CPU, in both rings. Its whole requirement is that
//! some CPU still takes an interrupt: [`poll`] reads two atomics and the TSC,
//! takes no lock, allocates nothing and calls nothing that can, so whatever a
//! wedged path is holding is not on this path.
//!
//! # What it does not cover, stated rather than implied
//!
//! - **The span before `clock::init`,** which no bound in this file or in
//!   [`crate::hardlockup`] reaches: no TSC period to convert a bound with, and
//!   no counter armed. A kernel that dies there still needs a hand.
//! - **A machine on which no CPU takes an interrupt at all.** Nothing polled
//!   from a running CPU can cover that; **that half is [`crate::hardlockup`]**,
//!   sampled by an NMI off a performance counter. The two are armed off this one
//!   parameter and compose **by scope and not by machine state**: this bound is
//!   the whole machine's and that one is any *single CPU*'s, so a machine with
//!   one deaf core is ended by that one, at the earlier bound, with a record
//!   that can name where the core is standing. Whichever fires takes the
//!   machine's one seal through [`claim_the_reset`].
//! - **A panic in progress**, which is not a gap but a stand-down:
//!   `apic::halt_all_cpus` calls [`stand_down`] before it holds the panel, so a
//!   panic report is never replaced by an expiry.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed};

/// Every phase [`boot_phase!`](crate::boot_phase) publishes, in the order a
/// boot reaches them, so a sealed record can name where the machine stopped.
/// Index 0 is a machine that has published none; the rest are the literals
/// `boot_phase!` is called with, and [`index_of`] refuses at compile time one
/// that is not here.
pub const PHASES: &[&str] = &[
    "before the first boot phase",
    "CPU ready",
    "storage ready",
    "peripherals ready",
    "subsystems ready",
    "devices ready",
    "complete",
];

/// The bound this boot was armed with, in milliseconds, or 0 for a boot that
/// named none. Written on the BSP before any AP exists.
static BOUND_MS: AtomicU64 = AtomicU64::new(0);

/// The `rdtsc` reading the machine is reset at, or 0 while unarmed.
///
/// **A TSC value and not a nanosecond one**, because `clock::nanos_since_boot`'s
/// multiply-and-divide is out of line and this is read on every timer tick of
/// every CPU.
static AT_TSC: AtomicU64 = AtomicU64::new(0);

/// Whether a CPU has taken the expiry. One machine, one seal, one reset.
static FIRED: AtomicBool = AtomicBool::new(false);

/// Take this machine's one seal-and-reset, or `false` where another CPU already
/// holds it — in which case the caller adds nothing to the page and goes no
/// further, because the page is being written. Shared with
/// [`crate::hardlockup`]: two bounds compose, one seal.
pub fn claim_the_reset() -> bool {
    !FIRED.swap(true, Relaxed)
}

/// Stand this bound down for the rest of the machine's life.
///
/// Called from `apic::halt_all_cpus` beside [`crate::hardlockup::stand_down`]:
/// from there this machine holds a panic report under a bound of its own, and an
/// expiry would seal a `WEDGED` record over it. Disarms rather than latching a
/// second flag, so [`poll`] stays one relaxed load.
pub fn stand_down() {
    AT_TSC.store(0, Relaxed);
}

/// Whether this boot runs under a bound at all — this one or
/// [`crate::hardlockup`]'s, since one parameter arms both.
///
/// **What the idle path asks before it re-arms a one-shot.** Both bounds rest on
/// some CPU taking a timer interrupt, and a CPU woken by an IPI and held in Ring
/// 0 takes none; on a boot that named no bound that re-arm is an x2APIC read and
/// two writes per wake that buy nothing.
pub fn armed() -> bool {
    AT_TSC.load(Relaxed) != 0
}

/// The last phase [`reached`] was told about, as an index into [`PHASES`].
static PHASE: AtomicU8 = AtomicU8::new(0);

/// The index `boot_phase!` publishes for its literal.
///
/// `const`, and it panics on a name [`PHASES`] does not carry — so adding a
/// boot phase without naming it here does not compile.
pub const fn index_of(name: &str) -> u8 {
    let mut i = 0;
    while i < PHASES.len() {
        if same(PHASES[i], name) {
            return i as u8;
        }
        i += 1;
    }
    panic!("boot_phase!: this phase is not one of `deadline::PHASES`")
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Publish the phase this machine has reached; `boot_phase!`'s, and nothing
/// else's.
pub fn reached(phase: u8) {
    PHASE.store(phase, Relaxed);
}

fn phase() -> &'static str {
    let at = PHASE.load(Relaxed) as usize;
    // Not indexed: this is read from inside the expiry, where a bounds check
    // that panicked would take the machine down inside the report about why it
    // is going down.
    match PHASES.get(at) {
        Some(name) => name,
        None => "an unpublished phase",
    }
}

/// Read the bound off the parameter line.
///
/// Beside `params::init`, on the same `&str` and for the same reason: the
/// parameter is in no reserved region, so `mm::init` may hand that memory out
/// and nothing may hold a borrow of it past here. The bound is only turned into
/// a deadline by [`start`], once there is a clock to measure one against.
pub fn claim(cmdline: &str) {
    match toyos_tco::deadline_in(cmdline) {
        None => {}
        Some(Ok(ms)) => BOUND_MS.store(ms, Relaxed),
        // Refused by name rather than booted unarmed: an image whose bound
        // nothing can read is an image with no deadline, and a boot that
        // silently had none is the case this whole mechanism exists to end.
        Some(Err(value)) => panic!(
            "{}{value}: the boot deadline is a number of milliseconds",
            toyos_tco::DEADLINE_PARAM
        ),
    }
}

/// Turn the bound into a deadline, once the TSC period is known.
///
/// Called after `clock::init` and the LAPIC timer's arm, which are the two
/// things [`poll`] needs: a period to convert with, and something that fires.
pub fn start() {
    let ms = BOUND_MS.load(Relaxed);
    if ms == 0 {
        return;
    }
    AT_TSC.store(crate::clock::tsc_deadline(ms.saturating_mul(1_000_000)), Relaxed);
    log!(
        "boot deadline: {ms} ms, after which this kernel seals a WEDGED record and writes the \
         reset register itself"
    );
    // The other half of the same parameter: what one CPU gets to take no
    // interrupt at all, which this poll cannot see because it is not running.
    // Written from here because everything that module does is reached from an
    // NMI, and none of it may say a word.
    log!("{}", crate::hardlockup::start(ms));
}

/// Whether this machine's bound has passed; the timer interrupt entry's, in
/// both rings, and nothing else's.
///
/// **One relaxed load in the callee on the unarmed path.** The Ring 0 call site
/// pays a caller-saved prologue on every tick of every CPU armed or not, and
/// that cost is the entry's rather than this function's.
///
/// `extern "sysv64"` because the Ring 0 half of the timer entry calls it from
/// naked assembly, where the ABI is written out rather than inferred.
pub extern "sysv64" fn poll() {
    let at = AT_TSC.load(Relaxed);
    if at == 0 || crate::arch::cpu::rdtsc() < at {
        return;
    }
    expire()
}

/// Seal why, stop what is doing DMA, and hand the machine back.
///
/// Runs inside an interrupt entry on whichever CPU noticed, so it is the same
/// no-lock, no-allocation, nothing-may-block region the panic path's seal runs
/// in — and for the stronger reason: every lock in this kernel is a thing the
/// wedge may be holding.
fn expire() -> ! {
    if !claim_the_reset() {
        // Another CPU is already sealing and resetting. This one has nothing to
        // add and must not race it into the page.
        crate::arch::cpu::halt();
    }
    crate::drivers::panic_console::seal_wedge(format_args!(
        "{EXPIRED}: a bound of {} ms, reached at {} ms, with this machine in `{}`. \
         The tail of the log ring follows — which is what nothing was draining.\n",
        BOUND_MS.load(Relaxed),
        crate::clock::nanos_since_boot() / 1_000_000,
        phase(),
    ));
    // The seal first, because the USB stop `reset_now` makes before it writes
    // the register is bounded but not instant, and this record is the
    // diagnostic the whole mechanism exists for.
    crate::drivers::acpi::reset_now()
}

/// What a sealed record opens with, and so what the next boot's loader prints
/// under `Previous boot's panic:`. A constant because the harness judges the
/// line and nothing links the two crates (`src/bootlog.rs`).
pub const EXPIRED: &str = "the boot deadline expired";

/// Wedge this machine on purpose, the way a defect does: the negative control
/// on everything above.
///
/// **Every CPU, and not just this one.** A wedge one core survives is a boot
/// that finishes, and a control the boot finishes is no control. From here every
/// CPU that reaches a scheduler pass stops taking them, with preemption disabled
/// and `IF` set — nothing panics, nothing halts, the LAPIC timers go on firing,
/// and no userland instruction runs again; [`this_cpu`] establishes that state
/// rather than inheriting it.
#[cfg(feature = "boot-actuators")]
pub fn stage_a_wedge() -> ! {
    log!("{WEDGE_STAGED}: every CPU stops taking scheduler passes from here");
    STAGED.store(true, Relaxed);
    // Kicked, and not left to arrive on their own: a CPU halted in the idle path
    // has stopped its own timer, so nothing would bring it to the pass this
    // wedge is taken at, and a core still asleep is not a core this control has
    // wedged.
    let me = crate::arch::percpu::cpu_id();
    for cpu in 0..crate::arch::smp::cpu_count() {
        if cpu != me {
            crate::arch::apic::kick_cpu(cpu);
        }
    }
    this_cpu()
}

/// What a staged wedge says before it stops: a `WEDGED` page whose text does not
/// hold this line is a machine the deadline ended for some other reason. Judged
/// by the harness, so it is a constant (`src/bootlog.rs`).
#[cfg(feature = "boot-actuators")]
pub const WEDGE_STAGED: &str = "wedge: staged, and only the boot deadline ends this machine";

/// A scheduler pass's first statement, where a staged wedge takes the CPU.
#[cfg(feature = "boot-actuators")]
pub fn wedge_if_staged() {
    if STAGED.load(Relaxed) {
        this_cpu()
    }
}

#[cfg(feature = "boot-actuators")]
static STAGED: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "boot-actuators")]
fn this_cpu() -> ! {
    // Preemption off, `IF` on and a one-shot armed: the shape a device
    // operation on this machine already runs in, so what the deadline has to
    // reach is the Ring 0 half of the timer entry and not the Rust half.
    //
    // **All three set, not assumed**, and not an `IrqGuard`: nothing here ever
    // puts them back. A CPU arriving from `stage_a_wedge` is inside the shutdown
    // syscall with `IF` masked, and one woken out of the idle halt has its
    // one-shot stopped however set `IF` is — either leaves a CPU taking no
    // interrupt at all, which is a hard lockup and not the state this control
    // claims.
    crate::preempt::disable();
    let arrived_awake = crate::arch::cpu::interrupts_enabled();
    crate::arch::apic::arm_within(toyos_sched::fair::QUANTUM_NS);
    crate::arch::cpu::enable_interrupts();
    log!(
        "wedge: cpu{} {}",
        crate::arch::percpu::cpu_id(),
        if arrived_awake { WEDGE_AWAKE } else { WEDGE_ARRIVED_DEAF },
    );
    // The hard-lockup control is this wedge and one CPU more, staged here —
    // where every CPU has already left the scheduler — because the idle loop it
    // would otherwise be staged from is one of the things this wedge stops.
    if crate::actuator::hard_lockup_probe() {
        crate::hardlockup::probe::stage();
    }
    loop {
        core::hint::spin_loop();
    }
}

/// What the CPU that staged the wedge says about the state it arrived in: a boot
/// on which no CPU says this is one the wedge never reached the CPU that asked
/// for it. Judged by the harness, so it is a constant (`src/bootlog.rs`).
#[cfg(feature = "boot-actuators")]
pub const WEDGE_ARRIVED_DEAF: &str =
    "arrived with interrupts off, through the syscall gate, and takes them again here";

/// What every other CPU says: they arrive from a scheduler pass, which already
/// had them.
#[cfg(feature = "boot-actuators")]
pub const WEDGE_AWAKE: &str = "arrived with interrupts on";

