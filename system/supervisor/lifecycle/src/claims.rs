//! Which claims a start of a row with one process mints.
//!
//! A `devices` PCI matcher names the one function it matches that the kernel
//! does not drive, and the claim names that function by its arrival. **A start
//! that follows an end is owed every claim the process before it held**: one
//! it cannot mint is no start, so a service never runs on without a device it
//! had.

use toyos_abi::inventory::{Arrival, Driven, Pci, PciAddr};
use toyos_abi::syscall::SyscallError;
use toyos_manifest::device::PciMatch;

/// Why a matcher names no one function.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Unresolved {
    /// No function matches it.
    Absent,
    /// Every function it matches is the kernel's.
    KernelDriven(Vec<PciAddr>),
    /// It matches every one of these, and a row with one process cannot say
    /// which.
    Ambiguous(Vec<PciAddr>),
}

/// The function `matcher` names on a machine whose inventory is `records` and
/// whose boot controller is `boot`.
pub fn function(matcher: &PciMatch, records: &[Pci], boot: Option<PciAddr>) -> Result<Arrival, Unresolved> {
    let matched: Vec<&Pci> = records.iter().filter(|pci| matcher.matches(pci, boot)).collect();
    let undriven: Vec<&Pci> = matched.iter().copied().filter(|pci| pci.driven != Driven::Kernel).collect();
    match (&undriven[..], &matched[..]) {
        ([one], _) => Ok(one.arrival),
        ([], []) => Err(Unresolved::Absent),
        ([], driven) => Err(Unresolved::KernelDriven(driven.iter().map(|pci| pci.at).collect())),
        (several, _) => Err(Unresolved::Ambiguous(several.iter().map(|pci| pci.at).collect())),
    }
}

/// Why a start holds no claim for an entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Missing {
    Unresolved(Unresolved),
    /// The kernel refused the claim.
    Refused(SyscallError),
    /// A virtio function that offers no `VIRTIO_F_ACCESS_PLATFORM` uses
    /// physical addresses whatever translates the bus (virtio 1.2 §6.1), so
    /// no domain confines what it reaches; or one whose offer could not be
    /// read, which is the same unknown.
    Untranslated(Option<String>),
}

/// Virtio 1.2 §4.1.2: a virtio function's vendor and device ids, transitional
/// and modern.
pub fn is_virtio(pci: &Pci) -> bool {
    pci.vendor == 0x1af4 && (0x1000..=0x107f).contains(&pci.device)
}

/// Whether a start goes on without `entry`'s claim. A matcher naming several
/// functions refuses the start, and so does a claim the process before it
/// held, `owed`.
pub fn goes_on_without(entry: &str, missing: &Missing, owed: &[String]) -> bool {
    !matches!(missing, Missing::Unresolved(Unresolved::Ambiguous(_))) && !owed.iter().any(|held| held == entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::syscall::PciId;

    const EDU: PciMatch = PciMatch::new([0xff, 0x00, 0x00], Some(PciId { vendor: 0x1234, device: 0x11e8 }));

    fn edu(arrival: u32, dev: u8, driven: Driven) -> Pci {
        Pci {
            arrival: Arrival(arrival),
            at: PciAddr { segment: 0, bus: 0, dev, func: 0 },
            vendor: 0x1234,
            device: 0x11e8,
            class: 0xff,
            subclass: 0,
            prog_if: 0,
            driven,
        }
    }

    fn at(dev: u8) -> PciAddr {
        PciAddr { segment: 0, bus: 0, dev, func: 0 }
    }

    #[test]
    fn a_virtio_function_is_known_by_its_ids_and_an_untranslated_one_is_gone_without() {
        let virtio = |device| Pci { vendor: 0x1af4, device, ..edu(1, 3, Driven::Free) };
        assert!(is_virtio(&virtio(0x1000)) && is_virtio(&virtio(0x1041)) && is_virtio(&virtio(0x107f)));
        assert!(!is_virtio(&virtio(0x1080)) && !is_virtio(&virtio(0x0fff)));
        assert!(!is_virtio(&edu(1, 3, Driven::Free)));
        let refused = Missing::Untranslated(None);
        assert!(goes_on_without("pci:02:00:00", &refused, &[]));
        assert!(!goes_on_without("pci:02:00:00", &refused, &["pci:02:00:00".to_string()]));
    }

    #[test]
    fn a_matcher_names_the_one_function_the_kernel_does_not_drive() {
        assert_eq!(function(&EDU, &[edu(3, 3, Driven::Kernel), edu(4, 4, Driven::Free)], None), Ok(Arrival(4)));
        // One another process holds is still the function named, and its
        // claim is refused by the kernel, which says so.
        assert_eq!(function(&EDU, &[edu(4, 4, Driven::Claimed)], None), Ok(Arrival(4)));
        assert_eq!(function(&EDU, &[], None), Err(Unresolved::Absent));
        assert_eq!(function(&EDU, &[edu(3, 3, Driven::Kernel)], None), Err(Unresolved::KernelDriven(vec![at(3)])));
    }

    /// Two functions a single-process row's matcher names are both named back,
    /// and its start is refused whatever it was owed.
    #[test]
    fn two_matching_functions_refuse_the_start_naming_both() {
        let two = function(&EDU, &[edu(3, 3, Driven::Free), edu(4, 4, Driven::Claimed)], None);
        assert_eq!(two, Err(Unresolved::Ambiguous(vec![at(3), at(4)])));
        let missing = Missing::Unresolved(two.unwrap_err());
        assert!(!goes_on_without("pci:ff:00:00/1234:11e8", &missing, &[]));
    }

    /// A start that follows an end is refused every claim the process before
    /// it held, and goes on without one it never had.
    #[test]
    fn a_start_goes_on_without_only_what_it_was_not_owed() {
        let entry = "pci:02:00:00/1af4:1041";
        let owed = [entry.to_string()];
        for missing in [Missing::Unresolved(Unresolved::Absent), Missing::Refused(SyscallError::AlreadyExists)] {
            assert!(goes_on_without(entry, &missing, &[]), "{missing:?}");
            assert!(!goes_on_without(entry, &missing, &owed), "{missing:?}");
            assert!(goes_on_without("acpi", &missing, &owed), "{missing:?}");
        }
    }
}
