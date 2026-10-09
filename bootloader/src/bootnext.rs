//! Point the firmware's next boot back at this loader.
//!
//! **The chain only closes if the machine comes back here.** A panicked kernel
//! resets through the FADT register, the firmware consumes whatever `BootNext`
//! it was given, and on the owner's laptop the next entry in the boot order is
//! Ubuntu — which reuses the black-box page long before anything reads it. So
//! every boot that hands the machine to a kernel first names *this* loader as
//! the next boot, and the pass after the reset is the one that reads the page
//! and decides whether to go on.
//!
//! The entry is found by the GPT partition GUID of the volume this image was
//! loaded from, which is the same identity `efibootmgr --disk … --part 1` writes
//! and the same one the metal driver flashes against — never by description, and
//! never by taking whatever `BootCurrent` happens to say, because a firmware
//! that booted us from a removable-media fallback path has no entry of ours at
//! all and must be told so rather than have one guessed at.

use crate::efi::{cstr16, CStr16, DevicePath, Handle, HardDrive, LoadedImage, SystemTable, VariableAttributes, GLOBAL_VARIABLE};

/// The head of every line this module writes.
const HEAD: &str = "Boot chain:";

/// What a `Boot####` variable's name is after the four hex digits are taken off.
const ENTRY_PREFIX: &str = "Boot";
const ENTRY_DIGITS: usize = 4;

/// `EFI_LOAD_OPTION`'s fixed head: a `UINT32` of attributes and a `UINT16`
/// device-path length, then a null-terminated `CHAR16` description, then the
/// device path itself (UEFI 2.10 §3.1.3).
const LOAD_OPTION_HEAD: usize = 6;

/// Set `BootNext` to this image's own entry, or say by name why it could not be.
///
/// A refusal is not a failure of the boot: the kernel still runs and still seals
/// its page. What is lost is the *next* boot, so the line says exactly that
/// rather than reporting a variable write.
pub fn point_at_us(handle: Handle, system_table: &SystemTable) {
    let Some(ours) = our_partition(handle, system_table) else {
        return println!(
            "{HEAD} firmware did not load this image off a GPT partition, so there is no entry \
             of ours to come back to and the boot after a reset is the firmware's own"
        );
    };
    let Some(entry) = entry_for(system_table, &ours) else {
        return println!(
            "{HEAD} no Boot#### entry on this machine names the partition this image came off, \
             so the boot after a reset is the firmware's own"
        );
    };
    let write = system_table.runtime_services().set_variable(
        cstr16!("BootNext"),
        &GLOBAL_VARIABLE,
        // Non-volatile, because it has to survive the reset that is the whole point.
        VariableAttributes::NON_VOLATILE
            | VariableAttributes::BOOTSERVICE_ACCESS
            | VariableAttributes::RUNTIME_ACCESS,
        &entry.to_le_bytes(),
    );
    match write {
        Ok(()) => println!("{HEAD} BootNext={entry:04X}, so this loader gets the machine back"),
        Err(e) => println!(
            "{HEAD} firmware refused BootNext={entry:04X} ({e}), so the boot after a reset is \
             its own"
        ),
    }
}

/// The GPT partition GUID of the volume firmware loaded this image from.
fn our_partition(handle: Handle, system_table: &SystemTable) -> Option<[u8; 16]> {
    let bs = system_table.boot_services();
    let image = bs.exclusive::<LoadedImage>(handle).ok()?;
    let device = image.device()?;
    let path = bs.exclusive::<DevicePath>(device).ok()?;
    hard_drive_guid(path.nodes())
}

/// The GPT signature of the first HARDDRIVE node in a device path, or `None`
/// where the path has none — a network boot, or a disk with no GPT.
fn hard_drive_guid<'a>(nodes: impl Iterator<Item = &'a [u8]>) -> Option<[u8; 16]> {
    for node in nodes.filter(|node| (node[0], node[1]) == HardDrive::TYPE) {
        let hd = HardDrive::parse(node)?;
        if hd.signature_type == HardDrive::GUID_SIGNATURE {
            return Some(hd.signature);
        }
    }
    None
}

/// The number of the `Boot####` entry whose device path names `ours`.
///
/// Every entry is read rather than only those in `BootOrder`: an entry the owner
/// has moved out of the order is still ours and still the one to come back to.
fn entry_for(system_table: &SystemTable, ours: &[u8; 16]) -> Option<u16> {
    let rt = system_table.runtime_services();
    let keys = rt.variable_keys().ok()?;
    let mut found: Option<u16> = None;
    for (name, vendor) in keys {
        if vendor != GLOBAL_VARIABLE {
            continue;
        }
        let Some(number) = entry_number(&name) else { continue };
        let Ok((bytes, _)) = rt.get_variable(&name, &vendor) else { continue };
        if !load_option_names(&bytes, ours) {
            continue;
        }
        // The lowest, so a machine carrying two entries for one partition is
        // answered the same way twice rather than by whichever enumerated first.
        found = Some(found.map_or(number, |seen: u16| seen.min(number)));
    }
    found
}

/// `Boot0003` is entry 3; anything else here is some other global variable.
fn entry_number(name: &CStr16) -> Option<u16> {
    let mut chars = name.units().iter().map(|&unit| char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER));
    for want in ENTRY_PREFIX.chars() {
        if chars.next()? != want {
            return None;
        }
    }
    let mut value: u16 = 0;
    let mut digits = 0;
    for ch in chars {
        value = value.checked_mul(16)?.checked_add(ch.to_digit(16)? as u16)?;
        digits += 1;
    }
    (digits == ENTRY_DIGITS).then_some(value)
}

/// Whether an `EFI_LOAD_OPTION`'s device path carries `ours`.
///
/// **Walked as bytes, bounded by the slice, and never handed to a pointer
/// iterator.** These bytes are whatever a vendor's NVRAM holds: a node claiming
/// a length of zero is an endless walk and one claiming a length past the
/// variable is a read off the end of it, so both are refused here rather than
/// trusted to a walker that follows the lengths it is given.
fn load_option_names(option: &[u8], ours: &[u8; 16]) -> bool {
    let Some(head) = option.get(..LOAD_OPTION_HEAD) else { return false };
    let path_len = u16::from_le_bytes([head[4], head[5]]) as usize;
    // The description is `CHAR16` and null-terminated, so the path starts after
    // the first pair of zero bytes on an even offset from the head.
    let mut at = LOAD_OPTION_HEAD;
    loop {
        let Some(pair) = option.get(at..at + 2) else { return false };
        at += 2;
        if pair == [0, 0] {
            break;
        }
    }
    let Some(mut path) = option.get(at..at.saturating_add(path_len)) else { return false };
    while let Some(node) = path.get(..NODE_HEADER) {
        let len = u16::from_le_bytes([node[2], node[3]]) as usize;
        // A node shorter than its own header, or longer than what is left, ends
        // the walk: neither can be stepped over.
        let Some(this) = path.get(..len).filter(|_| len >= NODE_HEADER) else { return false };
        if (this[0], this[1]) == HardDrive::TYPE {
            if let Some(guid) = gpt_signature(this) {
                return guid == *ours;
            }
        }
        path = path.get(len..).unwrap_or(&[]);
    }
    false
}

/// A device path node's type, subtype and length (UEFI 2.10 §10.2).
const NODE_HEADER: usize = 4;

/// A HARD_DRIVE node's GUID signature, or `None` where it carries another or
/// is not the length §10.3.5.1 gives it.
fn gpt_signature(node: &[u8]) -> Option<[u8; 16]> {
    HardDrive::parse(node).filter(|hd| hd.signature_type == HardDrive::GUID_SIGNATURE).map(|hd| hd.signature)
}
