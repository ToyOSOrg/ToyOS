//! Why a machine-wide translation invalidation was asked for: the census every
//! architecture's shootdown (`arch::tlb`) counts its callers by.

/// Which path issued a shootdown, so the census names who pays: `Dlopen` (a
/// `Shared` window or rollback unmap), `Pcid` (pool reclaim), `Mmio`, `Unmap`
/// (`Unmapped::drop`), `Pipe`, `Staged` (the ack-delay actuator), `Bench`
/// (`arch::tlb::bench`'s own, so a measured shootdown is never counted as one
/// a path in this kernel needed), `Stop` (the power-off's, for the answer from
/// Ring 0 every other CPU owes it, not for a flush).
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Origin {
    Dlopen,
    Pcid,
    Mmio,
    Unmap,
    Pipe,
    #[cfg_attr(not(feature = "test-actuators"), allow(dead_code))]
    Staged,
    #[cfg_attr(not(feature = "boot-actuators"), allow(dead_code))]
    Bench,
    // Ports are x86-64's alone, so AArch64 builds a variant it never issues.
    #[allow(dead_code)]
    Stop,
}

impl Origin {
    pub const COUNT: usize = 8;
    /// Order matches the variants; `tests/toyos.rs`'s `irq_census_conservation` reads the line back.
    pub const NAMES: [&'static str; Self::COUNT] =
        ["dlopen", "pcid", "mmio", "unmap", "pipe", "staged", "bench", "stop"];
}
