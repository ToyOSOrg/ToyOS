//! The PCI function firmware loaded this image through, read before boot
//! services end: the controller of the disk the boot partition is on, which
//! the supervisor's `@boot` rows drive.
//!
//! **The function is the one whose PCI I/O handle's device path is the longest
//! proper prefix of the boot device's**: a device path runs from the root
//! bridge through each PCI hop to the device below it (UEFI 2.10 §10.3), so
//! the last PCI handle on the way down is the controller the disk hangs off,
//! and `GetLocation()` (§14.4.13) names it as the kernel enumerates it.

use toyos_abi::boot::BootFunction;

use crate::efi::{DevicePath, Handle, LoadedImage, PciIo, SystemTable};

/// That function, or the zeroed [`BootFunction`] where there is none to name.
pub fn of(system_table: &SystemTable) -> BootFunction {
    let none = BootFunction::default();
    let bs = system_table.boot_services();
    let image = match bs.exclusive::<LoadedImage>(bs.image_handle()) {
        Ok(image) => image,
        Err(e) => {
            println!("Boot function: this image's LoadedImage would not open ({e}), so none is named");
            return none;
        }
    };
    let Some(device) = image.device() else {
        println!("Boot function: firmware names no device this image was loaded from, so none is named");
        return none;
    };
    let path = match bs.get::<DevicePath>(device) {
        Ok(path) => path,
        Err(e) => {
            println!("Boot function: the boot device has no device path ({e}), so none is named");
            return none;
        }
    };
    let boot: alloc::vec::Vec<&[u8]> = path.nodes().collect();
    let Ok(handles) = bs.handles::<PciIo>() else {
        println!("Boot function: no handle carries PCI I/O, so none is named");
        return none;
    };
    let mut best: Option<(usize, Handle)> = None;
    for &candidate in handles.iter().filter(|&&h| h != device) {
        let Ok(path) = bs.get::<DevicePath>(candidate) else { continue };
        let nodes: alloc::vec::Vec<&[u8]> = path.nodes().collect();
        if nodes.len() < boot.len() && boot.starts_with(&nodes) && best.is_none_or(|(len, _)| nodes.len() > len) {
            best = Some((nodes.len(), candidate));
        }
    }
    let Some((_, function)) = best else {
        println!("Boot function: the boot device is on no PCI function firmware names");
        return none;
    };
    let location = bs.get::<PciIo>(function).and_then(|io| io.location());
    match location {
        Ok([segment, bus, dev, func]) => match (u16::try_from(segment), u8::try_from(bus), u8::try_from(dev), u8::try_from(func)) {
            (Ok(segment), Ok(bus), Ok(dev), Ok(func)) if dev < 32 && func < 8 => {
                println!("Boot function: PCI {segment:04x}:{bus:02x}:{dev:02x}.{func}");
                BootFunction { present: 1, bus, dev, func, segment, reserved: [0; 2] }
            }
            _ => {
                println!("Boot function: firmware placed it at {segment:#x}:{bus:#x}:{dev:#x}.{func:#x}, which is no PCI address");
                none
            }
        },
        Err(e) => {
            println!("Boot function: GetLocation failed ({e}), so none is named");
            none
        }
    }
}
