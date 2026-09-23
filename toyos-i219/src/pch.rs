//! The PCH's MAC before its rings: what Intel's own Linux host driver for this
//! family writes into it on every initialisation, and nothing it does not.
//!
//! **None of these bits is in a document in hand.** The *Intel® 500 Series
//! Chipset Family On-Package Platform Controller Hub Datasheet, Volume 2*
//! (631120, rev 002) §8.2 publishes nine of this MAC's registers and calls
//! every bit below reserved or leaves its register out; the 82574's datasheet
//! describes another part. So each is stated as a fact about the hardware from
//! the host driver that drives this `8086:15fc` to a working network under
//! another operating system, and [`crate::regs`] names each where it stands.
//!
//! **One refusal.** That driver's own `GCR` write sets every bit above the six
//! no-snoop requests as it clears them, which no source explains; this one
//! clears the six and carries the rest.

use crate::regs::{self, ctrl, ctrl_ext, fflt_dbg, gcr, pbeccsts, rfctl, tarc, tctl, txdctl};
use crate::TX_CONTROL;
use crate::Registers;

/// One read-modify-write: `set` raised and `clear` lowered, every other bit as
/// the part holds it.
fn modify<R: Registers>(regs: &R, reg: usize, set: u32, clear: u32) {
    let held = regs.read(reg);
    regs.write(reg, (held | set) & !clear);
}

/// `TARC1`'s word: the three bits always set, and bit 28 exactly where `tctl`
/// — the transmit control this driver writes — has Multiple Request Support
/// clear.
pub(crate) fn tarc1(held: u32, tctl: u32) -> u32 {
    let single = if tctl & tctl::MULR == 0 { tarc::TARC1_SINGLE_REQUEST } else { 0 };
    (held & !tarc::TARC1_SINGLE_REQUEST) | tarc::TARC1_REQUIRED | single
}

/// Everything this module owes the part, after its reset and before its rings.
pub(crate) fn prepare<R: Registers>(regs: &R) {
    // The bits the host driver calls required for transmit and receive, and
    // its word to the firmware that a driver holds the function; strict
    // ordering of the part's own writes to memory.
    modify(
        regs,
        regs::CTRL_EXT,
        ctrl_ext::REQUIRED_22 | ctrl_ext::DRIVER_LOADED | ctrl_ext::RELAXED_ORDERING_DISABLE,
        0,
    );
    // Both queues alike: the host driver sets the second queue's the same as
    // the first's, as an erratum's workaround, and this driver's first is
    // written with its ring.
    regs.write(regs::TXDCTL1, txdctl::PCH);
    modify(regs, regs::TARC0, tarc::TARC0_REQUIRED, 0);
    let held = regs.read(regs::TARC1);
    regs.write(regs::TARC1, tarc1(held, TX_CONTROL));
    modify(regs, regs::RFCTL, rfctl::NFS_FILTERS_OFF, 0);
    modify(regs, regs::PBECCSTS, pbeccsts::ECC_ENABLE, 0);
    modify(regs, regs::CTRL, ctrl::MEHE, 0);
    // Every wake-up source off: the function is in D0 and driven.
    regs.write(regs::WUC, 0);
    modify(regs, regs::GCR, 0, gcr::NO_SNOOP);
    modify(regs, regs::FFLT_DBG, fflt_dbg::DONT_GATE_WAKE_DMA_CLOCK, 0);
}
