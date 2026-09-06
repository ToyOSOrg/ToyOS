//! What a panicked kernel does with the machine once its report is on the
//! panel: it holds the panel for [`toyos_tco::PANIC_BOUND_MS`] and then returns
//! the machine to firmware. A key press retires the bound for good — a key is
//! how a person at the machine says the panel is being read — and nobody
//! pressing one inside the bound means nobody is there to read it.
//!
//! **The bound is carried in TSC cycles, not nanoseconds.** A panic may land
//! before `clock::init`, where the calibrated clock reads zero for every
//! interval; the cycle count comes from the frequency CPUID states instead, and
//! both phases then compare the same `rdtsc` against the same unit. A machine
//! that states no frequency and has no calibrated clock cannot time anything,
//! and the arm line says so instead of resetting on a guess.
//!
//! The reset is the FADT's, through [`acpi::reboot`]; a machine whose reset
//! register this kernel has not decoded yet — every panic before
//! `acpi::init_power` — is refused by name and holds, with no fallback.
//!
//! [`acpi::reboot`]: crate::drivers::acpi::reboot

use crate::arch::cpu;
use crate::drivers::{acpi, serial};
use crate::time::{Budget, Duration};

/// The shipped bound.
const PANIC_BOUND: Budget = Budget::of(
    Duration::from_millis(toyos_tco::PANIC_BOUND_MS),
    "the machine returns itself to firmware instead of holding a panel nobody is reading",
);

/// `tco-fast`'s counterpart for this bound: a judge cannot spend the shipped
/// minute per boot, and its control cannot press a key inside a bound shorter
/// than the round trip that presses it.
#[cfg(feature = "boot-actuators")]
const FAST_BOUND: Budget = Budget::of(
    Duration::from_secs(5),
    "a guest reaches the reset inside one test, and a control still beats it to the keyboard",
);

/// Whether a reboot is armed on this panic, and when.
#[derive(Clone, Copy)]
pub enum Bound {
    /// Reset the machine at this `rdtsc` reading.
    At(u64),
    /// Hold the panel: somebody is reading it, or nothing here could time a
    /// wait, or this machine has no reset register to write.
    Held,
}

impl Bound {
    /// A key arrived: the machine is that person's from here on.
    pub fn retire(&mut self) {
        *self = Self::Held;
    }

    /// Reset the machine if the bound has passed; every wait on the panic path
    /// calls this, and it is the only place that decides the reset has come due.
    pub fn check(self) {
        if let Self::At(cycles) = self {
            if cpu::rdtsc() >= cycles {
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
    Cpuid,
}

impl Source {
    fn named(self) -> &'static str {
        match self {
            Source::Calibrated => "the calibrated clock",
            Source::Cpuid => "the TSC frequency CPUID states",
        }
    }
}

/// The two heads the panel's last line can have, kept apart here so neither
/// can be read as the other: one says a reset is coming and the other says
/// this machine will sit where it is.
const ARMED: &str = "panic: rebooting";
const HELD: &str = "panic: holding this panel";

/// The bound in `rdtsc` cycles from now, and which clock said so.
fn deadline(bound: Budget) -> Option<(u64, Source)> {
    if crate::clock::calibrated() {
        return Some((crate::clock::tsc_deadline(bound.nanos()), Source::Calibrated));
    }
    let hz = crate::clock::cpuid_tsc_hz()?;
    // Nanoseconds first, so a bound under a second is not rounded to nothing.
    let cycles = (u128::from(bound.nanos()) * u128::from(hz) / 1_000_000_000) as u64;
    Some((cpu::rdtsc().saturating_add(cycles), Source::Cpuid))
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
    // The clock and the reset register are two separate ways to have no
    // reboot, and a line naming one when the other is what failed answers
    // whoever reads the panel with nothing.
    match (deadline(budget), acpi::can_reboot()) {
        (Some((cycles, source)), true) => {
            if on_the_record {
                alert!(
                    "{ARMED} in {secs} s unless a key is pressed, timed by {}",
                    source.named()
                );
            } else {
                serial::panic_raw(b"panic: rebooting unless a key is pressed\n");
            }
            Bound::At(cycles)
        }
        (Some((_, source)), false) => {
            if on_the_record {
                alert!(
                    "{HELD}, timed by {}: this kernel has decoded no reset register to hand \
                     the machine back to firmware with",
                    source.named()
                );
            } else {
                serial::panic_raw(b"panic: holding this panel: no reset register\n");
            }
            Bound::Held
        }
        (None, _) => {
            if on_the_record {
                alert!(
                    "{HELD}: this CPU states no TSC frequency and none is calibrated, so \
                     nothing here can time a wait"
                );
            } else {
                serial::panic_raw(b"panic: holding this panel: no clock\n");
            }
            Bound::Held
        }
    }
}

/// Return the machine to firmware. The second of this path's two lines, and it
/// goes out raw: the log has already been flushed and drained by here.
pub fn reboot_now() -> ! {
    serial::panic_raw(
        b"\npanic: no key inside the bound, so nobody is here: returning this machine to \
          firmware\n",
    );
    acpi::reboot()
}
