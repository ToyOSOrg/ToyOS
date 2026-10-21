//! The spare controller a job claims: the one function of any of a set of
//! cards no driver holds. Named by its arrival, because a machine may have
//! two of one card.

use toyos::syscap::SysCap;
use toyos_abi::inventory::{Driven, Pci, RawRecord, Record};
use toyos_abi::syscall::PciId;

/// That function, or `None` where it is not one.
pub fn of(cap: &SysCap, ids: &[PciId]) -> Option<Pci> {
    one(cap, ids, Driven::Free)
}

/// The one function of `ids` that `driven` says of.
pub fn one(cap: &SysCap, ids: &[PciId], driven: Driven) -> Option<Pci> {
    let records: Vec<Record> =
        cap.records(|n| vec![RawRecord::EMPTY; n]).unwrap_or_else(|why| panic!("the job's capability reads no inventory: {why:?}"));
    let spares: Vec<Pci> = records
        .into_iter()
        .filter_map(|record| match record {
            Record::Pci(pci) if ids.contains(&PciId { vendor: pci.vendor, device: pci.device }) => Some(pci),
            _ => None,
        })
        .collect();
    match spares.iter().copied().filter(|pci| pci.driven == driven).collect::<Vec<_>>()[..] {
        [one] => Some(one),
        _ => None,
    }
}
