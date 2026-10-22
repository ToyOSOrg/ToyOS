//! The partitions the loader named: the ROOT it read, the booted slot's
//! volume, the log's, and the ESP firmware loaded it from (`Role::Boot`); and
//! the PCI function that ESP's disk is on.
//! This kernel reads none of them; it answers their GUIDs in the inventory, so
//! the log and boot file servers find theirs and the supervisor knows the
//! running slot, whoever drives the disk.

use alloc::vec::Vec;

use toyos_abi::boot::KernelArgs;
use toyos_abi::inventory::{PciAddr, Role};

use crate::sync::Lock;

static LOADED: Lock<Vec<(Role, [u8; 16])>> = Lock::new(Vec::new());

static BOOT_FUNCTION: Lock<Option<PciAddr>> = Lock::new(None);

/// Take the partitions the loader named out of its handoff.
pub fn init(args: &KernelArgs) {
    let named = [
        (Role::Root, args.root_partition_guid),
        (Role::Slot, args.slot_volume_guid),
        (Role::Log, args.log_partition_guid),
        (Role::Boot, if args.boot_partition_present == 0 { [0; 16] } else { args.boot_partition_guid }),
    ];
    *LOADED.lock() = named.into_iter().filter(|(_, guid)| *guid != [0; 16]).collect();
    let function = args.boot_function.at();
    if let Some(at) = function {
        log!("{}{:02x}:{:02x}.{} (segment {})", toyos_abi::boot::BOOT_FUNCTION_LINE, at.bus, at.dev, at.func, at.segment);
    }
    *BOOT_FUNCTION.lock() = function;
}

/// The PCI function firmware loaded this boot through, where the loader named one.
pub fn boot_function() -> Option<PciAddr> {
    *BOOT_FUNCTION.lock()
}

pub fn loaded() -> Vec<(Role, [u8; 16])> {
    LOADED.lock().clone()
}
