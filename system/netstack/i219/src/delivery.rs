//! Which message the kernel armed on the claim, read off the function's
//! capability list.
//!
//! **The Enable bit the kernel set is the answer**, read with the walk the
//! kernel arms by (`toyos_pci::caps`): MSI-X where the function publishes it,
//! MSI where it publishes only that. The mode decides which cause bits reach
//! the message (§7.4.1 against §7.4.2) and whether `IVAR` exists to be
//! written, so it is read off the function and never inferred from its device
//! id.

use toyos_pci::caps::CapWalk;
use toyos_pci::{msi, msix};

/// PCI Local Bus Specification 3.0 §6.7: the byte at `34h` points at the first
/// capability, and each capability's first dword is its id (7:0), the next
/// pointer (15:8) and, for MSI and MSI-X, Message Control (31:16).
const CAPABILITIES_PTR: u8 = 0x34;

/// The message the function raises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// §7.4.1: every cause has its fixed `ICR` bit, and every unmasked one is
    /// the function's single message.
    Msi,
    /// §7.4.2: a cause reaches a vector only through `IVAR`'s allocation of
    /// its queue-or-other name.
    Msix,
}

/// Why no delivery was read off the function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoDelivery<E> {
    /// The claim refused a read of its configuration space.
    Read { at: u8, why: E },
    /// The list ended at a link §6.7 forbids before an enabled capability: what
    /// lies past it is unknown, never absent.
    Truncated,
    /// Neither capability is on the list with its Enable bit set.
    NotArmed,
}

impl<E: core::fmt::Debug> core::fmt::Display for NoDelivery<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read { at, why } => {
                write!(f, "its configuration space refused a read at {at:#x}: {why:?}")
            }
            Self::Truncated => write!(f, "its capability list breaks off at a forbidden link"),
            Self::NotArmed => write!(f, "neither its MSI nor its MSI-X capability is enabled"),
        }
    }
}

impl Delivery {
    /// The mode the claim was armed in, where `read32` answers the
    /// configuration-space dword at a dword-aligned offset.
    pub fn of<E>(read32: impl Fn(u8) -> Result<u32, E>) -> Result<Self, NoDelivery<E>> {
        let read = |at: u8| read32(at).map_err(|why| NoDelivery::Read { at, why });
        let mut walk = CapWalk::new();
        let mut next = read(CAPABILITIES_PTR)? as u8;
        let mut msi_on = false;
        while let Some(at) = walk.step(next) {
            let header = read(at)?;
            let control = (header >> 16) as u16;
            match header as u8 {
                msix::CAP_ID if msix::Msix::is_enabled(control) => return Ok(Self::Msix),
                msi::CAP_ID => msi_on |= msi::Msi::is_enabled(control),
                _ => {}
            }
            next = (header >> 8) as u8;
        }
        if walk.truncated() {
            Err(NoDelivery::Truncated)
        } else if msi_on {
            Ok(Self::Msi)
        } else {
            Err(NoDelivery::NotArmed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// PCI 3.0 §6.8.1.3: MSI Enable is Message Control's bit 0. PCIe base
    /// §7.7.2.2: MSI-X Enable is its bit 15.
    const MSI_ON: u32 = 1 << 16;
    const MSIX_ON: u32 = 1 << 31;

    /// A configuration space holding `caps` as (offset, id, next, enable) and
    /// nothing else, its pointer at `first`.
    fn space(first: u8, caps: &[(u8, u8, u8, u32)]) -> BTreeMap<u8, u32> {
        let mut space = BTreeMap::from([(CAPABILITIES_PTR, first as u32)]);
        for &(at, id, next, enable) in caps {
            space.insert(at, id as u32 | (next as u32) << 8 | enable);
        }
        space
    }

    fn of(space: &BTreeMap<u8, u32>) -> Result<Delivery, NoDelivery<()>> {
        Delivery::of(|at| Ok(*space.get(&at).unwrap_or(&0)))
    }

    /// 631120 §8.1: the PCH's GbE function points at `C8h`, its power
    /// management capability, then MSI at `D0h`, then the function-level reset
    /// at `E0h`, which ends the list. The T14's `8086:15fc` ends it at MSI.
    #[test]
    fn the_pch_functions_list_is_msi() {
        let datasheet =
            space(0xC8, &[(0xC8, 0x01, 0xD0, 0), (0xD0, msi::CAP_ID, 0xE0, MSI_ON), (0xE0, 0x13, 0, 0)]);
        let t14 = space(0xC8, &[(0xC8, 0x01, 0xD0, 0), (0xD0, msi::CAP_ID, 0, MSI_ON)]);
        assert_eq!(of(&datasheet), Ok(Delivery::Msi));
        assert_eq!(of(&t14), Ok(Delivery::Msi));
    }

    /// The kernel arms MSI-X where it is published, and leaves MSI off.
    #[test]
    fn an_enabled_msix_capability_is_msix_wherever_it_stands() {
        for msi_enable in [0, MSI_ON] {
            let list = space(
                0x40,
                &[(0x40, msi::CAP_ID, 0x60, msi_enable), (0x60, 0x10, 0xA0, 0), (0xA0, msix::CAP_ID, 0, MSIX_ON)],
            );
            assert_eq!(of(&list), Ok(Delivery::Msix));
        }
    }

    /// An MSI-X capability that is published and not enabled is not the mode:
    /// MSI is.
    #[test]
    fn a_published_msix_left_off_is_not_the_mode() {
        let list = space(0x40, &[(0x40, msix::CAP_ID, 0x50, 0), (0x50, msi::CAP_ID, 0, MSI_ON)]);
        assert_eq!(of(&list), Ok(Delivery::Msi));
    }

    #[test]
    fn a_function_with_neither_enabled_is_refused() {
        let list = space(0x40, &[(0x40, msix::CAP_ID, 0x50, 0), (0x50, msi::CAP_ID, 0, 0)]);
        assert_eq!(of(&list), Err(NoDelivery::NotArmed));
        assert_eq!(of(&space(0, &[])), Err(NoDelivery::NotArmed));
    }

    /// A link back onto the list, or into the header, ends the walk by name
    /// even after an enabled MSI: an MSI-X past it may be what was armed.
    #[test]
    fn a_list_that_breaks_off_is_refused_and_not_read_as_msi() {
        let cycle = space(0x40, &[(0x40, msi::CAP_ID, 0x50, MSI_ON), (0x50, 0x01, 0x40, 0)]);
        assert_eq!(of(&cycle), Err(NoDelivery::Truncated));
        let into_header = space(0x40, &[(0x40, msi::CAP_ID, 0x20, MSI_ON)]);
        assert_eq!(of(&into_header), Err(NoDelivery::Truncated));
    }

    #[test]
    fn a_refused_read_is_handed_up_with_its_offset() {
        let refused = Delivery::of(|at| if at == CAPABILITIES_PTR { Ok(0x50) } else { Err("gone") });
        assert_eq!(refused, Err(NoDelivery::Read { at: 0x50, why: "gone" }));
    }
}
