//! The spare controller a job claims: the first function, in the inventory's
//! order, of any of a set of cards no driver holds. A bench booted off its
//! NVMe leaves both its xHCI controllers free, and the first is the one every
//! boot of it takes. Named by its arrival, because a machine may have two of
//! one card.

use toyos::syscap::SysCap;
use toyos_abi::inventory::{Driven, Pci, RawRecord, Record};
use toyos_abi::syscall::PciId;

/// That function, or `None` where every one of `ids` is held.
pub fn of(cap: &SysCap, ids: &[PciId]) -> Option<Pci> {
    all(cap, ids).into_iter().find(|pci| pci.driven == Driven::Free)
}

/// Every function of `ids`, in the inventory's order.
pub fn all(cap: &SysCap, ids: &[PciId]) -> Vec<Pci> {
    let records: Vec<Record> =
        cap.records(|n| vec![RawRecord::EMPTY; n]).unwrap_or_else(|why| panic!("the job's capability reads no inventory: {why:?}"));
    records
        .into_iter()
        .filter_map(|record| match record {
            Record::Pci(pci) if ids.contains(&PciId { vendor: pci.vendor, device: pci.device }) => Some(pci),
            _ => None,
        })
        .collect()
}
