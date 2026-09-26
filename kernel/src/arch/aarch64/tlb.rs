//! TLB invalidation across CPUs. AArch64 broadcasts it in hardware (`TLBI
//! …IS`), so the shootdown this interface stands for is a local instruction
//! plus `DSB ISH` once the kernel owns its page tables, the port's stage 4.


pub use crate::invalidation::Origin;

pub fn log_census() {
    owed!("TLB invalidation", "stage 4")
}

pub fn shootdown(_origin: Origin) {
    owed!("TLB invalidation", "stage 4")
}

pub fn poll() {
    owed!("TLB invalidation", "stage 4")
}

#[cfg(feature = "boot-actuators")]
pub fn bench() {
    owed!("TLB invalidation", "stage 4")
}

#[cfg(feature = "test-actuators")]
pub fn debug_arm_ack_delay(_nanos: u64) -> u64 {
    owed!("TLB invalidation", "stage 4")
}

#[cfg(feature = "test-actuators")]
pub fn debug_disarm_ack_delay() -> u64 {
    owed!("TLB invalidation", "stage 4")
}
