//! The PHY's power state and the firmware's handshake, as the MAC's own
//! datasheet defines them — every decision about them and none of the accesses
//! that carry them out.
//!
//! **Every `§8.` in this file is the third document.** What the T14's `MDIC`
//! sits in is not the 82574 but the PCH's integrated GbE controller at
//! `00:1f.6`, and its register file is §8.2 of the *Intel® 500 Series Chipset
//! Family On-Package Platform Controller Hub Datasheet, Volume 2 of 2*,
//! document 631120, revision 002 — Table 8-2 "Summary of GbE Memory Mapped I/O
//! Registers" and §8.2.1 through §8.2.9 below it.
//!
//! # Why the power state is between this driver and `MDIC` at all
//!
//! `MDIC` on this part is not an MDIO bus master. The *Intel Ethernet
//! Connection I219 Datasheet* (612523, rev 2.02) §1.2 says the PHY's registers
//! "are mapped into the MDIO space and can be accessed by the integrated LAN
//! controller through the PCIe or SMBus interconnects", and that "the MDIO
//! traffic is embedded in specific fields in SMBus packets or carried by
//! special packets over the PCIe encoded interconnect"; §2.2.2.1.1 is the
//! MAC's MDIO access packet and §2.2.2.1.2 the PHY's acknowledge/response to
//! it. **A cycle therefore ends when the LAN Connected Device answers over a
//! live interconnect, and not otherwise** — and §2.2's Table 2-1 puts that
//! interconnect in "Electrical Idle" for "S0 and PHY Power Down", "S0 and Idle
//! or Link Disconnect" and "S0 and Link in Low Power Idle", with SMBus "Not
//! Used" in all three.
//!
//! That is the shape of a `Ready` that never comes back and an `Error` that
//! never comes back either: no packet went out, so nothing ended the cycle.
//!
//! # What software is given, and what it is not
//!
//! Four readings and two bits. §8.2.1's `CTRL` bit 24 and §8.2.2's `CTRL_EXT`
//! bit 20 are the two the document gives software over the PHY's power, and
//! [`Correction`] is the whole of what this driver writes. §8.2.5's `PHY_CTRL`
//! and §8.2.9's `FWSM` are read and never written: the first is link-speed
//! policy and the second is the firmware's own report.
//!
//! **No Intel document found for this part gives software a sequence to
//! restore power to a PHY that is gated off.** The I219 datasheet §6.3.1.3 has
//! the power-down state entered "when the LAN_DISABLE_N pin is set to zero",
//! says "the I219 loses all functionality in this mode other than the ability
//! to power up again", and that "exiting this mode requires setting the
//! LAN_DISABLE_N pin to a logic one"; the PCH's own datasheet, Volume 1 of 2
//! (635218, rev 008) §19.1 Table 39, says that pin is driven by the PCH's
//! `LANPHYPC`, that "PCH will drive LANPHYPC low to put the PHY into a low
//! power state when functionality is not needed", and that it "is used to
//! indicate that power needs to be restored to the Platform LAN Connect
//! Device". Neither document names a register a driver writes to move it, and
//! I219 §6.4 leaves the Ultra Low Power mode's entry and exit "controlled by
//! the host driver (on non ME systems) or the ME FW" without publishing the
//! host half. So this file reads the state and moves only the two bits the
//! document gives; the host's hand on the pin itself is `CTRL` bits 16 and 17,
//! which Intel's own host driver for this family uses and no Intel document
//! publishes, and [`crate::wake`] is where this driver takes it.

use crate::regs::{ctrl, ctrl_ext, fwsm, mdic, phy_ctrl};

/// One reading of everything the MAC says about the PHY's power and the
/// firmware's handshake, taken as five words in one sweep.
///
/// **The whole word of each and not a field**, because which bits are standing
/// in a register three agents share is the question a bench boot is asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reading {
    /// §8.2.1's `GBECSR_00`, the Device Control register at offset `0h`.
    pub ctrl: u32,
    /// §8.2.2's `GBECSR_18`, the Extended Device Control register at `18h`.
    pub ctrl_ext: u32,
    /// §8.2.5's `GBECSR_F10` at `F10h`, whose reset value the document gives
    /// as `Ch`.
    pub phy_ctrl: u32,
    /// §8.2.9's `GBECSR_5B54` at `5B54h`, the firmware's own report.
    pub fwsm: u32,
    /// §8.2.3's `GBECSR_20` at `20h`, read for its bit 31 alone.
    pub mdic: u32,
}

impl Reading {
    /// Whether any of the five answered ones, which is a window that stopped
    /// decoding and not a register's value. **No correction is computed from
    /// such a reading**: the write would set every field of a register this
    /// driver owns one bit of.
    pub fn unrouted(self) -> Option<u32> {
        [self.ctrl, self.ctrl_ext, self.phy_ctrl, self.fwsm, self.mdic]
            .into_iter()
            .find(|word| *word == u32::MAX)
    }

    /// §8.2.1's bit 24: "PHY Power Down (PHYPDN): When cleared (0b), the PHY
    /// power down setting is controlled by the internal logic of PCH."
    ///
    /// The document describes only the cleared case, so the cleared case is
    /// the one this driver puts the part in and the set case is one it reports
    /// rather than acts on.
    pub fn phy_power_down(self) -> bool {
        self.ctrl & ctrl::PHY_POWER_DOWN != 0
    }

    /// §8.2.2's bit 20: "PHY Power Down Enable (PHYPDEN): When set, this bit
    /// enables the PHY to enter a low-power state when the LAN controller is
    /// at the DMOff/ D3 or with no WOL."
    pub fn low_power_entry_enabled(self) -> bool {
        self.ctrl_ext & ctrl_ext::PHY_POWER_DOWN_ENABLE != 0
    }

    /// §8.2.9's bit 15: "Firmware Valid Bit (FWVAL): 1 = Firmware is ready,
    /// 0 = Firmware is not ready."
    ///
    /// **Read and never acted on.** The document says what the bit means and
    /// nowhere says an MDI access depends on it, so a bring-up that refused on
    /// it would be refusing on this driver's guess.
    pub fn firmware_ready(self) -> bool {
        self.fwsm & fwsm::FIRMWARE_VALID != 0
    }

    /// §8.2.3's bit 31: "Wait: Set to 1 by the Gigabit Ethernet Controller to
    /// indicate that a PCI Express* to SMBus transition is taking place. The
    /// ME/Host should not issue new MDIC transactions while this bit is set to
    /// 1. This bit is auto cleared by hardware after the transition has
    ///    occurred."
    pub fn interconnect_in_transition(self) -> bool {
        self.mdic & mdic::WAIT != 0
    }

    /// What this driver writes to put the part in the state §8.2.1 and §8.2.2
    /// describe, and nothing else.
    ///
    /// **A bit that is already clear is not written.** The two registers are
    /// shared with the firmware and the part's own hardware — §8.2.4 calls
    /// them "shared CSR registers" — so a write nobody needs is one more way
    /// to lose what another agent put in the word between the read and it.
    pub fn correction(self) -> Correction {
        Correction {
            ctrl: self.phy_power_down().then_some(self.ctrl & !ctrl::PHY_POWER_DOWN),
            ctrl_ext: self
                .low_power_entry_enabled()
                .then_some(self.ctrl_ext & !ctrl_ext::PHY_POWER_DOWN_ENABLE),
        }
    }
}

impl core::fmt::Display for Reading {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "CTRL {:#010x}, CTRL_EXT {:#010x}, PHY_CTRL {:#010x}, FWSM {:#010x}, \
             MDIC {:#010x}: the PHY {}, a low-power entry {}, the firmware {}, and the \
             interconnect {}",
            self.ctrl,
            self.ctrl_ext,
            self.phy_ctrl,
            self.fwsm,
            self.mdic,
            if self.phy_power_down() {
                "is held in §8.2.1's power down"
            } else {
                "is left to the PCH's own logic by §8.2.1"
            },
            if self.low_power_entry_enabled() {
                "is enabled by §8.2.2"
            } else {
                "is not enabled by §8.2.2"
            },
            if self.firmware_ready() { "reports itself ready" } else { "reports itself not ready" },
            if self.interconnect_in_transition() {
                "is in §8.2.3's PCIe-to-SMBus transition"
            } else {
                "is not in §8.2.3's transition"
            },
        )
    }
}

/// The words §8.2.1's and §8.2.2's registers are to be left holding, and
/// `None` for one already holding them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Correction {
    pub ctrl: Option<u32>,
    pub ctrl_ext: Option<u32>,
}

impl Correction {
    /// Whether the part was already in the state the document describes, so
    /// this driver writes nothing at all.
    pub fn nothing(self) -> bool {
        self.ctrl.is_none() && self.ctrl_ext.is_none()
    }
}

impl core::fmt::Display for Correction {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match (self.ctrl, self.ctrl_ext) {
            (None, None) => f.write_str("nothing was written"),
            (Some(ctrl), None) => write!(f, "CTRL was written {ctrl:#010x}"),
            (None, Some(ext)) => write!(f, "CTRL_EXT was written {ext:#010x}"),
            (Some(ctrl), Some(ext)) => {
                write!(f, "CTRL was written {ctrl:#010x} and CTRL_EXT {ext:#010x}")
            }
        }
    }
}

/// One boot's whole account of the power step: the state the part was in, what
/// this driver wrote into it, and the state it was in afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settled {
    pub before: Reading,
    pub wrote: Correction,
    pub after: Reading,
}

impl Settled {
    /// Whether the part ended the step in the state §8.2.1 and §8.2.2
    /// describe — which is a reading of the part and never an assumption that
    /// a write took.
    pub fn settled(self) -> bool {
        !self.after.phy_power_down() && !self.after.low_power_entry_enabled()
    }
}

impl core::fmt::Display for Settled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "before it: {}; {}; after it: {}", self.before, self.wrote, self.after)
    }
}

/// §8.2.5's `PHY_CTRL` as the document gives its reset value — "Default: Ch",
/// which is "GbE Disable at non D0a" and "LPLU in non D0a" set.
///
/// Nothing decides on it: it is here so a reading that is *not* it is visible
/// as the firmware's configuration and not as a surprise.
pub const PHY_CTRL_RESET: u32 = phy_ctrl::GBE_DISABLE_NON_D0A | phy_ctrl::LPLU_NON_D0A;

const _: () = {
    assert!(PHY_CTRL_RESET == 0xC);
};
