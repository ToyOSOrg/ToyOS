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
pub const TCO2_STS: u16 = 0x06;
pub const TCO1_CNT: u16 = 0x08;
pub const TCO_TMR: u16 = 0x12;

/// `TCO2_STS`'s two bits are how a chipset that reset the machine last time
/// says it was this timer that did it.
pub const TCO_TMR_HLT: u16 = 1 << 11;
pub const TCO_SECOND_TO_STS: u16 = 1 << 1;
pub const TCO_BOOT_STS: u16 = 1 << 2;

/// `TCO1_CNT` declared whole, never a bit set into what was there: it also selects an interrupt this kernel wants none of.
pub const TCO1_CNT_RUN: u16 = 0;
pub const TCO1_CNT_HALT: u16 = TCO_TMR_HLT;

/// `TCO_TMR` is ten bits, the chipset ignores 0 and 1, a tick is 600 ms, and
/// the first expiry only latches a status bit — so a bound is two of them.
const TMR_MAX: u64 = 0x3ff;
const TMR_MIN: u64 = 2;
const TICK_MS: u64 = 600;
const EXPIRIES: u64 = 2;

/// The `TCO_TMR` whose expiries reach `bound_ms`, rounded down so the reset lands at or before the bound.
pub const fn timer_for(bound_ms: u64) -> Option<u16> {
    let ticks = bound_ms / (TICK_MS * EXPIRIES);
    if ticks < TMR_MIN || ticks > TMR_MAX {
        return None;
    }
    Some(ticks as u16)
}

pub fn bound_of(timer: u16) -> u64 {
    u64::from(timer) * TICK_MS * EXPIRIES
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Enable {
    pub reg: u16,
    pub bit: u32,
}

/// One chipset, and where its TCO block's base port is written down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chipset {
    pub vendor: u16,
    pub device: u16,
    pub base_reg: u16,
    pub base_mask: u32,
    pub base_offset: u16,
    pub enable: Enable,
}

/// Every chipset this kernel will write a TCO register on.
pub const CHIPSETS: &[Chipset] = &[
    // q35's LPC bridge: the TCO block sits inside the ACPI PM I/O window.
    Chipset {
        vendor: 0x8086,
        device: 0x2918,
        base_reg: 0x40,
        base_mask: 0xff80,
        base_offset: 0x60,
        enable: Enable { reg: 0x40, bit: 1 },
    },
    // Tiger Lake-LP's SMBus function: a 32-byte I/O base of its own, the block
    // at its start, and bit 0 of the register not part of the address.
    Chipset {
        vendor: 0x8086,
        device: 0xa0a3,
        base_reg: 0x50,
        base_mask: !1,
        base_offset: 0x00,
        enable: Enable { reg: 0x54, bit: 1 << 8 },
    },
];

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
    pub fn port(&self, base_reg: u32, enable_reg: u32) -> Result<u16, NoPort> {
        if base_reg == u32::MAX || enable_reg == u32::MAX {
            return Err(NoPort::Absent);
        }
        if enable_reg & self.enable.bit == 0 {
            return Err(NoPort::Disabled);
        }
        let base = base_reg & self.base_mask;
        // `TCO_TMR` is a 16-bit register, so `port + TCO_TMR + 1 <= 0xffff`.
        let top = base + u32::from(self.base_offset) + u32::from(TCO_TMR) + 1;
        if base == 0 || top > u32::from(u16::MAX) {
            return Err(NoPort::Base(base_reg));
        }
        Ok(base as u16 + self.base_offset)
    }
}
