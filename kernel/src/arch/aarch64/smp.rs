//! Other CPUs: PSCI `CPU_ON` for each GICC the MADT names, the port's stage 5.
//! Until then the boot CPU is the only one the roster holds.

use crate::smp_roster::Roster;

static ROSTER: Roster = Roster::new();

pub fn cpu_count() -> u32 {
    ROSTER.count()
}

/// Release the machine to the scheduler: with one CPU, nothing waits on it.
pub fn set_ready() {
    ROSTER.release();
}

/// Whether [`set_ready`] has run.
pub fn is_ready() -> bool {
    ROSTER.released()
}
