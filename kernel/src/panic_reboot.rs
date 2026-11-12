//! What a panicked kernel does with the machine once its report is out: it
//! holds the panel for [`toyos_tco::PANIC_BOUND_MS`] from the boot's first
//! fatal event and then ends the machine (`power::reset_now`), however early
//! the event.
//!
//! **The bound is carried in `cpu::counter` ticks**, converted once at the
//! best rate known when it is armed (`clock::ticks_at_least`): the calibrated
//! clock, the rate the CPU states, or a ceiling on any rate the counter may
//! have, at which the hold lasts the bound or longer and still ends. A fatal
//! path that arms after the first, a reentry or another CPU's, keeps the
//! first one's bound.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::arch::cpu;
use crate::clock::Rate;
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

/// The `cpu::counter` reading the machine is ended at, or 0 before the first
/// fatal path armed it.
static AT: AtomicU64 = AtomicU64::new(0);

/// Whether a fatal path ends the machine, and when.
#[derive(Clone, Copy)]
pub enum Bound {
    /// End the machine at this `cpu::counter` reading.
    At(u64),
    /// Hold the panel for good: this machine has no end this kernel can take,
    /// or nothing here can time a wait.
    Held,
}

impl Bound {
    /// Wait out the bound and end the machine: every fatal path's last call,
    /// on every CPU that takes one, so none halts with what it holds while
    /// another runs. Spins rather than `hlt`: nothing would wake it, and
    /// re-arming the timer would dispatch the scheduler mid-panic.
    pub fn hold(self) -> ! {
        let Self::At(at) = self else {
            cpu::halt();
        };
        while cpu::counter() < at {
            core::hint::spin_loop();
        }
        reboot_now()
    }
}

/// Which rate converted the bound, for the one line that says so.
fn named(rate: Rate) -> &'static str {
    match rate {
        Rate::Calibrated => "the calibrated clock",
        Rate::Stated => "the counter frequency the CPU states",
        Rate::Ceiling => "an uncalibrated ceiling on the counter's rate, so no sooner",
    }
}

/// The two heads the panel's last line can have, kept apart here so neither
/// can be read as the other: one says the machine ends and the other says it
/// will sit where it is.
const ARMED: &str = "panic: rebooting";
const HELD: &str = "panic: holding this panel";

/// Arm the bound, or take the one a fatal path armed first, and say so in one
/// line — the panel's last, because the panic path captures the log right
/// after this and paints that capture.
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
    let Some(how) = crate::arch::power::ending() else {
        say(on_the_record, [HELD, ": this kernel has no reset to hand the machine back to firmware with", ""]);
        return Bound::Held;
    };
    let first = AT.load(Relaxed);
    if first != 0 {
        say(on_the_record, [ARMED, " at the bound the first fatal path armed, ", how]);
        return Bound::At(first);
    }
    let Some((ticks, rate)) = crate::clock::ticks_at_least(budget.nanos()) else {
        say(on_the_record, [HELD, ": nothing here can time a wait", ""]);
        return Bound::Held;
    };
    let at = cpu::counter().saturating_add(ticks).max(1);
    // Another CPU's fatal path may have armed it since the load above.
    let at = match AT.compare_exchange(0, at, Relaxed, Relaxed) {
        Ok(previous) => previous,
        Err(first) => first,
    };
    let secs = budget.duration().millis() / 1_000;
    if on_the_record {
        alert!("{ARMED} in {secs} s, timed by {}, {how}", named(rate));
    } else {
        let mut uart = serial::panic_registers();
        uart.write(ARMED.as_bytes());
        uart.write(b" in ");
        uart.dec(secs);
        uart.write(b" s, timed by ");
        uart.write(named(rate).as_bytes());
        uart.write(b", ");
        uart.write(how.as_bytes());
        uart.write(b"\n");
    }
    Bound::At(at)
}

/// `parts`, as one line through the log or raw on the UART.
fn say(on_the_record: bool, parts: [&str; 3]) {
    if on_the_record {
        let [head, middle, tail] = parts;
        alert!("{head}{middle}{tail}");
    } else {
        let mut uart = serial::panic_registers();
        for part in parts {
            uart.write(part.as_bytes());
        }
        uart.write(b"\n");
    }
}

/// End the machine at the bound: once, by the first CPU to see it pass. Its
/// line goes out raw: the log has already been flushed and drained by here.
fn reboot_now() -> ! {
    if !crate::deadline::claim_the_reset() {
        cpu::halt();
    }
    serial::panic_registers()
        .write(b"\npanic: the bound is over: returning this machine to firmware\n");
    // Not `power::reboot`: its flush waits on the console wire, and a CPU this
    // panic stopped may be holding it.
    crate::power::reset_now()
}
