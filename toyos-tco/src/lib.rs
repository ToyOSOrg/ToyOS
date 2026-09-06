//! Every bound a boot arms and the metal loop waits on, and Intel's TCO
//! watchdog: its register block, and where a chipset keeps it.
//!
//! The block's layout is one thing across the generations this kernel targets;
//! **where its base comes from is not**, so that is a table keyed by PCI id
//! with a refusal for every machine not in it. A guess would be an I/O write to
//! whatever else lives at the port. Pure: the kernel supplies the config reads
//! and the port writes.
//!
//! Every bit named below is quoted from the *Intel 500 Series Chipset Family
//! On-Package Platform Controller Hub Datasheet, Volume 2 of 2*, document
//! 631120-002 revision 002, which is Tiger Lake-LP (`8086:a0xx`) — section
//! numbers are that document's. **The bits are not portable across PCH
//! generations even though the offsets are**: on the 100 Series (document
//! 332691-003EN) `TCO1_CNT`'s low byte is reserved entirely and the no-reboot
//! bit lives in a different register in SMBus configuration space. A row added
//! here for another generation owes its own citation.

#![no_std]
#![forbid(unsafe_code)]

/// Offsets from the block's base port.
pub const TCO_RLD: u16 = 0x00;
pub const TCO1_STS: u16 = 0x04;
pub const TCO2_STS: u16 = 0x06;
pub const TCO1_CNT: u16 = 0x08;
pub const TCO_TMR: u16 = 0x12;

/// `TCO1_STS` bit 3, whose datasheet name is `TIMEOUT` and not the
/// `TCO_TMR_STS` other sources use (§32.1.4): "Bit set to 1 by Intel PCH to
/// indicate that the SMI was caused by TCO timer reaching 0." It is the first
/// expiry's latch, so a machine that stayed up with this set expired once and
/// did not reboot.
pub const TCO1_STS_TIMEOUT: u16 = 1 << 3;

/// `TCO2_STS`'s two bits are how a chipset that reset the machine last time
/// says it was this timer that did it (§32.1.5): "If this bit is set and the
/// NO_REBOOT config bit is 0, then the Intel PCH will reboot the system after
/// the second timeout."
pub const TCO_TMR_HLT: u16 = 1 << 11;
pub const TCO_SECOND_TO_STS: u16 = 1 << 1;
pub const TCO_BOOT_STS: u16 = 1 << 2;

/// `TCO1_CNT` bit 0, `NO_REBOOT_MSUS` (§32.1.6): "This bit reflects the No
/// Reboot pin strap state... When set, the TCO timer will count down and
/// generate the SMI# on the first timeout, but will not reboot on the second
/// timeout." Software may clear it only where the strap was sampled low — it
/// "may not override the strap when [it] indicates No Reboot" — so it is
/// written clear as part of [`TCO1_CNT_RUN`] and then read back, never assumed.
pub const TCO1_CNT_NO_REBOOT: u16 = 1 << 0;

/// `TCO1_CNT` bit 12, `TCO_LOCK` (§32.1.6): "When set to 1, this bit prevents
/// writes from changing the TCO_EN bit... Once this bit is set to 1, it can not
/// be cleared by software writing a 0 to this bit location. A core-well reset is
/// required to change this bit from 1 to 0."
///
/// **The one bit a read-back of this register may not be judged on.** Firmware
/// that set it leaves it set through every write this tree makes, and what it
/// gates is `SMI_EN.TCO_EN` — not the countdown and not the reboot.
pub const TCO1_CNT_LOCK: u16 = 1 << 12;

/// `TCO1_CNT` declared whole, never a bit set into what was there: it also
/// selects an interrupt this kernel wants none of, and its bit 0 is the
/// no-reboot gate, which this value asks the chipset to clear.
pub const TCO1_CNT_RUN: u16 = 0;
pub const TCO1_CNT_HALT: u16 = TCO_TMR_HLT;

/// What a write of [`TCO1_CNT_RUN`] must read back as, given that
/// [`TCO1_CNT_LOCK`] survives it.
pub const fn cnt_took_the_write(read_back: u16) -> bool {
    read_back & !TCO1_CNT_LOCK == TCO1_CNT_RUN
}

/// `TCO_TMR` is ten bits, the chipset ignores 0 and 1, a tick is 600 ms, and
/// the first expiry only latches a status bit — so a bound is two of them. The
/// mask is public because reading the register back is how the kernel tells a
/// timer the bootloader armed from one nothing has touched.
pub const TMR_MASK: u16 = 0x3ff;
const TMR_MAX: u64 = TMR_MASK as u64;
const TMR_MIN: u64 = 2;
/// Public because a reader watching this timer count has to wait one of these
/// to see it move: a shorter wait proves nothing about a halted timer.
pub const TICK_MS: u64 = 600;
const EXPIRIES: u64 = 2;

/// The `TCO_TMR` whose expiries reach `bound_ms`, rounded down so the reset lands at or before the bound.
pub const fn timer_for(bound_ms: u64) -> Option<u16> {
    let ticks = bound_ms / (TICK_MS * EXPIRIES);
    if ticks < TMR_MIN || ticks > TMR_MAX {
        return None;
    }
    Some(ticks as u16)
}

/// The smallest `TCO_TMR` a PCH honours, whatever [`timer_for`]'s own floor
/// admits: the two differ, and a bound derived for hardware answers to this one.
pub const TMR_MIN_HARDWARE: u16 = 0x04;

/// The TCO bound, in milliseconds: the bootloader arms it before it jumps and
/// the kernel keeps feeding that same timer, so it is the bound from the
/// handoff onward. The largest the tick and the double expiry make exact at or
/// under ten seconds: eight ticks of 600 ms, twice.
pub const BOUND_MS: u64 = 9_600;

/// The bound the firmware's own watchdog is set to, in milliseconds. It covers
/// the span before the TCO arm, and `ExitBootServices` disables it; a minute is
/// this project's bound for every watchdog.
pub const FIRMWARE_BOUND_MS: u64 = 60_000;

/// The bound the test runner gives its whole job list, in milliseconds: the
/// chipset's timer is fed from scheduler passes, so a kernel that is alive
/// while a job never finishes is no wedge to it and nothing else ends the boot.
pub const JOB_BOUND_MS: u64 = 60_000;

/// [`BOUND_MS`]'s timer, which is neither a value this tree can fail to have
/// nor one the hardware would ignore.
pub const TIMER: u16 = match timer_for(BOUND_MS) {
    Some(timer) => {
        assert!(timer >= TMR_MIN_HARDWARE, "the loader's bound derives a TCO_TMR the PCH ignores");
        timer
    }
    None => panic!("the loader's bound reaches no TCO timer"),
};

/// The boot parameter that arms it: the bootloader reads it off
/// `\toyos\cmdline` and the kernel off the same bytes in `KernelArgs`, so
/// one machine cannot have one of the two armed and not the other.
pub const PARAM: &str = "watchdog";

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
