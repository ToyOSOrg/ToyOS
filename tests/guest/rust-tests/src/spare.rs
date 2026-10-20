//! The spare controller a job claims: the one function of any of a set of
//! cards the kernel does not drive, or, on a kernel that drives every one,
//! the only one there is. Named by its arrival, because a machine may have
//! two of one card.

use toyos::syscap::SysCap;
use toyos_abi::inventory::{Driven, Pci, RawRecord, Record};
use toyos_abi::syscall::PciId;

/// That function, or `None` where it is not one.
pub fn of(cap: &SysCap, ids: &[PciId]) -> Option<Pci> {
    let records: Vec<Record> =
        cap.records(|n| vec![RawRecord::EMPTY; n]).unwrap_or_else(|why| panic!("the job's capability reads no inventory: {why:?}"));
    let spares: Vec<Pci> = records
        .into_iter()
        .filter_map(|record| match record {
            Record::Pci(pci) if ids.contains(&PciId { vendor: pci.vendor, device: pci.device }) => Some(pci),
            _ => None,
        })
        .collect();
    let left: Vec<Pci> = spares.iter().copied().filter(|pci| pci.driven != Driven::Kernel).collect();
    match (left.as_slice(), spares.as_slice()) {
        ([one], _) | ([], [one]) => Some(*one),
        _ => None,
    }
}
