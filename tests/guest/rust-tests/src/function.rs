//! The PCI function a job claims, found in the inventory by what identifies
//! its card: a claim names a function by its arrival.

use toyos::endow::FromHandle;
use toyos::syscap::SysCap;
use toyos_abi::inventory::{Pci, RawRecord, Record};
use toyos_abi::syscall::{PciId, SyscallError};

/// The one function of card `id` this machine has, whoever drives it, or
/// `None`. Two of one card is a machine the job was not written for.
pub fn of(cap: &SysCap, id: PciId) -> Option<Pci> {
    let records: Vec<Record> =
        cap.records(|n| vec![RawRecord::EMPTY; n]).unwrap_or_else(|why| panic!("the job's capability reads no inventory: {why:?}"));
    let mut found = records.into_iter().filter_map(|record| match record {
        Record::Pci(pci) if pci.vendor == id.vendor && pci.device == id.device => Some(pci),
        _ => None,
    });
    let one = found.next()?;
    assert!(found.next().is_none(), "this machine has two functions of {:04x}:{:04x}", id.vendor, id.device);
    Some(one)
}

/// The claim on the one function of card `id`: `NotFound` where the machine
/// has none.
pub fn claim<T: FromHandle>(cap: &SysCap, id: PciId) -> Result<T, SyscallError> {
    of(cap, id).map_or(Err(SyscallError::NotFound), |pci| cap.claim_function(pci.arrival))
}
