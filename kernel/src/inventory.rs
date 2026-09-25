//! What the machine is made of, and who holds each part of it: the records
//! `SYS_DEVICE_INVENTORY` answers.
//!
//! **Assembled from what each subsystem already keeps**, and nothing is
//! measured here: the PCI functions and their identities are `pcidev`'s from
//! enumeration, the USB devices are the ones the xHCI driver bound, the block
//! devices are the ones registered, the partitions are what each probed disk's
//! table states, and the machine is what `SYS_SYSINFO`'s header says.
//!
//! **A holder is found where its handle is**: every process's table is walked
//! for device claims, one table at a time and with the process table dropped
//! first — the order `process::stats_of` takes the same two locks in. A claim
//! whose handle is in no table at that moment — moving between two, or queued
//! on a connection — has a device record that says it is claimed and no claim
//! record naming a holder, which is what is true.

use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::inventory::{
    Block, Claim, Claimed, Holder, Machine, PartState, Partition, Record, NAME_BYTES,
};

use crate::object::KObjectRef;
use crate::process;

/// Every record, in a fixed order: the machine, PCI, USB, block devices,
/// partitions, claims.
pub fn collect() -> Vec<Record> {
    let (memory_total, memory_used) = crate::mm::pmm::stats();
    let mut out = alloc::vec![Record::Machine(Machine {
        cpus: crate::arch::smp::cpu_count(),
        memory_total,
        memory_used,
    })];
    out.extend(crate::pcidev::inventory().into_iter().map(Record::Pci));
    out.extend(crate::drivers::xhci::inventory().into_iter().map(Record::Usb));
    out.extend(
        crate::block::registered()
            .iter()
            .map(|h| Record::Block(Block { device: h.device_id(), blocks: h.block_count() })),
    );
    let claims = claims();
    for (device, part, mounted) in crate::gpt::inventory() {
        let unique = part.unique_guid.0;
        let claimed = claims.iter().any(|c| c.on == Claimed::Partition(unique));
        out.push(Record::Partition(Partition {
            device,
            type_guid: part.type_guid.0,
            unique_guid: unique,
            first_lba: part.first_lba,
            lbas: part.lba_count(),
            state: match (mounted, claimed) {
                (true, _) => PartState::Mounted,
                (false, true) => PartState::Claimed,
                (false, false) => PartState::Free,
            },
        }));
    }
    out.extend(claims.into_iter().map(Record::Claim));
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
            let on = if let Some(slot) = claim.pci_slot() {
                // A slot nobody holds any more is a claim whose release is
                // under way; it names nothing.
                let Some(at) = crate::pcidev::held_at(slot) else { continue };
                Claimed::Pci(at)
            } else if let Some(guid) = claim.partition_guid() {
                Claimed::Partition(guid)
            } else {
                Claimed::Class(claim.class() as u8)
            };
            out.push(Claim { on, holder: Holder { pid, name } });
        }
    }
    out
}
