//! Reset and power-off: PSCI's `SYSTEM_RESET` and `SYSTEM_OFF` (Arm DEN0022
//! §5.12, §5.10), through the conduit [`super::psci`] kept. A machine without
//! PSCI has neither, and holds where either is asked for.
//!
//! Every line here goes straight to the UART, each whole under one fatal
//! path's hold of its registers ([`serial::panic_registers`]): nothing drains
//! the log ring after any of these calls. The hold is let go before a halt,
//! since a CPU halted holding it costs every later line the whole bound.

use super::{cpu, irqchip, percpu, psci};
use crate::drivers::serial;
use crate::time::{Cadence, Deadline, Duration, DEAF_CPU};

/// How often [`off`] asks again of a CPU PSCI still answers on: PSCI says a
/// CPU is off only to one that asks.
const ASK_AGAIN: Cadence = Cadence::every(
    Duration::from_millis(1),
    "one call into firmware per period, and a power-off at most one period after the last CPU is off",
);

pub fn can_reset() -> bool {
    psci::conduit().is_some()
}

/// Never a reason: `SYSTEM_OFF` takes nothing a process supplies.
pub fn off_refused() -> Option<&'static str> {
    None
}

/// `SYSTEM_RESET`, which asks nothing of the other CPUs (DEN0022 §5.12.2). A
/// wedge calls this: the call takes no lock, and only its refusal is said.
pub fn reset() -> ! {
    if let Some(psci) = psci::conduit() {
        let refusal = psci.system_reset();
        let mut uart = serial::panic_registers();
        uart.write(b"power: SYSTEM_RESET answered ");
        say_code(&mut uart, refusal);
        uart.write(b"; holding\n");
    }
    cpu::halt()
}

/// `SYSTEM_OFF`, once every other CPU the roster holds has turned itself off
/// with `CPU_OFF` and PSCI answers it off: the caller puts every core in a
/// known state first, and this is DEN0022 §5.10.3's own way to. The budget
/// for all of them is [`DEAF_CPU`]'s span from the SGI: a CPU PSCI still
/// answers on at its end is named, and the machine powers off regardless.
pub fn off(_stopping: crate::quiesce::Stopping) -> ! {
    cpu::disable_interrupts();
    let Some(psci) = psci::conduit() else { cpu::halt() };
    irqchip::off_all_but_self();
    // The tripwire's span without its panic: what keeps a CPU on may be
    // firmware's refusal, and firmware's word never panics this kernel.
    let deadline = Deadline::at(crate::clock::now() + Duration::from_nanos(DEAF_CPU.nanos()));
    let me = percpu::cpu_id();
    for cpu in (0..crate::smp::cpu_count()).filter(|&cpu| cpu != me) {
        let mpidr = toyos_gicv3::unpacked_affinity(crate::smp::hardware_id(cpu));
        loop {
            match psci.affinity_info(mpidr) {
                Ok(psci::Affinity::Off) => break,
                Ok(_) if !deadline.reached(crate::clock::now()) => {
                    let again = Deadline::at(crate::clock::now() + ASK_AGAIN.duration());
                    while !again.reached(crate::clock::now()) {
                        core::hint::spin_loop();
                    }
                }
                Ok(_) => {
                    let mut uart = serial::panic_registers();
                    uart.write(b"power: cpu");
                    uart.dec(u64::from(cpu));
                    uart.write(b" is not off by PSCI's answer inside the budget; SYSTEM_OFF regardless\n");
                    break;
                }
                Err(refusal) => {
                    let mut uart = serial::panic_registers();
                    uart.write(b"power: cpu");
                    uart.dec(u64::from(cpu));
                    uart.write(b"'s AFFINITY_INFO answered ");
                    say_code(&mut uart, refusal);
                    uart.write(b"; SYSTEM_OFF regardless\n");
                    break;
                }
            }
        }
    }
    let refusal = psci.system_off();
    let mut uart = serial::panic_registers();
    uart.write(b"power: SYSTEM_OFF answered ");
    say_code(&mut uart, refusal);
    uart.write(b"; holding\n");
    drop(uart);
    cpu::halt()
}

/// This CPU's part in [`off`], on the SGI that asked for it: its timer
/// stopped, since a core `CPU_OFF` turned off may take no interrupt (DEN0022
/// §5.5.2), then `CPU_OFF`. The SGI is never ended, as `SGI_HALT` is not, so
/// a refusal leaves this CPU halted with nothing signalled to it, and [`off`]
/// names it still on.
pub(super) fn cpu_off() -> ! {
    irqchip::stop_timer();
    if crate::actuator::power_off_spares_the_last_two() && percpu::cpu_id() + 2 >= crate::smp::cpu_count() {
        cpu::halt()
    }
    let Some(psci) = psci::conduit() else {
        unreachable!("power: SGI_OFF is raised only by `off`, which holds a conduit")
    };
    let refusal = psci.cpu_off();
    let mut uart = serial::panic_registers();
    uart.write(b"power: cpu");
    uart.dec(u64::from(percpu::cpu_id()));
    uart.write(b"'s CPU_OFF answered ");
    say_code(&mut uart, refusal);
    uart.write(b"\n");
    drop(uart);
    cpu::halt()
}

/// The code PSCI refused a call with, signed.
fn say_code(uart: &mut serial::PanicUart, code: i32) {
    if code < 0 {
        uart.write(b"-");
    }
    uart.dec(u64::from(code.unsigned_abs()));
}
