//! The performance monitor's overflow, as the hard-lockup detector's sample
//! source. On AArch64 that is `PMCCNTR_EL0` overflowing into a pseudo-NMI —
//! an interrupt at a priority `DAIF.I` does not mask, which needs every
//! interrupt mask in this kernel moved from `DAIF` to `ICC_PMR_EL1` — and no
//! stage of the port has taken that on. Only a boot under a deadline arms it,
//! and such a boot is refused here by name.

/// Start this CPU's counter overflowing into an NMI every `period` counter ticks.
pub fn arm(_period: u64) -> bool {
    owed!("the PMU overflow NMI", "no stage yet")
}

/// Whether this CPU's counter is the reason this NMI arrived.
pub fn overflowed() -> bool {
    owed!("the PMU overflow NMI", "no stage yet")
}

/// After an overflow's sample: clear it and reload.
pub fn rearm(_period: u64) {
    owed!("the PMU overflow NMI", "no stage yet")
}
