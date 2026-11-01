//! AArch64's half of `crate::idle`. Nothing declares an idle state on
//! AArch64 (`issues/aarch64-cpus-idle-only-in-wfi.md`): [`cpu_of`] names no
//! CPU, so no declaration is checked against [`facts`], and every CPU waits in
//! `wfi`.

use toyos_userbound::idle::{CpuIdle, Entry};

pub fn facts() -> CpuIdle {
    CpuIdle::default()
}

/// Never called: no declaration is kept.
pub fn enter(_entry: Entry) {
    unreachable!("AArch64 keeps no idle state deeper than wfi")
}

/// No processor UID names a CPU here: nothing declares one.
pub fn cpu_of(_uid: u32) -> Option<u32> {
    None
}
