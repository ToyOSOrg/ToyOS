//! Whether an address reaches the bus at all.
//!
//! The windows are a declaration of the memory the root bridges decode
//! ([`check`] decides whether one can be held at all); what is decided here is
//! where one extent stands against them, for the BARs firmware assigned and
//! for the address space this kernel would put one in.

use toyos_abi::acpi::{Apertures, RootBridgeWindow};

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
    /// A window over memory: a root bridge's window is MMIO and never RAM, so a
    /// declaration saying otherwise is wrong about one.
    Memory(RootBridgeWindow),
    /// A root's record is none the ABI names: a count or a run of windows
    /// past its array, an `_OSC` word it does not name, a commit beside no
    /// offer or of more than it offered, or a bus range that ends before it
    /// starts.
    Record,
    /// Two roots of one segment name one bus.
    Buses { segment: u16, first: u8, last: u8 },
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
            Self::Memory(w) => write!(f, "its window {:#x}..{:#x} lies over memory", w.base, w.end()),
            Self::Record => write!(f, "a root's record is none the ABI names"),
            Self::Buses { segment, first, last } => {
                write!(f, "two of its roots on segment {segment:#06x} share buses {first:#04x}..={last:#04x}")
            }
        }
    }
}

/// The root bridges' memory, held whole: at least one window, each an extent
/// that ends inside the address space and lies over no memory, no two sharing
/// an address. Only [`check`] makes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration<W>(W);

impl<W: AsRef<[RootBridgeWindow]>> Declaration<W> {
    pub fn windows(&self) -> &[RootBridgeWindow] {
        self.0.as_ref()
    }
}

/// `windows` as a [`Declaration`] on a machine whose memory is `memory`, as
/// `(start, end)`, or why it cannot be one. Refused whole, because a
/// declaration with one window dropped is one with a hole a BAR would be
/// placed in.
pub fn check<W: AsRef<[RootBridgeWindow]>>(windows: W, memory: &[(u64, u64)]) -> Result<Declaration<W>, Refused> {
    let list = windows.as_ref();
    if list.is_empty() {
        return Err(Refused::Empty);
    }
    for (at, window) in list.iter().enumerate() {
        if window.length == 0 || window.base.checked_add(window.length).is_none() {
            return Err(Refused::Malformed(*window));
        }
        if let Some(other) = list[..at].iter().find(|o| o.base < window.end() && window.base < o.end()) {
            return Err(Refused::Overlap(*other, *window));
        }
        if memory.iter().any(|&(start, end)| start < window.end() && window.base < end) {
            return Err(Refused::Memory(*window));
        }
    }
    Ok(Declaration(windows))
}

/// What the kernel holds of the root bridges' memory: nothing yet, a
/// declaration, or why there is none. It leaves [`Self::Unknown`] once and
/// never returns: a declaration is never replaced, and a refusal stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Held<W> {
    Unknown,
    Declared(Declaration<W>),
    Withheld(Withheld),
}

/// Why no declaration is held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Withheld {
    /// The declaration made was refused by [`check`].
    Refused(Refused),
    /// The only holder who may declare let go of its authority without
    /// declaring.
    Ended,
    /// A claim waited the whole of its deadline and nothing was declared.
    Unanswered,
}

impl core::fmt::Display for Withheld {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(why) => write!(f, "the declaration was refused: {why}"),
            Self::Ended => write!(f, "the acpi claim ended without declaring"),
            Self::Unanswered => write!(f, "nothing was declared before a claim's deadline"),
        }
    }
}

/// Why a declaration was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Declined {
    /// Refused whole, and now held as the answer.
    Refused(Refused),
    /// An answer is already held: a second declaration replaces nothing.
    Late,
}

impl<W: AsRef<[RootBridgeWindow]>> Held<W> {
    /// Take `windows` as the declaration on a machine whose memory is
    /// `memory`, from [`Self::Unknown`] alone; refused whole, the refusal is
    /// the answer.
    pub fn declare(&mut self, windows: W, memory: &[(u64, u64)]) -> Result<(), Declined> {
        if !matches!(self, Self::Unknown) {
            return Err(Declined::Late);
        }
        let (held, answer) = match check(windows, memory) {
            Ok(declaration) => (Self::Declared(declaration), Ok(())),
            Err(why) => (Self::Withheld(Withheld::Refused(why)), Err(Declined::Refused(why))),
        };
        *self = held;
        answer
    }

    /// Refuse a declaration whose records are none ([`records`]), from
    /// [`Self::Unknown`] alone; the refusal is the answer, as `declare`'s is.
    pub fn refuse(&mut self, why: Refused) -> Result<(), Declined> {
        if !matches!(self, Self::Unknown) {
            return Err(Declined::Late);
        }
        *self = Self::Withheld(Withheld::Refused(why));
        Err(Declined::Refused(why))
    }

    /// Nothing will be declared, for `why`: an answer where there was none,
    /// and nothing changed where there was one. Whether it was the answer.
    pub fn withhold(&mut self, why: Withheld) -> bool {
        let unknown = matches!(self, Self::Unknown);
        if unknown {
            *self = Self::Withheld(why);
        }
        unknown
    }

    /// The answer, or `None` while there is none to wait for yet.
    pub fn answer(&self) -> Option<Result<&Declaration<W>, Withheld>> {
        match self {
            Self::Unknown => None,
            Self::Declared(declaration) => Some(Ok(declaration)),
            Self::Withheld(why) => Some(Err(*why)),
        }
    }
}

/// Whether every root `declared` carries is one the ABI names, and no two
/// roots of one segment share a bus: what a declaration's windows are read
/// out of only once this holds.
pub fn records(declared: &Apertures) -> Result<(), Refused> {
    let bridges = declared.bridges().ok_or(Refused::Record)?;
    for (at, bridge) in bridges.iter().enumerate() {
        let named = bridge.first_bus <= bridge.last_bus && bridge.commit().is_some() && declared.windows_of(bridge).is_some();
        if !named {
            return Err(Refused::Record);
        }
        let shares = |other: &&toyos_abi::acpi::HostBridge| {
            other.segment == bridge.segment && other.first_bus <= bridge.last_bus && bridge.first_bus <= other.last_bus
        };
        if bridges[..at].iter().any(|other| shares(&other)) {
            return Err(Refused::Buses { segment: bridge.segment, first: bridge.first_bus, last: bridge.last_bus });
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

    /// The T14's and the AMD bench's recorded declarations, seven windows
    /// each, some meeting end to end, are held; an empty list, a window of no
    /// length or past the address space, and two windows sharing an address
    /// are each refused by name.
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
        assert_eq!(check(&recorded, &[]).map(|d| d.windows().len()), Ok(7));
        let amd_bench = [
            w(0xd000_0000, 0x70_0000),
            w(0xfc_9000_0000, 0x1020_0000),
            w(0xd070_0000, 0x2790_0000),
            w(0xfc00_0000, 0x1c0_0000),
            w(0xff00_0000, 0xea_b000),
            w(0x4_5020_0000, 0xf8_3fe0_0000),
            w(0xfc_a020_0000, 0xfe0_0000),
        ];
        assert_eq!(check(&amd_bench, &[]).map(|d| d.windows().len()), Ok(7));
        assert_eq!(check(&[], &[]), Err(Refused::Empty));
        assert_eq!(check(&[w(0x1000, 0)], &[]), Err(Refused::Malformed(w(0x1000, 0))));
        assert_eq!(check(&[w(u64::MAX, 2)], &[]), Err(Refused::Malformed(w(u64::MAX, 2))));
        assert_eq!(
            check(&[w(0x1000_0000, 0x100_0000), w(0x10ff_f000, 0x2000)], &[]),
            Err(Refused::Overlap(w(0x1000_0000, 0x100_0000), w(0x10ff_f000, 0x2000)))
        );
    }

    /// `virt`'s two windows (what its root's `_CRS` declares) beside its
    /// memory at 0x40000000 are held; a window reaching one byte into that
    /// memory, and one wholly over it, are refused by name, whatever their
    /// place in the list.
    #[test]
    fn a_window_over_memory_is_refused_by_name() {
        let w = |base, length| RootBridgeWindow { base, length };
        const MEMORY: [(u64, u64); 1] = [(0x4000_0000, 0x5000_0000)];
        let virt = [w(0x1000_0000, 0x2eff_0000), w(0x80_0000_0000, 0x80_0000_0000)];
        assert_eq!(check(&virt, &MEMORY).map(|d| d.windows().len()), Ok(2));
        let into = w(0x3f00_0000, 0x100_0001);
        assert_eq!(check(&[virt[1], into], &MEMORY), Err(Refused::Memory(into)));
        let over = w(0x4800_0000, 0x1000);
        assert_eq!(check(&[over, virt[0]], &MEMORY), Err(Refused::Memory(over)));
        assert_eq!(check(&[w(0x5000_0000, 0x1000)], &MEMORY).map(|d| d.windows().len()), Ok(1));
    }

    /// A machine whose firmware named nothing, and a span of no length: neither
    /// is an address anything decodes.
    #[test]
    fn nothing_is_inside_a_machine_that_named_no_window() {
        assert_eq!(decode(&[], 0xbcf0_0000, 0xbcf2_0000), Decode::Unrouted);
        assert_eq!(decode(&T14, 0, 0), Decode::Empty);
        assert_eq!(decode(&[], 0, 0), Decode::Empty);
    }

    /// The state leaves `Unknown` once: a declaration is held and a second
    /// one, whatever it says, is late; a refused declaration is the answer
    /// and the next one is late too; the holder's end and a claim's deadline
    /// answer only where nothing had, and change no answer held.
    #[test]
    fn the_declaration_is_answered_once_and_never_replaced() {
        let w = |base, length| RootBridgeWindow { base, length };
        let virt = [w(0x1000_0000, 0x2eff_0000), w(0x80_0000_0000, 0x80_0000_0000)];

        let mut held = Held::Unknown;
        assert_eq!(held.answer(), None);
        assert_eq!(held.declare(virt, &[]), Ok(()));
        assert_eq!(held.declare([w(0x1000, 0x1000); 2], &[]), Err(Declined::Late));
        assert!(!held.withhold(Withheld::Ended));
        assert!(!held.withhold(Withheld::Unanswered));
        assert_eq!(held.answer().map(|a| a.map(|d| d.windows().to_vec())), Some(Ok(virt.to_vec())));

        let mut refused = Held::Unknown;
        assert_eq!(refused.declare([w(0, 0); 2], &[]), Err(Declined::Refused(Refused::Malformed(w(0, 0)))));
        assert_eq!(refused.declare(virt, &[]), Err(Declined::Late));
        assert_eq!(refused.answer().map(|a| a.err()), Some(Some(Withheld::Refused(Refused::Malformed(w(0, 0))))));

        for why in [Withheld::Ended, Withheld::Unanswered] {
            let mut ended = Held::<[RootBridgeWindow; 2]>::Unknown;
            assert!(ended.withhold(why));
            assert!(!ended.withhold(Withheld::Unanswered));
            assert_eq!(ended.declare(virt, &[]), Err(Declined::Late));
            assert_eq!(ended.answer().map(|a| a.err()), Some(Some(why)));
        }
    }

    /// Each malformed record is refused by name before a window is read: a
    /// count past the array, a run of windows past those declared, an
    /// `_OSC` word the ABI does not name, a reversed bus range, and two roots
    /// of one segment sharing a bus; roots on two segments may share bus
    /// numbers. A refused record is the answer, and later declarations late.
    #[test]
    fn a_malformed_record_is_refused_by_name_and_is_the_answer() {
        use toyos_abi::acpi::{HostBridge, Osc};
        let w = RootBridgeWindow { base: 0x8000_0000, length: 0x1000_0000 };
        let mut good = Apertures::default();
        good.push(0, (0, 0x7f), &[w], Osc::Absent, None).expect("fits");
        assert_eq!(records(&good), Ok(()));
        let first = good.bridges[0];
        let with = |bridge: HostBridge| Apertures { bridges: [bridge; toyos_abi::acpi::MAX_BRIDGES], ..good };
        assert_eq!(records(&Apertures { bridge_count: u32::MAX, ..good }), Err(Refused::Record));
        assert_eq!(records(&with(HostBridge { window_count: 2, ..first })), Err(Refused::Record));
        assert_eq!(records(&with(HostBridge { osc: 7, ..first })), Err(Refused::Record));
        assert_eq!(records(&with(HostBridge { first_bus: 0x80, last_bus: 0x7f, ..first })), Err(Refused::Record));
        // A commit with no offer beside it.
        let mut uncommitted = Apertures::default();
        uncommitted.push(0, (0, 0x7f), &[w], Osc::Absent, Some(toyos_abi::acpi::NATIVE_HOT_PLUG)).expect("fits");
        assert_eq!(records(&uncommitted), Err(Refused::Record));
        let mut offered = Apertures::default();
        offered.push(0, (0, 0x7f), &[w], Osc::Offered(toyos_abi::acpi::NATIVE_HOT_PLUG), None).expect("fits");
        assert_eq!(records(&offered), Ok(()));

        let mut shared = good;
        shared.push(0, (0x40, 0xff), &[], Osc::Absent, None).expect("fits");
        assert_eq!(records(&shared), Err(Refused::Buses { segment: 0, first: 0x40, last: 0xff }));
        let mut apart = good;
        apart.push(1, (0, 0x7f), &[], Osc::Absent, None).expect("fits");
        assert_eq!(records(&apart), Ok(()));

        let mut held = Held::<[RootBridgeWindow; 1]>::Unknown;
        assert_eq!(held.refuse(Refused::Record), Err(Declined::Refused(Refused::Record)));
        assert_eq!(held.answer().map(|a| a.err()), Some(Some(Withheld::Refused(Refused::Record))));
        assert_eq!(held.declare([w], &[]), Err(Declined::Late));
        assert_eq!(held.refuse(Refused::Record), Err(Declined::Late));
    }
}
