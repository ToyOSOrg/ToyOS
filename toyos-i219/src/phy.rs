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

use core::cell::Cell;

use crate::regs::{self, extcnf, mdic};
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
            Self::MdiUnready => "an MDI transaction never reported §10.2.2.7's Ready",
            Self::MdiError => "the part reported §10.2.2.7's Error on a transaction",
            Self::Identity => "§9.5.2.3's identifier is not Intel's at either PHY address",
            Self::NotThisRegisterMap => "this part's PHY is not the one §9 describes",
        }
    }

    /// Every outcome, in exit-code order.
    pub const ALL: [Self; 17] = [
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
struct Arbitration<'a, R: Registers, C: Clock> {
    regs: &'a R,
    clock: &'a C,
    /// When this driver last reached the register, and `None` before its first
    /// access: nothing is owed a pace ahead of the first one.
    last: Cell<Option<u64>>,
}

impl<'a, R: Registers, C: Clock> Arbitration<'a, R, C> {
    fn over(regs: &'a R, clock: &'a C) -> Self {
        Self { regs, clock, last: Cell::new(None) }
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

    fn read(&self) -> u32 {
        self.pace();
        let reading = self.regs.read(regs::EXTCNF_CTRL);
        self.last.set(Some(self.clock.nanos()));
        reading
    }

    fn write(&self, value: u32) {
        self.pace();
        self.regs.write(regs::EXTCNF_CTRL, value);
        self.last.set(Some(self.clock.nanos()));
    }
}

/// The MDIO interface, held under §4.5.2's software ownership for as long as
/// this value lives.
///
/// **The release is a `Drop` and not a call**: §4.5.2 says "once the access
/// completes, the controlling agent must write a 0b to its ownership bit to
/// enable accesses by the other agents", and a bring-up that returned early
/// from any of the transactions below would otherwise leave the Management
/// Engine locked out for the boot. Every `?` in [`bring_up`] is such a return.
struct Owned<'a, R: Registers, C: Clock> {
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
    /// it declines to ask about. On the T14 the part grants it while
    /// manageability's own bit stands: the clause's "at any given time at most
    /// only one bit is 1b" is not what that silicon answers, and the grant is
    /// therefore this driver's bit reading back set and never its bit alone.
    /// **A bench fact**: one run of that request on that machine read
    /// `EXTCNF_CTRL` as `0x00300089`, wrote `0x003000a9`, and read `0x003000a9`
    /// back.
    ///
    /// **The one bit that is not asked over is a software bit already set.**
    /// This driver's request is that same bit, so one registered on top of
    /// another software agent's could not be told from its grant, and the
    /// release would clear a flag this driver never set.
    fn claim(regs: &'a R, clock: &'a C) -> Result<Self, PhyRefusal> {
        let mdio = Arbitration::over(regs, clock);
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
            // this driver on `MDIC` with no part behind it.
            if reading == u32::MAX {
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
    fn transact(&self, phy: u8, reg: u8, op: u32, data: u16) -> Result<u16, PhyRefusal> {
        let command = (data as u32 & mdic::DATA_MASK)
            | ((reg as u32 & mdic::ADDRESS_MASK) << mdic::REGADD_SHIFT)
            | ((phy as u32 & mdic::ADDRESS_MASK) << mdic::PHYADD_SHIFT)
            | op;
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
    /// §4.5.2: "the controlling agent must write a 0b to its ownership bit".
    ///
    /// The bit is this driver's to clear because [`Owned::claim`] saw it clear
    /// before it registered the request that set it.
    ///
    /// **After [`PhyRefusal::MdiUnready`] this releases with a transaction
    /// still in flight**, where §4.5.2 has the release follow the access's
    /// completion. A transaction this driver has already given
    /// [`MDI_DEADLINE_NANOS`] is not waited on a second time, and a bit left
    /// standing instead would keep every other agent off the interface for the
    /// boot.
    fn drop(&mut self) {
        let held = self.mdio.read();
        // Ones is a window that stopped decoding under this driver, and the
        // write would set every field of the register it answered for.
        if held == u32::MAX {
            return;
        }
        self.mdio.write(held & !extcnf::MDIO_SW_OWNERSHIP);
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
    // offers nothing to read that shortens it. The loop is because a pause
    // decides nothing — what says the delay is over is the clock.
    loop {
        let since = clock.nanos().saturating_sub(reset_at);
        if since >= LCD_RESET_DELAY_NANOS {
            break;
        }
        clock.pause(LCD_RESET_DELAY_NANOS - since);
    }

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
