//! What a panicked kernel does with the machine once its report is on the
//! panel: it holds the panel for [`toyos_tco::PANIC_BOUND_MS`] and then returns
//! the machine to firmware.
//!
//! **The bound is carried in counter ticks, not nanoseconds** (`cpu::counter`:
//! the TSC, the generic timer's count). A panic may land before `clock::init`,
//! where the calibrated clock reads zero for every interval; the tick count
//! comes from the frequency the CPU states instead (CPUID, `CNTFRQ_EL0`), and
//! both phases then compare the same counter against the same unit. A machine
//! that states no frequency and has no calibrated clock cannot time anything,
//! and the arm line says so instead of resetting on a guess.

use crate::arch::cpu;
use crate::drivers::serial;
use crate::time::{Budget, Duration};

/// The shipped bound.
const PANIC_BOUND: Budget = Budget::of(
    Duration::from_millis(toyos_tco::PANIC_BOUND_MS),
    "the machine returns itself to firmware instead of holding a panel nobody is reading",
);

/// `panic-reboot-fast`'s bound: a judge cannot spend the shipped minute per boot.
#[cfg(feature = "boot-actuators")]
const FAST_BOUND: Budget = Budget::of(
    Duration::from_secs(5),
    "a guest reaches the reset inside one test",
);

/// Whether a reboot is armed on this panic, and when.
#[derive(Clone, Copy)]
pub enum Bound {
    /// Reset the machine at this `cpu::counter` reading.
    At(u64),
    /// Hold the panel: nothing here could time a wait, or this machine has no
    /// reset this kernel performs.
    Held,
}

impl Bound {
    /// Reset the machine if the bound has passed; every wait on the panic path
    /// calls this, and it is the only place that decides the reset has come due.
    pub fn check(self) {
        if let Self::At(cycles) = self {
            if cpu::counter() >= cycles {
                reboot_now();
            }
        }
    }

    pub fn is_armed(self) -> bool {
        matches!(self, Self::At(_))
    }
}

/// Which clock converted the bound into cycles, for the one line that says so.
#[derive(Clone, Copy)]
enum Source {
    Calibrated,
    Stated,
}

impl Source {
    fn named(self) -> &'static str {
        match self {
            Source::Calibrated => "the calibrated clock",
            Source::Stated => "the counter frequency the CPU states",
        }
    }
}

/// The two heads the panel's last line can have, kept apart here so neither
/// can be read as the other: one says a reset is coming and the other says
/// this machine will sit where it is.
const ARMED: &str = "panic: rebooting";
const HELD: &str = "panic: holding this panel";

/// The bound in counter ticks from now, and which clock said so.
fn deadline(bound: Budget) -> Option<(u64, Source)> {
    if crate::clock::calibrated() {
        return Some((crate::clock::tsc_deadline(bound.nanos()), Source::Calibrated));
    }
    // Yoga image: a CPU that states no rate (AMD) is assumed to count at 5 GHz,
    // an upper bound, so the hold is at least the bound and the machine resets.
    let hz = crate::arch::cpu::stated_counter_hz().unwrap_or(5_000_000_000);
    // Nanoseconds first, so a bound under a second is not rounded to nothing.
    let cycles = (u128::from(bound.nanos()) * u128::from(hz) / 1_000_000_000) as u64;
    Some((cpu::counter().saturating_add(cycles), Source::Stated))
}

/// Arm the reboot and say so in one line — the panel's last, because the panic
/// path captures the log right after this and paints that capture.
///
/// `on_the_record` writes the line through the log, which puts it on the panel
/// and on the console; false is for the reentry guard, whose suspect is the log
/// path itself, and there the line goes to the UART raw and the panel carries none.
pub fn arm(on_the_record: bool) -> Bound {
    #[cfg(feature = "boot-actuators")]
    let budget = if crate::actuator::panic_reboot_fast() { FAST_BOUND } else { PANIC_BOUND };
    #[cfg(not(feature = "boot-actuators"))]
    let budget = PANIC_BOUND;

    // ASCII only, here and in every line below: the panel's font renders
    // anything outside 0x20..=0x7E as a dot.
    let secs = budget.duration().millis() / 1_000;
    match (deadline(budget), crate::power::can_reboot()) {
        (Some((cycles, source)), true) => {
            if on_the_record {
                alert!("{ARMED} in {secs} s, timed by {}", source.named());
            } else {
                serial::panic_registers().write(b"panic: rebooting\n");
            }
            Bound::At(cycles)
        }
        (Some((_, source)), false) => {
            if on_the_record {
                alert!(
                    "{HELD}, timed by {}: this kernel has no reset to hand the machine back to \
                     firmware with",
                    source.named()
                );
            } else {
                serial::panic_registers().write(b"panic: holding this panel: no reset\n");
            }
            Bound::Held
        }
        (None, _) => {
            if on_the_record {
                alert!(
                    "{HELD}: this CPU states no counter frequency and none is calibrated, so \
                     nothing here can time a wait"
                );
            } else {
                serial::panic_registers().write(b"panic: holding this panel: no clock\n");
            }
            Bound::Held
        }
    }
}

/// Return the machine to firmware. The second of this path's two lines, and it
/// goes out raw: the log has already been flushed and drained by here.
pub fn reboot_now() -> ! {
    serial::panic_registers()
        .write(b"\npanic: the bound is over: returning this machine to firmware\n");
    // Not `power::reboot`: its flush waits on the console wire, and a CPU this
    // panic stopped may be holding it.
    crate::power::reset_now()
}
