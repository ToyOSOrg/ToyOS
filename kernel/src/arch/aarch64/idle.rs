//! AArch64's half of `crate::idle`: nothing deeper than `wfi` is declared
//! here, since no AArch64 machine hands the kernel a declaration
//! (`issues/aarch64-cpus-idle-only-in-wfi.md`), so every CPU reports a timer
//! that does not run in a deep state and keeps C1.

use toyos_userbound::idle::{CpuIdle, Entry};

pub fn facts() -> CpuIdle {
    CpuIdle::default()
}

/// Never called: [`facts`] admits no state.
pub fn enter(_entry: Entry) {
    unreachable!("AArch64 admits no idle state deeper than wfi")
}

/// No processor UID names a CPU here: nothing declares one.
pub fn cpu_of(_uid: u32) -> Option<u32> {
    None
}
