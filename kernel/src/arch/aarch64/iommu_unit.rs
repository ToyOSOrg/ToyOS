//! The IOMMU: an SMMUv3 the IORT names, the port's stage 6. Until then no
//! unit exists, and [`init`] says so; a domain is refused rather than built.

use crate::drivers::pci::PciDevice;
use crate::log;

pub fn init(_rsdp_addr: u64, _devices: &[PciDevice]) {
    log!("IOMMU: the SMMUv3 is the port's stage 6; no device is translated this boot");
}

pub mod domain {
    use crate::iommu::{DomainId, IommuError, Iova, StreamId};

    /// No unit is driven, so there is no domain to give.
    pub fn create() -> Result<DomainId, IommuError> {
        Err(IommuError::NoUnit)
    }

    pub fn map(_id: DomainId, _phys: u64, _bytes: u64) -> Result<Iova, IommuError> {
        unreachable!("no domain exists: `create` refuses every one")
    }

    pub fn map_at(_id: DomainId, _at: Iova, _phys: u64, _bytes: u64) -> Result<(), IommuError> {
        unreachable!("no domain exists: `create` refuses every one")
    }

    pub fn unmap(_id: DomainId, _at: Iova, _bytes: u64) -> Result<(), IommuError> {
        unreachable!("no domain exists: `create` refuses every one")
    }

    pub fn attach(_stream: StreamId, _id: DomainId) {
        unreachable!("no domain exists: `create` refuses every one")
    }
}

pub mod interrupt {
    use crate::iommu::{Refused, StreamId};

    pub struct Msi {
        pub address: u32,
        pub data: u32,
    }

    pub struct Pin {
        pub low: u32,
        pub high: u32,
    }

    /// No unit, so nothing remaps: callers deliver directly.
    pub fn is_armed() -> bool {
        false
    }

    pub fn msi(_source: StreamId, _vector: u8, _dest: u32) -> Result<Msi, Refused> {
        unreachable!("no interrupt remapping without an IOMMU unit, and `is_armed` said so")
    }

    pub fn pin(_apic_id: u8, _vector: u8, _dest: u32, _level: bool) -> Result<Pin, Refused> {
        unreachable!("no interrupt remapping without an IOMMU unit, and `is_armed` said so")
    }
}

pub mod fault {
    use crate::iommu::StreamId;

    pub fn user_owned(_stream: StreamId, _slot: Option<usize>) {
        owed!("SMMUv3 fault reporting", "stage 6")
    }

    pub fn service() {
        owed!("SMMUv3 fault reporting", "stage 6")
    }
}
