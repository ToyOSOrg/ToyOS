//! Reset and power-off: PSCI's `SYSTEM_RESET` and `SYSTEM_OFF` (Arm DEN0022
//! §5.12, §5.10), through the conduit [`super::psci`] kept. A machine without
//! PSCI has neither, and holds where either is asked for.

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
/// wedge calls this, so it takes no lock and makes no other call.
pub fn reset() -> ! {
    if let Some(psci) = psci::conduit() {
        psci.system_reset();
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
        while !matches!(psci.affinity_info(mpidr), Ok(psci::Affinity::Off)) {
            if deadline.reached(crate::clock::now()) {
                // Raw: the console's last drain is behind this call.
                serial::panic_raw(b"power: cpu");
                serial::panic_raw_dec(u64::from(cpu));
                serial::panic_raw(b" is not off by PSCI's answer inside the budget; SYSTEM_OFF regardless\n");
                break;
            }
            core::hint::spin_loop();
        }
    }
    psci.system_off();
    cpu::halt()
}

/// This CPU's part in [`off`], on the SGI that asked for it: its timer
/// stopped, since a core `CPU_OFF` turned off may take no interrupt (DEN0022
/// §5.5.2), then `CPU_OFF`. A refusal leaves it halted, and [`off`] names it
/// still on.
pub(super) fn cpu_off() -> ! {
    irqchip::end(irqchip::SGI_OFF);
    irqchip::stop_timer();
    if let Some(psci) = psci::conduit() {
        psci.cpu_off();
    }
    cpu::halt()
}
