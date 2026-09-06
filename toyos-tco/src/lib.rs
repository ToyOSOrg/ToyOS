//! Intel's TCO watchdog: its register block, and where a chipset keeps it.
//!
//! The block's own layout is one thing across the generations this kernel
//! targets — the offsets below are QEMU's `include/hw/acpi/ich9_tco.h` enum,
//! and Intel's PCH datasheets give the same registers at the same offsets from
//! whatever base their chipset names. **Where that base comes from is not one
//! thing**, so it is a table keyed by PCI id with a row per chipset and a
//! refusal for every machine not in it. A guess would be an I/O write to
//! whatever else lives at the port.
//!
//! Pure: the kernel supplies the config-space reads and the port writes.

#![no_std]
#![forbid(unsafe_code)]

/// Offsets from the block's base port.
pub const TCO_RLD: u16 = 0x00;
pub const TCO1_STS: u16 = 0x04;
pub const TCO1_CNT: u16 = 0x08;
pub const TCO_TMR: u16 = 0x12;

/// `TCO1_CNT` bit 11, `TCO_TMR_HLT`: set halts the timer, clear runs it.
pub const TCO_TMR_HLT: u16 = 1 << 11;
/// `TCO1_STS` bit 3: the timer expired at least once.
pub const TCO_TIMEOUT: u16 = 1 << 3;

/// `TCO1_CNT` as this kernel declares it, running and halted. Declared rather
/// than a bit set into what was there: the register also selects an interrupt
/// this kernel wants none of, and a read-modify-write would carry whatever
/// firmware left in those bits into a machine that never chose it.
pub const TCO1_CNT_RUN: u16 = 0;
pub const TCO1_CNT_HALT: u16 = TCO_TMR_HLT;

/// `TCO_TMR` is ten bits, and the chipset ignores 0 and 1.
const TMR_MAX: u64 = 0x3ff;
const TMR_MIN: u64 = 2;
/// One tick, in milliseconds.
const TICK_MS: u64 = 600;
/// Expiries before the chipset acts: the first only latches `TCO_TIMEOUT`.
const EXPIRIES: u64 = 2;

/// The `TCO_TMR` value whose expiries reach `bound_ms`, or `None` when no
/// ten-bit value does.
///
/// Rounded down, so the reset lands at or before the bound rather than after
/// it: a watchdog that outran what it promised would let a wedge sit.
pub fn timer_for(bound_ms: u64) -> Option<u16> {
    let ticks = bound_ms / (TICK_MS * EXPIRIES);
    (TMR_MIN..=TMR_MAX).contains(&ticks).then_some(ticks as u16)
}

/// What `timer_for` bought, in milliseconds — what the kernel logs, because the
/// rounding means it is not the bound that was asked for.
pub fn bound_of(timer: u16) -> u64 {
    u64::from(timer) * TICK_MS * EXPIRIES
}

/// One chipset, and where its TCO block's base port is written down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chipset {
    pub vendor: u16,
    pub device: u16,
    /// The PCI config register holding the base, and the bits of it that are one.
    pub base_reg: u16,
    pub base_mask: u32,
    /// Added to the base to reach [`TCO_RLD`].
    pub base_offset: u16,
    /// A config register and a bit that must be set for the block to answer.
    pub enable: (u16, u32),
}

/// Every chipset this kernel will write a TCO register on.
///
/// **QEMU's q35 is the only row, and the only one judged.** A Tiger Lake-LP
/// row belongs here and is not written: its base is the SMBus function's own
/// register rather than the LPC bridge's PM window, and this tree has neither
/// the datasheet nor a reading off the machine to put a number to it —
/// `issues/hardware/the-tco-row-for-tiger-lake-is-unmeasured.md` carries the
/// command that closes it.
pub const CHIPSETS: &[Chipset] = &[
    // The ISA bridge at 00:1f.0 of QEMU's q35, measured with `info pci`. Its
    // PMBASE is bits 15:7 of config 0x40 with bit 0 the enable, and the TCO
    // block sits 0x60 into that window.
    Chipset {
        vendor: 0x8086,
        device: 0x2918,
        base_reg: 0x40,
        base_mask: 0xff80,
        base_offset: 0x60,
        enable: (0x40, 1),
    },
];

/// The row for a device, or `None` for a machine no row names.
pub fn chipset(vendor: u16, device: u16) -> Option<&'static Chipset> {
    CHIPSETS.iter().find(|c| c.vendor == vendor && c.device == device)
}

/// Why a chipset that has a row still names no port this kernel will write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoPort {
    /// A config register reading all ones: the function is not answering, and
    /// masking that would leave a plausible base made of a device's absence.
    Absent,
    /// The enable bit is clear, so the block answers nothing.
    Disabled,
    /// The register reads as zero, or puts the block past the port space.
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
        // The last register read has to fall inside the port space, not just the base.
        let top = base + u32::from(self.base_offset) + u32::from(TCO_TMR) + 1;
        if base == 0 || top > u32::from(u16::MAX) {
            return Err(NoPort::Base(base_reg));
        }
        Ok(base as u16 + self.base_offset)
    }
}
