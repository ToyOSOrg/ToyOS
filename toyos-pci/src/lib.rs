//! A PCI function's Base Address Registers and its two interrupt capability
//! structures, decoded as pure functions.
//!
//! A message-signalled interrupt is a DMA write the *device* performs, to an
//! address the kernel programs. Everything that decides that address comes out
//! of registers the device published: which BAR its table lives in, how far
//! into it, how wide its address register is. So every one of those numbers is
//! untrusted, and untrusted here means refused rather than corrected — a
//! function that names a reserved BAR indicator is not a function to truncate
//! into range, it is a function whose interrupts this kernel declines to arm.
//!
//! [`bar`] is that sentence one register lower down, and it is where the
//! reserved-indicator refusal stopped short: `msix` refuses a BAR *index* it
//! cannot use, and then the BAR that index named was decoded by a function
//! with no encoding of an I/O BAR it refused.
//!
//! No I/O and no register writes: the effects belong to `drivers/pci.rs`,
//! which is the one place in the kernel that touches any of the three.
//!
//! `no_std`, no allocation, no `unsafe`.

#![no_std]
#![forbid(unsafe_code)]

pub mod bar;
pub mod caps;
pub mod express;
pub mod msi;
pub mod msix;

/// Which of a function's two message capabilities a claim may be armed on.
///
/// **A function that publishes MSI-X is armed on MSI-X or on nothing.** Its
/// table and its PBA live in one of its BARs, and that BAR is withheld from the
/// holder only where this kernel armed the table itself — so a fall back to MSI
/// would hand the BAR over with the table still inside it, and a holder that can
/// write a table entry can point the device's write at any address the LAPIC
/// decodes. MSI is for a function that publishes no table at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    Msix,
    Msi,
}

/// `None` for a function that publishes neither capability: nothing to arm.
pub fn mechanism(publishes_msix: bool, publishes_msi: bool) -> Option<Mechanism> {
    match (publishes_msix, publishes_msi) {
        (true, _) => Some(Mechanism::Msix),
        (false, true) => Some(Mechanism::Msi),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole of the rule: what a function publishes decides this, and
    /// whether the arming then succeeded may not enter into it.
    #[test]
    fn a_function_that_publishes_msix_is_never_offered_msi() {
        assert_eq!(mechanism(true, true), Some(Mechanism::Msix));
        assert_eq!(mechanism(true, false), Some(Mechanism::Msix));
    }

    #[test]
    fn msi_is_for_a_function_with_no_table_in_a_bar() {
        assert_eq!(mechanism(false, true), Some(Mechanism::Msi));
        assert_eq!(mechanism(false, false), None);
    }
}
