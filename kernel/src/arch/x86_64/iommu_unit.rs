//! The x86-64 IOMMU: VT-d where firmware publishes a DMAR, AMD-Vi where it
//! publishes an IVRS, chosen once at [`init`] and asked through [`domain`],
//! [`fault`] and [`interrupt`].

use core::sync::atomic::{AtomicU8, Ordering};

use crate::drivers::pci::PciDevice;
use crate::log;
use crate::mm::policy::MmioPolicy;
use crate::mm::Mmio;

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
pub fn init(rsdp_addr: u64, devices: &[PciDevice], kernel: (u64, u64)) {
    let found = if super::vtd::init(rsdp_addr, devices, kernel) {
        Backend::Vtd
    } else if super::amdvi::init(rsdp_addr, devices) {
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

/// Every function the kernel drives is out of firmware's hands: AMD-Vi turns
/// its SMI filters on.
pub fn drivers_settled() {
    if backend() == Backend::AmdVi {
        super::amdvi::settle();
    }
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

/// The interrupt address range, which no domain's addresses may enter: a
/// request there is a message, not a DMA (VT-d Rev. 5.0 §3.15; AMD 48882 Rev.
/// 3.09 §2.2.9 for requests without a PASID).
pub const INTERRUPT_RANGE: (u64, u64) = (0xFEE0_0000, 0xFEF0_0000);

/// A device's address spaces, on whichever backend translates.
pub mod domain {
    use super::{backend, Backend};
    use crate::arch::x86_64::{amdvi, vtd};
    use crate::iommu::{DomainId, IommuError, Iova, Limit, StreamId};

    pub fn create(room: u64, windows: Option<&[toyos_abi::acpi::RootBridgeWindow]>) -> Result<(DomainId, Iova), IommuError> {
        match backend() {
            Backend::Vtd => vtd::domain::create(room, windows),
            Backend::AmdVi => amdvi::domain::create(room, windows),
            Backend::None => Err(IommuError::NoUnit),
        }
    }

    /// Map what firmware reserved for `stream` at its own addresses in `id`,
    /// before `stream` attaches to it. AMD-Vi refuses such a function a claim
    /// ([`unclaimable`]), so it has nothing to map.
    pub fn keep_reserved(id: DomainId, stream: StreamId) {
        if backend() == Backend::Vtd {
            vtd::domain::keep_reserved(id, stream);
        }
    }

    /// Why `stream` keeps the machine's own addresses rather than a domain of
    /// its own, if it does: AMD-Vi's reasons. VT-d answers `None` for every
    /// function: one an RMRR keeps moves off the identity domain with the
    /// range mapped in its own.
    pub fn untranslated(stream: StreamId) -> Option<&'static str> {
        match backend() {
            Backend::AmdVi => amdvi::domain::untranslated(stream),
            Backend::Vtd | Backend::None => None,
        }
    }

    /// Whether a function with no domain of its own reaches nothing: never,
    /// since both backends leave it on the identity domain.
    pub fn aborts_undomained(_stream: StreamId) -> bool {
        false
    }

    /// Why no claim may have `stream`, if none may: [`untranslated`]'s
    /// reasons and the requester ids AMD-Vi's platform speaks as; on VT-d, an
    /// RMRR that names it and does not keep it (`vtd::classify`).
    pub fn unclaimable(stream: StreamId) -> Option<&'static str> {
        match backend() {
            Backend::AmdVi => amdvi::domain::unclaimable(stream),
            Backend::Vtd => vtd::domain::unclaimable(stream),
            Backend::None => None,
        }
    }

    /// The backend that minted a `DomainId`; there is one, or none was minted.
    fn minted() -> Backend {
        let found = backend();
        assert!(found != Backend::None, "iommu: a domain used on a machine where no unit translates");
        found
    }

    pub fn map(id: DomainId, phys: u64, bytes: u64, limit: Limit) -> Result<Iova, IommuError> {
        match minted() {
            Backend::AmdVi => amdvi::domain::map(id, phys, bytes, limit),
            _ => vtd::domain::map(id, phys, bytes, limit),
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

/// Interrupt remapping, on whichever backend remaps: what a message is armed
/// with, and the claim's vector it delivers.
pub mod interrupt {
    use super::{backend, Backend};
    use crate::arch::x86_64::idt::{user_dev, ClaimVector};
    use crate::arch::x86_64::{amdvi, vtd};
    use crate::iommu::StreamId;

    pub struct Msi {
        pub address: u32,
        pub data: u32,
    }

    /// The words an I/O APIC redirection entry carries for a remapped pin.
    pub struct Pin {
        pub low: u32,
        pub high: u32,
    }

    /// Why a source could not be given an entry. Carried rather than
    /// collapsed: one message for all of them sends whoever reads it looking
    /// in the wrong place.
    #[derive(Clone, Copy)]
    pub enum Refused {
        /// Wider than the destination an entry holds without extended
        /// interrupt mode.
        DestinationTooWide(u32),
        /// Every entry in the table is already spoken for.
        TableFull,
        /// Every vector a claim's message may carry is held by another claim.
        NoVector,
        /// Firmware named no requester id for this interrupt controller.
        ControllerUnnamed(u8),
    }

    impl Refused {
        /// Whether this machine has run out of something a claim spends,
        /// rather than this source being one no entry can be written for.
        pub fn exhausted(self) -> bool {
            matches!(self, Self::TableFull | Self::NoVector)
        }
    }

    impl core::fmt::Display for Refused {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::DestinationTooWide(id) => {
                    write!(f, "apic id {id:#x} does not fit a remapping entry's destination")
                }
                Self::TableFull => write!(f, "the interrupt remapping table is full"),
                Self::NoVector => write!(f, "every vector a claimed function's message may carry is held"),
                Self::ControllerUnnamed(id) => {
                    write!(f, "firmware named no requester id for interrupt controller {id}")
                }
            }
        }
    }

    /// A claim's message and the vector it delivers, held until [`release`].
    pub struct Claimed {
        pub msi: Msi,
        vector: ClaimVector,
        /// VT-d's entry index; AMD-Vi's entry is the vector's own.
        entry: Option<usize>,
    }

    impl Claimed {
        /// What the message raises, for the record.
        pub fn interrupt(&self) -> impl core::fmt::Display {
            self.vector
        }
    }

    /// Whether a claimed function's every message reaches only its own
    /// interrupt: VT-d's source-id verification and its block on
    /// compatibility format where it remaps; AMD-Vi's requester table where
    /// it remaps and every unit filters SMIs, since a holder's write to the
    /// interrupt range with an SMI's type is no remapped message (48882
    /// §2.1.4.2, Table 10, Table 19).
    pub fn fences_claims() -> Result<(), crate::iommu::NotRemapped> {
        use crate::iommu::NotRemapped;
        match backend() {
            _ if !is_armed() => Err(NotRemapped::Unremapped),
            Backend::AmdVi if !amdvi::interrupt::smi_filtered_everywhere() => Err(NotRemapped::SmiUnfiltered),
            _ => Ok(()),
        }
    }

    pub fn is_armed() -> bool {
        match backend() {
            Backend::Vtd => vtd::interrupt::is_armed(),
            Backend::AmdVi => amdvi::interrupt::is_armed(),
            Backend::None => false,
        }
    }

    /// A vector for `pcidev` function `function` and an entry delivering it,
    /// written for `source` and published to every unit before this returns.
    pub fn claim(function: usize, source: StreamId) -> Result<Claimed, Refused> {
        let vector = user_dev::take(function).ok_or(Refused::NoVector)?;
        let made = match backend() {
            Backend::AmdVi => amdvi::interrupt::claim(vector.vector(), source).map(|msi| (msi, None)),
            Backend::Vtd => vtd::interrupt::claim(vector.vector(), source)
                .map(|entry| (vtd::interrupt::message(entry), Some(entry))),
            Backend::None => unreachable!("iommu: a claim's message made where nothing remaps"),
        };
        match made {
            Ok((msi, entry)) => Ok(Claimed { msi, vector, entry }),
            Err(why) => {
                user_dev::give_back(vector);
                Err(why)
            }
        }
    }

    /// A claim's entry gone from every unit, and then its vector free.
    pub fn release(claimed: &Claimed, source: StreamId) {
        match claimed.entry {
            Some(entry) => vtd::interrupt::release(entry, source),
            None => amdvi::interrupt::release(claimed.vector.vector(), source),
        }
        user_dev::give_back(claimed.vector);
    }

    /// An I/O APIC pin's redirection words, its entry written first.
    pub fn pin(apic_id: u8, vector: u8, dest: u32, level: bool) -> Result<Pin, Refused> {
        match backend() {
            Backend::AmdVi => amdvi::interrupt::pin(apic_id, vector, dest),
            Backend::Vtd | Backend::None => vtd::interrupt::pin(apic_id, vector, dest, level),
        }
    }
}
