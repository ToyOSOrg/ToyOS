//! The PCH's MAC before its rings: three registers written, each bit for a
//! property of the part this driver rests on, and nothing that stands on
//! neither a document nor such a property.
//!
//! - `CTRL_EXT` bit 17 (`regs::ctrl_ext::STRICT_WRITE_ORDER`): the part's
//!   writes to memory land in the order it made them. [`crate::I219::poll_rx`]
//!   reads a frame on the strength of its descriptor's `DD`, so a write-back
//!   that could pass the frame's own bytes would hand up a buffer the frame has
//!   not reached.
//! - `CTRL_EXT` bit 28 (`regs::ctrl_ext::DRIVER_HOLDS_THE_FUNCTION`): the
//!   firmware on this part shares the PHY with the host, and this bit is the
//!   host's word to it that a driver holds the function.
//! - §8.2.8's APM wake-up enable (the register's one writable bit) cleared:
//!   this driver arms no wake, and one the agent before it armed would
//!   otherwise stand while the function is driven.
//! - The PCIe control's six no-snoop requests cleared, and every other bit of
//!   that register carried: the descriptors and buffers live in memory the
//!   processor caches, and a DMA without the snoop attribute is not coherent
//!   with it.
//!
//! **The order is free.** The three registers are independent of one another;
//! what the hardware fixes is that all of them come after the reset, which
//! returns each to its default, and before the rings, whose first fetch is a
//! DMA the ordering and snoop settings govern.

use crate::regs::{self, ctrl_ext, pcie_control, wake_up};
use crate::Registers;

/// One read-modify-write: `set` raised and `clear` lowered, every other bit as
/// the part holds it.
fn modify<R: Registers>(regs: &R, reg: usize, set: u32, clear: u32) {
    let held = regs.read(reg);
    regs.write(reg, (held | set) & !clear);
}

/// Everything this module owes the part, after its reset and before its rings.
pub(crate) fn prepare<R: Registers>(regs: &R) {
    modify(
        regs,
        regs::CTRL_EXT,
        ctrl_ext::STRICT_WRITE_ORDER | ctrl_ext::DRIVER_HOLDS_THE_FUNCTION,
        0,
    );
    modify(regs, regs::WAKE_UP, 0, wake_up::APM_WAKE);
    modify(regs, regs::PCIE_CONTROL, 0, pcie_control::NO_SNOOP);
}
