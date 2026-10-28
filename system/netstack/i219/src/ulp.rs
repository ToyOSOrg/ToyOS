//! The I219's Ultra Low Power configuration, disarmed at every start — every
//! decision about it and none of the accesses that carry it out.
//!
//! I219 §6.4 leaves ULP's entry and exit to "the host driver (on non ME
//! systems) or the ME FW", so a configuration another agent armed may stand
//! when this driver starts. This driver arms no wake, so it leaves ULP unarmed:
//! the PHY then does not power itself down on a lost link.
//!
//! **Where the firmware is valid** (`FWSM` bit 15) the ME configures ULP, and
//! this driver writes none of it.
//!
//! Every `§9.` here is the *Intel Ethernet Connection I219 Datasheet* (612523,
//! rev 2.02). What it does not publish, and this driver relies on as properties
//! of the part: ULP Configuration 1's bit 11, which keeps sticky ULP across
//! `PERST#`, and bit 12, which keeps SMBus across it, both of which §9.5.7.1
//! calls reserved; page 770 register 17's bit 14, which enables K1 and which
//! ULP's entry clears; and `regs::ULP_SMBUS_HOLD`'s bit 5, the MAC's half of
//! bit 12. Disarming sets K1, clears the rest and the power-up indication,
//! commits the configuration with `START`, and is followed by the full reset
//! of the MAC and PHY.

use crate::phy::PhyRefusal;

/// §9.5.7.1's ULP Configuration 1: PHY address 01, page 779, register 16.
pub const PAGE_ULP: u16 = 779;
pub const CONFIGURATION: u8 = 16;

/// ULP Configuration 1's bits (§9.5.7.1, and the two module header bits).
pub mod config {
    /// "Starts the auto ULP configuration"; the part clears it when done.
    pub const START: u16 = 1 << 0;
    /// "Power-up-from-ULP indication": the part's, not an arming.
    pub const IND: u16 = 1 << 2;
    /// "Enter ULP on link disconnect".
    pub const STICKY: u16 = 1 << 4;
    pub const INBAND_EXIT: u16 = 1 << 5;
    pub const WOL_HOST: u16 = 1 << 6;
    /// "Reset to SMBus by default (on power on or on ULP exit ...)".
    pub const RESET_TO_SMBUS: u16 = 1 << 8;
    /// "Enable ULP on LAN disable (LANPHYPC)".
    pub const ON_LANPHYPC: u16 = 1 << 10;
    pub const KEEP_STICKY_ON_PERST: u16 = 1 << 11;
    pub const KEEP_SMBUS_ON_PERST: u16 = 1 << 12;

    /// Every bit an arming sets.
    pub const ARMING: u16 = STICKY
        | INBAND_EXIT
        | WOL_HOST
        | RESET_TO_SMBUS
        | ON_LANPHYPC
        | KEEP_STICKY_ON_PERST
        | KEEP_SMBUS_ON_PERST;
}

/// The power management register holding K1's enable: PHY address 01, page
/// 770, register 17, bit 14.
pub const PAGE_POWER: u16 = 770;
pub const POWER_CONTROL: u8 = 17;
pub const K1_ENABLE: u16 = 1 << 14;

/// `regs::ULP_SMBUS_HOLD`'s bit 5.
pub const SMBUS_HOLD: u32 = 1 << 5;

/// Every bit the disarm clears: the arming and the power-up indication.
pub const CLEARED: u16 = config::ARMING | config::IND;

/// The configuration with [`CLEARED`] cleared, to be written and then written
/// again with [`config::START`].
pub fn disarmed(configuration: u16) -> u16 {
    configuration & !(CLEARED | config::START)
}

pub fn k1_enabled(power_control: u16) -> u16 {
    power_control | K1_ENABLE
}

pub fn smbus_released(hold: u32) -> u32 {
    hold & !SMBUS_HOLD
}

/// The three words ULP's configuration is held in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct State {
    pub configuration: u16,
    pub power_control: u16,
    pub smbus_hold: u32,
}

impl State {
    /// Whether any part of an arming stands in it. K1 and the power-up
    /// indication are not: ULP's entry moves them, an arming does not.
    pub fn armed(self) -> bool {
        self.configuration & config::ARMING != 0 || self.smbus_hold & SMBUS_HOLD != 0
    }
}

impl core::fmt::Display for State {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "ULP configuration {:#06x} ({}, {}), power control {:#06x} (K1 {}), {:#x} {:#010x}",
            self.configuration,
            if self.armed() { "armed" } else { "unarmed" },
            if self.configuration & config::IND != 0 { "up from ULP" } else { "not up from ULP" },
            self.power_control,
            if self.power_control & K1_ENABLE != 0 { "on" } else { "off" },
            crate::regs::ULP_SMBUS_HOLD,
            self.smbus_hold,
        )
    }
}

/// What the start did about ULP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Disarm {
    /// The PHY never answered over PCIe, so nothing was read or written.
    Unreached,
    /// `FWSM` says the firmware is valid, and the ME owns ULP.
    FirmwareOwns,
    /// A PHY or MAC access failed part way.
    Refused(PhyRefusal),
    /// The state found, before the disarm was written.
    Disarmed { found: State },
}

impl core::fmt::Display for Disarm {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreached => f.write_str("ULP was not disarmed: the PHY never answered over PCIe"),
            Self::FirmwareOwns => f.write_str("ULP was left to the firmware, which FWSM says is valid"),
            Self::Refused(why) => write!(f, "ULP was not disarmed: {why}"),
            Self::Disarmed { found } => write!(f, "ULP was found with {found}, and disarmed"),
        }
    }
}
