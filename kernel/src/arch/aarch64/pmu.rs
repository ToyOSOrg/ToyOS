//! The performance monitor's overflow, as the hard-lockup detector's sample
//! source. On AArch64 that is `PMCCNTR_EL0` overflowing into a pseudo-NMI —
//! an interrupt the GICv3 delivers at a priority `DAIF.I` does not mask —
//! which needs the interrupt controller of the port's stage 4.


/// Start this CPU's counter overflowing into an NMI every `period` counter ticks.
pub fn arm(_period: u64) -> bool {
    owed!("the PMU overflow NMI", "stage 4")
}

/// Whether this CPU's counter is the reason this NMI arrived.
pub fn overflowed() -> bool {
    owed!("the PMU overflow NMI", "stage 4")
}

/// After an overflow's sample: clear it and reload.
pub fn rearm(_period: u64) {
    owed!("the PMU overflow NMI", "stage 4")
}
