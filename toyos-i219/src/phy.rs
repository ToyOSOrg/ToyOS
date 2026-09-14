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

use crate::regs::{self, extcnf, mdic};
use crate::{Clock, Registers};

/// How long [`Owned::claim`] waits for §4.5.2's ownership bit to read back set.
///
/// **A driver-chosen bound, not a datasheet one**: §4.5.2 describes the
/// handshake and gives no time for it, so a part whose Management Engine never
/// lets go says so instead of holding up the boot.
const OWNERSHIP_DEADLINE_NANOS: u64 = 20_000_000;

/// How long [`Owned::transact`] waits for §10.2.2.7's `Ready` bit.
///
/// **A driver-chosen bound, not a datasheet one**: §10.2.2.7 gives the
/// transaction no time at all, so a part that never ends one says so instead of
/// holding up the boot.
const MDI_DEADLINE_NANOS: u64 = 1_000_000;

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
    /// §9.3: "Register 31 is the page register in all pages of PHY address 01."
    pub const PAGE_SELECT: u8 = 31;
}

/// The page §9.5.3's port control registers live in.
pub(crate) const PAGE_PORT_CONTROL: u16 = 769;

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

/// Why the PHY was not reached, or not what this driver was told it would be.
///
/// **None of these ends the bring-up.** A MAC whose PHY this driver could not
/// configure is a link it cannot raise, not a register file it has mistaken for
/// another — and firmware may have left the link up already, which `STATUS.LU`
/// reports whatever happened here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhyRefusal {
    /// §4.5.2's handshake never granted: the ownership bit did not read back
    /// set inside the deadline, so something else holds the interface.
    OwnershipBusy { held_by: u32, after_nanos: u64 },
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
            Self::OwnershipBusy { held_by, after_nanos } => write!(
                f,
                "EXTCNF_CTRL read {held_by:#x} for {after_nanos} ns and never granted this \
                 driver the MDIO interface"
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
}

/// The MDIO interface, held under §4.5.2's software ownership for as long as
/// this value lives.
///
/// **The release is a `Drop` and not a call**: §4.5.2 says "once the access
/// completes, the controlling agent must write a 0b to its ownership bit to
/// enable accesses by the other agents", and a bring-up that returned early
/// from any of the transactions below would otherwise leave the Management
/// Engine locked out for the boot.
struct Owned<'a, R: Registers, C: Clock> {
    regs: &'a R,
    clock: &'a C,
}

impl<'a, R: Registers, C: Clock> Owned<'a, R, C> {
    /// §4.5.2: "A request for ownership is registered by writing a 1b into the
    /// respective bit [...] The requesting agent is granted access when the
    /// same bit is read as 1b (such as, access is not granted as long as the
    /// bit is 0b)."
    fn claim(regs: &'a R, clock: &'a C) -> Result<Self, PhyRefusal> {
        let started = clock.nanos();
        // Not a read-modify-write: §10.2.2.15 gives this register's other two
        // ownership bits to the other agents read-only and every field outside
        // the three an initial value of 0x0, so there is nothing here to carry.
        regs.write(regs::EXTCNF_CTRL, extcnf::MDIO_SW_OWNERSHIP);
        loop {
            let held = regs.read(regs::EXTCNF_CTRL);
            if held & extcnf::MDIO_SW_OWNERSHIP != 0 {
                return Ok(Self { regs, clock });
            }
            let waited = clock.nanos().saturating_sub(started);
            if waited >= OWNERSHIP_DEADLINE_NANOS {
                // The request itself is withdrawn, or §4.5.2's "at most only
                // one bit is 1b" would be a bit this driver left standing for a
                // grant it is no longer waiting on.
                regs.write(regs::EXTCNF_CTRL, 0);
                return Err(PhyRefusal::OwnershipBusy { held_by: held, after_nanos: waited });
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
    fn transact(&self, phy: u8, reg: u8, op: u32, data: u16) -> Result<u16, PhyRefusal> {
        let command = (data as u32 & mdic::DATA_MASK)
            | ((reg as u32 & mdic::ADDRESS_MASK) << mdic::REGADD_SHIFT)
            | ((phy as u32 & mdic::ADDRESS_MASK) << mdic::PHYADD_SHIFT)
            | op;
        self.regs.write(regs::MDIC, command);
        let started = self.clock.nanos();
        loop {
            let answer = self.regs.read(regs::MDIC);
            if answer & mdic::ERROR != 0 {
                return Err(PhyRefusal::MdiError { phy, reg });
            }
            if answer & mdic::READY != 0 {
                return Ok((answer & mdic::DATA_MASK) as u16);
            }
            let waited = self.clock.nanos().saturating_sub(started);
            if waited >= MDI_DEADLINE_NANOS {
                return Err(PhyRefusal::MdiUnready { phy, reg, after_nanos: waited });
            }
        }
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
}

impl<R: Registers, C: Clock> Drop for Owned<'_, R, C> {
    fn drop(&mut self) {
        self.regs.write(regs::EXTCNF_CTRL, 0);
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
) -> Result<Phy, PhyRefusal> {
    // §9.2: "After LCD reset to the I219 a delay of 10 ms is required before
    // attempting to access MDIO registers." A wait and not a poll: the document
    // offers nothing to read that shortens it.
    while clock.nanos().saturating_sub(reset_at) < LCD_RESET_DELAY_NANOS {}

    let mdi = Owned::claim(regs, clock)?;

    // §9.2: "Access using MDIO should be done only when bit 10 in page 769
    // register 16 is set." The bit is itself reached over MDIO, so this one
    // pair of transactions is made at whatever frequency the part came up in
    // and every transaction after it is not.
    mdi.select(PAGE_PORT_CONTROL)?;
    let custom = mdi.read(GENERAL, reg::CUSTOM_MODE)?;
    mdi.write(GENERAL, reg::CUSTOM_MODE, custom | custom_mode::REDUCED_MDIO_FREQUENCY)?;

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

    Ok(Phy { addr, id })
}
