//! The x86-64 IOMMU: VT-d where firmware publishes a DMAR, AMD-Vi where it
//! publishes an IVRS, chosen once at [`init`] and asked through [`domain`]
//! and [`fault`]. Only VT-d remaps an interrupt
//! (`issues/an-amd-vi-machine-isolates-no-device.md`, stage 6).

use core::sync::atomic::{AtomicU8, Ordering};

use crate::drivers::pci::PciDevice;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;
use toyos_abi::boot::RootBridgeWindow;

pub use super::vtd::interrupt;

/// Which backend [`init`] found: none until it has, and then fixed.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Backend {
    None,
    Vtd,
    AmdVi,
}

static BACKEND: AtomicU8 = AtomicU8::new(Backend::None as u8);

fn backend() -> Backend {
    match BACKEND.load(Ordering::Acquire) {
        1 => Backend::Vtd,
        2 => Backend::AmdVi,
        _ => Backend::None,
    }
}

/// x86-64's 52-bit physical-address ceiling: a register base at or above this
/// is not an address at all, and would otherwise wrap `DirectMap::as_ptr`'s
/// unchecked offset into the user half.
const MAX_PHYS: u64 = 1 << 52;

/// The units of whichever table firmware published.
pub fn init(rsdp_addr: u64, devices: &[PciDevice], windows: &[RootBridgeWindow]) {
    let found = if super::vtd::init(rsdp_addr, devices, windows) {
        Backend::Vtd
    } else if super::amdvi::init(rsdp_addr, devices, windows) {
        Backend::AmdVi
    } else {
        Backend::None
    };
    BACKEND.store(found as u8, Ordering::Release);
    // Once the backend is chosen: the selftest makes a domain through it.
    #[cfg(feature = "boot-actuators")]
    if found == Backend::AmdVi && crate::actuator::amdvi_selftest() {
        super::amdvi::selftest::run(devices);
    }
    if found != Backend::None {
        return;
    }
    // ACPI cannot tell "no IOMMU silicon" from "the IOMMU disabled in
    // firmware"; the line names both rather than guess.
    log!(
        "iommu: no DMAR or IVRS table — this platform has no IOMMU, or it is disabled in firmware setup \
         (look for \"VT-d\", \"AMD-Vi\" or \"IOMMU\")"
    );
}

/// Ceiling on units inventoried; a machine past it is told, not silently truncated.
pub(super) const MAX_UNITS: usize = 16;

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

/// Swaps the present 16-byte entry `old` at `entry` for `new` in one
/// `lock cmpxchg16b`: a unit may fetch it at any moment and must read the
/// one or the other, never a half of each.
pub(super) fn exchange16(entry: Mmio, old: [u64; 2], new: [u64; 2]) {
    let entry = entry.subregion(0, 16);
    assert!(entry.addr().is_multiple_of(16), "iommu: a 16-byte entry at {:#x} is not aligned to it", entry.addr());
    let (found_lo, found_hi): (u64, u64);
    // SAFETY: `entry` is 16 bytes of a table the caller's unit reads, 16-byte
    // aligned as asserted; `rbx`, which LLVM reserves, holds `new`'s low half
    // only between the two moves. Every operand names its register: LLVM may
    // give `rbx` to a `reg` operand, and the `xchg` would then swap the
    // address out of it.
    unsafe {
        core::arch::asm!(
            "xchg r8, rbx",
            "lock cmpxchg16b xmmword ptr [rsi]",
            "mov rbx, r8",
            in("rsi") entry.addr(),
            inout("r8") new[0] => _,
            in("rcx") new[1],
            inout("rax") old[0] => found_lo,
            inout("rdx") old[1] => found_hi,
            options(nostack),
        );
    }
    // `found` is `old` exactly when the swap happened; nothing else writes an
    // entry under its backend's lock, so anything else is a kernel bug.
    assert!(
        [found_lo, found_hi] == old,
        "iommu: the entry at {:#x} held {found_hi:#x}:{found_lo:#x}, not the {:#x}:{:#x} read before it was replaced",
        entry.addr(),
        old[1],
        old[0]
    );
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

/// A device's address spaces, on whichever backend translates.
pub mod domain {
    use super::{backend, Backend};
    use crate::arch::x86_64::{amdvi, vtd};
    use crate::iommu::{DomainId, IommuError, Iova, StreamId};

    pub fn create(room: u64) -> Result<(DomainId, Iova), IommuError> {
        match backend() {
            Backend::Vtd => vtd::domain::create(room),
            Backend::AmdVi => amdvi::domain::create(room),
            Backend::None => Err(IommuError::NoUnit),
        }
    }

    /// Whether firmware reserves memory `stream` must keep reaching, which a
    /// domain of its own would not map: an AMD-Vi IVMD names it.
    pub fn firmware_reserves(stream: StreamId) -> bool {
        backend() == Backend::AmdVi && amdvi::domain::keeps_identity(stream)
    }

    /// The backend that minted a `DomainId`; there is one, or none was minted.
    fn minted() -> Backend {
        let found = backend();
        assert!(found != Backend::None, "iommu: a domain used on a machine where no unit translates");
        found
    }

    pub fn map(id: DomainId, phys: u64, bytes: u64) -> Result<Iova, IommuError> {
        match minted() {
            Backend::AmdVi => amdvi::domain::map(id, phys, bytes),
            _ => vtd::domain::map(id, phys, bytes),
        }
    }

    pub fn map_at(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
        match minted() {
            Backend::AmdVi => amdvi::domain::map_at(id, at, phys, bytes),
            _ => vtd::domain::map_at(id, at, phys, bytes),
        }
    }

    pub fn place(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
        match minted() {
            Backend::AmdVi => amdvi::domain::place(id, at, phys, bytes),
            _ => vtd::domain::place(id, at, phys, bytes),
        }
    }

    pub fn unmap(id: DomainId, at: Iova, bytes: u64) -> Result<(), IommuError> {
        match minted() {
            Backend::AmdVi => amdvi::domain::unmap(id, at, bytes),
            _ => vtd::domain::unmap(id, at, bytes),
        }
    }

    pub fn attach(stream: StreamId, id: DomainId) {
        match minted() {
            Backend::AmdVi => amdvi::domain::attach(stream, id),
            _ => vtd::domain::attach(stream, id),
        }
    }
}

/// The fault interrupt, on whichever backend armed it.
pub mod fault {
    use super::{backend, Backend};
    use crate::arch::x86_64::{amdvi, vtd};

    pub fn service() {
        match backend() {
            Backend::Vtd => vtd::fault::service(),
            Backend::AmdVi => amdvi::fault::service(),
            Backend::None => panic!("iommu: a fault interrupt on a machine where no unit is armed"),
        }
    }
}
