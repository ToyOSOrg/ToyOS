//! The boot deadline: the one bound on this machine that nothing running on it
//! can hold off.
//!
//! Every other bound a boot runs under is fed or checked by the thing it
//! bounds. The firmware's watchdog dies at `ExitBootServices`. The chipset's
//! TCO has never counted on the owner's T14. The test runner's job bound is a
//! wall-clock check made *between* jobs, so a spawn that never returns is
//! outside it. The panic path's bound only exists once something has panicked.
//! A kernel that stops making progress without panicking is bounded by none of
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
//! - **The span before `clock::init`.** The deadline is a TSC value and there
//!   is no TSC period before then, so a kernel that dies in early bring-up is
//!   still a machine that needs a hand.
//! - **A machine on which no CPU takes an interrupt at all** — every core
//!   halted below the interrupt layer, or spinning with `IF` clear at once.
//!   Nothing polled from a running CPU can cover that.
//! - **A panic racing the seal.** [`expire`] loses the `PAINTING` latch to a
//!   CPU already inside the panel's fatal painter, and that CPU's own
//!   `record_panic` then replaces this record. That is the right outcome and
//!   not a gap: a panic report says more than a deadline does, and the panic
//!   path has a bound of its own.
//!
//! Both are the *same* seam, and it has a name: the sentinel CPU designed and
//! postponed in `issues/hardware/the-t14-boots-toyos-unattended.md`, a CPU
//! outside the roster spinning on the TSC. This is that design's first half —
//! the seal, the lock-free reset and the parameter are what it would have
//! needed too — and not a second mechanism beside it.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering::Relaxed};

/// Every phase [`boot_phase!`](crate::boot_phase) publishes, in the order a
/// boot reaches them, so a sealed record can name where the machine stopped.
///
/// Index 0 is the state of a machine that has published none. The rest are the
/// literals `boot_phase!` is called with, and [`index_of`] refuses at compile
/// time a literal that is not here — a phase the deadline cannot name is a
/// sealed record that says nothing about where the boot was.
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
/// **A TSC value and not a nanosecond one.** `clock::nanos_since_boot`'s
/// multiply-and-divide is out of line, and this is read on every timer tick of
/// every CPU; `src/redlist.rs`'s `dump_nmi_probe` is the instrument that
/// notices such a call from an interrupt entry.
static AT_TSC: AtomicU64 = AtomicU64::new(0);

/// Whether a CPU has taken the expiry. One machine, one seal, one reset.
static FIRED: AtomicBool = AtomicBool::new(false);

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
}

/// Whether this machine's bound has passed; the timer interrupt entry's, in
/// both rings, and nothing else's.
///
/// **One relaxed load on the unarmed path**, which is every boot the owner
/// flashes: this runs on every tick of every CPU.
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
    if FIRED.swap(true, Relaxed) {
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
/// that finishes, and a control the boot finishes is no control. From here
/// every CPU that reaches a scheduler pass stops taking them, with preemption
/// disabled and `IF` set — nothing panics, nothing halts, the LAPIC timers go
/// on firing, and no userland instruction runs again. That is strictly worse
/// than the T14's own wedge, where seven cores were still healthy, and the
/// deadline has to end it anyway.
#[cfg(feature = "boot-actuators")]
pub fn stage_a_wedge() -> ! {
    log!("{WEDGE_STAGED}: every CPU stops taking scheduler passes from here");
    STAGED.store(true, Relaxed);
    this_cpu()
}

/// What a staged wedge says before it stops, and the witness a sealed record
/// carries: a `WEDGED` page whose text does not hold this line is a machine the
/// deadline ended for some other reason, which is what makes the control a
/// control. Judged by the harness, so it is a constant (`src/bootlog.rs`).
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
    // Preemption off and `IF` on: the shape a device operation on this machine
    // already runs in, so what the deadline has to reach is the Ring 0 half of
    // the timer entry and not the Rust half.
    crate::preempt::disable();
    loop {
        core::hint::spin_loop();
    }
}

