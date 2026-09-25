//! Which claim each of `pcidev`'s slots is for: the decision alone, over the
//! array the kernel's slot lock guards.
//!
//! **A slot a function left a residue in is that function's alone.** Its
//! domain keeps the device addresses the function was left aimed at, and the
//! function stays attached to it; a second function given that slot would be
//! handed those addresses and share the domain with a part that may still
//! reach it. So [`reserve`] answers a requester its own residue slot first, a
//! free slot next, and never another requester's residue.

/// Who a slot is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Free,
    /// A claim on this requester holds it.
    Held(u16),
    /// Nobody holds it, and its domain keeps the addresses this requester's
    /// function was left aimed at.
    Residue(u16),
}

/// Why a requester is given no slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A claim on this requester holds a slot already.
    Owned,
    /// Every slot is held, or is another function's residue.
    Exhausted,
}

/// Take a slot for `who`: the one its residue is in, else a free one.
pub fn reserve(slots: &mut [Slot], who: u16) -> Result<usize, Refused> {
    if slots.contains(&Slot::Held(who)) {
        return Err(Refused::Owned);
    }
    let slot = slots
        .iter()
        .position(|slot| *slot == Slot::Residue(who))
        .or_else(|| slots.iter().position(|slot| *slot == Slot::Free))
        .ok_or(Refused::Exhausted)?;
    slots[slot] = Slot::Held(who);
    Ok(slot)
}

/// Give a slot up: its function's alone while it left a residue, free
/// otherwise. A slot that was not held — a hand-over refused before it bound
/// anything — is free.
pub fn release(slots: &mut [Slot], slot: usize, residue: bool) {
    slots[slot] = match slots[slot] {
        Slot::Held(who) if residue => Slot::Residue(who),
        _ => Slot::Free,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const NIC: u16 = 0x0010;
    const OTHER: u16 = 0x0018;

    #[test]
    fn a_second_function_takes_a_free_slot_and_never_anothers_residue() {
        let mut slots = [Slot::Residue(NIC), Slot::Free, Slot::Free, Slot::Free];
        assert_eq!(reserve(&mut slots, OTHER), Ok(1));
        assert_eq!(slots[0], Slot::Residue(NIC), "another function's claim took the residue's slot");
    }

    #[test]
    fn a_function_goes_back_to_its_own_residue_ahead_of_a_free_slot() {
        let mut slots = [Slot::Free, Slot::Residue(OTHER), Slot::Residue(NIC), Slot::Free];
        assert_eq!(reserve(&mut slots, NIC), Ok(2));
        assert_eq!(slots[2], Slot::Held(NIC));
    }

    #[test]
    fn other_functions_residues_are_no_free_slot() {
        let mut slots = [Slot::Residue(NIC), Slot::Held(3), Slot::Residue(4), Slot::Held(5)];
        assert_eq!(reserve(&mut slots, OTHER), Err(Refused::Exhausted));
        assert_eq!(slots, [Slot::Residue(NIC), Slot::Held(3), Slot::Residue(4), Slot::Held(5)]);
    }

    #[test]
    fn a_held_function_is_owned() {
        let mut slots = [Slot::Free, Slot::Held(NIC), Slot::Free, Slot::Free];
        assert_eq!(reserve(&mut slots, NIC), Err(Refused::Owned));
    }

    #[test]
    fn a_release_keeps_the_slot_only_for_a_residue() {
        let mut slots = [Slot::Held(NIC), Slot::Held(OTHER), Slot::Free, Slot::Free];
        release(&mut slots, 0, true);
        release(&mut slots, 1, false);
        release(&mut slots, 2, true);
        assert_eq!(slots, [Slot::Residue(NIC), Slot::Free, Slot::Free, Slot::Free]);
    }
}
