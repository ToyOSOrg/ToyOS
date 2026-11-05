//! Ask the firmware to overwrite memory before the next boot, so whoever
//! resets the machine into a medium of their own reads nothing this boot left
//! in RAM (TCG PC Client Platform Reset Attack Mitigation Specification 1.2).
//!
//! **Set on every pass and never cleared.** The firmware clears the request
//! once it has overwritten memory (§4.1.3), and a clean shutdown would be the
//! OS's to clear it, which takes a runtime `SetVariable` no ToyOS kernel makes.
//! `DisableAutoDetect` stays 0, so firmware that can tell an orderly shutdown
//! may skip that wipe (§3.3).

use crate::efi::{cstr16, CStr16, Guid, Status, SystemTable, VariableAttributes};

/// `MEMORY_ONLY_RESET_CONTROL_GUID` (§4.1.1).
const VENDOR: Guid = Guid::new(0xe20939be, 0x32d4, 0x41be, [0xa1, 0x50, 0x89, 0x7f, 0x85, 0xd4, 0x98, 0x29]);

const NAME: &CStr16 = cstr16!("MemoryOverwriteRequestControl");

/// The variable's one byte with `ClearMemory`, bit 0, set, and every other bit
/// 0: `DisableAutoDetect` and the reserved bits (§4.1.2, Table 1).
const CLEAR_MEMORY: u8 = 0x01;

/// The attributes §4.1.2 requires; firmware refuses any others.
const ATTRIBUTES: u32 =
    VariableAttributes::NON_VOLATILE | VariableAttributes::BOOTSERVICE_ACCESS | VariableAttributes::RUNTIME_ACCESS;

const HEAD: &str = "Memory overwrite:";

/// Set the request where the firmware defines it, and say what it held before,
/// which may be another system's request.
///
/// The firmware creates the variable and an OS loader never does (§4.1), so
/// firmware that has none is told nothing.
pub fn request(system_table: &SystemTable) {
    let rt = system_table.runtime_services();
    let (found, attributes) = match rt.get_variable(NAME, &VENDOR) {
        Ok(read) => read,
        Err(Status::NOT_FOUND) => {
            return println!(
                "{HEAD} firmware defines no MemoryOverwriteRequestControl, so it promises no wipe \
                 after a reset and nothing is written"
            );
        }
        Err(e) => {
            return println!(
                "{HEAD} firmware would not read MemoryOverwriteRequestControl ({e}), so nothing is \
                 written and the next boot may find this one's memory"
            );
        }
    };
    match rt.set_variable(NAME, &VENDOR, ATTRIBUTES, &[CLEAR_MEMORY]) {
        Ok(()) => println!(
            "{HEAD} MemoryOverwriteRequestControl read {found:02x?} (attributes {attributes:#x}) and \
             is now [{CLEAR_MEMORY:02x}], so firmware overwrites memory before the next boot"
        ),
        Err(e) => println!(
            "{HEAD} MemoryOverwriteRequestControl reads {found:02x?} (attributes {attributes:#x}) and \
             firmware refused [{CLEAR_MEMORY:02x}] ({e}), so the next boot may find this one's memory"
        ),
    }
}
