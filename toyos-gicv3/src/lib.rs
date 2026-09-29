//! The GICv3's pure decisions (GIC architecture specification IHI 0069H):
//! how a CPU's affinity is packed, how an SGI names the CPU it is raised on,
//! and how a redistributor region is walked to the frame of one CPU. The
//! kernel's `arch::aarch64::irqchip` reads the registers; everything here is
//! arithmetic on what it read, so the host can ask it about CPUs the boot CPU
//! alone never exercises.

#![no_std]

/// One 64 KiB register frame. A redistributor is two — `RD_base`, then
/// `SGI_base` — and two more for virtual LPIs where `GICR_TYPER.VLPIS` says so.
pub const FRAME: u64 = 0x1_0000;

/// `GICR_TYPER.VLPIS`: this redistributor has the two virtual-LPI frames.
const TYPER_VLPIS: u64 = 1 << 1;
/// `GICR_TYPER.Last`: the last redistributor in its region.
const TYPER_LAST: u64 = 1 << 4;

/// `MPIDR_EL1`'s four affinity fields packed into 32 bits,
/// `Aff3:Aff2:Aff1:Aff0`, as `GICR_TYPER` carries them in its top word.
pub const fn packed_affinity(mpidr: u64) -> u32 {
    ((mpidr & 0xFF_FFFF) | ((mpidr >> 8) & 0xFF00_0000)) as u32
}

/// `ICC_SGI1R_EL1` raising SGI `intid` on the one CPU whose packed affinity is
/// `target`: `Aff3`, `Aff2` and `Aff1` name its cluster, `RS` the range of
/// sixteen `Aff0` values it falls in, and the target list its one bit there.
pub const fn sgi1r(intid: u32, target: u32) -> u64 {
    assert!(intid < 16, "an SGI's INTID is below 16");
    let aff0 = (target & 0xFF) as u64;
    let aff1 = (target >> 8 & 0xFF) as u64;
    let aff2 = (target >> 16 & 0xFF) as u64;
    let aff3 = (target >> 24) as u64;
    aff3 << 48 | (aff0 >> 4) << 44 | aff2 << 32 | (intid as u64) << 24 | aff1 << 16 | 1 << (aff0 & 0xF)
}

/// The offset, in a redistributor region `length` bytes long, of the
/// redistributor whose affinity is `me`; `typer` reads `GICR_TYPER` of the
/// redistributor at an offset. The walk steps by each one's own frames and
/// stops at the one marked last.
pub fn find_redistributor(length: u64, me: u32, typer: impl Fn(u64) -> u64) -> Option<u64> {
    let mut offset = 0;
    while offset + 2 * FRAME <= length {
        let word = typer(offset);
        if (word >> 32) as u32 == me {
            return Some(offset);
        }
        if word & TYPER_LAST != 0 {
            return None;
        }
        offset += if word & TYPER_VLPIS != 0 { 4 * FRAME } else { 2 * FRAME };
    }
    None
}
