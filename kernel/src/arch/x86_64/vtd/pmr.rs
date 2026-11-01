//! Protected memory regions (VT-d §11.4.8). On a unit of major version 2 or
//! later, and on every one since Skylake, they block DMA to the memory they
//! cover whether or not the unit translates, while the unit's own reads of its
//! remapping structures are exempt (§11.4.8.1, `EPM`). So holding them from
//! before the first unit's translation goes off until the last unit translates
//! keeps the hand-over's tables, and this kernel, out of every device's reach.
//!
//! The same regions go on every unit, as §11.4.8.1 requires for them to
//! protect anything. They avoid every RMRR, so firmware's DMA to its reserved
//! memory runs on through the hand-over.

use alloc::vec::Vec;

use crate::log;
use crate::mm::Mmio;
use crate::time::{Duration, Tripwire};
use kernel::dmaguard::{self, Span};

const PMEN_REG: u64 = 0x64;
const PLMBASE_REG: u64 = 0x68;
const PLMLIMIT_REG: u64 = 0x6C;
const PHMBASE_REG: u64 = 0x70;
const PHMLIMIT_REG: u64 = 0x78;

/// `PMEN.EPM`, and `PMEN.PRS` reporting it in force.
const ENABLE: u32 = 1 << 31;
const STATUS: u32 = 1 << 0;

const STATUS_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "a unit whose protected regions never report their state is one whose reach nothing can state",
);

/// Protected memory firmware left on, off: its bases and limits may not be
/// rewritten while it is (§11.4.8.2-§11.4.8.5), and left on it would block
/// DMA after the hand-over.
pub fn hand_over(index: usize, regs: Mmio) {
    if regs.read_u32(PMEN_REG) & STATUS != 0 {
        log!("iommu: unit{index} was handed over with protected memory on; it goes off first");
        set(index, regs, false);
    }
}

/// One unit the hand-over would guard.
pub struct Candidate {
    pub index: usize,
    pub regs: Mmio,
    /// `CAP.PLMR` and `CAP.PHMR` both.
    pub regions: bool,
    /// It invalidates through its registers, which the window needs.
    pub registers: bool,
}

/// Every unit's protected regions, programmed and not yet on.
pub struct Guard {
    units: Vec<(usize, Mmio)>,
}

/// The same regions programmed on every unit, covering every `cover` range,
/// or `None` with the reason logged: the hand-over is then unguarded. `ram` is
/// what firmware's map says the OS owns, `avoid` every RMRR, `top` the end of
/// what the identity domain maps.
pub fn program(
    units: &[Candidate],
    described: usize,
    ram: &[(u64, u64)],
    avoid: &[(u64, u64)],
    cover: &[(u64, u64)],
    top: u64,
) -> Option<Guard> {
    fn unguarded(why: core::fmt::Arguments) -> Option<Guard> {
        log!("iommu: the hand-over is unguarded: {why}, so every function reaches all of memory while the units are programmed");
        None
    }
    if units.len() < described {
        return unguarded(format_args!(
            "{} of {described} units are programmed, and the regions protect only where every unit holds them",
            units.len()
        ));
    }
    if let Some(unit) = units.iter().find(|u| !u.regions) {
        return unguarded(format_args!("unit{} has no protected memory regions (CAP.PLMR or CAP.PHMR clear)", unit.index));
    }
    if let Some(unit) = units.iter().find(|u| !u.registers) {
        return unguarded(format_args!(
            "unit{} cannot invalidate through its registers, and its queue's memory may lie under the regions",
            unit.index
        ));
    }
    // The coarsest alignment any base or limit decodes (§11.4.8.2-§11.4.8.5),
    // so one pair of regions suits every unit.
    let mut low_align = 1u64;
    let mut high_align = 1u64;
    for unit in units {
        let regs = unit.regs;
        regs.write_u32(PLMBASE_REG, u32::MAX);
        regs.write_u32(PLMLIMIT_REG, u32::MAX);
        regs.write_u64(PHMBASE_REG, u64::MAX);
        regs.write_u64(PHMLIMIT_REG, u64::MAX);
        let low = [regs.read_u32(PLMBASE_REG), regs.read_u32(PLMLIMIT_REG)];
        let high = [regs.read_u64(PHMBASE_REG), regs.read_u64(PHMLIMIT_REG)];
        if low.contains(&0) || high.contains(&0) {
            return unguarded(format_args!(
                "unit{} decodes no protected region (PLMBASE/LIMIT={low:x?} PHMBASE/LIMIT={high:x?})",
                unit.index
            ));
        }
        for bits in low {
            low_align = low_align.max(1 << bits.trailing_zeros());
        }
        for bits in high {
            high_align = high_align.max(1 << bits.trailing_zeros());
        }
    }
    let low = dmaguard::largest(ram, 0, dmaguard::FOUR_GIB, avoid, low_align, cover);
    let high = dmaguard::largest(ram, dmaguard::FOUR_GIB, top, avoid, high_align, cover);
    log!(
        "iommu: protected memory for the hand-over on {} unit(s): low {} high {}, aligned to {low_align:#x} and {high_align:#x}",
        units.len(),
        Shown(low),
        Shown(high),
    );
    for &range in cover {
        if !low.is_some_and(|s| s.holds(range)) && !high.is_some_and(|s| s.holds(range)) {
            return unguarded(format_args!("{:#x}..{:#x}, which it must protect, lies outside both regions", range.0, range.1));
        }
    }
    log!("iommu: the regions cover the hand-over's tables and this kernel: {:x?}", cover);
    for unit in units {
        let regs = unit.regs;
        // A limit below its base is a region switched off (§11.4.8.3, §11.4.8.5).
        let (base, limit) = low.map_or((low_align, 0), |s| (s.base, s.end - 1));
        regs.write_u32(PLMBASE_REG, base as u32);
        regs.write_u32(PLMLIMIT_REG, limit as u32);
        let (base, limit) = high.map_or((high_align, 0), |s| (s.base, s.end - 1));
        regs.write_u64(PHMBASE_REG, base);
        regs.write_u64(PHMLIMIT_REG, limit);
    }
    Some(Guard { units: units.iter().map(|u| (u.index, u.regs)).collect() })
}

impl Guard {
    pub fn arm(&self) {
        for &(index, regs) in &self.units {
            set(index, regs, true);
        }
        log!("iommu: protected memory on for {} unit(s)", self.units.len());
    }

    pub fn release(self) {
        for &(index, regs) in &self.units {
            set(index, regs, false);
        }
        log!("iommu: protected memory released on {} unit(s)", self.units.len());
    }
}

/// §11.4.8.1: `EPM` written, then `PRS` waited for before it changes again.
fn set(index: usize, regs: Mmio, on: bool) {
    regs.write_u32(PMEN_REG, if on { ENABLE } else { 0 });
    let deadline = crate::clock::nanos_since_boot() + STATUS_TIMEOUT.nanos();
    while (regs.read_u32(PMEN_REG) & STATUS != 0) != on {
        assert!(
            crate::clock::nanos_since_boot() < deadline,
            "iommu: unit{index}'s protected regions never reported {}, PMEN={:#010x}",
            if on { "on" } else { "off" },
            regs.read_u32(PMEN_REG)
        );
        core::hint::spin_loop();
    }
}

struct Shown(Option<Span>);

impl core::fmt::Display for Shown {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Some(s) => write!(f, "{:#x}..{:#x}", s.base, s.end),
            None => f.write_str("none"),
        }
    }
}
