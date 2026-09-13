//! Whether an address reaches the bus at all.
//!
//! The windows are the ones firmware named
//! ([`toyos_abi::boot::KernelArgs::root_bridge_windows`]); what is decided here
//! is where one extent stands against them, for the BARs firmware assigned and
//! for the address space this kernel would put one in.

use toyos_abi::boot::RootBridgeWindow;

/// Where an extent stands against the memory windows the root bridges decode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decode {
    /// An extent of no length: nothing decodes it and nothing needs to.
    Empty,
    /// Wholly inside the window at this base.
    Inside(u64),
    /// Inside none of them.
    Unrouted,
}

/// Where `base .. end` stands against `windows`.
///
/// **An empty window list decodes nothing.** A machine whose firmware would not
/// say is not one where everything is inside a window; it is one where nothing
/// is known to be.
pub fn decode(windows: &[RootBridgeWindow], base: u64, end: u64) -> Decode {
    if base >= end {
        return Decode::Empty;
    }
    match windows.iter().find(|window| window.holds(base, end)) {
        Some(window) => Decode::Inside(window.base),
        None => Decode::Unrouted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ThinkPad T14's own two windows, as its firmware answered them on
    /// metal run 34.
    const T14: [RootBridgeWindow; 2] = [
        RootBridgeWindow { base: 0xa200_0000, length: 0x1b00_0000 },
        RootBridgeWindow { base: 0x40_0000_0000, length: 0x20_3dc0_0000 },
    ];

    #[test]
    fn an_extent_is_inside_a_window_only_when_the_whole_of_it_is() {
        // The I219's BAR on that machine, 128 KiB at 0xbcf00000.
        assert_eq!(decode(&T14, 0xbcf0_0000, 0xbcf2_0000), Decode::Inside(0xa200_0000));
        // The PCH SPI controller's, which no window on that machine holds.
        assert_eq!(decode(&T14, 0xfe01_0000, 0xfe01_1000), Decode::Unrouted);

        // The last byte of a window is inside it and the first byte past is not.
        assert_eq!(decode(&T14, 0xbcff_f000, 0xbd00_0000), Decode::Inside(0xa200_0000));
        assert_eq!(decode(&T14, 0xbcff_f000, 0xbd00_0001), Decode::Unrouted);
        // A base inside and an extent that is not: what a decision over the
        // base alone would call inside.
        assert_eq!(decode(&T14, 0xbcff_0000, 0xc000_0000), Decode::Unrouted);
        // And the second window is reached, not merely the first.
        assert_eq!(
            decode(&T14, 0x60_3db8_0000, 0x60_3db9_0000),
            Decode::Inside(0x40_0000_0000)
        );
    }

    /// The kernel's own 64-bit placement window on that same boot, which begins
    /// at the address firmware's high window ends at.
    #[test]
    fn a_window_that_begins_where_firmwares_ends_is_outside_it() {
        assert_eq!(decode(&T14, 0x60_3dc0_0000, 0x60_40c0_0000), Decode::Unrouted);
    }

    /// A machine whose firmware named nothing, and a span of no length: neither
    /// is an address anything decodes.
    #[test]
    fn nothing_is_inside_a_machine_that_named_no_window() {
        assert_eq!(decode(&[], 0xbcf0_0000, 0xbcf2_0000), Decode::Unrouted);
        assert_eq!(decode(&T14, 0, 0), Decode::Empty);
        assert_eq!(decode(&[], 0, 0), Decode::Empty);
    }
}
