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
//! bit this file writes "Reserved". **The rest is properties of the part that
//! neither document publishes**, stated as such:
//!
//! - `CTRL` bits 16 and 17 put `LANPHYPC` — I219 §5.2's `LAN_DISABLE_N`, "the
//!   only external signal that can reset the PHY" — in the host's hand, and
//!   holding it low for at least 10 µs takes the PHY's power away and restores
//!   it; the PHY's configuration time after that cycle is set to 50 ms first
//!   (`regs::LANPHYPC_TIMING`), `CTRL_EXT` bit 2 reports the cycle done, and
//!   the PHY is given 30 ms more before it is asked anything.
//! - `CTRL_EXT` bit 11 moves the MAC's end of the interconnect onto SMBus, and
//!   the MAC carries no cycle over it for 50 ms after; a PHY reached that way
//!   is moved back to PCIe by clearing I219 §9.5.3.4's Force SMBus bit and then
//!   the MAC's, and the write that moves the PHY ends in `MDIC.Error` whether
//!   or not the PHY took it.
//! - `CTRL.RST` and `CTRL.PHY_RST` written together start both ends of the
//!   interconnect over at once, where `FWSM` bit 6 allows a PHY reset; the
//!   part takes no register access for 20 ms after that write, and then sets
//!   `STATUS` bit 9 once the MAC has configured the PHY.
//!
//! **What this driver does with them is its own.** The PHY is asked as found,
//! and only where it does not answer does [`LADDER`] climb: the power pin
//! first, because it is the one reach I219 §5.2 and §6.3.1.3 give to a PHY
//! that is powered down, and §9.5.7.1's ULP configuration ties Ultra Low
//! Power to the same pin ("Enable ULP on LAN disable (LANPHYPC)"); then SMBus,
//! because the same register can have the PHY come back "on power on or on ULP
//! exit" on SMBus, where no cycle over PCIe reaches it; then a cycle with the
//! MAC already on SMBus; then PCIe again. It stops at the first answer.
//!
//! **The pace these accesses are made at is
//! [`crate::phy::ARBITRATION_PACE_NANOS`]**, because every one of them is under
//! §8.2.4's flag and the T14 is the one machine whose answer to a faster pace
//! was a power-off. The pace is wider than every floor above, and the
//! constants below assert it.

use crate::phy::PhyRefusal;
use crate::power::Settled;
use crate::regs::{ctrl, ctrl_ext, fwsm, lanphypc_timing};

/// How long `LANPHYPC` is held low before it is given back: the floor a power
/// cycle takes.
pub const LANPHYPC_HOLD_NANOS: u64 = 10_000;

/// How long `CTRL_EXT`'s cycle-done bit is waited on, a driver-chosen bound.
/// **Not a refusal when it runs out**: whether the PHY answers is the question
/// the next ask settles.
pub const POWER_CYCLE_DONE_DEADLINE_NANOS: u64 = 100_000_000;

/// How long passes after the cycle is done before the PHY is asked anything.
pub const POWER_CYCLE_SETTLE_NANOS: u64 = 30_000_000;

/// How long passes after the MAC is forced onto SMBus before a cycle is issued.
pub const SMBUS_SETTLE_NANOS: u64 = 50_000_000;

/// How long nothing in the register file is touched after a full reset.
pub const RESET_QUIET_NANOS: u64 = 20_000_000;

/// How long `STATUS` bit 9 is waited on after a full reset: I219
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

/// Each rung followed by one ask: a cycle, SMBus, a cycle on SMBus, and PCIe
/// again. The ladder stops at the first answer.
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

/// What one wake found the part's power registers holding, and every ask it
/// made, in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Woke {
    /// §8.2's power step as the wake took it, before any ask.
    pub power: Settled,
    asked: [Option<(Moment, Answer)>; ASKS],
}

/// The first ask, one after each rung of [`LADDER`], and one after taking a PHY
/// that answered on SMBus back to PCIe.
pub const ASKS: usize = 1 + LADDER.len() + 1;

impl Woke {
    /// A wake that has taken its power step and asked nothing yet.
    pub(crate) fn found(power: Settled) -> Self {
        Self { power, asked: [None; ASKS] }
    }

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
        write!(f, "the power step, {}", self.power)?;
        for (moment, answer) in self.asked() {
            write!(f, "; {moment}: {answer}")?;
        }
        Ok(())
    }
}

/// `regs::LANPHYPC_TIMING` with the PHY's configuration time at fifty
/// milliseconds and every other bit carried.
pub fn configuration_50ms(timing: u32) -> u32 {
    (timing & !lanphypc_timing::CONFIGURATION_MASK) | lanphypc_timing::CONFIGURATION_50MS
}

/// `CTRL` with `LANPHYPC` in the host's hand and driven low.
pub fn lanphypc_low(ctrl: u32) -> u32 {
    (ctrl | ctrl::LANPHYPC_HOST_DRIVEN) & !ctrl::LANPHYPC_LEVEL
}

/// `CTRL` with `LANPHYPC` given back to the PCH.
pub fn lanphypc_released(ctrl: u32) -> u32 {
    ctrl & !ctrl::LANPHYPC_HOST_DRIVEN
}

pub fn smbus_forced(ctrl_ext: u32) -> u32 {
    ctrl_ext | ctrl_ext::MAC_ON_SMBUS
}

pub fn smbus_released(ctrl_ext: u32) -> u32 {
    ctrl_ext & !ctrl_ext::MAC_ON_SMBUS
}

/// Whether the part says the power cycle is done.
pub fn power_cycle_done(ctrl_ext: u32) -> bool {
    ctrl_ext & ctrl_ext::LANPHYPC_CYCLE_DONE != 0
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

/// Whether the firmware lets the host reset the PHY. A `LANPHYPC` cycle is a
/// reset of the PHY by its power pin, so the same bit forbids that too.
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
    /// How long after the write `STATUS` bit 9 read set, or `None`
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
            (true, Some(nanos)) => {
                write!(f, ", the PHY configured {} us after", nanos / 1_000)
            }
            (true, None) => write!(
                f,
                ", and STATUS never said the PHY was configured inside {} ms",
                LAN_INIT_DEADLINE_NANOS / 1_000_000
            ),
        }
    }
}
