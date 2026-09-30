//! What the EL1 system registers hold on every CPU in this machine, and what
//! EL2 is left holding when the kernel is entered there. One declaration:
//! [`super::boot`]'s entry writes every register here whole, from these
//! constants, before the MMU is on; [`check`] reads the EL1 ones back and
//! refuses a CPU whose registers say anything else. Nothing else writes any
//! of them, bar [`ICC_SRE`].
//!
//! Field positions are Arm ARM K.a, chapter D24 (the register descriptions),
//! and the GIC architecture specification (IHI 0069H), chapter 12, for the
//! `ICC_` registers.

use core::sync::atomic::AtomicU64;

use crate::log;

/// `MAIR_EL1`: the three memory types the loader's tables and this kernel
/// map with, by `AttrIndx` — declared once, beside the descriptors that name them.
pub use toyos_bootmap::aarch64::MAIR;

/// `SCTLR_EL1`: MMU on (`M`), data and instruction caches on (`C`, `I`),
/// stack alignment checked at EL1 and EL0 (`SA`, `SA0`), AArch32 EL0's `IT`
/// and `SETEND` disabled (`ITD`, `SED` — RES1 on a CPU with no AArch32 EL0,
/// and nothing this kernel runs is AArch32), over the bits Armv8.0 makes
/// RES1 (29, 28, 23, 22, 20, 11). `WXN` stays clear: the direct map the
/// kernel runs from is writable and executable both. Little-endian at both
/// levels; EL0's cache maintenance and `WFI`/`WFE` trap.
pub const SCTLR: u64 = SCTLR_RES1 | 1 << 0 | 1 << 2 | 1 << 3 | 1 << 4 | 1 << 7 | 1 << 8 | 1 << 12;

/// `SCTLR_EL1` with the MMU and caches off — [`SCTLR`] less `M`, `C`, `I`,
/// `SA` and `SA0` — the only other value the entry writes: what a CPU entered at EL1 under firmware's tables is put through
/// before its translation registers change.
pub const SCTLR_MMU_OFF: u64 = SCTLR_RES1 | 1 << 7 | 1 << 8;

const SCTLR_RES1: u64 = 1 << 29 | 1 << 28 | 1 << 23 | 1 << 22 | 1 << 20 | 1 << 11;

/// `TCR_EL1` but for `IPS`: 48-bit regions from both tables (`T0SZ` = `T1SZ` =
/// 16), 4 KiB granules (`TG0` = 0, `TG1` = 2), walks inner-shareable and
/// write-back write-allocate cacheable, `TTBR0_EL1` naming the ASID (`A1`
/// clear), and 16-bit ASIDs (`AS`), which [`check`] requires the CPU to have.
pub const TCR: u64 =
    16 | 1 << 8 | 1 << 10 | 3 << 12 | 16 << 16 | 1 << 24 | 1 << 26 | 3 << 28 | 2 << 30 | 1 << 36;

/// `TCR_EL1.IPS`'s position: the output address size, taken from
/// `ID_AA64MMFR0_EL1.PARange` (the same encoding), because a larger `IPS`
/// than the CPU implements is a reserved value.
pub const TCR_IPS_SHIFT: u64 = 32;

/// `CPACR_EL1`: `FPEN` = 0b11, so FP and SIMD trap at neither EL1 nor EL0. The
/// kernel is built soft-float and touches them only to save and restore a
/// thread's registers; `ZEN` and `SMEN` stay clear, so SVE and SME trap
/// everywhere.
pub const CPACR: u64 = 0b11 << 20;

/// `CNTKCTL_EL1`: `EL0VCTEN` alone, so EL0 reads the virtual count and its
/// frequency — the clock page's counter (`toyos_abi::arch::counter`) — and
/// nothing else of the generic timer.
pub const CNTKCTL: u64 = 1 << 1;

/// `ICC_SRE_EL1`: the GICv3 CPU interface through system registers (`SRE`),
/// with FIQ and IRQ bypass disabled (`DFB`, `DIB`). Written and read back by
/// `super::irqchip::init`, not the entry: on a CPU with no such interface the
/// access is an undefined instruction, which this kernel's vectors report and
/// firmware's, still installed at the entry, do not.
pub const ICC_SRE: u64 = 0b111;

/// `HCR_EL2` when entered at EL2: `RW`, so EL1 is AArch64, and nothing else —
/// no stage-2 translation, no trap, `E2H` clear, and `IMO`/`FMO` clear, so a
/// physical interrupt is taken at EL1.
pub const HCR_EL2: u64 = 1 << 31;

/// `CNTHCTL_EL2` when entered at EL2: `EL1PCTEN` and `EL1PCEN`, and every
/// other field clear — among them FEAT_ECV's `EL1TVT` and `EL1TVCT`, which
/// reset UNKNOWN and would trap EL1's virtual timer and count to EL2.
pub const CNTHCTL_EL2: u64 = 1 << 1 | 1 << 0;

/// `CPTR_EL2` when entered at EL2 (`E2H` clear): its RES1 bits (13, 12, 9:0)
/// and `TFP` clear, so FP is `CPACR_EL1`'s decision alone.
pub const CPTR_EL2: u64 = 0x33FF;

/// `ICC_SRE_EL2` when entered at EL2: [`ICC_SRE`]'s three bits at EL2, and
/// `Enable`, without which EL1's own `ICC_SRE_EL1` traps to EL2.
pub const ICC_SRE_EL2: u64 = 0b1111;

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
/// holds anything else; then say what it holds, and that it was entered at `el`.
pub fn check(el: u64) {
    // `ID_AA64MMFR0_EL1.ASIDBits` = 0b0010: without 16-bit ASIDs `TCR_EL1.AS`
    // is RES0, and the read-back below would name the register, not the reason.
    let asid_bits = read!("id_aa64mmfr0_el1") >> 4 & 0xF;
    assert_eq!(asid_bits, 0b0010, "control registers: this CPU has 8-bit ASIDs, and the declaration's TCR_EL1.AS needs 16");
    let declared = [
        ("SCTLR_EL1", read!("sctlr_el1"), SCTLR),
        ("TCR_EL1", read!("tcr_el1"), tcr()),
        ("MAIR_EL1", read!("mair_el1"), MAIR),
        ("CPACR_EL1", read!("cpacr_el1"), CPACR),
        ("CNTKCTL_EL1", read!("cntkctl_el1"), CNTKCTL),
    ];
    for (name, live, value) in declared {
        assert_eq!(live, value, "control registers: {name} holds {live:#x}, and the declaration says {value:#x}");
    }
    // What the drop from EL2 left, or the EL1 entry kept: EL1, on `SP_EL1`.
    let (now, spsel) = (read!("CurrentEL") >> 2 & 3, read!("SPSel") & 1);
    assert_eq!((now, spsel), (1, 1), "control registers: running at EL{now} on SP_EL{spsel}, not EL1 on SP_EL1");
    log!(
        "control registers: SCTLR_EL1={SCTLR:#x} TCR_EL1={:#x} MAIR_EL1={:#x} CPACR_EL1={CPACR:#x} \
         CNTKCTL_EL1={CNTKCTL:#x}, as declared; entered at EL{el}{}",
        tcr(),
        MAIR,
        if el == 2 {
            ", HCR_EL2 read back as declared, CNTHCTL_EL2/CNTVOFF_EL2/CPTR_EL2/ICC_SRE_EL2 written, and dropped to EL1"
        } else {
            ""
        },
    );
}
