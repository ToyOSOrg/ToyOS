//! Why a machine-wide translation invalidation was asked for, the census every
//! architecture's shootdown (`arch::tlb`) counts its callers by, and which half
//! of the address space it answers for.

/// Which path issued a shootdown, so the census names who pays: `Pcid` (pool
/// reclaim), `Mmio`, `Unmap` (a user space's leaves taken away), `Staged` (a test actuator's), `Bench`
/// (`arch::tlb::bench`'s own, so a measured shootdown is never counted as one
/// a path in this kernel needed), `Stop` (the power-off's, for the answer from
/// Ring 0 every other CPU owes it, not for a flush).
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Origin {
    Pcid,
    Mmio,
    Unmap,
    #[cfg_attr(not(feature = "test-actuators"), allow(dead_code))]
    Staged,
    #[cfg_attr(not(feature = "boot-actuators"), allow(dead_code))]
    Bench,
    // Ports are x86-64's alone, so AArch64 builds a variant it never issues.
    #[allow(dead_code)]
    Stop,
}

/// Which half of every address space a shootdown's write changed: x86-64's
/// kernel-half leaves are global, so only a change there asks a flush that
/// drops global translations.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Half {
    User,
    // The kernel half is global on x86-64 alone, so AArch64 builds a variant it never issues.
    #[allow(dead_code)]
    Kernel,
}

impl Origin {
    pub const COUNT: usize = 6;
    /// Order matches the variants; `tests/toyos.rs`'s `irq_census_conservation` reads the line back.
    pub const NAMES: [&'static str; Self::COUNT] =
        ["pcid", "mmio", "unmap", "staged", "bench", "stop"];
}
