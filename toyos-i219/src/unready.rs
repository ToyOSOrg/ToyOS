//! Why §10.2.2.7's `Ready` does not come back on this part — one bench boot's
//! question, and no part of the driver.
//!
//! **This is an instrument and it is owed a deletion.** [`bring_up`] asks the
//! part for the page register and the part never reports the transaction
//! ended; the trail that boot leaves carries one `read MDIC` and then the
//! deadline, so what the poll saw is unread. Everything below exists to read
//! it, runs only where that is the wall the bring-up hit, and goes when the
//! wall is named.
//!
//! **What it can establish, and what it cannot.** §10.2.2.7 has the MAC set
//! `Ready` "at the end of the MDI transaction" and `Error` "when it fails to
//! complete an MDI read" — both are the *MAC's* report of its own cycle, and
//! neither is the PHY's answer. A transaction aimed at a PHY address nothing
//! drives still ends: the MAC clocks the frame out and reads ones off an
//! undriven bus. So `Ready` that never comes back is a cycle the MAC never
//! finished, which is a statement about the MDI interface and not about which
//! address the PHY is at — and that is why [`Held::sweep`] is asked only once
//! some transaction on this part has ended at all.
//!
//! **Three transactions, one variable each**, all at the address and register
//! the bring-up failed on where they can be:
//!
//! 1. [`Held::paced`] is a read of §9.5.2.3's identifier, polled with every
//!    reading of `MDIC` made durable before the next is taken — so the whole
//!    poll is on the device whatever the machine does next, and the poll is
//!    paced by its own durability.
//! 2. [`Held::unpaced`] is the same read polled exactly as [`Owned::transact`]
//!    polls one: a tight loop inside `MDI_DEADLINE_NANOS`. It is the pacing
//!    hypothesis's control, taken on the same part in the same boot.
//! 3. [`Held::written`] is the page-select write the bring-up died on,
//!    repeated with the durable poll: a write where the first two are reads,
//!    and the only one of the three that changes anything in the PHY.
//!
//! [`bring_up`]: crate::phy::bring_up

use crate::crumbs::{Step, Trail};
use crate::phy::{self, Owned, Phy, PhyRefusal};
use crate::regs::{self, mdic};
use crate::{Clock, Registers};

/// How many readings of `MDIC` one durably-polled transaction is watched over.
///
/// **A bound in readings and not in nanoseconds**, because what this measures
/// is the poll itself: each reading costs the trail one append and one flush
/// to the device, so the count is also the width of the window the part is
/// given, and the crumbs' own timestamps say how wide that was.
pub const SAMPLES: u32 = 16;

/// The same for one address of [`Held::sweep`].
///
/// §10.2.2.7 bounds its transaction with nothing, so one reading is no bound
/// at all: a part is allowed to take several, and this crate's model of it
/// does. Four is the smallest count that admits that latitude and still costs
/// a bounded number of lines over all thirty-two addresses — and every address
/// is left as soon as `Ready` or `Error` stands, so the count is only ever
/// paid where the part says nothing.
const SWEEP_SAMPLES: u32 = 4;

/// How many PHY addresses §10.2.2.7's five-bit `PHYADD` field counts.
pub const ADDRESSES: usize = 32;

/// What one transaction ended on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ended {
    /// The last word read out of `MDIC` before the transaction was given up
    /// on — the whole register and not a field of it, because which bits are
    /// standing is the question.
    pub last: u32,
    /// How many readings of `MDIC` it took to get there.
    pub samples: u32,
    /// From the command word going in to that last reading coming back.
    pub after_nanos: u64,
}

impl Ended {
    /// §10.2.2.7's `Ready`: the MAC's report that its own cycle finished.
    pub fn ready(self) -> bool {
        self.last & mdic::READY != 0
    }

    /// §10.2.2.7's `Error`: a cycle the MAC could not complete. It ends the
    /// transaction too, and the data field is then not what the PHY said.
    pub fn errored(self) -> bool {
        self.last & mdic::ERROR != 0
    }
}

impl core::fmt::Display for Ended {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "MDIC read {:#010x} after {} reading(s) over {} us, and {}",
            self.last,
            self.samples,
            self.after_nanos / 1_000,
            match (self.ready(), self.errored()) {
                (_, true) => "the part reported Error",
                (true, false) => "the part reported Ready",
                (false, false) => "neither Ready nor Error ever stood",
            }
        )
    }
}

/// What this instrument asked while it held §4.5.2's interface.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Held {
    /// `MDIC` on the first reading after the interface was granted, before
    /// any command of this instrument's went in.
    pub under_claim: u32,
    /// The identifier read, polled one durable reading at a time.
    pub paced: Ended,
    /// The same read polled as the driver polls one, and what that poll
    /// answered.
    pub unpaced: Result<u16, PhyRefusal>,
    /// `MDIC` read once after that poll gave up, which is the word the
    /// driver's own refusal never carries off the machine.
    pub unpaced_last: u32,
    /// The page-select write the bring-up died on, repeated.
    pub written: Ended,
    /// One `MDIC` word per PHY address, asked only where some transaction on
    /// this part ended and the identifier did not answer at §9.3's first
    /// address — the one question the other three cannot settle between them.
    pub sweep: Option<[u32; ADDRESSES]>,
}

/// One boot's reading of the wall.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Asked {
    /// `MDIC` as the bring-up left it, read before the interface is asked for
    /// again: the abandoned transaction's own register, long after the
    /// deadline it was given.
    pub settled: u32,
    /// What was asked under the interface, or why it could not be taken this
    /// time. §4.5.2's flag is registered and given back around all of it, as
    /// the bring-up does.
    pub held: Result<Held, PhyRefusal>,
}

impl core::fmt::Display for Asked {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "MDIC settled at {:#010x}", self.settled)?;
        let held = match &self.held {
            Err(why) => return write!(f, ", and the interface was not taken again: {why}"),
            Ok(held) => held,
        };
        write!(f, "; granted, it read {:#010x}", held.under_claim)?;
        write!(f, "; the identifier read, one durable reading at a time: {}", held.paced)?;
        match held.unpaced {
            Ok(data) => write!(f, "; polled as the driver polls one it answered {data:#06x}")?,
            Err(why) => write!(f, "; polled as the driver polls one: {why}")?,
        }
        write!(f, ", leaving MDIC at {:#010x}", held.unpaced_last)?;
        write!(f, "; the page-select write: {}", held.written)?;
        match held.sweep {
            None => f.write_str("; no PHY address was swept"),
            Some(asked) => {
                f.write_str("; the identifier answers")?;
                for (addr, word) in asked.iter().enumerate() {
                    write!(f, " {addr:02}:{word:#010x}")?;
                }
                Ok(())
            }
        }
    }
}

/// One read, bracketed by two durable lines: the [`Step::Read`] the register
/// window leaves before it, and the reading itself behind it.
///
/// **The reading is the only crumb of the two orders.** A machine that ends
/// between them leaves the `read` with no `saw` after it, which is exactly the
/// statement that the access is where it ended.
fn seen<R: Registers, T: Trail>(regs: &R, trail: &T, reg: usize) -> u32 {
    let value = regs.read(reg);
    trail.crumb(Step::Saw { reg, value });
    value
}

/// One transaction, written and then read out of `MDIC` up to `samples` times,
/// every reading durable before the next is taken.
///
/// **Not [`Owned::transact`] and never a replacement for it**: it gives up on
/// a count of readings rather than on a clock, and it keeps `Error` instead of
/// refusing on it, because a transaction that ends in `Error` is one that
/// ended and that is half the question.
fn watch<R: Registers, C: Clock, T: Trail>(
    mdi: &Owned<R, C>,
    trail: &T,
    command: u32,
    samples: u32,
) -> Ended {
    let (regs, clock) = mdi.part();
    regs.write(regs::MDIC, command);
    let started = clock.nanos();
    let mut ended = Ended { last: 0, samples: 0, after_nanos: 0 };
    while ended.samples < samples {
        ended.last = seen(regs, trail, regs::MDIC);
        ended.samples += 1;
        if ended.ready() || ended.errored() {
            break;
        }
    }
    ended.after_nanos = clock.nanos().saturating_sub(started);
    ended
}

/// Ask the part why it never reported the end of the bring-up's first MDI
/// transaction — and ask nothing at all where that is not what happened.
///
/// `wall` is what [`crate::phy::bring_up`] answered on this boot. Every other
/// refusal it has is about something this instrument cannot read: an interface
/// that was never granted, a window that answers ones, a PHY whose identifier
/// is not Intel's. Only [`PhyRefusal::MdiUnready`] is the question, so only
/// that one is asked.
pub fn interrogate<'a, R: Registers, C: Clock, T: Trail>(
    regs: &'a R,
    clock: &'a C,
    trail: &T,
    wall: Result<Phy, PhyRefusal>,
) -> Option<Asked> {
    if !matches!(wall, Err(PhyRefusal::MdiUnready { .. })) {
        return None;
    }
    // Before §4.5.2's flag is asked for again: a read is not an MDI
    // transaction and takes no interface — what it answers is the register the
    // bring-up walked away from.
    let settled = seen(regs, trail, regs::MDIC);
    let held = Owned::claim(regs, clock).map(|mdi| {
        let under_claim = seen(regs, trail, regs::MDIC);

        // §9.5.2.3's identifier at §9.3's first address, read rather than
        // written: the bring-up's failing transaction with one field changed.
        let identifier =
            phy::command(phy::GENERAL, phy::reg::IDENTIFIER_HIGH, mdic::OP_READ, 0);
        let paced = watch(&mdi, trail, identifier, SAMPLES);

        let unpaced = mdi.transact(phy::GENERAL, phy::reg::IDENTIFIER_HIGH, mdic::OP_READ, 0);
        let unpaced_last = seen(regs, trail, regs::MDIC);

        // The transaction the bring-up died on, word for word.
        let page = phy::command(
            phy::GENERAL,
            phy::reg::PAGE_SELECT,
            mdic::OP_WRITE,
            phy::PAGE_PORT_CONTROL << phy::PAGE_SHIFT,
        );
        let written = watch(&mdi, trail, page, SAMPLES);

        let answered = (paced.last & mdic::DATA_MASK) as u16;
        let sweep = ((paced.ready() || written.ready())
            && answered != phy::IDENTIFIER_HIGH_INTEL)
            .then(|| {
                let mut asked = [0u32; ADDRESSES];
                for (addr, word) in asked.iter_mut().enumerate() {
                    let at = phy::command(
                        addr as u8,
                        phy::reg::IDENTIFIER_HIGH,
                        mdic::OP_READ,
                        0,
                    );
                    *word = watch(&mdi, trail, at, SWEEP_SAMPLES).last;
                }
                asked
            });

        Held { under_claim, paced, unpaced, unpaced_last, written, sweep }
    });
    Some(Asked { settled, held })
}
