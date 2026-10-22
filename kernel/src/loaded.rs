//! The partitions the loader named: the ROOT it read, the booted slot's
//! volume, the log's, and the ESP firmware loaded it from (`Role::Boot`).
//! This kernel reads none of them; it answers their GUIDs in the inventory, so
//! the log and boot file servers find theirs and the supervisor knows the
//! running slot, whoever drives the disk.

use alloc::vec::Vec;

use toyos_abi::boot::KernelArgs;
use toyos_abi::inventory::Role;

use crate::sync::Lock;

static LOADED: Lock<Vec<(Role, [u8; 16])>> = Lock::new(Vec::new());

/// Take the partitions the loader named out of its handoff.
pub fn init(args: &KernelArgs) {
    let named = [
        (Role::Root, args.root_partition_guid),
        (Role::Slot, args.slot_volume_guid),
        (Role::Log, args.log_partition_guid),
        (Role::Boot, if args.boot_partition_present == 0 { [0; 16] } else { args.boot_partition_guid }),
    ];
    *LOADED.lock() = named.into_iter().filter(|(_, guid)| *guid != [0; 16]).collect();
}

pub fn loaded() -> Vec<(Role, [u8; 16])> {
    LOADED.lock().clone()
}
