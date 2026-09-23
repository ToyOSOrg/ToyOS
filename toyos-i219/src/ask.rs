//! One question put to §4.5.2's arbitration, and nothing done with the answer.
//!
//! [`crate::phy`]'s bring-up observes the MDIO interface free before it asks
//! for it, so on a part whose manageability bit stands for the whole of that
//! wait it never asks — and never learns what asking would have been answered.
//! This module asks. §4.5.2: "A request for ownership is registered by writing
//! a 1b into the respective bit [...] The requesting agent is granted access
//! when the same bit is read as 1b." The request is registered whoever else's
//! bit stands, the register is sampled until the software bit reads back set or
//! [`BOUND_NANOS`] has passed, and the request is withdrawn either way.
//!
//! **`EXTCNF_CTRL` is the only register this module reaches**, and §4.5.2's
//! software ownership bit the only bit of it that it writes: the bit the clause
//! gives a driver for exactly this, written once and written back to 0b on
//! every path that set it. No `MDIC` transaction is started and no PHY register
//! is read or written, so an agent that owns the PHY keeps the whole of it.
//!
//! **One table, read at both ends**, as [`crate::phy::Outcome`] is: the caller
//! exits with [`Reading::exit_code`], the kernel records the code, and the
//! harness reads it back through [`Reading::from_exit_code`]. The two tables
//! share the block that starts at 64 and share one code in it,
//! [`Reading::Unrouted`], which is the same sentence in both.

use crate::crumbs::{Trail, Witnessed};
use crate::phy::{self, Outcome};
use crate::regs::{self, extcnf};
use crate::{Clock, Refusal, Registers};

/// The longest a software request is left registered without a grant.
///
/// **A driver-chosen bound, not a datasheet one**: §4.5.2 gives its handshake
/// no time. Two seconds is past any wait a boot could afford to build on, so
/// [`Answer::NeverGranted`] is a statement about the arbitration and not about
/// this number.
pub const BOUND_NANOS: u64 = 2_000_000_000;

/// The edge between [`Answer::GrantedQuickly`] and [`Answer::GrantedSlowly`]:
/// the deadline [`crate::phy`]'s bring-up already gives the same handshake, so
/// a grant inside it is one that bring-up would have had by asking.
pub const QUICK_NANOS: u64 = phy::DEADLINE_NANOS;

/// How long the caller is asked to give the processor away between two
/// samples, which is also the longest a grant is held before it is seen and
/// given back.
pub const CADENCE_NANOS: u64 = 1_000_000;

/// Which of §4.5.2's two agents that are not software one reading of
/// `EXTCNF_CTRL` names.
///
/// The software bit is left out because around a request it is this module's
/// own: whether it read back set is [`Answer`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Others {
    Nobody,
    Hardware,
    Manageability,
    HardwareAndManageability,
}

impl Others {
    pub const ALL: [Self; 4] = [
        Self::Nobody,
        Self::Hardware,
        Self::Manageability,
        Self::HardwareAndManageability,
    ];

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

/// Whether the software bit read back set, and on which side of
/// [`QUICK_NANOS`] the sample that saw it was taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answer {
    GrantedQuickly,
    GrantedSlowly,
    NeverGranted,
}

impl Answer {
    pub const ALL: [Self; 3] = [
        Self::GrantedQuickly,
        Self::GrantedSlowly,
        Self::NeverGranted,
    ];
}

/// What the arbitration answered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reading {
    /// `EXTCNF_CTRL` answered ones, so nothing decodes it. Nothing was written
    /// after the reading that said so.
    Unrouted,
    /// The software bit was already set and stayed set for
    /// [`phy::DEADLINE_NANOS`]: another software agent's flag. **Nothing was
    /// written**, because the same bit read back set would say nothing, and
    /// clearing it afterwards would clear a flag this module never set.
    SoftwareFlagStood,
    /// The request was registered and has been withdrawn.
    Asked {
        /// Who else's bit stood in the last reading before the request.
        before: Others,
        answer: Answer,
        /// Who else's bit stood in the sample that ended the wait: the one
        /// that saw the grant, or the last one inside [`BOUND_NANOS`].
        after: Others,
    },
}

/// [`Reading::SoftwareFlagStood`]'s code, the first past [`Outcome`]'s block.
const FIRST_OWN_CODE: i32 = 78;
/// The Rust runtime ends a panicking process with this, so no reading may.
const PANIC_CODE: i32 = 101;

impl Reading {
    /// Every reading, in exit-code order.
    pub fn all() -> impl Iterator<Item = Self> {
        let asked = Answer::ALL.into_iter().flat_map(|answer| {
            Others::ALL.into_iter().flat_map(move |before| {
                Others::ALL.into_iter().map(move |after| Self::Asked {
                    before,
                    answer,
                    after,
                })
            })
        });
        [Self::Unrouted, Self::SoftwareFlagStood]
            .into_iter()
            .chain(asked)
    }

    pub fn exit_code(self) -> i32 {
        let (before, answer, after) = match self {
            Self::Unrouted => return Outcome::Unrouted.exit_code(),
            Self::SoftwareFlagStood => return FIRST_OWN_CODE,
            Self::Asked {
                before,
                answer,
                after,
            } => (before, answer, after),
        };
        let index = (answer as i32 * 4 + before as i32) * 4 + after as i32;
        let code = FIRST_OWN_CODE + 1 + index;
        if code >= PANIC_CODE {
            code + 1
        } else {
            code
        }
    }

    /// The reading an exit code names, or `None` for a process that ended for
    /// some other reason.
    pub fn from_exit_code(code: i32) -> Option<Self> {
        Self::all().find(|reading| reading.exit_code() == code)
    }
}

impl core::fmt::Display for Reading {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unrouted => write!(
                f,
                "EXTCNF_CTRL answers ones, so nothing decodes it and no request was left in it"
            ),
            Self::SoftwareFlagStood => write!(
                f,
                "§4.5.2's software bit was already set for {} ns, so it is another agent's and \
                 no request was registered over it",
                phy::DEADLINE_NANOS
            ),
            Self::Asked {
                before,
                answer,
                after,
            } => {
                write!(
                    f,
                    "§4.5.2's software request was registered with {before} holding the \
                           MDIO interface, "
                )?;
                match answer {
                    Answer::GrantedQuickly => write!(f, "read back set inside {QUICK_NANOS} ns")?,
                    Answer::GrantedSlowly => write!(
                        f,
                        "read back set after {QUICK_NANOS} ns and inside {BOUND_NANOS} ns"
                    )?,
                    Answer::NeverGranted => write!(f, "never read back set in {BOUND_NANOS} ns")?,
                }
                write!(f, " with {after} holding it, and was withdrawn")
            }
        }
    }
}

/// Reset the function as [`crate::I219::open`] does, wait out §9.2's delay as
/// [`crate::phy`]'s bring-up does, and ask.
///
/// `pause` gives the processor away for about that many nanoseconds and
/// decides nothing: every bound here is read off `clock`, so a pause that
/// returns early or late moves when a sample is taken and never what it means.
pub fn after_reset<R: Registers, C: Clock>(
    regs: &R,
    clock: &C,
    pause: impl Fn(u64),
) -> Result<Reading, Refusal> {
    before_asking(regs, clock, &pause)?;
    Ok(ask(regs, clock, &pause))
}

/// [`after_reset`] with a durable line on both sides of every access the
/// *question* makes, which is every access from `EXTCNF_CTRL` on.
///
/// **The same question, recorded and not changed.** The reset and §9.2's delay
/// are taken through the bare window exactly as [`after_reset`] takes them, and
/// what follows is [`ask`] itself over a [`Witnessed`] view: same register,
/// same bit, same order, same words. What the trail does move is how many
/// samples fit inside [`BOUND_NANOS`] — every bound here is read off `clock`,
/// so two durable writes per access buy fewer polls in the same wait and never
/// a different one.
pub fn after_reset_witnessed<R: Registers, C: Clock, T: Trail>(
    regs: R,
    clock: &C,
    pause: impl Fn(u64),
    trail: T,
) -> Result<Reading, Refusal> {
    before_asking(&regs, clock, &pause)?;
    Ok(ask(&Witnessed::over(regs, trail), clock, &pause))
}

/// What both entry points do before the question: refuse a window the
/// arbitration is not inside, reset the function as [`crate::I219::open`] does,
/// and wait out §9.2's delay.
fn before_asking<R: Registers, C: Clock>(
    regs: &R,
    clock: &C,
    pause: &impl Fn(u64),
) -> Result<(), Refusal> {
    if regs.bytes() < regs::REGISTER_BYTES {
        return Err(Refusal::Window {
            given: regs.bytes(),
            needed: regs::REGISTER_BYTES,
        });
    }
    let reset = crate::reset(regs, clock)?;
    // The instant the bring-up would ask at, so the answer is one it would get.
    loop {
        let since = clock.nanos().saturating_sub(reset.at);
        if since >= phy::LCD_RESET_DELAY_NANOS {
            break;
        }
        pause(phy::LCD_RESET_DELAY_NANOS - since);
    }
    Ok(())
}

fn ask<R: Registers, C: Clock>(regs: &R, clock: &C, pause: &impl Fn(u64)) -> Reading {
    let started = clock.nanos();
    let before = loop {
        let reading = regs.read(regs::EXTCNF_CTRL);
        if reading == u32::MAX {
            return Reading::Unrouted;
        }
        if reading & extcnf::MDIO_SW_OWNERSHIP == 0 {
            break reading;
        }
        if clock.nanos().saturating_sub(started) >= phy::DEADLINE_NANOS {
            return Reading::SoftwareFlagStood;
        }
        pause(CADENCE_NANOS);
    };

    // One bit of the register is this module's and the rest is read and
    // carried, as in `phy::Owned::claim`.
    regs.write(regs::EXTCNF_CTRL, before | extcnf::MDIO_SW_OWNERSHIP);
    let asked_at = clock.nanos();
    let (answer, after) = loop {
        let reading = regs.read(regs::EXTCNF_CTRL);
        // Read after the sample, so "inside" is never claimed for a sample
        // taken outside.
        let waited = clock.nanos().saturating_sub(asked_at);
        if reading == u32::MAX {
            return Reading::Unrouted;
        }
        if reading & extcnf::MDIO_SW_OWNERSHIP != 0 {
            let answer = if waited <= QUICK_NANOS {
                Answer::GrantedQuickly
            } else {
                Answer::GrantedSlowly
            };
            break (answer, reading);
        }
        if waited >= BOUND_NANOS {
            break (Answer::NeverGranted, reading);
        }
        pause(CADENCE_NANOS);
    };

    // §4.5.2: "the controlling agent must write a 0b to its ownership bit" —
    // and a request never granted is withdrawn the same way, or it would stand
    // for a grant nobody is waiting on.
    let held = regs.read(regs::EXTCNF_CTRL);
    if held == u32::MAX {
        return Reading::Unrouted;
    }
    regs.write(regs::EXTCNF_CTRL, held & !extcnf::MDIO_SW_OWNERSHIP);
    Reading::Asked {
        before: Others::in_reading(before),
        answer,
        after: Others::in_reading(after),
    }
}
