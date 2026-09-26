//! The anti-rollback floor: the highest image version a boot has proven, in a
//! firmware variable this loader alone can write.
//!
//! **Why a variable and not the disk.** The variable is created
//! non-volatile and boot-services-only (UEFI 2.10 §8.2: without
//! `EFI_VARIABLE_RUNTIME_ACCESS` it is neither readable nor writable once
//! `ExitBootServices` has run), so no kernel this loader hands the machine to
//! — and nothing it lets write the disk — can lower it. A variable that
//! carries runtime access was not made here: something running after a
//! handoff created it, so its value is not read, and it is replaced.
//!
//! What it cannot defend is `toyos_update::policy`'s to say: anything booted
//! before this loader, and the firmware's own reset of its variables.

use alloc::string::String;
use uefi::prelude::*;
use uefi::table::runtime::{VariableAttributes, VariableVendor};
use uefi::{cstr16, guid, CStr16};

/// The variable's name and vendor: `33BE3D4A-30E6-49F5-8050-F169D93A20FB`,
/// minted for this and used for nothing else.
const NAME: &CStr16 = cstr16!("ToyOSImageFloor");
const VENDOR: VariableVendor = VariableVendor(guid!("33be3d4a-30e6-49f5-8050-f169d93a20fb"));

/// The only attributes the floor is written with.
const ATTRIBUTES: VariableAttributes =
    VariableAttributes::NON_VOLATILE.union(VariableAttributes::BOOTSERVICE_ACCESS);

/// The head of every line this module writes.
const HEAD: &str = "Anti-rollback floor:";

/// The floor, as this loader left it — `0` where no boot has proven one —
/// and a line to say once the log is open, where the variable was not what
/// this loader writes. Read before the log opens, because the slots' record
/// written then needs it.
pub fn read(system_table: &SystemTable<Boot>) -> (u64, Option<String>) {
    let rt = system_table.runtime_services();
    let mut buf = [0u8; 8];
    match rt.get_variable(NAME, &VENDOR, &mut buf) {
        Ok((value, attributes)) if attributes == ATTRIBUTES && value.len() == 8 => {
            (u64::from_le_bytes(value.try_into().expect("eight bytes")), None)
        }
        Ok((value, attributes)) => {
            let deleted = match rt.delete_variable(NAME, &VENDOR) {
                Ok(()) => String::from("deleted"),
                Err(e) => alloc::format!("not deleted either: firmware refused ({e})"),
            };
            let note = alloc::format!(
                "{HEAD} the variable carries {attributes:?} and {} bytes, so this loader did not \
                 write it; it is not read, and is {deleted}",
                value.len()
            );
            (0, Some(note))
        }
        Err(e) if e.status() == Status::NOT_FOUND => (0, None),
        // A variable that will not read is one this loader cannot vouch for,
        // and every image this pass boots is held to no floor.
        Err(e) => (0, Some(alloc::format!("{HEAD} firmware would not read it ({e}), so this boot is held to none"))),
    }
}

/// Raise the floor to `to`, and say so.
pub fn raise(system_table: &SystemTable<Boot>, from: u64, to: u64) {
    if to <= from {
        return;
    }
    let rt = system_table.runtime_services();
    match rt.set_variable(NAME, &VENDOR, ATTRIBUTES, &to.to_le_bytes()) {
        Ok(()) => println!("{HEAD} {to}, raised from {from} by the boot that proved it"),
        Err(e) => println!("{HEAD} firmware refused {to} ({e}), so it stays {from}"),
    }
}
