//! Other CPUs: PSCI `CPU_ON` for each GICC the MADT names, the port's stage 5.
//! Until then the boot CPU is the only one running, and the machine is never
//! released to the scheduler.


/// CPUs running: the boot CPU alone.
pub fn cpu_count() -> u32 {
    1
}

pub fn set_ready() {
    owed!("other CPUs", "stage 5")
}

/// Never: the boot ends before the machine is released.
pub fn is_ready() -> bool {
    false
}
