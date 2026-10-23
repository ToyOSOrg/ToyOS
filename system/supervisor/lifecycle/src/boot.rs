//! What the supervisor reads of where firmware loaded this boot from
//! ([`toyos_abi::boot::BootSources`], the kernel's copy of the loader's): a
//! line for each source, and the controller a `@boot` entry names.
//!
//! **`@boot` is the controller of the loader's own source**, the device
//! firmware booted, and only where the loader placed one on a path that
//! parses; otherwise it names nothing and the line says why. The slot volume's
//! source is said beside it, so a boot whose payload came off another device
//! than its loader shows both. Neither is authority over the device it names.

use toyos_abi::boot::{BootSource, BootSources};
use toyos_abi::inventory::PciAddr;
use toyos_devicepath::Path;
use toyos_manifest::device::BOOT_SAID;

/// The lines to say, and the controller `@boot` names.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub lines: [String; 3],
    pub controller: Option<PciAddr>,
}

pub fn read(sources: &BootSources) -> Read {
    let (loader, controller) = source("loader", &sources.loader);
    let (slot, _) = source("slot volume", &sources.slot);
    let boot = match controller {
        Some(at) => format!("{BOOT_SAID}{} (segment {})", pci_at(at), at.segment),
        None => "supervisor: `@boot` names no function: the loader placed its own source on none".to_string(),
    };
    Read { lines: [loader, slot, boot], controller }
}

fn source(name: &str, source: &BootSource) -> (String, Option<PciAddr>) {
    let refused = |why: &dyn std::fmt::Display| (format!("supervisor: the {name}'s source is refused: {why}"), None);
    let bytes = match source.path() {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (format!("supervisor: the {name}'s source names no device path"), None),
        Err(why) => return refused(&why),
    };
    let path = match Path::parse(bytes) {
        Ok(path) => path,
        Err(why) => return refused(&why),
    };
    match source.controller.at() {
        Ok(Some(at)) => (format!("supervisor: the {name} came off {path}, on PCI {}", pci_at(at)), Some(at)),
        Ok(None) => (format!("supervisor: the {name} came off {path}, on no PCI function the loader placed"), None),
        Err(why) => refused(&why),
    }
}

fn pci_at(at: PciAddr) -> String {
    format!("{:02x}:{:02x}.{}", at.bus, at.dev, at.func)
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::boot::Controller;

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

    fn on_stick(controller: Controller) -> BootSource {
        let mut source = BootSource { path_len: STICK.len() as u16, controller, ..BootSource::NONE };
        source.path[..STICK.len()].copy_from_slice(&STICK);
        source
    }

    fn placed() -> Controller {
        Controller { present: 1, bus: 3, dev: 0, func: 4, ..Controller::NONE }
    }

    #[test]
    fn the_loaders_controller_is_the_one_boot_names() {
        let read = read(&BootSources { loader: on_stick(placed()), slot: on_stick(placed()) });
        assert_eq!(read.controller, Some(AT));
        assert_eq!(read.lines, [
            format!("supervisor: the loader came off {SAID}, on PCI 03:00.4"),
            format!("supervisor: the slot volume came off {SAID}, on PCI 03:00.4"),
            "supervisor: `@boot` is PCI 03:00.4 (segment 0)".to_string(),
        ]);
    }

    /// The slot volume's controller is said, never taken for `@boot`.
    #[test]
    fn a_payload_off_another_device_is_said_and_names_nothing() {
        let read = read(&BootSources { loader: on_stick(Controller::NONE), slot: on_stick(placed()) });
        assert_eq!(read.controller, None);
        assert!(read.lines[0].ends_with("on no PCI function the loader placed"), "{}", read.lines[0]);
        assert!(read.lines[1].ends_with("on PCI 03:00.4"), "{}", read.lines[1]);
        assert_eq!(read.lines[2], "supervisor: `@boot` names no function: the loader placed its own source on none");
    }

    /// Every way a source the kernel answers can be wrong names nothing and
    /// says why: a length past the array, a path that does not parse, a
    /// controller neither present nor absent, and no path at all.
    #[test]
    fn a_hostile_source_is_refused_and_names_no_controller() {
        let mut unterminated = on_stick(placed());
        unterminated.path[STICK.len() - 4..STICK.len()].fill(0);
        unterminated.path_len = STICK.len() as u16 - 2;
        let mut zero_node = on_stick(placed());
        zero_node.path[2] = 0;
        let cases: [(BootSource, &str); 5] = [
            (BootSource { path_len: 513, ..on_stick(placed()) }, "supervisor: the loader's source is refused: its path is 513 bytes, past 512"),
            (unterminated, "supervisor: the loader's source is refused: the path ends at byte 72 with no End node"),
            (zero_node, "supervisor: the loader's source is refused: the node at byte 0 claims 0 bytes, short of its header"),
            (on_stick(Controller { present: 2, ..placed() }), "supervisor: the loader's source is refused: its controller's present is 2"),
            (BootSource { controller: placed(), ..BootSource::NONE }, "supervisor: the loader's source names no device path"),
        ];
        for (loader, said) in cases {
            let read = read(&BootSources { loader, slot: BootSource::NONE });
            assert_eq!((read.controller, read.lines[0].as_str()), (None, said));
            assert_eq!(read.lines[1], "supervisor: the slot volume's source names no device path");
        }
    }
}
