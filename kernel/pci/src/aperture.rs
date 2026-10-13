//! Whether an address reaches the bus at all.
//!
//! The windows are a declaration of the memory the root bridges decode
//! ([`check`] decides whether one can be held at all); what is decided here is
//! where one extent stands against them, for the BARs firmware assigned and
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

/// Why a declaration of the root bridges' memory is refused whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// It names no window, so nothing on the machine is known to be routed.
    Empty,
    /// A window of no length, or one whose end is past the address space.
    Malformed(RootBridgeWindow),
    /// Two windows name one address: whichever is wrong, the declaration does
    /// not say what the bridges decode.
    Overlap(RootBridgeWindow, RootBridgeWindow),
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => write!(f, "it names no window"),
            Self::Malformed(w) => write!(f, "its window {:#x}+{:#x} is no extent of the address space", w.base, w.length),
            Self::Overlap(a, b) => write!(
                f,
                "its windows {:#x}..{:#x} and {:#x}..{:#x} share an address",
                a.base,
                a.end(),
                b.base,
                b.end()
            ),
        }
    }
}

/// Whether `windows` can be held as the root bridges' memory: at least one
/// window, each an extent that ends inside the address space, no two sharing
/// an address. Refused whole, because a declaration with one window dropped
/// is one with a hole a BAR would be placed in.
pub fn check(windows: &[RootBridgeWindow]) -> Result<(), Refused> {
    if windows.is_empty() {
        return Err(Refused::Empty);
    }
    for (at, window) in windows.iter().enumerate() {
        if window.length == 0 || window.base.checked_add(window.length).is_none() {
            return Err(Refused::Malformed(*window));
        }
        if let Some(other) = windows[..at].iter().find(|o| o.base < window.end() && window.base < o.end()) {
            return Err(Refused::Overlap(*other, *window));
        }
    }
    Ok(())
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
    match windows.iter().find(|window| window.holds(base, end - base)) {
        Some(window) => Decode::Inside(window.base),
        None => Decode::Unrouted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ThinkPad T14's own two windows.
    const T14: [RootBridgeWindow; 2] = [
        RootBridgeWindow { base: 0xa200_0000, length: 0x1b00_0000 },
        RootBridgeWindow { base: 0x40_0000_0000, length: 0x20_3dc0_0000 },
    ];

    /// Every window is asked and the answer names the one that holds the
    /// extent, which is what a decision over the first window alone could not.
    #[test]
    fn the_window_an_extent_is_inside_is_the_one_named() {
        assert_eq!(decode(&T14, 0xbcf0_0000, 0xbcf2_0000), Decode::Inside(0xa200_0000));
        assert_eq!(
            decode(&T14, 0x60_3db8_0000, 0x60_3db9_0000),
            Decode::Inside(0x40_0000_0000)
        );
        assert_eq!(decode(&T14, 0xfe01_0000, 0xfe01_1000), Decode::Unrouted);
    }

    /// The T14's recorded declaration, seven windows that meet end to end, is
    /// held; an empty list, a window of no length or past the address space,
    /// and two windows sharing an address are each refused by name.
    #[test]
    fn a_declaration_is_held_only_whole_and_disjoint() {
        let w = |base, length| RootBridgeWindow { base, length };
        let recorded = [
            w(0xa200_0000, 0x1b00_0000),
            w(0x40_0000_0000, 0x20_3dc0_0000),
            w(0xa080_0000, 0x180_0000),
            w(0xbd00_0000, 0x300_0000),
            w(0xff00_0000, 0xb8_0000),
            w(0xffd3_a070, 0x2c_5f90),
            w(0x60_3dc0_0000, 0x1f_c240_0000),
        ];
        assert_eq!(check(&recorded), Ok(()));
        assert_eq!(check(&[]), Err(Refused::Empty));
        assert_eq!(check(&[w(0x1000, 0)]), Err(Refused::Malformed(w(0x1000, 0))));
        assert_eq!(check(&[w(u64::MAX, 2)]), Err(Refused::Malformed(w(u64::MAX, 2))));
        assert_eq!(
            check(&[w(0x1000_0000, 0x100_0000), w(0x10ff_f000, 0x2000)]),
            Err(Refused::Overlap(w(0x1000_0000, 0x100_0000), w(0x10ff_f000, 0x2000)))
        );
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
