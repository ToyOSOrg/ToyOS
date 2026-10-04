//! The Power Management capability, and the one reset it carries: a function
//! taken to D3hot and back to D0 comes back in its reset state unless it says
//! it will not (PCI Bus Power Management Interface Specification 1.2, §5.4 and
//! §5.6.1, `No_Soft_Reset`).

/// This capability's id in a function's capability list.
pub const CAP_ID: u8 = 0x01;

/// Byte offset of the Power Management Control/Status register.
pub const PMCSR: u64 = 0x04;

/// PMCSR bits 1:0, the power state.
const STATE: u16 = 0b11;
pub const D0: u16 = 0b00;
pub const D3HOT: u16 = 0b11;

/// PMCSR bit 3: the function keeps its state across D3hot → D0.
const NO_SOFT_RESET: u16 = 1 << 3;

/// §5.6.1 table 5-6: the wait after a write that moves a function into or out
/// of D3hot, before it may be touched again.
pub const TRANSITION_NANOS: u64 = 10_000_000;

/// Whether the D3hot round trip resets this function.
pub const fn resets(pmcsr: u16) -> bool {
    pmcsr & NO_SOFT_RESET == 0
}

/// The PMCSR word that moves the function to `state`. Every other bit is
/// preserved, `PME_Status` (bit 15, write-one-to-clear) excepted: writing back
/// a one there would clear an event nobody asked to clear.
pub const fn to(pmcsr: u16, state: u16) -> u16 {
    (pmcsr & !STATE & !(1 << 15)) | (state & STATE)
}

pub const fn state(pmcsr: u16) -> u16 {
    pmcsr & STATE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_soft_reset_is_bit_three_and_nothing_else() {
        assert!(resets(0));
        assert!(!resets(1 << 3));
        for bit in 0..16 {
            assert_eq!(resets(1 << bit), bit != 3, "bit {bit}");
        }
    }

    #[test]
    fn a_transition_changes_the_state_bits_and_clears_no_event() {
        assert_eq!(to(0x0000, D3HOT), 0x0003);
        assert_eq!(to(0x0003, D0), 0x0000);
        assert_eq!(to(0x8100, D3HOT), 0x0103);
        assert_eq!(state(to(0x0108, D3HOT)), D3HOT);
    }
}
