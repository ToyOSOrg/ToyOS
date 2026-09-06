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

use uefi::prelude::*;
use uefi::proto::device_path::media::PartitionSignature;
use uefi::proto::device_path::{DevicePath, DeviceSubType, DeviceType};
use uefi::proto::loaded_image::LoadedImage;
use uefi::table::runtime::{VariableAttributes, VariableVendor};
use uefi::CStr16;

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
pub fn point_at_us(handle: Handle, system_table: &SystemTable<Boot>) {
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
        &VariableVendor::GLOBAL_VARIABLE,
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
fn our_partition(handle: Handle, system_table: &SystemTable<Boot>) -> Option<[u8; 16]> {
    let bs = system_table.boot_services();
    let image = bs.open_protocol_exclusive::<LoadedImage>(handle).ok()?;
    let device = image.device()?;
    let path = bs.open_protocol_exclusive::<DevicePath>(device).ok()?;
    hard_drive_guid(path.node_iter())
}

/// The GPT signature of the first HARDDRIVE node in a device path, or `None`
/// where the path has none — a network boot, or a disk with no GPT.
fn hard_drive_guid<'a>(nodes: impl Iterator<Item = &'a uefi::proto::device_path::DevicePathNode>) -> Option<[u8; 16]> {
    for node in nodes {
        if node.full_type() != (DeviceType::MEDIA, DeviceSubType::MEDIA_HARD_DRIVE) {
            continue;
        }
        let hd = <&uefi::proto::device_path::media::HardDrive>::try_from(node).ok()?;
        if let PartitionSignature::Guid(guid) = hd.partition_signature() {
            return Some(guid.to_bytes());
        }
    }
    None
}

/// The number of the `Boot####` entry whose device path names `ours`.
///
/// Every entry is read rather than only those in `BootOrder`: an entry the owner
/// has moved out of the order is still ours and still the one to come back to.
fn entry_for(system_table: &SystemTable<Boot>, ours: &[u8; 16]) -> Option<u16> {
    let rt = system_table.runtime_services();
    let keys = rt.variable_keys().ok()?;
    let mut found: Option<u16> = None;
    for key in keys {
        if key.vendor != VariableVendor::GLOBAL_VARIABLE {
            continue;
        }
        let Ok(name) = key.name() else { continue };
        let Some(number) = entry_number(name) else { continue };
        let Ok((bytes, _)) = rt.get_variable_boxed(name, &key.vendor) else { continue };
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
    let mut chars = name.iter().map(|c| char::from(*c));
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
    let Some(path) = option.get(at..at.saturating_add(path_len)) else { return false };
    // SAFETY: `DevicePath::from_ffi_ptr` needs a well-formed path; the bytes
    // come out of firmware's own `Boot####` variable, whose declared length the
    // slice above is bounded by, and the iterator below reads no node header it
    // has not first bounded against that slice.
    let path = unsafe { DevicePath::from_ffi_ptr(path.as_ptr().cast()) };
    hard_drive_guid(path.node_iter()).is_some_and(|guid| guid == *ours)
}
