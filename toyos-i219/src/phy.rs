//! The PHY, and every decision about reaching it.
//!
//! **Two documents meet in this file.** What the MAC offers is `MDIC`
//! (§10.2.2.7 of the *Intel 82574 GbE Controller Family Datasheet*) under
//! §4.5.2's ownership arbitration, and every `§10.` and `§4.` below is that
//! document. What answers on the other side of it is the *Intel Ethernet
//! Connection I219 Datasheet*, order number 612523, revision 2.02, whose §9 is
//! the PHY's own register map; every `§9.` below is that one. §4.6.3.1 is why
//! both are needed at once: "Refer to the PHY documentation for the
//! initialization and link setup steps. The device driver uses the MDIC
//! register to initialize the PHY and setup the link."
//!
//! **A third document governs the T14's MAC**, and every `§8.` below is it:
//! the *Intel® 500 Series Chipset Family On-Package Platform Controller Hub
//! Datasheet, Volume 2 of 2*, document 631120, revision 002, §8.2.
//! [`crate::power`] is what it says about the PHY's power state and the
//! firmware's handshake, and why a cycle on this part can fail to start.

use core::cell::Cell;

use crate::power::{Reading, Settled};
use crate::regs::{self, extcnf, mdic};
use crate::wake::{self, Action, Answer, Moment, Woke};
use crate::{Clock, Link, Registers, Speed};

/// How long [`Owned::transact`] waits for §10.2.2.7's `Ready` before the part
/// is refused by name.
///
/// **A driver-chosen bound, not a datasheet one**: §10.2.2.7 gives its
/// transaction no time, so a part that does not end one inside this is refused
/// instead of holding up the boot.
pub(crate) const MDI_DEADLINE_NANOS: u64 = 100_000_000;

/// How long each of §4.5.2's two waits is given: another agent's software flag
/// to go, and this driver's own request to be granted.
///
/// **A driver-chosen bound, not a datasheet one**: §4.5.2 gives its handshake
/// no time. At [`ARBITRATION_PACE_NANOS`] between accesses this is five
/// readings of the register, and the one grant this driver has measured on the
/// part came back at the first reading after the request — so a bound this wide
/// refuses an arbitration that is not answering rather than one that is slow.
pub(crate) const ARBITRATION_DEADLINE_NANOS: u64 = 500_000_000;

/// How long this driver leaves §4.5.2's arbitration alone between two accesses
/// of its own.
///
/// **A bench fact and not a datasheet one.** Neither §4.5.2 nor §10.2.2.15 asks
/// for any pace at all. On the T14 this same request, put to this same register
/// with about a millisecond between accesses, left a machine that had to be
/// powered off by hand; the run that put it with 88 to 133 ms between accesses
/// came back and answered. One run each, and the cause is unmeasured: this is
/// the wider of the two paces, and it stands until something measures why.
pub const ARBITRATION_PACE_NANOS: u64 = 100_000_000;

const _: () = {
    // The narrowest gap between two accesses of the run that came back was
    // 88 ms, and nothing has been measured between that and the millisecond the
    // machine did not come back from. A pace under it is a number no reading of
    // that machine supports, so it does not compile.
    assert!(ARBITRATION_PACE_NANOS >= 88_000_000);
};

/// §9.2: "After LCD reset to the I219 a delay of 10 ms is required before
/// attempting to access MDIO registers."
///
/// Measured from the MAC reset this driver issued, because the LAN Connected
/// Device is what that reset reaches and nothing in either document lets a
/// driver observe whether it did.
pub(crate) const LCD_RESET_DELAY_NANOS: u64 = 10_000_000;

/// §9.3: the I219's registers "are spread over two PHY addresses 01, 02, where
/// general registers are located under PHY address 01 and the PHY specific
/// registers are at PHY address 02". Table 9-1 says which address each register
/// below is at, and these two names are what that table is read through.
pub(crate) const GENERAL: u8 = 1;
pub(crate) const SPECIFIC: u8 = 2;

/// The registers of the PHY this driver touches, each at the address and page
/// Table 9-1 gives it.
pub(crate) mod reg {
    /// Control (§9.5.2.1), PHY address 02, any page.
    pub const CONTROL: u8 = 0;
    /// PHY Identifier 1 and 2 (§9.5.2.3, §9.5.2.4), PHY address 02, any page.
    pub const IDENTIFIER_HIGH: u8 = 2;
    pub const IDENTIFIER_LOW: u8 = 3;
    /// Auto-Negotiation Advertisement (§9.5.2.5), PHY address 02, any page.
    pub const ADVERTISE: u8 = 4;
    /// 1000BASE-T Control (§9.5.2.10), PHY address 02, any page.
    pub const CONTROL_1000T: u8 = 9;
    /// Custom Mode Control (§9.5.3.1), PHY address 01, page 769.
    pub const CUSTOM_MODE: u8 = 16;
    /// Port General Configuration (§9.5.3.2), PHY address 01, page 769.
    pub const PORT_GENERAL: u8 = 17;
    /// OEM Bits (§9.5.8.2), PHY address 01, page 0.
    pub const OEM_BITS: u8 = 25;
    /// §9.3: "Register 31 is the page register in all pages of PHY address 01."
    pub const PAGE_SELECT: u8 = 31;
}

/// Port General Configuration bits (§9.5.3.2).
pub mod port_general {
    /// MACPD_enable (bit 2). §9.5.3.2: "When set to 1b, pages 800 and 801 are
    /// enabled for configuration and Host_WU_Active is not blocked for
    /// writes."
    pub const MACPD_ENABLE: u16 = 1 << 2;
    /// Host_WU_Active (bit 4). §9.5.3.2: "Enables host wake up from the I219.
    /// This bit is reset by power on reset only" — so a PHY an earlier
    /// operating system armed for wake-up keeps it through every reset this
    /// driver issues but a power cycle.
    pub const HOST_WAKE_UP_ACTIVE: u16 = 1 << 4;
}

/// The page §9.5.3's port control registers live in.
pub(crate) const PAGE_PORT_CONTROL: u16 = 769;

/// The page §9.5.8's general registers live in: §9.5.8.2's heading and §8.1's
/// "PHY address 1, page 0, register 25" agree on it.
pub(crate) const PAGE_GENERAL: u16 = 0;

/// OEM Bits (§9.5.8.2): the two bits that decide which speed auto-negotiation
/// may resolve to, and the restart that makes them take effect.
///
/// **A write alone changes nothing on the wire.** §8.1: bits 2 and 6 "will not
/// take effect unless" a software reset (§9.5.2.1's bit 15) or this register's
/// own restart occurs — §9.5.2.1's Restart Auto-Negotiation is not one of the
/// two — and "the latest occurring event (signal toggling or register write)
/// will determine the values in the registers", so the MAC's own load of them
/// from `PHY_CTRL` and a driver's write replace each other.
pub mod oem_bits {
    /// `rev_aneg`, bit 2: Low Power Link Up. §8.1's Table 8-1 then resolves
    /// "10 full-duplex" first and "1000 full-duplex" last.
    pub const LOW_POWER_LINK_UP: u16 = 1 << 2;
    /// `a1000_dis`, bit 6: "When set to 1b, 1000 Mb/s speed is disabled." Its
    /// footnote: "When PE_RST_N goes low (switches to SMBus), its value becomes
    /// 1b."
    pub const GIGABIT_DISABLED: u16 = 1 << 6;
    /// `Aneg_now`, bit 10: "Restart auto-negotiation. This bit is self
    /// clearing."
    pub const RESTART_AUTONEG: u16 = 1 << 10;
}

/// The OEM Bits a function in D0 is owed, from what the register held and
/// what the MAC's `PHY_CTRL` says the platform allows.
///
/// **The platform's D0a policy and nothing else.** PCH Vol 2 §8.2.5 gives
/// `PHY_CTRL` two bits that hold in D0a — "Global GbE Disable", bit 6, "in all
/// power states", and "LPLU in D0a", bit 1 — and two that hold only outside it,
/// bits 3 and 2, which the NVM sets by default because "GbE is not supported in
/// Sx states". A driver in D0 carries the first two into the PHY and clears
/// what the second two left there. Every other field of the register is
/// carried, as §9.1 asks, and the register's own restart goes with the write
/// only where `fwsm` allows a PHY reset, the condition the reset reads.
pub fn oem_bits_in_d0(found: u16, phy_ctrl: u32, fwsm: u32) -> u16 {
    use crate::regs::phy_ctrl;
    let mut wanted = found & !(oem_bits::LOW_POWER_LINK_UP | oem_bits::GIGABIT_DISABLED);
    if phy_ctrl & phy_ctrl::LPLU_D0A != 0 {
        wanted |= oem_bits::LOW_POWER_LINK_UP;
    }
    if phy_ctrl & phy_ctrl::GLOBAL_GBE_DISABLE != 0 {
        wanted |= oem_bits::GIGABIT_DISABLED;
    }
    if wake::phy_reset_allowed(fwsm) {
        wanted |= oem_bits::RESTART_AUTONEG;
    }
    wanted
}

/// §9.3: "Setting the page is done by writing page_num x 32 to Register 31.
/// This is because only the 11 MSBs of register 31 are used for defining the
/// page."
pub(crate) const PAGE_SHIFT: u32 = 5;

/// Control register bits (§9.5.2.1).
pub(crate) mod control {
    /// Restart Auto-Negotiation (bit 9), self-clearing. §9.5.2.1: "1b =
    /// Restarts auto-negotiation process."
    pub const RESTART_AUTONEG: u16 = 1 << 9;
    /// Isolate (bit 10). §9.5.2.1: set, it "isolates the PHY from the MII or
    /// GMII interfaces" — which is the MAC's only path to it.
    pub const ISOLATE: u16 = 1 << 10;
    /// Power Down (bit 11). §9.5.2.1: "1b = Power down."
    pub const POWER_DOWN: u16 = 1 << 11;
    /// Auto-Negotiation Enable (bit 12). §9.5.2.1: cleared, "the link
    /// configuration is determined manually".
    pub const AUTONEG_ENABLE: u16 = 1 << 12;
    /// Loopback (bit 14), the "master enable for digital and analog loopback".
    pub const LOOPBACK: u16 = 1 << 14;
    /// Reset (bit 15). §9.5.2.1: "Writing a 1b to this bit causes immediate PHY
    /// reset."
    pub const RESET: u16 = 1 << 15;
}

/// Auto-Negotiation Advertisement bits (§9.5.2.5).
pub(crate) mod advertise {
    /// Selector Field (bits 4:0). §9.5.2.5: "00001b = IEEE 802.3 CSMA/CD."
    pub const SELECTOR_802_3: u16 = 0b00001;
    pub const HALF_10: u16 = 1 << 5;
    pub const FULL_10: u16 = 1 << 6;
    pub const HALF_100: u16 = 1 << 7;
    pub const FULL_100: u16 = 1 << 8;

    /// Everything below a gigabit this link may fall back to, and no flow
    /// control: this driver negotiates none, and `CTRL.RFCE` and `CTRL.TFCE`
    /// are what §4.6.3.2 would have it set from the resolution if it did.
    pub const WANTED: u16 = SELECTOR_802_3 | HALF_10 | FULL_10 | HALF_100 | FULL_100;
}

/// 1000BASE-T Control bits (§9.5.2.10).
pub(crate) mod control_1000t {
    /// Advertise 1000BASE-T Full-Duplex Capability (bit 9).
    pub const FULL: u16 = 1 << 9;
}

/// Custom Mode Control bits (§9.5.3.1).
pub(crate) mod custom_mode {
    /// MDIO frequency access (bit 10). §9.5.3.1: "1b = reduced MDIO frequency
    /// access", and §9.2 says access "should be done only when bit 10 in page
    /// 769 register 16 is set".
    pub const REDUCED_MDIO_FREQUENCY: u16 = 1 << 10;
}

/// §9.5.2.3's default for PHY Identifier 1: "the PHY identifier composed of bits
/// 3 through 18 of the Organizationally Unique Identifier", which for Intel's
/// assigned 00-AA-00 after bit reversal is this. It is the one word in the PHY
/// that says a read reached the PHY: neither a window that answers ones nor an
/// address nothing drives can produce it.
pub(crate) const IDENTIFIER_HIGH_INTEL: u16 = 0x0154;

/// Which of §4.5.2's two agents that are not software one reading of
/// `EXTCNF_CTRL` names.
///
/// **The software bit is left out because around a request it is this driver's
/// own**: whether it read back set is the grant, and the one reading in which
/// it is somebody else's is [`PhyRefusal::SoftwareFlagStood`], which is what
/// this type accompanies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Others {
    Nobody,
    Hardware,
    Manageability,
    HardwareAndManageability,
}

impl Others {
    pub fn in_reading(extcnf: u32) -> Self {
        match (
            extcnf & extcnf::MDIO_HW_OWNERSHIP != 0,
            extcnf & extcnf::MDIO_MNG_OWNERSHIP != 0,
        ) {
            (false, false) => Self::Nobody,
            (true, false) => Self::Hardware,
            (false, true) => Self::Manageability,
            (true, true) => Self::HardwareAndManageability,
        }
    }
}

impl core::fmt::Display for Others {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Nobody => "nobody else",
            Self::Hardware => "the part's own hardware",
            Self::Manageability => "the manageability agent",
            Self::HardwareAndManageability => "the part's own hardware and the manageability agent",
        })
    }
}

/// Why the PHY was not reached, or not what this driver was told it would be.
///
/// **None of these ends the bring-up.** A MAC whose PHY this driver could not
/// configure is a link it cannot raise, not a register file it has mistaken for
/// another — and firmware may have left the link up already, which `STATUS.LU`
/// reports whatever happened here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhyRefusal {
    /// A register this sequence reaches answered ones, so nothing decodes it
    /// and no write was made to it.
    Unrouted { reg: usize },
    /// §4.5.2's *software* bit was already set when this driver looked and
    /// stayed set for [`ARBITRATION_DEADLINE_NANOS`], so it is another software
    /// agent's — a grant it holds under one reading of the clause, a flag it
    /// set under the other. **No request was registered**: this driver's own
    /// request is the same bit, so one written over that flag could not be told
    /// from it, and the release below would clear a flag this driver never set.
    ///
    /// `beside` is the *last* reading before the deadline: the arbitration
    /// moves while a driver waits on it, and who stood beside that flag when
    /// the wait was given up on is what a machine can act on.
    SoftwareFlagStood { beside: Others, after_nanos: u64 },
    /// The request was registered and the software bit never read back set
    /// inside [`ARBITRATION_DEADLINE_NANOS`]. **The request has been
    /// withdrawn**, or it would stand for a grant nobody is waiting on.
    ///
    /// The word and not an [`Others`], because a grant that never came names
    /// nobody: the last reading here may have every bit clear, which is the
    /// arbitration between two agents and not one of them holding it.
    GrantNeverCame { held_by: u32, after_nanos: u64 },
    /// §8.2.3's `Wait` bit stood for the whole of [`MDI_DEADLINE_NANOS`], and
    /// while it stands "the ME/Host should not issue new MDIC transactions" —
    /// so no command was written.
    MdiWaiting { phy: u8, reg: u8, after_nanos: u64 },
    /// §10.2.2.7's `Ready` bit never came back for one transaction.
    MdiUnready { phy: u8, reg: u8, after_nanos: u64 },
    /// §10.2.2.7's `Error` bit: the part "fails to complete an MDI read".
    MdiError { phy: u8, reg: u8 },
    /// §9.5.2.3's identifier is not Intel's at either of §9.3's two PHY
    /// addresses, so nothing this driver knows the register map of answered.
    Identity { specific: u32, general: u32 },
    /// The part's PHY is not the one §9 describes. §10.2.2.7 addresses the
    /// 82574's own as "1 = Gigabit PHY. 2 = PCIe PHY" and it has neither §9.3's
    /// page register nor §9.5.3's paged registers, so this sequence would be
    /// aimed at registers that are not there.
    NotThisRegisterMap,
}

impl core::fmt::Display for PhyRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unrouted { reg } => write!(
                f,
                "register {reg:#x} answers ones, so nothing decodes it and this driver wrote \
                 nothing into it"
            ),
            Self::SoftwareFlagStood { beside, after_nanos } => write!(
                f,
                "§4.5.2's software ownership bit was another agent's for {after_nanos} ns, with \
                 {beside} beside it, so no request of this driver's was ever registered over it"
            ),
            Self::GrantNeverCame { held_by, after_nanos } => write!(
                f,
                "EXTCNF_CTRL read {held_by:#x} for {after_nanos} ns after this driver registered \
                 §4.5.2's request, which was never granted and has been withdrawn"
            ),
            Self::MdiWaiting { phy, reg, after_nanos } => write!(
                f,
                "MDIC.Wait stood for {after_nanos} ns, so the transaction on PHY {phy} register \
                 {reg} was never issued: §8.2.3 says the host \"should not issue new MDIC \
                 transactions while this bit is set\""
            ),
            Self::MdiUnready { phy, reg, after_nanos } => write!(
                f,
                "an MDI transaction on PHY {phy} register {reg} was still not ready \
                 {after_nanos} ns after it was written"
            ),
            Self::MdiError { phy, reg } => write!(
                f,
                "the part reported MDIC.E on PHY {phy} register {reg}, which is a transaction \
                 it could not complete"
            ),
            Self::Identity { specific, general } => write!(
                f,
                "the PHY identifier reads {specific:#010x} at PHY address {SPECIFIC:02} and \
                 {general:#010x} at {GENERAL:02}, and Intel's own OUI makes its high word \
                 {IDENTIFIER_HIGH_INTEL:#06x} wherever the PHY is"
            ),
            Self::NotThisRegisterMap => write!(
                f,
                "this part's PHY answers MDIC under the 82574's own addressing and not the \
                 I219 register map this bring-up is written from"
            ),
        }
    }
}

/// What the PHY answered while this driver held it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Phy {
    /// Which of §9.3's two PHY addresses answered §9.5.2.3, and therefore the
    /// one every other register below was reached at.
    pub addr: u8,
    /// §9.5.2.3 and §9.5.2.4's two registers, the high one first.
    pub id: u32,
    /// §9.5.3.2's Port General Configuration as the bring-up found it, before
    /// it cleared `Host_WU_Active` out of it.
    pub port_general: u16,
    /// §9.5.8.2's OEM Bits: as found, as written, and as read back after.
    pub oem: Oem,
}

/// One bring-up's account of §9.5.8.2's OEM Bits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Oem {
    pub found: u16,
    pub wrote: u16,
    /// Read straight after the write: `Aneg_now` has cleared itself or is still
    /// running, and the two speed bits say whether anything replaced them.
    pub after: u16,
}

impl Oem {
    fn words(bits: u16) -> &'static str {
        match (
            bits & oem_bits::LOW_POWER_LINK_UP != 0,
            bits & oem_bits::GIGABIT_DISABLED != 0,
        ) {
            (false, false) => "no low-power link-up, gigabit allowed",
            (true, false) => "low-power link-up, gigabit allowed",
            (false, true) => "no low-power link-up, gigabit disabled",
            (true, true) => "low-power link-up, gigabit disabled",
        }
    }
}

impl core::fmt::Display for Oem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "its OEM Bits read {:#06x} ({}), were written {:#06x} and read {:#06x} after ({})",
            self.found,
            Oem::words(self.found),
            self.wrote,
            self.after,
            Oem::words(self.after)
        )
    }
}

impl core::fmt::Display for Phy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "answers at PHY address {:02} as {:#010x}, its Port General Configuration read \
             {:#06x}{}; {}",
            self.addr,
            self.id,
            self.port_general,
            if self.port_general & port_general::HOST_WAKE_UP_ACTIVE != 0 {
                " with host wake-up left armed, which was cleared"
            } else {
                ""
            },
            self.oem
        )
    }
}

/// What one probe boot answers, with every field dropped: the bring-up's
/// outcome and, where it brought the PHY up, the link that came after it —
/// which is what one exit code carries off a machine whose console reaches
/// nobody.
///
/// **One table, read at both ends.** netd exits with [`Outcome::exit_code`]
/// when its caller asks for the outcome that way, the kernel records the code
/// in its `exit:` record, and the harness reads the record back through
/// [`Outcome::from_exit_code`]. The block starts at 64: clear of 0, a clean
/// exit, of the codes at 128 and above that a process ends with when it did
/// not choose its own end, and of 101, which the Rust runtime ends a panicking
/// netd with and which would therefore read back as an outcome the PHY never
/// gave.
///
/// **A PHY that came up is seven codes and not one.** The question the probe
/// is flashed for is whether the cable came up, and the only channel it has is
/// this number — so the link's absence and each speed and duplex §10.2.2.2's
/// `STATUS` can resolve to has a code of its own.
///
/// **[`PhyRefusal::SoftwareFlagStood`] is four codes and not one**, for the
/// same reason: a flag that is another software agent's is the one refusal
/// whose cause is another agent, and which of §4.5.2's other two stood beside
/// it is what a machine with no console cannot otherwise say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(i32)]
pub enum Outcome {
    BroughtUpNoLink = 64,
    LinkAt10Half = 65,
    LinkAt10Full = 66,
    LinkAt100Half = 67,
    LinkAt100Full = 68,
    LinkAt1000Half = 69,
    LinkAt1000Full = 70,
    Unrouted = 71,
    SoftwareFlagStood = 72,
    SoftwareFlagStoodBesideHardware = 73,
    SoftwareFlagStoodBesideManageability = 74,
    SoftwareFlagStoodBesideBoth = 75,
    GrantNeverCame = 76,
    MdiUnready = 77,
    /// **Out of order because a code is never reused and never renumbered**:
    /// every code above is already on a boot log somewhere. 82 is retired: it
    /// was a refusal this driver no longer makes.
    MdiWaiting = 81,
    MdiError = 78,
    Identity = 79,
    NotThisRegisterMap = 80,
}

impl core::fmt::Display for Outcome {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.link() {
            Some(Link::Up { speed, full_duplex }) => write!(
                f,
                "the PHY was brought up and the link came at {} Mb/s {}",
                speed.mbps(),
                if full_duplex { "full duplex" } else { "half duplex" }
            ),
            Some(Link::Down) => {
                write!(f, "the PHY was brought up and no link came inside the driver's bound")
            }
            None => write!(f, "the bring-up refused the PHY: {}", self.refusal_word()),
        }
    }
}

impl Outcome {
    /// The clause each refusal stands for, in a word — the other half of
    /// [`Display`], and never reached for an outcome that brought the PHY up.
    ///
    /// [`Display`]: core::fmt::Display
    fn refusal_word(self) -> &'static str {
        match self {
            Self::BroughtUpNoLink
            | Self::LinkAt10Half
            | Self::LinkAt10Full
            | Self::LinkAt100Half
            | Self::LinkAt100Full
            | Self::LinkAt1000Half
            | Self::LinkAt1000Full => "it did not",
            Self::Unrouted => "a register it reaches answers ones",
            Self::SoftwareFlagStood => "§4.5.2's software flag was another agent's",
            Self::SoftwareFlagStoodBesideHardware => {
                "§4.5.2's software flag was another agent's, the part's own hardware beside it"
            }
            Self::SoftwareFlagStoodBesideManageability => {
                "§4.5.2's software flag was another agent's, the manageability agent beside it"
            }
            Self::SoftwareFlagStoodBesideBoth => {
                "§4.5.2's software flag was another agent's, the part's own hardware and the \
                 manageability agent beside it"
            }
            Self::GrantNeverCame => "§4.5.2's request was never granted",
            Self::MdiWaiting => "§8.2.3's Wait bit never went, so no transaction was issued",
            Self::MdiUnready => "an MDI transaction never reported §10.2.2.7's Ready",
            Self::MdiError => "the part reported §10.2.2.7's Error on a transaction",
            Self::Identity => "§9.5.2.3's identifier is not Intel's at either PHY address",
            Self::NotThisRegisterMap => "this part's PHY is not the one §9 describes",
        }
    }

    /// Every outcome, in exit-code order.
    pub const ALL: [Self; 18] = [
        Self::BroughtUpNoLink,
        Self::LinkAt10Half,
        Self::LinkAt10Full,
        Self::LinkAt100Half,
        Self::LinkAt100Full,
        Self::LinkAt1000Half,
        Self::LinkAt1000Full,
        Self::Unrouted,
        Self::SoftwareFlagStood,
        Self::SoftwareFlagStoodBesideHardware,
        Self::SoftwareFlagStoodBesideManageability,
        Self::SoftwareFlagStoodBesideBoth,
        Self::GrantNeverCame,
        Self::MdiUnready,
        Self::MdiError,
        Self::Identity,
        Self::NotThisRegisterMap,
        Self::MdiWaiting,
    ];

    /// The outcome of a bring-up, and of the link that followed it.
    ///
    /// **`link` is read only where the PHY came up**: what a part whose PHY
    /// this driver never configured reports about its link is the agent before
    /// it talking, and the refusal is the whole of what that boot has to say.
    pub fn of(phy: Result<Phy, PhyRefusal>, link: Link) -> Self {
        match phy {
            Ok(_) => match link {
                Link::Down => Self::BroughtUpNoLink,
                Link::Up { speed: Speed::Mbps10, full_duplex: false } => Self::LinkAt10Half,
                Link::Up { speed: Speed::Mbps10, full_duplex: true } => Self::LinkAt10Full,
                Link::Up { speed: Speed::Mbps100, full_duplex: false } => Self::LinkAt100Half,
                Link::Up { speed: Speed::Mbps100, full_duplex: true } => Self::LinkAt100Full,
                Link::Up { speed: Speed::Mbps1000, full_duplex: false } => Self::LinkAt1000Half,
                Link::Up { speed: Speed::Mbps1000, full_duplex: true } => Self::LinkAt1000Full,
            },
            Err(PhyRefusal::Unrouted { .. }) => Self::Unrouted,
            Err(PhyRefusal::SoftwareFlagStood { beside, .. }) => match beside {
                Others::Nobody => Self::SoftwareFlagStood,
                Others::Hardware => Self::SoftwareFlagStoodBesideHardware,
                Others::Manageability => Self::SoftwareFlagStoodBesideManageability,
                Others::HardwareAndManageability => Self::SoftwareFlagStoodBesideBoth,
            },
            Err(PhyRefusal::GrantNeverCame { .. }) => Self::GrantNeverCame,
            Err(PhyRefusal::MdiWaiting { .. }) => Self::MdiWaiting,
            Err(PhyRefusal::MdiUnready { .. }) => Self::MdiUnready,
            Err(PhyRefusal::MdiError { .. }) => Self::MdiError,
            Err(PhyRefusal::Identity { .. }) => Self::Identity,
            Err(PhyRefusal::NotThisRegisterMap) => Self::NotThisRegisterMap,
        }
    }

    /// The link this outcome carries, or `None` where the PHY was not brought
    /// up at all — on which the link is the agent before this driver's and
    /// never this bring-up's reading.
    pub fn link(self) -> Option<Link> {
        let up = |speed, full_duplex| Some(Link::Up { speed, full_duplex });
        match self {
            Self::BroughtUpNoLink => Some(Link::Down),
            Self::LinkAt10Half => up(Speed::Mbps10, false),
            Self::LinkAt10Full => up(Speed::Mbps10, true),
            Self::LinkAt100Half => up(Speed::Mbps100, false),
            Self::LinkAt100Full => up(Speed::Mbps100, true),
            Self::LinkAt1000Half => up(Speed::Mbps1000, false),
            Self::LinkAt1000Full => up(Speed::Mbps1000, true),
            Self::Unrouted
            | Self::SoftwareFlagStood
            | Self::SoftwareFlagStoodBesideHardware
            | Self::SoftwareFlagStoodBesideManageability
            | Self::SoftwareFlagStoodBesideBoth
            | Self::GrantNeverCame
            | Self::MdiWaiting
            | Self::MdiUnready
            | Self::MdiError
            | Self::Identity
            | Self::NotThisRegisterMap => None,
        }
    }

    /// Whether the PHY was brought up, whatever the link then did.
    pub fn came_up(self) -> bool {
        self.link().is_some()
    }

    pub fn exit_code(self) -> i32 {
        self as i32
    }

    /// The outcome an exit code names, or `None` for a code outside
    /// [`Self::ALL`] — a process that ended for some other reason.
    pub fn from_exit_code(code: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|outcome| outcome.exit_code() == code)
    }
}

/// Every access this driver makes to §4.5.2's arbitration, paced.
///
/// **The pace is the whole of what this adds.** The register is one word two
/// other agents read and write too, and [`ARBITRATION_PACE_NANOS`] is how much
/// of it this driver leaves them between two accesses of its own.
pub(crate) struct Arbitration<'a, R: Registers, C: Clock> {
    regs: &'a R,
    clock: &'a C,
    /// When this driver last reached the register, and `None` before its first
    /// access: nothing is owed a pace ahead of the first one.
    last: Cell<Option<u64>>,
}

impl<'a, R: Registers, C: Clock> Arbitration<'a, R, C> {
    /// `last` is when this driver last reached the flag's registers under an
    /// earlier hold, so the pace runs across two holds as it runs inside one.
    fn over(regs: &'a R, clock: &'a C, last: Option<u64>) -> Self {
        Self { regs, clock, last: Cell::new(last) }
    }

    /// Give the processor away until [`ARBITRATION_PACE_NANOS`] has passed
    /// since this driver's last access to the register.
    ///
    /// A loop, because a pause decides nothing: it may return early, and what
    /// says the pace has been kept is the clock.
    fn pace(&self) {
        let Some(last) = self.last.get() else {
            return;
        };
        loop {
            let since = self.clock.nanos().saturating_sub(last);
            if since >= ARBITRATION_PACE_NANOS {
                return;
            }
            self.clock.pause(ARBITRATION_PACE_NANOS - since);
        }
    }

    /// One paced read of any register reached under the claim.
    ///
    /// **The pace is over the agents and not over one register.** §8.2.4 calls
    /// what the flag arbitrates "shared CSR registers with the firmware and
    /// hardware" — a set and not a word — so every access this driver makes
    /// under the flag is paced from the same clock reading, and `MDIC`'s own
    /// poll is the one exception (it is the transaction, not the arbitration).
    fn read_at(&self, reg: usize) -> u32 {
        self.pace();
        let reading = self.regs.read(reg);
        self.last.set(Some(self.clock.nanos()));
        reading
    }

    fn write_at(&self, reg: usize, value: u32) {
        self.pace();
        self.regs.write(reg, value);
        self.last.set(Some(self.clock.nanos()));
    }

    /// Give the processor away until `nanos` has passed since this driver's
    /// last access — a wait the part is owed after a step, which
    /// the pace alone would not keep where it is the shorter of the two.
    fn settle(&self, nanos: u64) {
        let since = self.last.get().unwrap_or_else(|| self.clock.nanos());
        hold(self.clock, since, nanos);
    }

    fn read(&self) -> u32 {
        self.read_at(regs::EXTCNF_CTRL)
    }

    fn write(&self, value: u32) {
        self.write_at(regs::EXTCNF_CTRL, value);
    }

    /// Take this driver's software bit out of the register, reading it again
    /// for as long as it answers ones and at most [`ARBITRATION_DEADLINE_NANOS`]
    /// from now: a word of ones is a window that is not decoding this read, and
    /// a bit left standing because of one would keep every other agent off the
    /// interface for the boot. A bit that reads clear is not written, and past
    /// the bound nothing is written at all — a composed word would set every
    /// field of a register this driver owns one bit of.
    fn withdraw(&self) {
        let started = self.clock.nanos();
        loop {
            let held = self.read();
            if held != u32::MAX {
                if held & extcnf::MDIO_SW_OWNERSHIP != 0 {
                    self.write(held & !extcnf::MDIO_SW_OWNERSHIP);
                }
                return;
            }
            if self.clock.nanos().saturating_sub(started) >= ARBITRATION_DEADLINE_NANOS {
                return;
            }
        }
    }
}

/// Give the processor away until `nanos` has passed since `since` on `clock`.
///
/// A loop, because a pause decides nothing: what says the time has passed is
/// the clock.
pub(crate) fn hold<C: Clock>(clock: &C, since: u64, nanos: u64) {
    loop {
        let passed = clock.nanos().saturating_sub(since);
        if passed >= nanos {
            return;
        }
        clock.pause(nanos - passed);
    }
}

/// §10.2.2.7's command word: the data, the five-bit register address, the
/// five-bit PHY address and the op-code, and `Ready` clear — "it should be
/// reset to 0b by software at the same time the command is written".
pub(crate) fn command(phy: u8, reg: u8, op: u32, data: u16) -> u32 {
    (data as u32 & mdic::DATA_MASK)
        | ((reg as u32 & mdic::ADDRESS_MASK) << mdic::REGADD_SHIFT)
        | ((phy as u32 & mdic::ADDRESS_MASK) << mdic::PHYADD_SHIFT)
        | op
}

/// The MDIO interface, held under §4.5.2's software ownership for as long as
/// this value lives.
///
/// **The release is a `Drop` and not a call**: §4.5.2 says "once the access
/// completes, the controlling agent must write a 0b to its ownership bit to
/// enable accesses by the other agents", and a bring-up that returned early
/// from any of the transactions below would otherwise leave the Management
/// Engine locked out for the boot. Every `?` in [`bring_up`] is such a return.
pub(crate) struct Owned<'a, R: Registers, C: Clock> {
    mdio: Arbitration<'a, R, C>,
}

impl<'a, R: Registers, C: Clock> Owned<'a, R, C> {
    /// §4.5.2: "A request for ownership is registered by writing a 1b into the
    /// respective bit [...] The requesting agent is granted access when the
    /// same bit is read as 1b (such as, access is not granted as long as the
    /// bit is 0b)."
    ///
    /// **The request is registered whoever else's bit stands.** §4.5.2 gives
    /// the arbitration a priority order — "manageability, software and then
    /// hardware" — which is a statement about requests that are waiting, so an
    /// interface another agent holds is one this driver asks for and never one
    /// it declines to ask about.
    ///
    /// **The one bit that is not asked over is a software bit already set.**
    /// This driver's request is that same bit, so one registered on top of
    /// another software agent's could not be told from its grant, and the
    /// release would clear a flag this driver never set.
    ///
    /// `after` is when this driver last gave an earlier hold back, and the
    /// first access of this one is paced from it.
    pub(crate) fn claim(regs: &'a R, clock: &'a C, after: Option<u64>) -> Result<Self, PhyRefusal> {
        let mdio = Arbitration::over(regs, clock, after);
        // §4.5.2 arbitrates three bits of this register, so one bit is the whole
        // of what this driver writes in it and the rest is read and carried.
        // The read and the write are two accesses and an agent writing between
        // them loses what it wrote — which a composed word would lose on every
        // write instead of on a race.
        let mut held = mdio.read();
        // Ones is nothing decoding this offset, and the write below would
        // otherwise set every field of a register this driver owns one bit of.
        if held == u32::MAX {
            return Err(PhyRefusal::Unrouted { reg: regs::EXTCNF_CTRL });
        }
        let started = clock.nanos();
        // Re-read on every turn, so the refusal carries who stood beside that
        // flag when the wait ended and not who stood there when it began — the
        // arbitration moves underneath a driver waiting on it.
        while held & extcnf::MDIO_SW_OWNERSHIP != 0 {
            let waited = clock.nanos().saturating_sub(started);
            if waited >= ARBITRATION_DEADLINE_NANOS {
                return Err(PhyRefusal::SoftwareFlagStood {
                    beside: Others::in_reading(held),
                    after_nanos: waited,
                });
            }
            held = mdio.read();
            if held == u32::MAX {
                return Err(PhyRefusal::Unrouted { reg: regs::EXTCNF_CTRL });
            }
        }
        mdio.write(held | extcnf::MDIO_SW_OWNERSHIP);
        let asked_at = clock.nanos();
        loop {
            let reading = mdio.read();
            // Ones before the bit is read out of it: every bit of that word is
            // set, and a grant read out of a window nothing decodes would put
            // this driver on `MDIC` with no part behind it. The request is
            // standing, so it is withdrawn before the refusal goes up.
            if reading == u32::MAX {
                mdio.withdraw();
                return Err(PhyRefusal::Unrouted { reg: regs::EXTCNF_CTRL });
            }
            if reading & extcnf::MDIO_SW_OWNERSHIP != 0 {
                return Ok(Self { mdio });
            }
            let waited = clock.nanos().saturating_sub(asked_at);
            if waited >= ARBITRATION_DEADLINE_NANOS {
                // The request itself is withdrawn: a bit left standing would be
                // a request for a grant nobody is waiting on, and the agent it
                // is registered against would answer it to nobody.
                mdio.write(reading & !extcnf::MDIO_SW_OWNERSHIP);
                return Err(PhyRefusal::GrantNeverCame { held_by: reading, after_nanos: waited });
            }
        }
    }

    /// One MDI transaction, written and then polled to its `Ready` bit.
    ///
    /// §10.2.2.7 gives the same shape to a read and a write: the command word
    /// carries `Ready` clear — "it should be reset to 0b by software at the
    /// same time the command is written" — and the part sets it "at the end of
    /// the MDI transaction". `Error` is read first, because a transaction the
    /// part could not complete still ends and still sets `Ready` over a data
    /// field that is not what the PHY said.
    pub(crate) fn transact(&self, phy: u8, reg: u8, op: u32, data: u16) -> Result<u16, PhyRefusal> {
        // §8.2.3: while `Wait` stands "the ME/Host should not issue new MDIC
        // transactions", and the bit "is auto cleared by hardware after the
        // transition has occurred" — so it is waited out rather than acted on,
        // and a part that never clears it is refused with nothing written.
        let waiting_since = self.mdio.clock.nanos();
        loop {
            if self.mdio.regs.read(regs::MDIC) & mdic::WAIT == 0 {
                break;
            }
            let waited = self.mdio.clock.nanos().saturating_sub(waiting_since);
            if waited >= MDI_DEADLINE_NANOS {
                return Err(PhyRefusal::MdiWaiting { phy, reg, after_nanos: waited });
            }
        }
        let command = command(phy, reg, op, data);
        self.mdio.regs.write(regs::MDIC, command);
        let started = self.mdio.clock.nanos();
        loop {
            let answer = self.mdio.regs.read(regs::MDIC);
            if answer & mdic::ERROR != 0 {
                return Err(PhyRefusal::MdiError { phy, reg });
            }
            if answer & mdic::READY != 0 {
                return Ok((answer & mdic::DATA_MASK) as u16);
            }
            let waited = self.mdio.clock.nanos().saturating_sub(started);
            if waited >= MDI_DEADLINE_NANOS {
                return Err(PhyRefusal::MdiUnready { phy, reg, after_nanos: waited });
            }
        }
    }


    /// One sweep of §8.2's five power and handshake registers, paced as every
    /// other access under the flag is.
    pub(crate) fn power_reading(&self) -> Reading {
        let seen = |reg| self.mdio.read_at(reg);
        Reading {
            ctrl: seen(regs::CTRL),
            ctrl_ext: seen(regs::CTRL_EXT),
            phy_ctrl: seen(regs::PHY_CTRL),
            fwsm: seen(regs::FWSM),
            mdic: seen(regs::MDIC),
        }
    }

    /// Put the part in the state §8.2.1 describes, and read back what it then
    /// holds.
    ///
    /// **Three sweeps and not two would be one too many**: the reading before,
    /// the write the reading calls for, and the reading after. A part already
    /// in that state is written nothing at all, and its two readings are then
    /// the same sweep taken twice — which is what says the state is the part's
    /// and not this driver's.
    ///
    /// The registers that are *not* written are [`crate::power`]'s header:
    /// §8.2.2's `PHYPDEN`, §8.2.5's `PHY_CTRL` and §8.2.9's `FWSM`.
    pub(crate) fn settle_power(&self) -> Result<Settled, PhyRefusal> {
        let before = self.power_reading();
        if before.unrouted().is_some() {
            return Err(PhyRefusal::Unrouted { reg: regs::CTRL });
        }
        let wrote = before.correction();
        if let Some(value) = wrote {
            self.mdio.write_at(regs::CTRL, value);
        }
        let after = self.power_reading();
        if after.unrouted().is_some() {
            return Err(PhyRefusal::Unrouted { reg: regs::CTRL });
        }
        Ok(Settled { before, wrote, after })
    }

    fn read(&self, phy: u8, reg: u8) -> Result<u16, PhyRefusal> {
        self.transact(phy, reg, mdic::OP_READ, 0)
    }

    fn write(&self, phy: u8, reg: u8, data: u16) -> Result<(), PhyRefusal> {
        self.transact(phy, reg, mdic::OP_WRITE, data).map(|_| ())
    }

    /// §9.3: "For PHY address 01, in order to access registers other than 0-15,
    /// software should first set the page register to map to the appropriate
    /// page."
    fn select(&self, page: u16) -> Result<(), PhyRefusal> {
        self.write(GENERAL, reg::PAGE_SELECT, page << PAGE_SHIFT)
    }

    /// Which of §9.3's two addresses the PHY answers §9.5.2.3 at, and what it
    /// answered.
    ///
    /// **The document does not settle this and the part does.** §9.3's prose
    /// says registers 0 to 15 at PHY address 01 "are identical in all the pages
    /// and are the IEEE defined registers"; Table 9-1 places Control and the
    /// identifier at PHY address 02. Table 9-1's address is asked first because
    /// a table of exact placements is the more specific statement.
    fn identify(&self) -> Result<(u8, u32), PhyRefusal> {
        let mut answered = [0u32; 2];
        for (at, addr) in [SPECIFIC, GENERAL].into_iter().enumerate() {
            let high = self.read(addr, reg::IDENTIFIER_HIGH)?;
            let low = self.read(addr, reg::IDENTIFIER_LOW)?;
            answered[at] = ((high as u32) << 16) | low as u32;
            if high == IDENTIFIER_HIGH_INTEL {
                return Ok((addr, answered[at]));
            }
        }
        Err(PhyRefusal::Identity { specific: answered[0], general: answered[1] })
    }

    /// One paced access to a register §8.2.4's flag covers, for a caller
    /// outside this module that has to make it under the flag.
    pub(crate) fn write_paced(&self, reg: usize, value: u32) {
        self.mdio.write_at(reg, value);
    }

    /// Ask the PHY for §9.5.2.3's and §9.5.2.4's identifier, at each of §9.3's
    /// two addresses in [`Self::identify`]'s order, and keep `MDIC` as the last
    /// transaction left it: the part's own word for how the ask ended.
    ///
    /// **An address that ends its cycle with nothing is left for the next; a
    /// cycle that never ends stops the ask**, because the interconnect that did
    /// not carry one transaction carries none, whatever address it names.
    pub(crate) fn ask(&self) -> Answer {
        let mdic = || self.mdio.regs.read(regs::MDIC);
        let mut last = 0;
        for addr in [SPECIFIC, GENERAL] {
            let high = self.read(addr, reg::IDENTIFIER_HIGH);
            last = mdic();
            match high {
                Ok(high) if high != u16::MAX => {
                    let low = self.read(addr, reg::IDENTIFIER_LOW);
                    last = mdic();
                    match low {
                        Ok(low) if low != u16::MAX => {
                            return Answer::Answered {
                                addr,
                                id: ((high as u32) << 16) | low as u32,
                            }
                        }
                        Ok(_) | Err(PhyRefusal::MdiError { .. }) => {}
                        Err(PhyRefusal::MdiUnready { .. }) => {
                            return Answer::Silent { mdic: last }
                        }
                        Err(why) => return Answer::Refused(why),
                    }
                }
                Ok(_) | Err(PhyRefusal::MdiError { .. }) => {}
                Err(PhyRefusal::MdiUnready { .. }) => return Answer::Silent { mdic: last },
                Err(why) => return Answer::Refused(why),
            }
        }
        Answer::Failed { mdic: last }
    }

    /// Cycle `LANPHYPC` the way [`crate::wake`]'s header sets out: the PHY's
    /// configuration time first, the pin held low for one pace, the cycle-done
    /// bit waited on, and [`wake::POWER_CYCLE_SETTLE_NANOS`] after.
    fn power_cycle(&self) {
        let timing = self.mdio.read_at(regs::LANPHYPC_TIMING);
        self.mdio.write_at(regs::LANPHYPC_TIMING, wake::configuration_50ms(timing));
        let held = self.mdio.read_at(regs::CTRL);
        let low = wake::lanphypc_low(held);
        self.mdio.write_at(regs::CTRL, low);
        // The pace between these two writes is the hold, and it is wider than
        // the cycle's floor by a compile-time assertion.
        self.mdio.write_at(regs::CTRL, wake::lanphypc_released(low));
        let released_at = self.mdio.clock.nanos();
        loop {
            let ext = self.mdio.read_at(regs::CTRL_EXT);
            let waited = self.mdio.clock.nanos().saturating_sub(released_at);
            if wake::power_cycle_done(ext) || waited >= wake::POWER_CYCLE_DONE_DEADLINE_NANOS {
                break;
            }
        }
        self.mdio.settle(wake::POWER_CYCLE_SETTLE_NANOS);
    }

    fn force_smbus(&self) {
        let ext = self.mdio.read_at(regs::CTRL_EXT);
        self.mdio.write_at(regs::CTRL_EXT, wake::smbus_forced(ext));
        self.mdio.settle(wake::SMBUS_SETTLE_NANOS);
    }

    fn release_smbus(&self) {
        let ext = self.mdio.read_at(regs::CTRL_EXT);
        self.mdio.write_at(regs::CTRL_EXT, wake::smbus_released(ext));
    }

    /// A PHY that answered with the MAC on SMBus, taken back to PCIe at both
    /// ends: I219 §9.5.3.4's Force SMBus first, then the MAC's.
    ///
    /// **The write that moves the PHY is not judged by its `Error`**: that
    /// write ends in `Error` on this part whether or not the PHY took it, and
    /// the ask after this is what says whether the PHY is on PCIe.
    fn back_on_pcie(&self) {
        // Refused or not, the MAC's end is let go below and the ask after this
        // is the judge: a MAC left on SMBus is one no PCIe-side PHY answers.
        let _ = self.select(PAGE_PORT_CONTROL).and_then(|()| {
            let control = self.read(GENERAL, wake::SMBUS_CONTROL)?;
            match self.write(GENERAL, wake::SMBUS_CONTROL, wake::phy_smbus_released(control)) {
                Err(PhyRefusal::MdiError { .. }) => Ok(()),
                other => other,
            }
        });
        self.release_smbus();
    }
}

impl<R: Registers, C: Clock> Drop for Owned<'_, R, C> {
    /// §4.5.2: "the controlling agent must write a 0b to its ownership bit".
    ///
    /// The bit is this driver's to clear because [`Owned::claim`] saw it clear
    /// before it registered the request that set it. A flag the part already
    /// let go — a reset clears it — is not written again: the write would
    /// carry every other field of a register three agents share for nothing.
    ///
    /// **After [`PhyRefusal::MdiUnready`] this releases with a transaction
    /// still in flight**, where §4.5.2 has the release follow the access's
    /// completion. A transaction this driver has already given
    /// [`MDI_DEADLINE_NANOS`] is not waited on a second time, and a bit left
    /// standing instead would keep every other agent off the interface for the
    /// boot.
    fn drop(&mut self) {
        self.mdio.withdraw();
    }
}

/// Take the MDIO interface, make the PHY able to bring a link up, and give the
/// interface back.
///
/// `reset_at` is when `CTRL.RST` was written, which §9.2's delay below is
/// measured from.
pub(crate) fn bring_up<R: Registers, C: Clock>(
    regs: &R,
    clock: &C,
    reset_at: u64,
    after: Option<u64>,
) -> Result<Phy, PhyRefusal> {
    // §9.2: "After LCD reset to the I219 a delay of 10 ms is required before
    // attempting to access MDIO registers." A wait and not a poll: the document
    // offers nothing to read that shortens it. The loop is because a pause
    // decides nothing — what says the delay is over is the clock.
    hold(clock, reset_at, LCD_RESET_DELAY_NANOS);

    let mdi = Owned::claim(regs, clock, after)?;

    // The MDI cycle is an in-band packet to the LAN Connected Device (I219
    // §1.2, §2.2.2.1.1), so a PHY the MAC holds powered down answers no cycle
    // at all. §8.2.1's bit is where that is put right — under the flag §8.2.4
    // arbitrates the shared CSRs with, and before any transaction is issued.
    let power = mdi.settle_power()?;

    // §9.2: "Access using MDIO should be done only when bit 10 in page 769
    // register 16 is set." The bit is itself reached over MDIO, so this one
    // pair of transactions is made at whatever frequency the part came up in
    // and every transaction after it is not.
    mdi.select(PAGE_PORT_CONTROL)?;
    let custom = mdi.read(GENERAL, reg::CUSTOM_MODE)?;
    mdi.write(GENERAL, reg::CUSTOM_MODE, custom | custom_mode::REDUCED_MDIO_FREQUENCY)?;

    // §7.4, step 6 of arming the PHY for wake-up: the host "should issue a LCD
    // reset to the I219 before clearing the Host_WU_Active bit" — which is
    // where this is, behind the reset that reached the PHY. A read-modify-write
    // for §9.1's sake, and no write at all where the bit is already clear.
    let port = mdi.read(GENERAL, reg::PORT_GENERAL)?;
    if port & port_general::HOST_WAKE_UP_ACTIVE != 0 {
        mdi.write(GENERAL, reg::PORT_GENERAL, port & !port_general::HOST_WAKE_UP_ACTIVE)?;
    }

    let (addr, id) = mdi.identify()?;

    // §9.5.2.5: "any write to the Auto-Negotiation Advertisement register,
    // prior to auto-negotiation completion, is followed by a restart of
    // auto-negotiation" — so both abilities are written before the restart
    // below, or the negotiation would run over the abilities they replaced.
    mdi.write(addr, reg::ADVERTISE, advertise::WANTED)?;
    mdi.write(addr, reg::CONTROL_1000T, control_1000t::FULL)?;

    // A read-modify-write, because §9.1 says "other fields in the same 16-bit
    // register must be loaded with their default values" and what is in them is
    // the configuration the integrated LAN controller left.
    let control = mdi.read(addr, reg::CONTROL)?;
    let wanted = (control
        & !(control::POWER_DOWN | control::ISOLATE | control::LOOPBACK | control::RESET))
        | control::AUTONEG_ENABLE
        | control::RESTART_AUTONEG;
    mdi.write(addr, reg::CONTROL, wanted)?;

    // Two restarts: the OEM Bits' own is what makes their speed bits take
    // effect (§8.1), and the Control register's above covers the firmware that
    // blocks a PHY reset, where the OEM Bits' is not written.
    mdi.select(PAGE_GENERAL)?;
    let found = mdi.read(GENERAL, reg::OEM_BITS)?;
    let wrote = oem_bits_in_d0(found, power.after.phy_ctrl, power.after.fwsm);
    mdi.write(GENERAL, reg::OEM_BITS, wrote)?;
    let after = mdi.read(GENERAL, reg::OEM_BITS)?;

    Ok(Phy { addr, id, port_general: port, oem: Oem { found, wrote, after } })
}

/// Bring the PHY within reach of `MDIC` before [`crate::I219::open`] resets
/// the part: ask it as found, and where it does not answer, climb
/// [`wake::LADDER`] one rung at a time with an ask after each.
///
/// **Every ask is recorded with the `MDIC` word it ended on**, so the account
/// says in the part's own words at which rung the PHY first answered — or that
/// it never did. A PHY that answers on SMBus is taken back to PCIe and asked
/// once more there, because PCIe is the interface this driver's link runs on
/// (I219 §12.1.4).
///
/// **The MAC is never left on SMBus.** Whatever ends the ladder — an answer, a
/// refusal, the last rung — a MAC this wake put on SMBus is taken off it before
/// the flag goes back.
///
/// **None of this refuses the bring-up.** A PHY still out of reach after the
/// last rung is [`bring_up`]'s to refuse by name after the reset, and the
/// wake's own account goes beside that refusal.
pub(crate) fn wake<R: Registers, C: Clock>(
    regs: &R,
    clock: &C,
    after: Option<u64>,
) -> Result<Woke, PhyRefusal> {
    let mdi = Owned::claim(regs, clock, after)?;
    // §8.2.1's bit first, as [`bring_up`] puts it after the reset: a MAC
    // holding its PHY down carries no cycle to it, so every ask below would be
    // an ask of the MAC and not of the PHY.
    let mut woke = Woke::found(mdi.settle_power()?);
    let first = mdi.ask();
    woke.push(Moment::AsFound, first);
    if !matches!(first, Answer::Silent { .. } | Answer::Failed { .. }) {
        return Ok(woke);
    }
    let firmware = mdi.mdio.read_at(regs::FWSM);
    let mut on_smbus = false;
    for (action, moment) in wake::LADDER {
        if !wake::rung_allowed(action, firmware) {
            continue;
        }
        match action {
            Action::PowerCycle => mdi.power_cycle(),
            Action::ForceSmbus => {
                mdi.force_smbus();
                on_smbus = true;
            }
            Action::ReleaseSmbus => {
                mdi.release_smbus();
                on_smbus = false;
            }
        }
        let answer = mdi.ask();
        woke.push(moment, answer);
        if answer.answered() && on_smbus {
            mdi.back_on_pcie();
            on_smbus = false;
            woke.push(Moment::BackOnPcie, mdi.ask());
        }
        if !matches!(answer, Answer::Silent { .. } | Answer::Failed { .. }) {
            break;
        }
    }
    // A ladder that stopped with the MAC still on SMBus leaves it there for
    // nobody: the bring-up after the reset asks over PCIe.
    if on_smbus {
        mdi.release_smbus();
    }
    Ok(woke)
}
