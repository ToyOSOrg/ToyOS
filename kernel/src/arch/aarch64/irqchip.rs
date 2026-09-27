//! The interrupt controller: a GICv3 — distributor, one redistributor per CPU
//! and an ITS for MSIs — the port's stage 4. Until then no interrupt is
//! delivered, and every way of raising one is owed.


/// Wake `cpu` so it runs a scheduler pass: an SGI.
pub fn kick_cpu(_cpu: u32) {
    owed!("the interrupt controller", "stage 4")
}

pub fn kick_all_but_self() {
    owed!("the interrupt controller", "stage 4")
}

/// Raise `vector` on this CPU.
pub fn send_self(_vector: u8) {
    owed!("the interrupt controller", "stage 4")
}

/// A pseudo-NMI to `cpu`.
pub fn send_nmi(_cpu: u32) {
    owed!("the interrupt controller", "stage 4")
}

/// This CPU's timer, armed to fire within `nanos`.
pub fn arm_within(_nanos: u64) {
    owed!("the timer", "stage 4")
}

/// Nothing to stop: before stage 5 the boot CPU is the only one running.
pub fn stop_other_cpus() {}
