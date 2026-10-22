//! What the machine is made of, and who holds each part of it: the records
//! `SYS_DEVICE_INVENTORY` answers.
//!
//! **Assembled from what each subsystem already keeps**: the PCI functions and
//! their identities are `pcidev`'s from enumeration, and the partitions the
//! loader named are `loaded`'s.
//!
//! **A holder is found where its handle is**: every process's table is walked
//! for device claims, one table at a time and with the process table dropped
//! first — the order `process::stats_of` takes the same two locks in.

use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::inventory::{Claim, Claimed, Holder, Loaded, Record, NAME_BYTES};

use crate::object::KObjectRef;
use crate::process;

/// Every record, in a fixed order: PCI, claims, and the partitions the loader
/// named.
pub fn collect() -> Vec<Record> {
    let mut out: Vec<Record> = crate::pcidev::inventory().into_iter().map(Record::Pci).collect();
    out.extend(claims().into_iter().map(Record::Claim));
    out.extend(crate::loaded::loaded().into_iter().map(|(role, unique_guid)| Record::Loaded(Loaded { role, unique_guid })));
    out
}

/// Every device claim in a process's handle table, and that process.
fn claims() -> Vec<Claim> {
    let processes: Vec<(u32, [u8; NAME_BYTES], Arc<_>)> = {
        let guard = process::PROCESS_TABLE.lock();
        let Some(table) = guard.as_ref() else { return Vec::new() };
        table
            .iter()
            .map(|(_, proc)| (proc.pid().raw(), *proc.name(), Arc::clone(proc.process_data())))
            .collect()
    };
    let mut out = Vec::new();
    for (pid, name, data) in processes {
        let data = data.lock();
        for (_, entry) in data.handles.iter() {
            let KObjectRef::Device(claim) = entry.object() else { continue };
            let on = if claim.class() == toyos_abi::syscall::DeviceType::PciFunction {
                // A function the last handle has let go names nothing.
                let Some(at) = claim.pci(crate::pcidev::held_at) else { continue };
                Claimed::Pci(at)
            } else {
                Claimed::Class(claim.class())
            };
            out.push(Claim { on, holder: Holder { pid, name } });
        }
    }
    out
}
