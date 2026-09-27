//! Point the firmware's next boot back at this loader.
//!
//! **The chain only closes if the machine comes back here.** A panicked kernel
//! resets through the FADT register, the firmware consumes whatever `BootNext`
//! it was given. So every boot that hands the machine to a kernel first names
//! *this* loader as the next boot, and the pass after the reset is the one that
//! reads the page and decides whether to go on.
//!
//! The entry is found by the GPT partition GUID of the volume this image was
//! loaded from, which is the same identity `efibootmgr --disk … --part 1` writes
//! and the same one the metal driver flashes against — never by description, and
//! never by taking whatever `BootCurrent` happens to say, because a firmware
//! that booted us from a removable-media fallback path has no entry of ours at
//! all and must be told so rather than have one guessed at.

use uefi::prelude::*;
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;

use crate::bootvars;

/// The head of every line this module writes.
const HEAD: &str = "Boot chain:";

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
    let rt = system_table.runtime_services();
    let entry = match bootvars::naming(rt, &ours) {
        Ok(Some(entry)) => entry,
        Ok(None) => {
            return println!(
                "{HEAD} no active Boot#### entry on this machine names the partition this image came \
                 off, so the boot after a reset is the firmware's own"
            )
        }
        Err(why) => return println!("{HEAD} {why}, so the boot after a reset is the firmware's own"),
    };
    match bootvars::boot_next(rt, entry) {
        Ok(()) => println!("{HEAD} BootNext={entry:04X}, so this loader gets the machine back"),
        Err(why) => println!("{HEAD} {why}, so the boot after a reset is its own"),
    }
}

/// The GPT partition GUID of the volume firmware loaded this image from.
pub(crate) fn our_partition(handle: Handle, system_table: &SystemTable<Boot>) -> Option<[u8; 16]> {
    let bs = system_table.boot_services();
    let image = bs.open_protocol_exclusive::<LoadedImage>(handle).ok()?;
    let device = image.device()?;
    let path = bs.open_protocol_exclusive::<DevicePath>(device).ok()?;
    bootvars::hard_drive(&path).ok().map(|(_, guid)| guid)
}
