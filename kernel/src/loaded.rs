//! What the loader said this boot was loaded from. The partitions it named: the
//! ROOT it read, the booted slot's volume, the log's, and the ESP firmware
//! loaded it from (`Role::Boot`), answered by GUID in the inventory, so the log
//! and boot file servers find theirs and the supervisor knows the running slot,
//! whoever drives the disk. And the devices those came off
//! ([`toyos_abi::boot::BootSources`]), kept as handed and answered whole by
//! `SYS_BOOT_SOURCES`. This kernel reads none of them and interprets no path.

use alloc::vec::Vec;

use toyos_abi::boot::{BootSource, BootSources, KernelArgs};
use toyos_abi::inventory::Role;

use crate::sync::Lock;

static LOADED: Lock<Vec<(Role, [u8; 16])>> = Lock::new(Vec::new());

static SOURCES: Lock<BootSources> = Lock::new(BootSources::NONE);

/// Take the partitions and the sources the loader named out of its handoff.
///
/// # Panics
/// Where a source is not one the loader writes: a field out of its range, a
/// path that is no device path, bytes outside the fields, or a controller with
/// no path.
pub fn init(args: &KernelArgs) {
    let named = [
        (Role::Root, args.root_partition_guid),
        (Role::Slot, args.slot_volume_guid),
        (Role::Log, args.log_partition_guid),
        (Role::Boot, if args.boot_partition_present == 0 { [0; 16] } else { args.boot_partition_guid }),
    ];
    *LOADED.lock() = named.into_iter().filter(|(_, guid)| *guid != [0; 16]).collect();
    let sources = args.boot_sources;
    held("loader", &sources.loader);
    held("slot", &sources.slot);
    *SOURCES.lock() = sources;
}

fn held(name: &str, source: &BootSource) {
    let refused = |why: &dyn core::fmt::Display| -> ! { panic!("boot: the loader handed the {name} source, and {why}") };
    let path = source.path().unwrap_or_else(|why| refused(&why));
    let controller = source.controller.at().unwrap_or_else(|why| refused(&why));
    if let Some(Err(why)) = path.map(toyos_devicepath::Path::parse) {
        refused(&why);
    }
    let len = path.map_or(0, <[u8]>::len);
    if source.path[len..].iter().any(|&b| b != 0) || source.reserved != [0; 6] || source.controller.reserved != [0; 2] {
        refused(&"it carries bytes outside its fields");
    }
    if controller.is_some() && path.is_none() {
        refused(&"it names a controller and no path");
    }
}

pub fn loaded() -> Vec<(Role, [u8; 16])> {
    LOADED.lock().clone()
}

pub fn sources() -> BootSources {
    *SOURCES.lock()
}
