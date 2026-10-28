//! Protected memory regions (VT-d §11.4.8): while a unit's translation is off,
//! it blocks DMA to them, and a unit's own reads of its remapping structures
//! are exempt (§11.4.8.1, `EPM`). They hold while every unit is handed over,
//! so a unit with translation off passes no DMA to the memory they cover.
//!
//! The same regions go on every unit, as §11.4.8.1 requires for them to
//! protect anything. They avoid every RMRR, so firmware's own DMA to its
//! reserved memory runs on through the hand-over.

use alloc::vec::Vec;

use crate::log;
use crate::mm::Mmio;
use crate::time::{Duration, Tripwire};
use kernel::dmaguard::{self, Span};

/// Below 16 MiB an Intel host bridge may decode legacy ranges and the optional
/// ISA hole as MMIO (11th Generation Intel Core datasheet, 631122 §2.5.1), and
/// no protected region may cover MMIO (§11.4.8.1).
const LOW_FLOOR: u64 = 16 << 20;

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

/// Every unit's protected regions, programmed and not yet on.
pub struct Guard {
    units: Vec<(usize, Mmio)>,
}

/// Programs the same regions on every unit in `units`, each `(index, registers,
/// supported)`, or says why it cannot. `ram` is firmware's RAM, `avoid`
/// every RMRR, `top` the highest memory the identity domain maps.
pub fn program(units: &[(usize, Mmio, bool)], ram: &[(u64, u64)], avoid: &[(u64, u64)], top: u64) -> Option<Guard> {
    if let Some(&(index, _, _)) = units.iter().find(|(_, _, supported)| !supported) {
        log!(
            "iommu: unit{index} has no protected memory regions (CAP.PLMR or CAP.PHMR clear), so while \
             the units are handed over every function behind one reaches all of memory"
        );
        return None;
    }
    // The coarsest alignment any unit decodes, so one pair of regions suits them all.
    let mut low_align = 1u64;
    let mut high_align = 1u64;
    for &(index, regs, _) in units {
        regs.write_u32(PLMBASE_REG, u32::MAX);
        regs.write_u64(PHMBASE_REG, u64::MAX);
        let (low, high) = (regs.read_u32(PLMBASE_REG), regs.read_u64(PHMBASE_REG));
        if low == 0 || high == 0 {
            log!("iommu: unit{index} decodes no protected region base (PLMBASE={low:#x} PHMBASE={high:#x})");
            return None;
        }
        low_align = low_align.max(1 << low.trailing_zeros());
        high_align = high_align.max(1 << high.trailing_zeros());
    }
    let low = dmaguard::largest(ram, LOW_FLOOR, dmaguard::FOUR_GIB, avoid, low_align);
    let high = dmaguard::largest(ram, dmaguard::FOUR_GIB, top, avoid, high_align);
    log!(
        "iommu: protected memory for the hand-over on {} unit(s): low {} high {}, aligned to {low_align:#x} and {high_align:#x}",
        units.len(),
        Shown(low),
        Shown(high),
    );
    for &(_, regs, _) in units {
        // A limit below its base is a region switched off (§11.4.8.3, §11.4.8.5).
        let (base, limit) = low.map_or((low_align, 0), |s| (s.base, s.end - 1));
        regs.write_u32(PLMBASE_REG, base as u32);
        regs.write_u32(PLMLIMIT_REG, limit as u32);
        let (base, limit) = high.map_or((high_align, 0), |s| (s.base, s.end - 1));
        regs.write_u64(PHMBASE_REG, base);
        regs.write_u64(PHMLIMIT_REG, limit);
    }
    Some(Guard { units: units.iter().map(|&(index, regs, _)| (index, regs)).collect() })
}

impl Guard {
    pub fn arm(&self) {
        for &(index, regs) in &self.units {
            set(index, regs, true);
        }
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
