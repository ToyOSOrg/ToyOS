//! What a firmware-reserved memory region (a VT-d RMRR) does to each function
//! it names, by the owner's ruling of 2026-10-04 ("Display and USB only"):
//! a display or USB controller the region names alone keeps it, mapped into
//! its driver's own domain; every other function it names is refused a claim.

/// PCI class codes, as config space's dword at 08h holds them above its
/// revision: base class 03h is a display controller, 0C03h a USB one.
const DISPLAY: u32 = 0x03;
const USB: u32 = 0x0C03;

/// One function's verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The region, `start..end`, kept in the function's own domain.
    Kept(u64, u64),
    /// No claim may have the function, and why.
    Refused(&'static str),
}

/// A function named by a region over `start..end`.
///
/// `alone`: the region names this function and nothing else, no bridge's
/// sub-hierarchy and no path that could not be followed. `class`: config
/// space's class code (dword 08h shifted right by 8), or `None` for a
/// function nobody enumerated. A region not whole 4 KiB pages is refused,
/// since a page would reach memory firmware did not reserve.
pub fn verdict(alone: bool, class: Option<u32>, start: u64, end: u64) -> Verdict {
    let controller = class.is_some_and(|c| c >> 16 == DISPLAY || c >> 8 == USB);
    if !alone {
        Verdict::Refused("firmware reserves memory for it that another device shares")
    } else if !controller {
        Verdict::Refused("firmware reserves memory for it, and it is neither a display nor a USB controller")
    } else if !start.is_multiple_of(4096) || !end.is_multiple_of(4096) || end <= start {
        Verdict::Refused("firmware reserves memory for it that is not whole 4 KiB pages")
    } else {
        Verdict::Kept(start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The T14's: `0x9c000000..0xa07fffff`, naming the iGPU (class 030000h) alone.
    const T14: (u64, u64) = (0x9c00_0000, 0xa080_0000);

    #[test]
    fn a_display_controller_named_alone_keeps_its_region() {
        assert_eq!(verdict(true, Some(0x03_00_00), T14.0, T14.1), Verdict::Kept(T14.0, T14.1));
    }

    #[test]
    fn a_usb_controller_named_alone_keeps_a_small_region() {
        // An xHCI (0C0330h) with the usual few pages, not 2 MiB aligned.
        assert_eq!(verdict(true, Some(0x0C_03_30), 0x7b80_3000, 0x7b80_5000), Verdict::Kept(0x7b80_3000, 0x7b80_5000));
    }

    #[test]
    fn a_display_controller_that_shares_its_region_is_refused() {
        assert!(matches!(verdict(false, Some(0x03_00_00), T14.0, T14.1), Verdict::Refused(_)));
    }

    #[test]
    fn a_function_neither_display_nor_usb_is_refused() {
        // A network controller, and a USB-class one that is not a host
        // controller of the USB subclass (0C80h, a serial bus "other").
        assert!(matches!(verdict(true, Some(0x02_00_00), T14.0, T14.1), Verdict::Refused(_)));
        assert!(matches!(verdict(true, Some(0x0C_80_00), T14.0, T14.1), Verdict::Refused(_)));
        assert!(matches!(verdict(true, None, T14.0, T14.1), Verdict::Refused(_)));
    }

    #[test]
    fn a_region_not_whole_pages_is_refused() {
        assert!(matches!(verdict(true, Some(0x03_00_00), T14.0 + 0x800, T14.1), Verdict::Refused(_)));
        assert!(matches!(verdict(true, Some(0x03_00_00), T14.0, T14.0), Verdict::Refused(_)));
    }
}
