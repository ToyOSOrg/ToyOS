//! Whether the I219's PHY answers before a reset of the MAC alone, and whether
//! it answers after one — one bench boot's question, and no part of the driver.
//!
//! **This is an instrument and it is owed a deletion.** The bring-up this
//! driver used to run reset the MAC alone and then found `MDIC` never ending a
//! cycle; Intel's host driver for this family never resets that MAC without
//! its PHY where the firmware allows both (`crate::wake`'s header). This asks
//! the PHY for its identifier on the part exactly as the firmware handed it
//! over, resets the MAC alone as the old bring-up did, and asks again — so the
//! trail says in the part's own words whether that reset is what took the PHY
//! out of reach. It runs ahead of [`crate::I219::open`], which then wakes and
//! resets the part its own way, and goes once the reading is taken.
//!
//! Each ask takes §8.2.4's flag and gives it back as the bring-up does, and
//! every transaction's `MDIC` word is a durable crumb behind it.

use crate::crumbs::Trail;
use crate::phy::{self, Owned, PhyRefusal};
use crate::wake::{Answer, Moment};
use crate::{reset, Clock, Refusal, Registers, Whole};

/// What the scout found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Scouted {
    /// The PHY as the firmware left it.
    pub before: Result<Answer, PhyRefusal>,
    /// The MAC-alone reset, and why it did not finish where it did not.
    pub reset: Result<(), Refusal>,
    /// The PHY after that reset and §9.2's 10 ms, or `None` where the reset
    /// did not finish and nothing was asked.
    pub after: Option<Result<Answer, PhyRefusal>>,
}

impl core::fmt::Display for Scouted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let asked = |f: &mut core::fmt::Formatter<'_>, asked: &Result<Answer, PhyRefusal>| {
            match asked {
                Ok(answer) => write!(f, "{answer}"),
                Err(why) => write!(f, "the flag was not taken ({why})"),
            }
        };
        f.write_str("before the MAC-alone reset the PHY ")?;
        asked(f, &self.before)?;
        match (&self.reset, &self.after) {
            (Err(why), _) => write!(f, "; the reset did not finish: {why}"),
            (Ok(()), Some(after)) => {
                f.write_str("; after it the PHY ")?;
                asked(f, after)
            }
            (Ok(()), None) => f.write_str("; nothing was asked after it"),
        }
    }
}

/// One ask under the flag, given back after, and when it was given back.
fn asked<R: Registers, C: Clock, T: Trail>(
    regs: &R,
    clock: &C,
    trail: &T,
    moment: Moment,
    after: Option<u64>,
) -> (Result<Answer, PhyRefusal>, u64) {
    let answer = Owned::claim(regs, clock, after).map(|mdi| mdi.ask(trail, moment));
    (answer, clock.nanos())
}

/// Ask, reset the MAC alone, wait out §9.2's 10 ms, and ask again.
pub fn around_the_reset<R: Registers, C: Clock, T: Trail>(
    regs: &R,
    clock: &C,
    trail: &T,
) -> Scouted {
    let (before, released) = asked(regs, clock, trail, Moment::BeforeReset, None);
    let reset = reset(regs, clock, Whole::MacAlone);
    let after = reset.as_ref().ok().map(|reset| {
        phy::hold(clock, reset.at, phy::LCD_RESET_DELAY_NANOS);
        let (after, released) = asked(regs, clock, trail, Moment::AfterReset, Some(released));
        // The bring-up's own first hold follows this one, and it is paced from
        // nothing of this instrument's: the pace is kept here instead.
        phy::hold(clock, released, phy::ARBITRATION_PACE_NANOS);
        after
    });
    Scouted { before, reset: reset.map(|_| ()), after }
}
