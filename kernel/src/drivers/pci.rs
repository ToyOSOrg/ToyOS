use alloc::vec::Vec;

use toyos_pci::{bar, caps, msi, msix};

use crate::mm::Mmio;
use crate::mm::paging::MmioPolicy;
use crate::log;

const VENDOR_ID: u64 = 0x00;
const DEVICE_ID: u64 = 0x02;
const COMMAND: u64 = 0x04;
const PROG_IF: u64 = 0x09;
const SUBCLASS: u64 = 0x0A;
const CLASS: u64 = 0x0B;
const HEADER_TYPE: u64 = 0x0E;
pub(crate) const CAPABILITIES_PTR: u64 = 0x34;

const MULTI_FUNCTION: u8 = 0x80;
const INVALID_VENDOR: u16 = 0xFFFF;

/// The one MSI-X table entry this kernel programs; a device's queues must point at it too.
pub const MSIX_ENTRY: u16 = 0;

// Every device interrupt in this kernel targets this LAPIC address, so all land on cpu0.
const MSG_ADDR: u32 = 0xFEE0_0000;

pub struct Capability<'a> {
    device: &'a PciDevice,
    offset: u64,
}

impl Capability<'_> {
    pub fn id(&self) -> u8 {
        self.device.read_config_u8(self.offset)
    }

    /// The config-space offset this capability sits at, for the cap self-test to name the link the walk yielded.
    #[cfg(feature = "boot-actuators")]
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub fn read_u8(&self, field: u64) -> u8 {
        self.device.read_config_u8(self.offset + field)
    }

    pub fn read_u16(&self, field: u64) -> u16 {
        self.device.read_config_u16(self.offset + field)
    }

    pub fn read_u32(&self, field: u64) -> u32 {
        self.device.read_config_u32(self.offset + field)
    }

    pub fn write_u16(&self, field: u64, val: u16) {
        self.device.write_config_u16(self.offset + field, val)
    }

    pub fn write_u32(&self, field: u64, val: u32) {
        self.device.write_config_u32(self.offset + field, val)
    }
}

/// PCI device identified by ECAM base + Bus/Device/Function.
#[derive(Clone, Copy)]
pub struct PciDevice {
    mmio: Mmio,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciDevice {
    fn new(ecam: &crate::mm::Mmio, bus: u8, dev: u8, func: u8) -> Self {
        let offset = ((bus as u64) << 20)
            | ((dev as u64) << 15)
            | ((func as u64) << 12);
        Self { mmio: ecam.subregion(offset, 4096), bus, dev, func }
    }

    /// A function over a caller-owned config-space window, for the cap self-test to drive the real walk over lists no hardware in reach produces.
    #[cfg(feature = "boot-actuators")]
    pub(crate) fn over_config(mmio: crate::mm::Mmio) -> Self {
        Self { mmio, bus: 0, dev: 0, func: 0 }
    }

    pub fn vendor_id(&self) -> u16 {
        self.mmio.read_u16(VENDOR_ID)
    }

    pub fn device_id(&self) -> u16 {
        self.mmio.read_u16(DEVICE_ID)
    }

    pub fn read_config_u8(&self, offset: u64) -> u8 {
        self.mmio.read_u8(offset)
    }

    pub fn read_config_u16(&self, offset: u64) -> u16 {
        self.mmio.read_u16(offset)
    }

    pub fn read_config_u32(&self, offset: u64) -> u32 {
        self.mmio.read_u32(offset)
    }

    /// The physical address Memory Space BAR `index` names, or why it names none; `index` must be ≤ [`bar::MAX_INDEX`].
    pub fn memory_bar(&self, index: u8) -> Result<bar::Memory, bar::Unusable> {
        // A bad index is a caller bug, not a device's claim, so this fails fast instead of returning Err.
        assert!(index <= bar::MAX_INDEX, "PCI: BAR {index} — a Type 0 header has six");
        let offset = bar::BASE + index as u64 * 4;
        match bar::decode(self.mmio.read_u32(offset))? {
            bar::Width::Narrow(memory) => Ok(memory),
            bar::Width::Wide(wide) => wide.with_high(self.mmio.read_u32(offset + 4)),
        }
    }

    /// The byte size Memory Space BAR `index` advertises, by the spec's
    /// write-ones probe; `index` must be ≤ [`bar::MAX_INDEX`]. Memory decode is
    /// off for the probe, so nothing can read through the BAR mid-dance.
    pub fn bar_size(&self, index: u8) -> Result<u64, bar::BadSize> {
        assert!(index <= bar::MAX_INDEX, "PCI: BAR {index} — a Type 0 header has six");
        let offset = bar::BASE + index as u64 * 4;
        let cmd = self.mmio.read_u16(COMMAND);
        self.mmio.write_u16(COMMAND, cmd & !0x2);
        let lo = self.mmio.read_u32(offset);
        self.mmio.write_u32(offset, u32::MAX);
        let mask_lo = self.mmio.read_u32(offset);
        self.mmio.write_u32(offset, lo);
        let mask_hi = bar::is_wide(lo).then(|| {
            let hi = self.mmio.read_u32(offset + 4);
            self.mmio.write_u32(offset + 4, u32::MAX);
            let mask = self.mmio.read_u32(offset + 4);
            self.mmio.write_u32(offset + 4, hi);
            mask
        });
        self.mmio.write_u16(COMMAND, cmd);
        bar::advertised_size(mask_lo, mask_hi)
    }

    pub fn write_config_u16(&self, offset: u64, val: u16) {
        self.mmio.write_u16(offset, val)
    }

    pub fn write_config_u32(&self, offset: u64, val: u32) {
        self.mmio.write_u32(offset, val)
    }

    /// Enable memory space access and bus mastering in PCI command register.
    pub fn enable_bus_master(&self) {
        let cmd = self.mmio.read_u16(COMMAND);
        self.mmio.write_u16(COMMAND, cmd | 0x06);
    }

    /// Clear bus mastering only — memory space stays, so config and BAR reads still work.
    pub fn disable_bus_master(&self) {
        let cmd = self.mmio.read_u16(COMMAND);
        self.mmio.write_u16(COMMAND, cmd & !0x04);
    }

    /// Point this function's [`MSIX_ENTRY`] at `vector` and enable it, or return false if MSI-X cannot be armed.
    pub fn enable_msix(&self, vector: u8) -> bool {
        let Some(cap) = self.capabilities().find(|c| c.id() == msix::CAP_ID) else {
            return false;
        };
        let control = cap.read_u16(msix::MESSAGE_CONTROL);
        let table = match msix::Msix::decode(control, cap.read_u32(msix::TABLE)) {
            Ok(table) => table,
            Err(why) => {
                log!("PCI {:02x}:{:02x}.{}: MSI-X not armed, {}",
                    self.bus, self.dev, self.func, why);
                return false;
            }
        };
        // Decoded, not assumed memory: a device may name a BAR that is an I/O BAR.
        let base = match self.memory_bar(table.bir()) {
            Ok(memory) => memory.address(),
            Err(why) => {
                log!("PCI {:02x}:{:02x}.{}: MSI-X not armed, its table names BAR {} and {}",
                    self.bus, self.dev, self.func, table.bir(), why);
                return false;
            }
        };
        let address = match table.table_address(base) {
            Ok(address) => address,
            Err(why) => {
                log!("PCI {:02x}:{:02x}.{}: MSI-X not armed, {}",
                    self.bus, self.dev, self.func, why);
                return false;
            }
        };

        let entry = address + MSIX_ENTRY as u64 * msix::ENTRY_BYTES;
        let table = crate::mm::paging::map_mmio(entry, 0x1000, MmioPolicy::Uncacheable);

        table.write_u32(msix::ENTRY_ADDRESS_LO, MSG_ADDR);
        table.write_u32(msix::ENTRY_ADDRESS_HI, 0);
        table.write_u32(msix::ENTRY_DATA, vector as u32);
        table.write_u32(msix::ENTRY_VECTOR_CONTROL, msix::ENTRY_UNMASKED);

        cap.write_u16(msix::MESSAGE_CONTROL, msix::Msix::enabled(control));
        true
    }

    /// Point this function's single MSI message at `vector` and enable it.
    pub fn enable_msi(&self, vector: u8) -> bool {
        let Some(cap) = self.capabilities().find(|c| c.id() == msi::CAP_ID) else {
            return false;
        };

        let control = cap.read_u16(msi::MESSAGE_CONTROL);
        let msi = msi::Msi::decode(control);
        cap.write_u32(msi.address_lo(), MSG_ADDR);
        if let Some(address_hi) = msi.address_hi() {
            cap.write_u32(address_hi, 0);
        }
        cap.write_u16(msi.data(), vector as u16);
        if let Some(mask) = msi.mask() {
            cap.write_u32(mask, 0);
        }
        cap.write_u16(msi::MESSAGE_CONTROL, msi::Msi::enabled(control));
        true
    }

    pub fn capabilities(&self) -> CapabilityIter<'_> {
        let first = self.mmio.read_u8(CAPABILITIES_PTR);
        CapabilityIter { device: self, walk: caps::CapWalk::new(), next: first }
    }

    pub fn is_id(&self, vendor: u16, device: u16) -> bool {
        self.vendor_id() == vendor && self.device_id() == device
    }

    pub fn matches_class(&self, class: u8, subclass: u8, prog_if: Option<u8>) -> bool {
        if self.mmio.read_u8(CLASS) != class { return false; }
        if self.mmio.read_u8(SUBCLASS) != subclass { return false; }
        match prog_if {
            Some(pi) => self.mmio.read_u8(PROG_IF) == pi,
            None => true,
        }
    }
}

pub struct CapabilityIter<'a> {
    device: &'a PciDevice,
    walk: caps::CapWalk,
    next: u8,
}

impl<'a> Iterator for CapabilityIter<'a> {
    type Item = Capability<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        // The next-pointer is the device's; a malformed or cyclic link ends the
        // walk rather than running it off the window or forever.
        let offset = self.walk.step(self.next)?;
        self.next = self.device.read_config_u8(offset as u64 + 1);
        Some(Capability { device: self.device, offset: offset as u64 })
    }
}

/// The most functions [`enumerate`] will hand back; the rest are logged, not enumerated.
const MAX_DEVICES: usize = 256;

/// Every PCIe function ECAM decodes, in bus/device/function order; drivers must select all matches, not the first.
pub fn enumerate(ecam: &crate::mm::Mmio) -> Vec<PciDevice> {
    log!("PCI: Enumerating devices...");

    let mut found: Vec<PciDevice> = Vec::new();
    'scan: for bus in 0..=255u16 {
        for dev in 0..32u8 {
            let root = PciDevice::new(ecam, bus as u8, dev, 0);
            if root.vendor_id() == INVALID_VENDOR { continue; }

            let funcs = if root.read_config_u8(HEADER_TYPE) & MULTI_FUNCTION != 0 { 8 } else { 1 };
            for func in 0..funcs {
                let pci = PciDevice::new(ecam, bus as u8, dev, func);
                if pci.vendor_id() == INVALID_VENDOR { continue; }

                print_device(&pci);
                if found.len() == MAX_DEVICES {
                    log!("PCI: more than {} functions decoded; the rest are not enumerated",
                        MAX_DEVICES);
                    break 'scan;
                }
                found.push(pci);
            }
        }
    }

    log!("PCI: Enumeration complete, {} functions.", found.len());
    found
}

fn print_device(pci: &PciDevice) {
    log!(
        "  PCI {:02x}:{:02x}.{} [{:02x}{:02x}] vendor={:04x} device={:04x} prog_if={:02x}",
        pci.bus, pci.dev, pci.func,
        pci.read_config_u8(CLASS), pci.read_config_u8(SUBCLASS),
        pci.vendor_id(), pci.device_id(),
        pci.read_config_u8(PROG_IF)
    );
}
