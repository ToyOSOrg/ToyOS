//! Intel's TCO watchdog: its register block, and where a chipset keeps it.
//!
//! The block's layout is one thing across the generations this kernel targets;
//! **where its base comes from is not**, so that is a table keyed by PCI id
//! with a refusal for every machine not in it. A guess would be an I/O write to
//! whatever else lives at the port. Pure: the kernel supplies the config reads
//! and the port writes.

#![no_std]
#![forbid(unsafe_code)]

/// Offsets from the block's base port.
pub const TCO_RLD: u16 = 0x00;
pub const TCO1_STS: u16 = 0x04;
pub const TCO1_CNT: u16 = 0x08;
pub const TCO_TMR: u16 = 0x12;

/// `TCO1_CNT` bit 11: set halts the timer. `TCO1_STS` bit 3: it expired.
pub const TCO_TMR_HLT: u16 = 1 << 11;
pub const TCO_TIMEOUT: u16 = 1 << 3;

/// `TCO1_CNT` declared whole, never a bit set into what was there: it also selects an interrupt this kernel wants none of.
pub const TCO1_CNT_RUN: u16 = 0;
pub const TCO1_CNT_HALT: u16 = TCO_TMR_HLT;

/// `TCO_TMR` is ten bits, the chipset ignores 0 and 1, a tick is 600 ms, and
/// the first expiry only latches `TCO_TIMEOUT` — so a bound is two of them.
const TMR_MAX: u64 = 0x3ff;
const TMR_MIN: u64 = 2;
const TICK_MS: u64 = 600;
const EXPIRIES: u64 = 2;

/// The `TCO_TMR` whose expiries reach `bound_ms`, rounded down so the reset lands at or before the bound.
pub fn timer_for(bound_ms: u64) -> Option<u16> {
    let ticks = bound_ms / (TICK_MS * EXPIRIES);
    (TMR_MIN..=TMR_MAX).contains(&ticks).then_some(ticks as u16)
}

/// What `timer_for` bought, which the rounding means is not what was asked for.
pub fn bound_of(timer: u16) -> u64 {
    u64::from(timer) * TICK_MS * EXPIRIES
}

/// One chipset, and where its TCO block's base port is written down: the
/// config register, the bits of it that are the address, the offset to
/// [`TCO_RLD`], and the register and bit that must be set for it to answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chipset {
    pub vendor: u16,
    pub device: u16,
    pub base_reg: u16,
    pub base_mask: u32,
    pub base_offset: u16,
    pub enable: (u16, u32),
}

/// Every chipset this kernel will write a TCO register on. **QEMU's q35 is the
/// only row, and the only one a harness can judge**; a Tiger Lake-LP row belongs
/// here and this tree has no number for its base, which
/// `issues/hardware/the-tco-row-for-tiger-lake-is-unmeasured.md` closes.
pub const CHIPSETS: &[Chipset] = &[Chipset {
    vendor: 0x8086,
    device: 0x2918,
    base_reg: 0x40,
    base_mask: 0xff80,
    base_offset: 0x60,
    enable: (0x40, 1),
}];

/// The row for a device, or `None` for a machine no row names.
pub fn chipset(vendor: u16, device: u16) -> Option<&'static Chipset> {
    CHIPSETS.iter().find(|c| c.vendor == vendor && c.device == device)
}

/// Why a chipset with a row still names no port this kernel will write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoPort {
    /// All ones: masking a function that is not answering leaves a plausible base.
    Absent,
    Disabled,
    Base(u32),
}

impl Chipset {
    /// The block's base port, from this device's `base_reg` and `enable` reads.
    pub fn port(&self, base_reg: u32, enable_reg: u32) -> Result<u16, NoPort> {
        if base_reg == u32::MAX || enable_reg == u32::MAX {
            return Err(NoPort::Absent);
        }
        if enable_reg & self.enable.1 == 0 {
            return Err(NoPort::Disabled);
        }
        let base = base_reg & self.base_mask;
        // The last register read has to land inside the port space, not just the base.
        let top = base + u32::from(self.base_offset) + u32::from(TCO_TMR) + 1;
        if base == 0 || top > u32::from(u16::MAX) {
            return Err(NoPort::Base(base_reg));
        }
        Ok(base as u16 + self.base_offset)
    }
}
