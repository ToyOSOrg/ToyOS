//! The x86-64 IOMMU: VT-d where firmware publishes a DMAR, AMD-Vi where it
//! publishes an IVRS.
//!
//! Only VT-d gives a domain or remaps an interrupt. AMD-Vi programs no unit,
//! so on its machines VT-d holds none: every [`domain`] is refused `NoUnit`
//! and no [`interrupt`] is armed
//! (`issues/an-amd-vi-machine-isolates-no-device.md`).

use crate::drivers::pci::PciDevice;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;
use toyos_abi::boot::RootBridgeWindow;

pub use super::vtd::{domain, fault, interrupt};

/// x86-64's 52-bit physical-address ceiling: a register base at or above this
/// is not an address at all, and would otherwise wrap `DirectMap::as_ptr`'s
/// unchecked offset into the user half.
const MAX_PHYS: u64 = 1 << 52;

/// The units of whichever table firmware published.
pub fn init(rsdp_addr: u64, devices: &[PciDevice], windows: &[RootBridgeWindow]) {
    if super::vtd::init(rsdp_addr, devices, windows) || super::amdvi::init(rsdp_addr) {
        return;
    }
    // ACPI cannot tell "no IOMMU silicon" from "the IOMMU disabled in
    // firmware"; the line names both rather than guess.
    log!(
        "iommu: no DMAR or IVRS table — this platform has no IOMMU, or it is disabled in firmware setup \
         (look for \"VT-d\", \"AMD-Vi\" or \"IOMMU\")"
    );
}

/// Firmware's register base, mapped only if aligned to its `len`-byte window
/// and within the physical range — never clamped to fit, since a base in
/// usable RAM would decode as plausible registers until a write lands in
/// somebody's heap.
pub(super) fn register_window(base: u64, len: u64) -> Option<Mmio> {
    if base == 0 || !base.is_multiple_of(len) || base >= MAX_PHYS {
        return None;
    }
    Some(crate::mm::paging::map_mmio(base, len, MmioPolicy::Uncacheable))
}

/// One character per boolean; `n` is printed rather than omitted, since an
/// absent field would look like a forgotten one.
pub(super) fn yn(v: bool) -> char {
    if v {
        'y'
    } else {
        'n'
    }
}
