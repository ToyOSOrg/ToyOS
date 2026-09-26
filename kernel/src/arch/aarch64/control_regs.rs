//! What the EL1 system registers hold on every CPU in this machine, and what
//! EL2 is left holding when the kernel is entered there. One declaration:
//! [`super::boot`]'s entry writes every register here whole, from these
//! constants, before the MMU is on; [`check`] reads the EL1 ones back and
//! refuses a CPU whose registers say anything else. Nothing else writes any
//! of them.
//!
//! Field positions are Arm ARM K.a, chapter D24 (the register descriptions).

use core::sync::atomic::{AtomicU64, Ordering};

use crate::log;

/// `MAIR_EL1`: the three memory types the loader's tables and this kernel
/// map with, by `AttrIndx` — declared once, beside the descriptors that name them.
pub use toyos_bootmap::aarch64::MAIR;

/// `SCTLR_EL1`: MMU on (`M`), data and instruction caches on (`C`, `I`),
/// stack alignment checked at EL1 and EL0 (`SA`, `SA0`), AArch32 EL0's `IT`
/// and `SETEND` disabled (`ITD`, `SED` — RES1 on a CPU with no AArch32 EL0,
/// and nothing this kernel runs is AArch32), over the bits Armv8.0 makes
/// RES1 (29, 28, 23, 22, 20, 11). `WXN` stays clear: the
/// loader's blocks are writable and executable both until the kernel owns
/// its tables. Little-endian at both levels; EL0's cache maintenance and
/// `WFI`/`WFE` trap.
pub const SCTLR: u64 = SCTLR_RES1 | 1 << 0 | 1 << 2 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 8 | 1 << 12;

/// `SCTLR_EL1` with the MMU and caches off — [`SCTLR`] less `M`, `C`, `I`,
/// `SA` and `SA0` — the only other value the entry writes: what a CPU entered at EL1 under firmware's tables is put through
/// before its translation registers change.
pub const SCTLR_MMU_OFF: u64 = SCTLR_RES1 | 1 << 7 | 1 << 8;

const SCTLR_RES1: u64 = 1 << 29 | 1 << 28 | 1 << 23 | 1 << 22 | 1 << 20 | 1 << 11;

/// `TCR_EL1` but for `IPS`: 48-bit regions from both tables (`T0SZ` = `T1SZ` =
/// 16), 4 KiB granules (`TG0` = 0, `TG1` = 2), walks inner-shareable and
/// write-back write-allocate cacheable, 8-bit ASIDs.
pub const TCR: u64 = 16 | 1 << 8 | 1 << 10 | 3 << 12 | 16 << 16 | 1 << 24 | 1 << 26 | 3 << 28 | 2 << 30;

/// `TCR_EL1.IPS`'s position: the output address size, taken from
/// `ID_AA64MMFR0_EL1.PARange` (the same encoding), because a larger `IPS`
/// than the CPU implements is a reserved value.
pub const TCR_IPS_SHIFT: u64 = 32;

/// `CPACR_EL1`: `FPEN` = 0, so FP and SIMD trap at EL1 and EL0. The kernel is
/// built soft-float and uses neither; user mode's are stage 7's.
pub const CPACR: u64 = 0;

/// `HCR_EL2` when entered at EL2: `RW`, so EL1 is AArch64, and nothing else —
/// no stage-2 translation, no trap, `E2H` clear.
pub const HCR_EL2: u64 = 1 << 31;

/// `CNTHCTL_EL2` when entered at EL2: `EL1PCTEN` and `EL1PCEN`, so EL1 reads the
/// physical counter and programs its timer without trapping.
pub const CNTHCTL_EL2: u64 = 1 << 1 | 1 << 0;

/// `CPTR_EL2` when entered at EL2 (`E2H` clear): its RES1 bits (13, 12, 9:0)
/// and `TFP` clear, so FP is `CPACR_EL1`'s decision alone.
pub const CPTR_EL2: u64 = 0x33FF;

/// `SPSR_EL2` for the drop: EL1 on `SP_EL1` (`M` = 0b0101), `D`, `A`, `I`, `F` masked.
pub const SPSR_EL2_TO_EL1: u64 = 0x3C5;

/// The exception level the loader entered the kernel at, as the entry recorded it.
pub static ENTRY_EL: AtomicU64 = AtomicU64::new(0);

/// One EL1 system register, read.
macro_rules! read {
    ($reg:literal) => {{
        let value: u64;
        // SAFETY: reads an EL1 system register, which EL1 may always read.
        unsafe { core::arch::asm!(concat!("mrs {}, ", $reg), out(reg) value, options(nomem, nostack, preserves_flags)) };
        value
    }};
}

/// `TCR_EL1` whole, as this CPU's physical address range makes it.
pub fn tcr() -> u64 {
    TCR | (read!("id_aa64mmfr0_el1") & 0xF) << TCR_IPS_SHIFT
}

/// Read every EL1 register the declaration names back, and refuse a CPU that
/// holds anything else; then say what it holds.
pub fn check() {
    let declared = [
        ("SCTLR_EL1", read!("sctlr_el1"), SCTLR),
        ("TCR_EL1", read!("tcr_el1"), tcr()),
        ("MAIR_EL1", read!("mair_el1"), MAIR),
        ("CPACR_EL1", read!("cpacr_el1"), CPACR),
    ];
    for (name, live, value) in declared {
        assert_eq!(live, value, "control registers: {name} holds {live:#x}, and the declaration says {value:#x}");
    }
    let el = ENTRY_EL.load(Ordering::Relaxed);
    log!(
        "control registers: SCTLR_EL1={SCTLR:#x} TCR_EL1={:#x} MAIR_EL1={:#x} CPACR_EL1={CPACR:#x}, \
         as declared; entered at EL{el}{}",
        tcr(),
        MAIR,
        if el == 2 { ", HCR_EL2/CNTHCTL_EL2/CPTR_EL2 written as declared and dropped to EL1" } else { "" },
    );
}
