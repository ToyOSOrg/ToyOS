//! What the machine is made of, and who holds each part of it: the records
//! `SYS_DEVICE_INVENTORY` answers.
//!
//! **Assembled from what each subsystem already keeps**: the PCI functions and
//! their identities are `pcidev`'s from enumeration, the USB devices are the
//! ones the xHCI driver bound, the block devices are the ones registered, and
//! the partitions are what each disk's table stated when `gpt::probe` listed
//! it. A partition's state is the block layer's hold on exactly its span, the
//! record every view is refused against.
//!
//! **A holder is found where its handle is**: every process's table is walked
//! for device claims, one table at a time and with the process table dropped
//! first — the order `process::stats_of` takes the same two locks in.

use alloc::sync::Arc;
use alloc::vec::Vec;

use toyos_abi::inventory::{Block, Claim, Claimed, Holder, PartState, Partition, Record, NAME_BYTES};

use crate::object::KObjectRef;
use crate::process;

/// Every record, in a fixed order: PCI, USB, block devices, partitions,
/// claims.
pub fn collect() -> Vec<Record> {
    let mut out: Vec<Record> = crate::pcidev::inventory().into_iter().map(Record::Pci).collect();
    out.extend(crate::drivers::xhci::inventory().into_iter().map(Record::Usb));
    out.extend(
        crate::block::registered()
            .iter()
            .map(|h| Record::Block(Block { device: h.device_id(), blocks: h.block_count() })),
    );
    for (device, part, holder) in crate::gpt::inventory() {
        out.push(Record::Partition(Partition {
            device,
            index: part.index,
            type_guid: part.type_guid.0,
            unique_guid: part.unique_guid.0,
            first_lba: part.first_lba,
            lbas: part.lba_count(),
            state: match holder {
                None => PartState::Free,
                Some(crate::block::Holder::Kernel(_)) => PartState::Kernel,
                Some(crate::block::Holder::Claim) => PartState::Claimed,
            },
        }));
    }
    out.extend(claims().into_iter().map(Record::Claim));
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
            } else if claim.class() == toyos_abi::syscall::DeviceType::Partition {
                // A partition the last handle has let go names nothing.
                let Some((device, unique_guid)) = claim.partition_on() else { continue };
                Claimed::Partition { device, unique_guid }
            } else {
                Claimed::Class(claim.class())
            };
            out.push(Claim { on, holder: Holder { pid, name } });
        }
    }
    out
}
