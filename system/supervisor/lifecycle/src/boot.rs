//! What the supervisor reads of where this boot came from
//! ([`toyos_bootnames::Names`], the loader's record as the kernel handed it
//! on): a line for the partitions, one for each source, and the controller a
//! `@boot` entry names.
//!
//! **`@boot` is the controller of the loader's own source**, the device
//! firmware booted, and only where the loader placed one; otherwise it names
//! nothing and the line says why. The slot volume's source is said beside it,
//! so a boot whose payload came off another device than its loader shows both.
//! Neither is authority over the device it names.

use toyos_abi::inventory::PciAddr;
use toyos_bootnames::{Names, Source};
use toyos_gpt::Guid;
use toyos_manifest::device::BOOT_SAID;

/// The lines to say, and the controller `@boot` names.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub lines: [String; 4],
    pub controller: Option<PciAddr>,
}

pub fn read(names: &Names<'_>) -> Read {
    let partitions = format!(
        "supervisor: the loader named ROOT {}, the slot volume {}, the log {} and {}",
        Guid(names.root),
        Guid(names.slot),
        Guid(names.log),
        names.boot.map_or("no boot partition".to_string(), |boot| format!("the boot partition {}", Guid(boot)))
    );
    let (loader, controller) = source("loader", names.loader_source);
    let (slot, _) = source("slot volume", names.slot_source);
    let boot = match controller {
        Some(at) => format!("{BOOT_SAID}{} (segment {})", pci_at(at), at.segment),
        None => "supervisor: `@boot` names no function: the loader placed its own source on none".to_string(),
    };
    Read { lines: [partitions, loader, slot, boot], controller }
}

fn source(name: &str, source: Option<Source<'_>>) -> (String, Option<PciAddr>) {
    let Some(Source { path, controller }) = source else {
        return (format!("supervisor: the {name}'s source names no device path"), None);
    };
    match controller.map(|c| PciAddr { segment: c.segment, bus: c.bus, dev: c.dev, func: c.func }) {
        Some(at) => (format!("supervisor: the {name} came off {path}, on PCI {}", pci_at(at)), Some(at)),
        None => (format!("supervisor: the {name} came off {path}, on no PCI function the loader placed"), None),
    }
}

fn pci_at(at: PciAddr) -> String {
    format!("{:02x}:{:02x}.{}", at.bus, at.dev, at.func)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_bootnames::Function;
    use toyos_devicepath::Path;

    /// The AMD bench's ESP on a stick in its USB-C port, 03:00.4: the nodes as
    /// that firmware named them on a bench run, then a HARDDRIVE node whose
    /// GUID, start and size are made up.
    const STICK: [u8; 76] = [
        2, 1, 12, 0, 0xd0, 0x41, 0x03, 0x0a, 0, 0, 0, 0, // PciRoot(0x0)
        1, 1, 6, 0, 1, 8, // Pci(0x8,0x1)
        1, 1, 6, 0, 4, 0, // Pci(0x0,0x4)
        3, 5, 6, 0, 4, 0, // USB(0x4,0x0)
        4, 1, 42, 0, 1, 0, 0, 0, // HD(1,
        0, 8, 0, 0, 0, 0, 0, 0, // 0x800,
        0, 0x10, 0, 0, 0, 0, 0, 0, // 0x1000)
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 2, 2, // GPT, its GUID
        0x7f, 0xff, 4, 0, // End
    ];
    const SAID: &str = "PciRoot(0x0)/Pci(0x8,0x1)/Pci(0x0,0x4)/USB(0x4,0x0)/HD(1,GPT,04030201-0605-0807-090A-0B0C0D0E0F10,0x800,0x1000)";
    const AT: PciAddr = PciAddr { segment: 0, bus: 3, dev: 0, func: 4 };

    fn on_stick(placed: bool) -> Option<Source<'static>> {
        let controller = placed.then_some(Function { segment: 0, bus: 3, dev: 0, func: 4 });
        Some(Source { path: Path::parse(&STICK).unwrap(), controller })
    }

    fn named(loader_source: Option<Source<'static>>, slot_source: Option<Source<'static>>) -> Names<'static> {
        Names { root: [0x10; 16], slot: [0x20; 16], log: [0x30; 16], boot: Some([0x40; 16]), loader_source, slot_source }
    }

    #[test]
    fn the_loaders_controller_is_the_one_boot_names() {
        let read = read(&named(on_stick(true), on_stick(true)));
        assert_eq!(read.controller, Some(AT));
        assert_eq!(read.lines, [
            "supervisor: the loader named ROOT 10101010-1010-1010-1010-101010101010, the slot volume \
             20202020-2020-2020-2020-202020202020, the log 30303030-3030-3030-3030-303030303030 and the boot partition \
             40404040-4040-4040-4040-404040404040"
                .to_string(),
            format!("supervisor: the loader came off {SAID}, on PCI 03:00.4"),
            format!("supervisor: the slot volume came off {SAID}, on PCI 03:00.4"),
            "supervisor: `@boot` is PCI 03:00.4 (segment 0)".to_string(),
        ]);
    }

    /// The slot volume's controller is said, never taken for `@boot`.
    #[test]
    fn a_payload_off_another_device_is_said_and_names_nothing() {
        let read = read(&named(on_stick(false), on_stick(true)));
        assert_eq!(read.controller, None);
        assert!(read.lines[1].ends_with("on no PCI function the loader placed"), "{}", read.lines[1]);
        assert!(read.lines[2].ends_with("on PCI 03:00.4"), "{}", read.lines[2]);
        assert_eq!(read.lines[3], "supervisor: `@boot` names no function: the loader placed its own source on none");
    }

    #[test]
    fn a_boot_firmware_named_nothing_of_says_so() {
        let read = read(&Names { boot: None, ..named(None, None) });
        assert_eq!(read.controller, None);
        assert!(read.lines[0].ends_with("and no boot partition"), "{}", read.lines[0]);
        assert_eq!(read.lines[1], "supervisor: the loader's source names no device path");
        assert_eq!(read.lines[2], "supervisor: the slot volume's source names no device path");
    }
}
