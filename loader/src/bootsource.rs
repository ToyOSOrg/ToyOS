//! Where firmware loaded this boot from, read before boot services end: each
//! source's device path, and the PCI function on it, for the kernel to hand on
//! unread ([`toyos_abi::boot::BootSources`]).
//!
//! **A source's controller is the function whose PCI I/O handle's device path
//! is the longest proper prefix of the source's**: a path runs from the root
//! bridge through each PCI hop to the device below it (UEFI 2.10 §10.3), so the
//! last PCI handle on the way down is the one the disk hangs off, and
//! `GetLocation()` (§14.4.13) places it as the kernel enumerates it.
//!
//! Every way a source comes up short is said by name and carried as absence.

use alloc::vec::Vec;

use toyos_abi::boot::{BootSource, BootSources, Controller};
use toyos_devicepath::Path;

use crate::efi::{BootServices, DevicePath, Handle, LoadedImage, PciIo, SystemTable};

/// The head of every line this module writes.
const HEAD: &str = "Boot source:";

/// The loader's source and the slot volume's, `slot_volume` by its unique GUID.
pub fn sources(system_table: &SystemTable, slot_volume: &[u8; 16]) -> BootSources {
    let bs = system_table.boot_services();
    let loader = match bs.exclusive::<LoadedImage>(bs.image_handle()) {
        Ok(image) => match image.device() {
            Some(device) => of(bs, "loader", device),
            None => {
                println!("{HEAD} firmware names no device the loader was loaded from");
                BootSource::NONE
            }
        },
        Err(e) => {
            println!("{HEAD} the loader's LoadedImage would not open ({e})");
            BootSource::NONE
        }
    };
    let slot = match crate::loaderlog::volume_handle(bs, slot_volume) {
        Ok(volume) => of(bs, "slot", volume),
        Err(why) => {
            println!("{HEAD} the slot volume is not found again: {why}");
            BootSource::NONE
        }
    };
    BootSources { loader, slot }
}

/// `device`'s path and controller.
fn of(bs: &BootServices, name: &str, device: Handle) -> BootSource {
    let mut source = BootSource::NONE;
    let firmware = match bs.get::<DevicePath>(device) {
        Ok(path) => path,
        Err(e) => {
            println!("{HEAD} the {name}'s device has no device path ({e})");
            return source;
        }
    };
    let path = match firmware.path() {
        Ok(path) => path,
        Err(why) => {
            println!("{HEAD} the {name}'s device path is refused: {why}");
            return source;
        }
    };
    let bytes = path.bytes();
    source.path[..bytes.len()].copy_from_slice(bytes);
    source.path_len = u16::try_from(bytes.len()).expect("a path within DEVICE_PATH_BYTES");
    source.controller = controller(bs, name, device, path);
    match source.controller.at() {
        Ok(Some(at)) => println!(
            "{HEAD} the {name} came off {path}, on PCI {:04x}:{:02x}:{:02x}.{}",
            at.segment, at.bus, at.dev, at.func
        ),
        _ => println!("{HEAD} the {name} came off {path}"),
    }
    source
}

fn controller(bs: &BootServices, name: &str, device: Handle, path: Path<'_>) -> Controller {
    let none = Controller::NONE;
    let nodes: Vec<&[u8]> = path.nodes().map(|n| n.bytes()).collect();
    let Ok(handles) = bs.handles::<PciIo>() else {
        println!("{HEAD} no handle carries PCI I/O, so the {name} is on none");
        return none;
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
        return none;
    };
    match bs.get::<PciIo>(function).and_then(|io| io.location()) {
        Ok([segment, bus, dev, func]) => match (u16::try_from(segment), u8::try_from(bus), u8::try_from(dev), u8::try_from(func)) {
            (Ok(segment), Ok(bus), Ok(dev), Ok(func)) if dev < 32 && func < 8 => {
                Controller { present: 1, bus, dev, func, segment, reserved: [0; 2] }
            }
            _ => {
                println!("{HEAD} firmware placed the {name}'s controller at {segment:#x}:{bus:#x}:{dev:#x}.{func:#x}, which is no PCI address");
                none
            }
        },
        Err(e) => {
            println!("{HEAD} GetLocation failed for the {name}'s controller ({e})");
            none
        }
    }
}
