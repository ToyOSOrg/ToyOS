//! Reset and power-off: PSCI's `SYSTEM_RESET` and `SYSTEM_OFF` (Arm DEN0022
//! §5.12, §5.10), through the conduit [`super::psci`] kept. A machine without
//! PSCI has neither, and holds where either is asked for.
//!
//! Every line here goes to the UART raw: it takes no lock, and nothing drains
//! the log ring after any of these calls.

use super::{cpu, irqchip, percpu, psci};
use crate::drivers::serial;
use crate::time::{Budget, Deadline, Duration};

/// How long the other CPUs get to be off by PSCI's answer before this one
/// powers the machine off anyway.
const OTHERS_OFF: Budget = Budget::of(
    Duration::from_millis(100),
    "the machine powers off with the CPUs PSCI still answers on, each named on the console",
);

pub fn can_reset() -> bool {
    psci::conduit().is_some()
}

/// `SYSTEM_RESET`, which asks nothing of the other CPUs (DEN0022 §5.12.2): a
/// wedge calls this, so it takes no lock.
pub fn reset() -> ! {
    if let Some(psci) = psci::conduit() {
        let refusal = psci.system_reset();
        serial::panic_raw(b"power: SYSTEM_RESET answered ");
        say_code(refusal);
        serial::panic_raw(b"; holding\n");
    }
    cpu::halt()
}

/// `SYSTEM_OFF`, once every other CPU the roster holds has turned itself off
/// with `CPU_OFF` and PSCI answers it off: the caller puts every core in a
/// known state first, and this is DEN0022 §5.10.3's own way to.
pub fn off() -> ! {
    cpu::disable_interrupts();
    let Some(psci) = psci::conduit() else { cpu::halt() };
    irqchip::off_all_but_self();
    let deadline = Deadline::at(crate::clock::now() + OTHERS_OFF.duration());
    let me = percpu::cpu_id();
    for cpu in (0..crate::smp::cpu_count()).filter(|&cpu| cpu != me) {
        let mpidr = toyos_gicv3::unpacked_affinity(crate::smp::hardware_id(cpu));
        loop {
            match psci.affinity_info(mpidr) {
                Ok(psci::Affinity::Off) => break,
                Ok(_) if !deadline.reached(crate::clock::now()) => core::hint::spin_loop(),
                Ok(_) => {
                    serial::panic_raw(b"power: cpu");
                    serial::panic_raw_dec(u64::from(cpu));
                    serial::panic_raw(b" is not off by PSCI's answer inside the budget; SYSTEM_OFF regardless\n");
                    break;
                }
                Err(refusal) => {
                    serial::panic_raw(b"power: cpu");
                    serial::panic_raw_dec(u64::from(cpu));
                    serial::panic_raw(b"'s AFFINITY_INFO answered ");
                    say_code(refusal);
                    serial::panic_raw(b"; SYSTEM_OFF regardless\n");
                    break;
                }
            }
        }
    }
    let refusal = psci.system_off();
    serial::panic_raw(b"power: SYSTEM_OFF answered ");
    say_code(refusal);
    serial::panic_raw(b"; holding\n");
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
    serial::panic_raw(b"power: cpu");
    serial::panic_raw_dec(u64::from(percpu::cpu_id()));
    serial::panic_raw(b"'s CPU_OFF answered ");
    say_code(refusal);
    serial::panic_raw(b"\n");
    cpu::halt()
}

/// The code PSCI refused a call with, signed.
fn say_code(refusal: psci::Error) {
    let code = refusal.code();
    if code < 0 {
        serial::panic_raw(b"-");
    }
    serial::panic_raw_dec(u64::from(code.unsigned_abs()));
}
