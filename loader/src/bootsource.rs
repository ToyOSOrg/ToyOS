//! What this boot was loaded from, read before boot services end and written
//! as the record the supervisor reads ([`toyos_bootnames::Names`]), which the
//! kernel hands on unread: the partitions by role, and each source's device
//! path with the PCI function on it.
//!
//! **A source's controller is the function whose PCI I/O handle's device path
//! is the longest proper prefix of the source's**: a path runs from the root
//! bridge through each PCI hop to the device below it (UEFI 2.10 §10.3), so the
//! last PCI handle on the way down is the one the disk hangs off, and
//! `GetLocation()` (§14.4.13) places it as the kernel enumerates it.
//!
//! Every way a source comes up short is said by name and carried as absence.

use alloc::vec::Vec;

use toyos_bootnames::{Function, Guid, Names, Source};
use toyos_devicepath::Path;

use crate::efi::{BootServices, DevicePath, Handle, LoadedImage, PciIo, Scoped, SystemTable};

/// The head of every line this module writes.
const HEAD: &str = "Boot source:";

/// The record: `root`, `slot` and `log` as the loader read them, `boot` the
/// partition firmware loaded it from, and the devices the loader and the slot
/// volume are on.
pub fn record(system_table: &SystemTable, root: Guid, slot: Guid, log: Guid, boot: Option<Guid>) -> Vec<u8> {
    let bs = system_table.boot_services();
    let loader_device = match bs.exclusive::<LoadedImage>(bs.image_handle()) {
        Ok(image) => image.device().or_else(|| {
            println!("{HEAD} firmware names no device the loader was loaded from");
            None
        }),
        Err(e) => {
            println!("{HEAD} the loader's LoadedImage would not open ({e})");
            None
        }
    };
    let slot_device = crate::loaderlog::volume_handle(bs, &slot)
        .inspect_err(|why| println!("{HEAD} the slot volume is not found again: {why}"))
        .ok();
    // Firmware's paths, open until the record is written out of them.
    let loader_path = loader_device.and_then(|device| firmware_path(bs, "loader", device));
    let slot_path = slot_device.and_then(|device| firmware_path(bs, "slot", device));
    let loader_source = loader_device.zip(loader_path.as_deref()).and_then(|(device, path)| source(bs, "loader", device, path));
    let slot_source = slot_device.zip(slot_path.as_deref()).and_then(|(device, path)| source(bs, "slot", device, path));
    Names { root, slot, log, boot, loader_source, slot_source }.encode()
}

fn firmware_path<'a>(bs: &'a BootServices, name: &str, device: Handle) -> Option<Scoped<'a, DevicePath>> {
    bs.get::<DevicePath>(device).inspect_err(|e| println!("{HEAD} the {name}'s device has no device path ({e})")).ok()
}

/// `device`'s path and controller.
fn source<'a>(bs: &BootServices, name: &str, device: Handle, firmware: &'a DevicePath) -> Option<Source<'a>> {
    let path = match firmware.path() {
        Ok(path) => path,
        Err(why) => {
            println!("{HEAD} the {name}'s device path is refused: {why}");
            return None;
        }
    };
    let controller = controller(bs, name, device, path);
    match controller {
        Some(at) => println!(
            "{HEAD} the {name} came off {path}, on PCI {:04x}:{:02x}:{:02x}.{}",
            at.segment, at.bus, at.dev, at.func
        ),
        None => println!("{HEAD} the {name} came off {path}"),
    }
    Some(Source { path, controller })
}

fn controller(bs: &BootServices, name: &str, device: Handle, path: Path<'_>) -> Option<Function> {
    let nodes: Vec<&[u8]> = path.nodes().map(|n| n.bytes()).collect();
    let Ok(handles) = bs.handles::<PciIo>() else {
        println!("{HEAD} no handle carries PCI I/O, so the {name} is on none");
        return None;
    };
    let mut best: Option<(usize, Handle)> = None;
    for &candidate in handles.iter().filter(|&&h| h != device) {
        let Ok(firmware) = bs.get::<DevicePath>(candidate) else { continue };
        let Ok(theirs) = firmware.path() else { continue };
        let theirs: Vec<&[u8]> = theirs.nodes().map(|n| n.bytes()).collect();
        if theirs.len() < nodes.len() && nodes.starts_with(&theirs) && best.is_none_or(|(len, _)| theirs.len() > len) {
            best = Some((theirs.len(), candidate));
        }
    }
    let Some((_, function)) = best else {
        println!("{HEAD} the {name}'s path runs through no PCI function firmware names");
        return None;
    };
    match bs.get::<PciIo>(function).and_then(|io| io.location()) {
        Ok([segment, bus, dev, func]) => match (u16::try_from(segment), u8::try_from(bus), u8::try_from(dev), u8::try_from(func)) {
            (Ok(segment), Ok(bus), Ok(dev), Ok(func)) if dev < 32 && func < 8 => {
                Some(Function { segment, bus, dev, func })
            }
            _ => {
                println!("{HEAD} firmware placed the {name}'s controller at {segment:#x}:{bus:#x}:{dev:#x}.{func:#x}, which is no PCI address");
                None
            }
        },
        Err(e) => {
            println!("{HEAD} GetLocation failed for the {name}'s controller ({e})");
            None
        }
    }
}
