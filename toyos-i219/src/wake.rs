//! Bringing the I219 within reach of `MDIC`, and the reset that keeps it there
//! — every decision about both and none of the accesses that carry them out.
//!
//! # Why a PHY can be out of reach at all
//!
//! `MDIC` on this part is an in-band packet over the PCIe-based interconnect
//! or over SMBus (I219 §1.2, §2.2.2.1.1), and I219 §12.1.4 has "the PHY
//! automatically [switch] the in-band traffic between PCIe and SMBus based on
//! the platform power state". A PHY that is powered down, in Ultra Low Power
//! (I219 §6.4), or talking SMBus while the MAC talks PCIe answers no cycle, and
//! the MAC's `Ready` never comes back. I219 §6.4 gives ULP's exit to "the host
//! driver (on non ME systems) or the ME FW" and publishes no host sequence.
//!
//! # What this file stands on
//!
//! The documents are the *Intel Ethernet Connection I219 Datasheet* (612523,
//! rev 2.02) and the *Intel® 500 Series Chipset Family On-Package Platform
//! Controller Hub Datasheet, Volume 2* (631120, rev 002), whose §8.2 calls every
//! bit this file writes "Reserved". **The rest is the behaviour of Intel's own
//! Linux host driver for this family, stated as fact about the hardware**:
//!
//! - `CTRL` bits 16 and 17 put `LANPHYPC` — I219 §5.2's `LAN_DISABLE_N`, "the
//!   only external signal that can reset the PHY" — in the host's hand, and
//!   holding it low for at least 10 µs power-cycles the PHY; `FEXTNVM3`'s PHY
//!   configuration counter is set to 50 ms first, `CTRL_EXT` bit 2 reports the
//!   cycle done, and 30 ms more pass before the PHY is asked anything.
//! - `CTRL_EXT` bit 11 forces the MAC onto SMBus, and 50 ms pass after it
//!   before a cycle, for the MAC to finish retrying what it was doing; a PHY
//!   reached that way is taken back to PCIe by clearing I219 §9.5.3.4's Force
//!   SMBus bit and then the MAC's, and the write that switches the PHY ends in
//!   `MDIC.Error` by nature.
//! - That driver power-cycles the PHY on every load where the firmware reports
//!   itself not valid, and forces SMBus and cycles again where the identifier
//!   still does not answer.
//! - Its full reset writes `CTRL.RST` and `CTRL.PHY_RST` together whenever
//!   `FWSM` bit 6 allows a PHY reset, "to make sure the interface between MAC
//!   and the external PHY is reset", touches no register for 20 ms after it,
//!   and then waits on `STATUS.LAN_INIT_DONE`.
//!
//! **The pace these accesses are made at is [`crate::phy::ARBITRATION_PACE_NANOS`]
//! and not the host driver's**, because every one of them is under §8.2.4's
//! flag and the T14 is the one machine whose answer to a faster pace was a
//! power-off. The pace is wider than every floor above, and the constants
//! below assert it.

use crate::phy::PhyRefusal;
use crate::regs::{ctrl, ctrl_ext, fextnvm3, fwsm};

/// How long `LANPHYPC` is held low before the override is let go: the host
/// driver's floor.
pub const LANPHYPC_HOLD_NANOS: u64 = 10_000;

/// How long `CTRL_EXT`'s cycle-done bit is waited on: the host driver's twenty
/// readings five milliseconds apart. **Not a refusal when it runs out** — that
/// driver goes on either way, and so does this one: whether the PHY answers is
/// the question the next ask settles.
pub const POWER_CYCLE_DONE_DEADLINE_NANOS: u64 = 100_000_000;

/// How long passes after the cycle is done before the PHY is asked anything.
pub const POWER_CYCLE_SETTLE_NANOS: u64 = 30_000_000;

/// How long passes after the MAC is forced onto SMBus before a cycle is issued.
pub const SMBUS_SETTLE_NANOS: u64 = 50_000_000;

/// How long nothing in the register file is touched after a full reset.
pub const RESET_QUIET_NANOS: u64 = 20_000_000;

/// How long `STATUS.LAN_INIT_DONE` is waited on after a full reset: I219
/// Table 5-2's `Tr2init`, "completing a PHY configuration following a reset
/// complete indication", at most 0.5 s. **Not a refusal when it runs out**: the
/// reading is what the boot has to say, and the PHY's own answer after it is
/// the test of whether the configuration finished.
pub const LAN_INIT_DEADLINE_NANOS: u64 = 500_000_000;

/// How long passes between two readings of `STATUS` inside that wait.
pub const LAN_INIT_PACE_NANOS: u64 = 100_000;

const _: () = {
    // Every access of the power cycle is paced from the last, so the hold is
    // at least one pace long; a pace under the hold would let the override go
    // before the PHY lost power.
    assert!(crate::phy::ARBITRATION_PACE_NANOS >= LANPHYPC_HOLD_NANOS);
};

/// I219 §9.5.3.4's SMBus Control register, PHY address 01, page 769, register
/// 23, and its bit 0: "Force SMBus, reset on PCI reset de-assertion".
pub const SMBUS_CONTROL: u8 = 23;
pub const SMBUS_CONTROL_FORCE: u16 = 1 << 0;

/// Where in one boot the PHY is asked for its identifier.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Moment {
    /// The first ask of [`crate::I219::open`], before its reset.
    AsFound,
    /// After `LANPHYPC` was cycled.
    PowerCycled,
    /// After the MAC was forced onto SMBus.
    SmbusForced,
    /// After `LANPHYPC` was cycled with the MAC on SMBus.
    PowerCycledOnSmbus,
    /// After the MAC was let off SMBus again.
    SmbusReleased,
    /// After a PHY that answered on SMBus was taken back to PCIe, both ends.
    BackOnPcie,
}

impl Moment {
    pub const ALL: [Self; 6] = [
        Self::AsFound,
        Self::PowerCycled,
        Self::SmbusForced,
        Self::PowerCycledOnSmbus,
        Self::SmbusReleased,
        Self::BackOnPcie,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::AsFound => "as-found",
            Self::PowerCycled => "power-cycled",
            Self::SmbusForced => "smbus-forced",
            Self::PowerCycledOnSmbus => "power-cycled-on-smbus",
            Self::SmbusReleased => "smbus-released",
            Self::BackOnPcie => "back-on-pcie",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|moment| moment.name() == word)
    }
}

impl core::fmt::Display for Moment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// One thing done to reach a PHY that did not answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    PowerCycle,
    ForceSmbus,
    ReleaseSmbus,
}

/// The host driver's order, each rung followed by one ask: a cycle, SMBus, a
/// cycle on SMBus, and PCIe again. The ladder stops at the first answer.
pub const LADDER: [(Action, Moment); 4] = [
    (Action::PowerCycle, Moment::PowerCycled),
    (Action::ForceSmbus, Moment::SmbusForced),
    (Action::PowerCycle, Moment::PowerCycledOnSmbus),
    (Action::ReleaseSmbus, Moment::SmbusReleased),
];

/// What one ask for the identifier got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    /// A cycle ended with `Ready` and a word that is not ones: §9.5.2.3's and
    /// §9.5.2.4's registers at `addr`. Whether the word is Intel's is the
    /// bring-up's to judge, not this ask's.
    Answered { addr: u8, id: u32 },
    /// No cycle ended: `MDIC` still read the command, `Ready` and `Error` both
    /// clear. The last word read out of it.
    Silent { mdic: u32 },
    /// Every cycle ended, in `Error` or with ones at both addresses: the MAC
    /// ran it and nothing answered. The last word read out of `MDIC`.
    Failed { mdic: u32 },
    /// No cycle was issued, and the refusal says why.
    Refused(PhyRefusal),
}

impl Answer {
    pub fn answered(self) -> bool {
        matches!(self, Self::Answered { .. })
    }
}

impl core::fmt::Display for Answer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Answered { addr, id } => write!(f, "answered {id:#010x} at PHY address {addr:02}"),
            Self::Silent { mdic } => {
                write!(f, "never ended a cycle, MDIC still reading {mdic:#010x}")
            }
            Self::Failed { mdic } => {
                write!(f, "ended every cycle with nothing answering, MDIC {mdic:#010x}")
            }
            Self::Refused(why) => write!(f, "was not asked: {why}"),
        }
    }
}

/// Every ask one wake made, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Woke {
    asked: [Option<(Moment, Answer)>; ASKS],
}

/// The first ask, one after each rung of [`LADDER`], and one after taking a PHY
/// that answered on SMBus back to PCIe.
pub const ASKS: usize = 1 + LADDER.len() + 1;

impl Default for Woke {
    fn default() -> Self {
        Self { asked: [None; ASKS] }
    }
}

impl Woke {
    pub(crate) fn push(&mut self, moment: Moment, answer: Answer) {
        let slot = self.asked.iter_mut().find(|slot| slot.is_none());
        *slot.expect("a wake makes at most ASKS asks") = Some((moment, answer));
    }

    pub fn asked(&self) -> impl Iterator<Item = (Moment, Answer)> + '_ {
        self.asked.iter().flatten().copied()
    }

    /// Where the PHY answered, if it did: the last ask, because every ask
    /// after the first answer is the ladder's own cleanup.
    pub fn answered(&self) -> Option<(Moment, Answer)> {
        self.asked().last().filter(|(_, answer)| answer.answered())
    }
}

impl core::fmt::Display for Woke {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (at, (moment, answer)) in self.asked().enumerate() {
            if at > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{moment}: {answer}")?;
        }
        Ok(())
    }
}

/// `FEXTNVM3` with its PHY configuration counter at fifty milliseconds and
/// every other bit carried.
pub fn phy_cfg_counter(fextnvm3: u32) -> u32 {
    (fextnvm3 & !fextnvm3::PHY_CFG_COUNTER_MASK) | fextnvm3::PHY_CFG_COUNTER_50MS
}

/// `CTRL` with `LANPHYPC` in the host's hand and driven low.
pub fn lanphypc_low(ctrl: u32) -> u32 {
    (ctrl | ctrl::LANPHYPC_OVERRIDE) & !ctrl::LANPHYPC_VALUE
}

/// `CTRL` with `LANPHYPC` given back to the PCH.
pub fn lanphypc_released(ctrl: u32) -> u32 {
    ctrl & !ctrl::LANPHYPC_OVERRIDE
}

pub fn smbus_forced(ctrl_ext: u32) -> u32 {
    ctrl_ext | ctrl_ext::FORCE_SMBUS
}

pub fn smbus_released(ctrl_ext: u32) -> u32 {
    ctrl_ext & !ctrl_ext::FORCE_SMBUS
}

/// Whether the part says the power cycle is done.
pub fn power_cycle_done(ctrl_ext: u32) -> bool {
    ctrl_ext & ctrl_ext::LCD_POWER_CYCLE_DONE != 0
}

/// I219 §9.5.3.4's register with Force SMBus cleared and every other field
/// carried.
pub fn phy_smbus_released(smbus_control: u16) -> u16 {
    smbus_control & !SMBUS_CONTROL_FORCE
}

/// The full reset's `CTRL` word: `RST`, and `PHY_RST` beside it wherever the
/// firmware allows a PHY reset.
pub fn reset_word(ctrl: u32, fwsm: u32) -> u32 {
    let phy = if phy_reset_allowed(fwsm) { ctrl::PHY_RST } else { 0 };
    ctrl | ctrl::RST | phy
}

/// Whether the firmware lets the host reset the PHY — and with it, cycle
/// `LANPHYPC`: the host driver for this family reports a cycle "blocked by ME"
/// on the same bit and does not make it.
pub fn phy_reset_allowed(fwsm: u32) -> bool {
    fwsm & fwsm::PHY_RESET_ALLOWED != 0
}

/// Whether a rung of [`LADDER`] may be taken on a part whose firmware reads
/// `fwsm`: a power cycle only where a PHY reset is allowed, and SMBus always.
pub fn rung_allowed(action: Action, fwsm: u32) -> bool {
    match action {
        Action::PowerCycle => phy_reset_allowed(fwsm),
        Action::ForceSmbus | Action::ReleaseSmbus => true,
    }
}

/// What the full reset did, for the one line a caller prints about it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FullReset {
    /// Whether `PHY_RST` went out beside `RST`.
    pub phy_reset: bool,
    /// Whether §8.2.4's flag was held across the write, and why not.
    pub flag: Result<(), PhyRefusal>,
    /// How long after the write `STATUS.LAN_INIT_DONE` read set, or `None`
    /// where [`LAN_INIT_DEADLINE_NANOS`] ran out first. Not waited on at all
    /// where no PHY reset went out.
    pub init_done_after_nanos: Option<u64>,
}

impl core::fmt::Display for FullReset {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.phy_reset {
            f.write_str("the MAC and the PHY were reset together")?;
        } else {
            f.write_str("the MAC alone was reset, the firmware blocking a PHY reset")?;
        }
        match self.flag {
            Ok(()) => f.write_str(" under the software flag")?,
            Err(why) => write!(f, " without the software flag ({why})")?,
        }
        match (self.phy_reset, self.init_done_after_nanos) {
            (false, _) => Ok(()),
            (true, Some(nanos)) => write!(f, ", LAN_INIT_DONE {} us after", nanos / 1_000),
            (true, None) => write!(
                f,
                ", and LAN_INIT_DONE never read set inside {} ms",
                LAN_INIT_DEADLINE_NANOS / 1_000_000
            ),
        }
    }
}
