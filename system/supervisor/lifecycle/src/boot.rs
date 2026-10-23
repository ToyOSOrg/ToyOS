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

    /// A stick on the AMD bench's USB-C controller, 03:00.4, as its ESP's path.
    const STICK: [u8; 32] = [
        2, 1, 12, 0, 0xd0, 0x41, 0x03, 0x0a, 0, 0, 0, 0, // PciRoot(0x0)
        1, 1, 6, 0, 1, 8, // Pci(0x8,0x1)
        1, 1, 6, 0, 4, 0, // Pci(0x0,0x4)
        3, 5, 6, 0, 1, 0, // USB(0x1,0x0)
        0x7f, 0xff, // End, its length below
    ];
    const AT: PciAddr = PciAddr { segment: 0, bus: 3, dev: 0, func: 4 };

    fn on_stick(controller: Controller) -> BootSource {
        let mut source = BootSource { path_len: 34, controller, ..BootSource::NONE };
        source.path[..32].copy_from_slice(&STICK);
        source.path[32..34].copy_from_slice(&[4, 0]);
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
            "supervisor: the loader came off PciRoot(0x0)/Pci(0x8,0x1)/Pci(0x0,0x4)/USB(0x1,0x0), on PCI 03:00.4",
            "supervisor: the slot volume came off PciRoot(0x0)/Pci(0x8,0x1)/Pci(0x0,0x4)/USB(0x1,0x0), on PCI 03:00.4",
            "supervisor: `@boot` is PCI 03:00.4 (segment 0)",
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
        unterminated.path_len = 32;
        let mut zero_node = on_stick(placed());
        zero_node.path[2] = 0;
        let cases: [(BootSource, &str); 5] = [
            (BootSource { path_len: 513, ..on_stick(placed()) }, "supervisor: the loader's source is refused: its path is 513 bytes, past 512"),
            (unterminated, "supervisor: the loader's source is refused: the path ends at byte 30 with no End node"),
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
