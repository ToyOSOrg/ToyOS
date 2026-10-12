//! Measurement scout, never landed: the Wi-Fi design's S1 read-out
//! (`pci-readout`). For every radio (class 02:80) and NVMe (class 01:08)
//! function: its position, every bridge between it and the root with the bus
//! numbers and windows each forwards, its BARs with the size each advertises,
//! and its MSI and MSI-X capabilities, the table's and the PBA's BIR and offset
//! included. Read before any driver binds, as `pcidev::publish` sizes every
//! BAR: nothing is mid-transfer while a probe takes memory decode off.

use alloc::format;
use alloc::string::String;

use toyos_pci::{bar, bridge, msi, msix};

use crate::drivers::pci::{NoCapability, PciDevice};

const HEADER_TYPE: u64 = 0x0E;
const PROG_IF: u64 = 0x09;
const SUBCLASS: u64 = 0x0A;
const CLASS: u64 = 0x0B;
/// Type 1 header: primary, secondary and subordinate bus number, a byte each
/// from here (PCI-to-PCI Bridge Architecture Specification §3.2.5.2 to §3.2.5.4).
const BUS_NUMBERS: u64 = 0x18;
/// MSI Message Control bit 0 (PCIe §7.7.1.2).
const MSI_ENABLE: u16 = 1 << 0;
/// MSI-X Message Control bits 15 and 14, and Table Size in 10:0 (PCIe §7.7.2.2).
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_FUNCTION_MASK: u16 = 1 << 14;
const MSIX_TABLE_SIZE: u16 = 0x7FF;
/// The PBA's BIR and offset, from the MSI-X capability header (PCIe §7.7.2.5).
const MSIX_PBA: u64 = 0x08;
const BIR: u32 = 0x7;

pub fn readout(devices: &[PciDevice]) {
    let mut read = 0usize;
    for device in devices {
        let what = match (device.read_config_u8(CLASS), device.read_config_u8(SUBCLASS)) {
            (0x02, 0x80) => "radio",
            (0x01, 0x08) => "nvme",
            _ => continue,
        };
        read += 1;
        let at = name(device);
        log!(
            "pci-readout: PCI {at} [{:04x}:{:04x}] {what} class={:02x}:{:02x}:{:02x}",
            device.vendor_id(),
            device.device_id(),
            device.read_config_u8(CLASS),
            device.read_config_u8(SUBCLASS),
            device.read_config_u8(PROG_IF),
        );
        bars(device, &at);
        messages(device, &at);
        path(device, devices, &at);
    }
    log!("pci-readout: {read} of {} functions read out", devices.len());
}

fn name(device: &PciDevice) -> String {
    format!("{:02x}:{:02x}.{}", device.bus, device.dev, device.func)
}

fn yn(on: bool) -> char {
    if on { 'y' } else { 'n' }
}

fn bars(device: &PciDevice, at: &str) {
    let slots = device.bar_slots();
    let mut index = 0u8;
    while index < slots {
        let low = device.read_config_u32(bar::BASE + index as u64 * 4);
        let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
        match device.memory_bar(index) {
            Ok(memory) => {
                let size = match device.bar_size(index) {
                    Ok(bytes) => format!("{bytes:#x}"),
                    Err(why) => format!("unread, {why}"),
                };
                log!(
                    "pci-readout: PCI {at} bar{index} {} prefetchable={} at={:#x} size={size}",
                    if wide { "mem64" } else { "mem32" },
                    yn(memory.prefetchable()),
                    memory.address(),
                );
            }
            Err(bar::Unusable::Unassigned) => {}
            Err(why) => log!("pci-readout: PCI {at} bar{index} {why}"),
        }
        // A 64-bit BAR's high half is the next register and no BAR of its own.
        index += if wide { 2 } else { 1 };
    }
}

fn messages(device: &PciDevice, at: &str) {
    match device.capability(msi::CAP_ID) {
        Ok(cap) => {
            let control = cap.read_u16(msi::MESSAGE_CONTROL);
            let shape = msi::Msi::decode(control);
            log!(
                "pci-readout: PCI {at} msi cap={:#04x} control={control:#06x} wide={} \
                 per-vector-mask={} vectors={} enabled={}",
                cap.offset(),
                yn(shape.wide()),
                yn(shape.mask().is_some()),
                shape.vectors(),
                yn(control & MSI_ENABLE != 0),
            );
        }
        Err(why) => log!("pci-readout: PCI {at} msi {}", absent(why)),
    }
    match device.capability(msix::CAP_ID) {
        Ok(cap) => {
            let control = cap.read_u16(msix::MESSAGE_CONTROL);
            let table = cap.read_u32(msix::TABLE);
            let pba = cap.read_u32(MSIX_PBA);
            log!(
                "pci-readout: PCI {at} msix cap={:#04x} control={control:#06x} entries={} \
                 table=bar{}+{:#x} pba=bar{}+{:#x} enabled={} masked={}",
                cap.offset(),
                (control & MSIX_TABLE_SIZE) + 1,
                table & BIR,
                table & !BIR,
                pba & BIR,
                pba & !BIR,
                yn(control & MSIX_ENABLE != 0),
                yn(control & MSIX_FUNCTION_MASK != 0),
            );
        }
        Err(why) => log!("pci-readout: PCI {at} msix {}", absent(why)),
    }
}

fn absent(why: NoCapability) -> &'static str {
    match why {
        NoCapability::Absent => "none",
        NoCapability::Truncated => "unread: the capability list ends at a link the spec forbids",
    }
}

fn is_bridge(device: &PciDevice) -> bool {
    device.read_config_u8(HEADER_TYPE) & 0x7F == bridge::HEADER_TYPE_BRIDGE
}

/// Each bridge from the one whose secondary bus `device` is on up to the
/// root bus, nearest first.
fn path(device: &PciDevice, devices: &[PciDevice], at: &str) {
    let mut bus = device.bus;
    // A bus is one bridge's secondary at most, so a walk that sees more hops
    // than there are buses has met firmware that numbered a loop.
    for _ in 0..=u8::MAX {
        let above = devices.iter().find(|d| {
            is_bridge(d) && d.bus != bus && d.read_config_u32(BUS_NUMBERS).to_le_bytes()[1] == bus
        });
        let Some(bridge) = above else {
            log!("pci-readout: PCI {at} path ends on root bus {bus:#04x}");
            return;
        };
        let [primary, secondary, subordinate, _] = bridge.read_config_u32(BUS_NUMBERS).to_le_bytes();
        let window = |forwarded: Option<bridge::Window>| match forwarded {
            Some(w) => format!("{:#x}..{:#x}", w.start, w.end),
            None => String::from("none"),
        };
        log!(
            "pci-readout: PCI {at} behind PCI {} [{:04x}:{:04x}] primary={primary:#04x} \
             secondary={secondary:#04x} subordinate={subordinate:#04x} mem={} prefetch={}",
            name(bridge),
            bridge.vendor_id(),
            bridge.device_id(),
            window(bridge::window(bridge.read_config_u32(bridge::MEMORY_BASE))),
            window(bridge::prefetch(
                bridge.read_config_u32(bridge::PREFETCH_BASE),
                bridge.read_config_u32(bridge::PREFETCH_BASE_UPPER),
                bridge.read_config_u32(bridge::PREFETCH_LIMIT_UPPER),
            )),
        );
        bus = bridge.bus;
    }
    log!("pci-readout: PCI {at} path climbs past 256 bridges: firmware numbered a loop");
}
