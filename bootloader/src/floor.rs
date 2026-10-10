//! The anti-rollback floor: the highest image version a boot has proven, in a
//! firmware variable this loader alone can write — one per signing key, named
//! and judged by `toyos_update::floor`, whose header says whose floor a loader
//! keeps and which others it deletes.
//!
//! **Why a variable and not the disk.** The variable is created
//! non-volatile and boot-services-only (UEFI 2.10 §8.2: without
//! `EFI_VARIABLE_RUNTIME_ACCESS` it is neither readable nor writable once
//! `ExitBootServices` has run), so no kernel this loader hands the machine to
//! — and nothing it lets write the disk — can lower it.

use alloc::string::String;
use alloc::vec::Vec;
use toyos_update::floor::{self, Read, Scope, Stored};
use crate::efi::{CString16, Guid, SystemTable, VariableAttributes};

/// Whose images this loader's floor holds, as its build decided.
const SCOPE: Scope = Scope::from_word(env!("TOYOS_IMAGE_FLOOR"));

/// The vendor every floor is under: `33BE3D4A-30E6-49F5-8050-F169D93A20FB`,
/// minted for this and used for nothing else.
const VENDOR: Guid = Guid::new(0x33be3d4a, 0x30e6, 0x49f5, [0x80, 0x50, 0xf1, 0x69, 0xd9, 0x3a, 0x20, 0xfb]);

/// The only attributes the floor is written with.
const ATTRIBUTES: u32 = VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS;

/// The head of every line this module writes.
const HEAD: &str = "Anti-rollback floor:";

/// This loader's floor: its variable's name, and the version it holds.
pub struct Floor {
    name: CString16,
    pub value: u64,
}

/// The floor, as this loader left it — `0` where no boot has proven one —
/// and the lines to say once the log is open; or why the stored floor is
/// refused, which boots nothing. Read before the log opens, because the
/// slots' record written then needs it.
pub fn read(system_table: &SystemTable, log_guid: &[u8; 16]) -> Result<(Floor, Vec<String>), String> {
    let rt = system_table.runtime_services();
    let own = floor::name(SCOPE, &crate::slot::KEY, log_guid);
    let name = CString16::new(own.as_str()).expect("a floor's name is ASCII");
    let mut notes = Vec::new();
    match rt.variable_keys() {
        Ok(keys) => {
            for (other, _) in keys.iter().filter(|(_, vendor)| *vendor == VENDOR) {
                let text = alloc::format!("{other}");
                if floor::stale(SCOPE, &own, &text) {
                    let deleted = match rt.delete_variable(other, &VENDOR) {
                        Ok(()) => String::from("deleted"),
                        Err(e) => alloc::format!("not deleted: firmware refused ({e})"),
                    };
                    notes.push(alloc::format!("{HEAD} {text} is no floor this {} loader keeps; {deleted}", SCOPE.word()));
                }
            }
        }
        Err(e) => notes.push(alloc::format!("{HEAD} firmware would not list its variables ({e}), so no stale floor is deleted")),
    }
    let got = rt.get_variable(&name, &VENDOR);
    let stored = Stored::answered(got.as_ref().map(|(value, attributes)| (&value[..], *attributes)).map_err(|e| e.0));
    let refused = |why: floor::Refused| {
        alloc::format!(
            "{HEAD} {name}: {why}, so nothing this loader wrote is there, and it is refused rather than read as no \
             floor. Nothing boots until the variable {name} under vendor {} is deleted, from the firmware's setup or \
             a UEFI shell",
            VENDOR
        )
    };
    match floor::judge(stored) {
        Ok(Read::Floor(value)) => {
            notes.push(alloc::format!("{HEAD} {name} ({} scope) holds {value}", SCOPE.word()));
            Ok((Floor { name, value }, notes))
        }
        Ok(Read::RuntimeMade { attributes, len }) => {
            let value = floor::deleted(attributes, rt.delete_variable(&name, &VENDOR).map_err(|e| e.0))
                .map_err(refused)?;
            notes.push(alloc::format!(
                "{HEAD} {name} carries runtime access ({attributes:#x}, {len} bytes), so it was made after a \
                 handoff and no floor this loader wrote stands behind it; it is deleted, and the floor is {value}"
            ));
            Ok((Floor { name, value }, notes))
        }
        Err(why) => Err(refused(why)),
    }
}

/// Raise `floor` to `to`, and say so.
pub fn raise(system_table: &SystemTable, floor: &mut Floor, to: u64) {
    if to <= floor.value {
        println!("{HEAD} stays {}, which is at or above {to}", floor.value);
        return;
    }
    let rt = system_table.runtime_services();
    match rt.set_variable(&floor.name, &VENDOR, ATTRIBUTES, &to.to_le_bytes()) {
        Ok(()) => {
            println!("{HEAD} {to}, raised from {} by the boot that proved it", floor.value);
            floor.value = to;
        }
        Err(e) => println!("{HEAD} firmware refused {to} ({e}), so it stays {}", floor.value),
    }
}
