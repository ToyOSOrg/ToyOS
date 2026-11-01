use alloc::vec::Vec;

use toyos_acpi::EcamWindow;
use toyos_pci::{bar, caps, msi, msix};

use crate::mm::Mmio;
use crate::mm::policy::MmioPolicy;
use crate::log;

const VENDOR_ID: u64 = 0x00;
const DEVICE_ID: u64 = 0x02;
const COMMAND: u64 = 0x04;
const REVISION_ID: u64 = 0x08;
/// `COMMAND` bit 2, PCI 3.0 §6.2.2: without it the function issues no transaction of its own.
const BUS_MASTER: u16 = 0x04;
/// `COMMAND` bit 1, PCI 3.0 §6.2.2: without it the function decodes none of its BARs.
const MEMORY_SPACE: u16 = 0x02;
const PROG_IF: u64 = 0x09;
const SUBCLASS: u64 = 0x0A;
const CLASS: u64 = 0x0B;
const HEADER_TYPE: u64 = 0x0E;
pub(crate) const CAPABILITIES_PTR: u64 = 0x34;

const MULTI_FUNCTION: u8 = 0x80;
const INVALID_VENDOR: u16 = 0xFFFF;

/// The one MSI-X table entry this kernel programs; a device's queues must point at it too.
pub const MSIX_ENTRY: u16 = 0;

/// Why a walk of a function's capability list answered no capability.
pub enum NoCapability {
    /// The walk reached the list's terminator and nothing on it carried the id.
    Absent,
    /// The walk ended at a link the spec forbids, so whether the function
    /// publishes the capability past it was never read.
    Truncated,
}

/// Why a function has no MSI-X entry this kernel can write.
pub enum NoEntry {
    /// No MSI-X capability came off the walk, which is a table this function
    /// does not have only where the walk reached the list's terminator.
    NoTable(NoCapability),
    /// It publishes one whose table this kernel could not reach.
    Unusable,
}

/// Where a function's [`MSIX_ENTRY`] is, found before any message is made for it.
pub(in crate::drivers) struct MsixEntry<'a> {
    cap: Capability<'a>,
    control: u16,
    address: u64,
}

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

    pub fn write_u8(&self, field: u64, val: u8) {
        self.device.write_config_u8(self.offset + field, val)
    }

    pub fn write_u16(&self, field: u64, val: u16) {
        self.device.write_config_u16(self.offset + field, val)
    }

    pub fn write_u32(&self, field: u64, val: u32) {
        self.device.write_config_u32(self.offset + field, val)
    }
}

/// Takes the window rather than a device: the DMA-fault handler has a requester
/// id, no [`PciDevice`], and no lock it may take.
pub fn stop_bus_mastering(config: Mmio) {
    let cmd = config.read_u16(COMMAND);
    config.write_u16(COMMAND, cmd & !BUS_MASTER);
}

/// One function's ECAM window: PCIe extended config space, PCI 3.0 §7.2.2.
///
const CONFIG_BYTES: u64 = 4096;

/// A PCI function, by its bus, device and function and the configuration
/// space its ECAM window gives it.
#[derive(Clone, Copy)]
pub struct PciDevice {
    mmio: Mmio,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

impl PciDevice {
    /// A function over a caller-owned config-space window, for the cap self-test to drive the real walk over lists no hardware in reach produces.
    #[cfg(feature = "boot-actuators")]
    pub(crate) fn over_config(mmio: crate::mm::Mmio) -> Self {
        Self { mmio, bus: 0, dev: 0, func: 0 }
    }

    /// The whole window, for the DMA-fault handler: it publishes these at boot
    /// and clears Bus Master Enable through the one whose requester id matches.
    pub fn config_window(&self) -> Mmio {
        self.mmio
    }

    pub fn vendor_id(&self) -> u16 {
        self.mmio.read_u16(VENDOR_ID)
    }

    pub fn device_id(&self) -> u16 {
        self.mmio.read_u16(DEVICE_ID)
    }

    pub fn revision_id(&self) -> u8 {
        self.mmio.read_u8(REVISION_ID)
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
        match bar::decode(index, self.mmio.read_u32(offset))? {
            bar::Width::Narrow(memory) => Ok(memory),
            bar::Width::Wide(wide) => wide.with_high(self.mmio.read_u32(offset + 4)),
        }
    }

    /// How many BAR slots this function's header declares: six on a Type 0
    /// header, two on a Type 1 bridge, and none on a header type this kernel
    /// does not decode.
    ///
    /// **Every walk over this function's BARs is bounded by this.** A Type 1
    /// header's registers past BAR 1 are its primary, secondary and subordinate
    /// bus numbers and its I/O window, so a walk of six reads a bus number as an
    /// address — and [`Self::bar_size`]'s write-ones probe would set that
    /// bridge's secondary and subordinate bus to 0xFF for the length of it,
    /// cutting off every function behind the bridge.
    pub fn bar_slots(&self) -> u8 {
        match self.read_config_u8(HEADER_TYPE) & !MULTI_FUNCTION {
            0 => bar::MAX_INDEX + 1,
            1 => 2,
            _ => 0,
        }
    }

    /// The byte size Memory Space BAR `index` advertises, by the spec's
    /// write-ones probe; `index` must be ≤ [`bar::MAX_INDEX`]. Memory decode is
    /// off for the probe, so nothing can read through the BAR mid-dance.
    pub fn bar_size(&self, index: u8) -> Result<u64, bar::BadSize> {
        assert!(index <= bar::MAX_INDEX, "PCI: BAR {index} — a Type 0 header has six");
        let offset = bar::BASE + index as u64 * 4;
        let lo = self.mmio.read_u32(offset);
        // The probe's width comes from the decode, which refuses what must
        // not be probed — a Wide claim in slot 5 has no high half, so
        // neither probe write can land on the CardBus CIS pointer at 0x28.
        // An unassigned register still sizes: the spec's probe needs no address.
        let wide = match bar::decode(index, lo) {
            Ok(bar::Width::Wide(_)) => true,
            Ok(bar::Width::Narrow(_)) | Err(bar::Unusable::Unassigned) => false,
            Err(why) => return Err(bar::BadSize::NotMemory(why)),
        };
        let cmd = self.mmio.read_u16(COMMAND);
        self.mmio.write_u16(COMMAND, cmd & !0x2);
        self.mmio.write_u32(offset, u32::MAX);
        let mask_lo = self.mmio.read_u32(offset);
        self.mmio.write_u32(offset, lo);
        let mask_hi = wide.then(|| {
            let hi = self.mmio.read_u32(offset + 4);
            self.mmio.write_u32(offset + 4, u32::MAX);
            let mask = self.mmio.read_u32(offset + 4);
            self.mmio.write_u32(offset + 4, hi);
            mask
        });
        self.mmio.write_u16(COMMAND, cmd);
        bar::advertised_size(mask_lo, mask_hi)
    }

    pub fn write_config_u8(&self, offset: u64, val: u8) {
        self.mmio.write_u8(offset, val)
    }

    pub fn write_config_u16(&self, offset: u64, val: u16) {
        self.mmio.write_u16(offset, val)
    }

    pub fn write_config_u32(&self, offset: u64, val: u32) {
        self.mmio.write_u32(offset, val)
    }

    /// Memory decode, without bus mastering: the BARs answer, and the function
    /// still issues no transaction of its own.
    pub fn enable_memory_space(&self) {
        self.set_memory_decode(true);
    }

    /// `COMMAND` bit 1, PCI 3.0 §6.2.2. Off across a BAR write, so nothing can
    /// read through a register that is half-programmed.
    pub fn set_memory_decode(&self, on: bool) {
        let cmd = self.mmio.read_u16(COMMAND);
        self.mmio.write_u16(COMMAND, if on { cmd | MEMORY_SPACE } else { cmd & !MEMORY_SPACE });
    }

    /// Bus mastering alone, on a function whose memory decode is already on.
    ///
    /// Split from [`Self::enable_bus_master`] because a function handed to a
    /// process must not master the bus until its address space at the unit
    /// exists, and its BARs have to answer before that.
    pub fn start_bus_mastering(&self) {
        let cmd = self.mmio.read_u16(COMMAND);
        self.mmio.write_u16(COMMAND, cmd | BUS_MASTER);
    }

    /// Enable memory space access and bus mastering in PCI command register.
    ///
    /// **What a kernel driver calls, and nothing else does**, which is why it
    /// is where a function is recorded as this kernel's: the set of functions
    /// the kernel drives is exactly the set it lets reach memory, so no list
    /// has to be kept in step by hand.
    pub fn enable_bus_master(&self) {
        crate::pcidev::note_kernel_driver(self);
        self.enable_memory_space();
        self.start_bus_mastering();
    }

    /// Clear bus mastering only — memory space stays, so config and BAR reads still work.
    pub fn disable_bus_master(&self) {
        stop_bus_mastering(self.mmio);
    }

    /// Answers once every message this function sent before the writes that
    /// silenced it has reached the root complex: a read's completion passes
    /// none of the function's earlier posted writes, and the read itself none
    /// of this kernel's (PCIe Base §2.4.1).
    pub fn drain_messages(&self) {
        self.mmio.read_u16(COMMAND);
    }

    pub(in crate::drivers) fn msix_entry(&self) -> Result<MsixEntry<'_>, NoEntry> {
        let cap = self.capability(msix::CAP_ID).map_err(NoEntry::NoTable)?;
        let control = cap.read_u16(msix::MESSAGE_CONTROL);
        let table = match msix::Msix::decode(control, cap.read_u32(msix::TABLE)) {
            Ok(table) => table,
            Err(why) => {
                log!("PCI {:02x}:{:02x}.{}: MSI-X not armed, {}",
                    self.bus, self.dev, self.func, why);
                return Err(NoEntry::Unusable);
            }
        };
        // Decoded, not assumed memory: a device may name a BAR that is an I/O BAR.
        let base = match self.memory_bar(table.bir()) {
            Ok(memory) => memory.address(),
            Err(why) => {
                log!("PCI {:02x}:{:02x}.{}: MSI-X not armed, its table names BAR {} and {}",
                    self.bus, self.dev, self.func, table.bir(), why);
                return Err(NoEntry::Unusable);
            }
        };
        match table.table_address(base) {
            Ok(address) => Ok(MsixEntry { cap, control, address }),
            Err(why) => {
                log!("PCI {:02x}:{:02x}.{}: MSI-X not armed, {}",
                    self.bus, self.dev, self.func, why);
                Err(NoEntry::Unusable)
            }
        }
    }

    fn write_msix(&self, entry: MsixEntry<'_>, address: u32, data: u32) -> Mmio {
        let at = entry.address + MSIX_ENTRY as u64 * msix::ENTRY_BYTES;
        let table = crate::mm::paging::map_mmio(at, 0x1000, MmioPolicy::Uncacheable);

        table.write_u32(msix::ENTRY_ADDRESS_LO, address);
        table.write_u32(msix::ENTRY_ADDRESS_HI, 0);
        table.write_u32(msix::ENTRY_DATA, data);
        table.write_u32(msix::ENTRY_VECTOR_CONTROL, msix::ENTRY_UNMASKED);

        entry.cap.write_u16(msix::MESSAGE_CONTROL, msix::Msix::enabled(entry.control));
        self.report_message(
            "msix",
            table.read_u32(msix::ENTRY_ADDRESS_LO),
            table.read_u32(msix::ENTRY_DATA),
        );
        table
    }

    /// The message as the device's own registers hold it, read back rather than restated.
    fn report_message(&self, kind: &str, address: u32, data: u32) {
        log!("PCI {:02x}:{:02x}.{}: {kind} address={address:#010x} data={data:#010x}",
            self.bus, self.dev, self.func);
    }

    /// Point an IOMMU unit's own function's MSI at `address`/`data` and
    /// enable it: the one message no unit remaps (AMD 48882 Capability Offset
    /// 10h), so it is armed here and not through an interrupt remapping entry.
    /// `false` where the function has no MSI capability.
    pub(crate) fn arm_unit_msi(&self, address: u32, data: u32) -> bool {
        let Ok(cap) = self.capability(msi::CAP_ID) else {
            return false;
        };
        self.write_msi(cap, address, data);
        true
    }

    fn write_msi(&self, cap: Capability<'_>, address: u32, data: u32) {
        let control = cap.read_u16(msi::MESSAGE_CONTROL);
        let msi = msi::Msi::decode(control);
        cap.write_u32(msi.address_lo(), address);
        if let Some(address_hi) = msi.address_hi() {
            cap.write_u32(address_hi, 0);
        }
        cap.write_u16(msi.data(), data as u16);
        if let Some(mask) = msi.mask() {
            cap.write_u32(mask, 0);
        }
        cap.write_u16(msi::MESSAGE_CONTROL, msi::Msi::enabled(control));
        self.report_message(
            "msi",
            cap.read_u32(msi.address_lo()),
            cap.read_u16(msi.data()) as u32,
        );
    }

    /// Mask every MSI-X vector at once with the capability's Function Mask:
    /// nothing where the function has no MSI-X capability.
    pub fn mask_msix_function(&self) {
        let Ok(cap) = self.capability(msix::CAP_ID) else { return };
        let control = cap.read_u16(msix::MESSAGE_CONTROL);
        cap.write_u16(msix::MESSAGE_CONTROL, msix::Msix::function_masked(control));
    }

    /// Put MSI back off.
    ///
    /// The per-vector mask an arming cleared stays clear: a function whose Mask
    /// bit this set would owe a message on the set-to-clear transition a later
    /// arming makes of it, with its Pending bit set (PCIe §7.7.1.7).
    pub fn disable_msi(&self) {
        let Ok(cap) = self.capability(msi::CAP_ID) else { return };
        let control = cap.read_u16(msi::MESSAGE_CONTROL);
        cap.write_u16(msi::MESSAGE_CONTROL, msi::Msi::disabled(control));
    }

    /// Every reader that asks a function what it has asks here, so no reader
    /// can mistake a list that ended early for one that named nothing.
    pub fn capability(&self, id: u8) -> Result<Capability<'_>, NoCapability> {
        let mut walk = self.capabilities();
        match walk.find(|cap| cap.id() == id) {
            Some(cap) => Ok(cap),
            None if walk.walk.truncated() => Err(NoCapability::Truncated),
            None => Err(NoCapability::Absent),
        }
    }

    pub fn capabilities(&self) -> CapabilityIter<'_> {
        let first = self.mmio.read_u8(CAPABILITIES_PTR);
        CapabilityIter { device: self, walk: caps::CapWalk::new(), next: first }
    }

    /// What this function at `at` says it is, for the inventory; driven by
    /// nobody until [`crate::pcidev::inventory`] says otherwise.
    pub fn identity(
        &self,
        arrival: toyos_abi::inventory::Arrival,
        at: toyos_abi::inventory::PciAddr,
    ) -> toyos_abi::inventory::Pci {
        toyos_abi::inventory::Pci {
            arrival,
            at,
            vendor: self.vendor_id(),
            device: self.device_id(),
            class: self.read_config_u8(CLASS),
            subclass: self.read_config_u8(SUBCLASS),
            prog_if: self.read_config_u8(PROG_IF),
            driven: toyos_abi::inventory::Driven::Free,
        }
    }

    #[cfg(feature = "boot-actuators")]
    pub fn is_id(&self, vendor: u16, device: u16) -> bool {
        self.vendor_id() == vendor && self.device_id() == device
    }

}

/// Point a claimed function's [`MSIX_ENTRY`] at the message `claim_msi` made
/// for it and enable it, and answer where its table is. Takes the message and
/// nothing else: the function is the one the entry was written for, and no
/// other message reaches here.
pub fn arm_claimed_msix(message: &crate::iommu::Remapped) -> Result<u64, NoEntry> {
    let pci = message.function();
    let entry = pci.msix_entry()?;
    let table = entry.address;
    pci.write_msix(entry, message.address(), message.data());
    Ok(table)
}

/// Mask the armed [`MSIX_ENTRY`] of the table at `table`, the address
/// [`arm_claimed_msix`] answered and never a device's later answer: a device
/// gone or answering ones takes the write as nothing.
pub fn mask_claimed_msix(table: u64) {
    crate::mm::paging::map_mmio(table + MSIX_ENTRY as u64 * msix::ENTRY_BYTES, 0x1000, MmioPolicy::Uncacheable)
        .write_u32(msix::ENTRY_VECTOR_CONTROL, msix::ENTRY_MASKED);
}

/// [`arm_claimed_msix`] for a function with MSI and no MSI-X; `false` where it
/// has no MSI either.
pub fn arm_claimed_msi(message: &crate::iommu::Remapped) -> bool {
    let pci = message.function();
    let Ok(cap) = pci.capability(msi::CAP_ID) else {
        return false;
    };
    pci.write_msi(cap, message.address(), message.data());
    true
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

/// One ECAM window, mapped over exactly the buses it decodes.
#[derive(Clone, Copy)]
struct Window {
    ecam: EcamWindow,
    mmio: Mmio,
}

impl Window {
    /// The configuration space of a function on a bus this window decodes.
    fn function(&self, bus: u8, dev: u8, func: u8) -> Option<Mmio> {
        Some(self.mmio.subregion(self.ecam.offset(bus, dev, func)?, CONFIG_BYTES))
    }
}

/// The windows [`enumerate`] walked.
static WINDOWS: crate::sync::Lock<Vec<Window>> = crate::sync::Lock::new(Vec::new());

/// The configuration space of the function at this address, whether or not
/// one answers there: an absent function reads all ones. `None` on a bus no
/// window decodes.
pub fn function_window(bus: u8, dev: u8, func: u8) -> Option<Mmio> {
    WINDOWS.lock().iter().find_map(|window| window.function(bus, dev, func))
}

/// The ECAM windows [`enumerate`] walked, in the order it walked them.
pub fn windows() -> Vec<EcamWindow> {
    WINDOWS.lock().iter().map(|window| window.ecam).collect()
}

/// The most functions [`enumerate`] will hand back; the rest are logged, not enumerated.
const MAX_DEVICES: usize = 256;

/// Every PCIe function the windows decode, window by window and in
/// bus/device/function order within one; drivers must select all matches,
/// not the first.
///
/// **Each window is mapped and read over the buses it decodes and no
/// further**: past its end bus the addresses are other hardware's, which
/// reads as functions that are not there.
pub fn enumerate(ecam: &[EcamWindow]) -> Vec<PciDevice> {
    log!("PCI: Enumerating devices...");
    let windows: Vec<Window> = ecam
        .iter()
        .map(|&ecam| {
            let (start, bytes) = ecam.decoded();
            Window { ecam, mmio: crate::mm::paging::map_mmio(start, bytes, MmioPolicy::Uncacheable) }
        })
        .collect();
    *WINDOWS.lock() = windows.clone();

    let mut found: Vec<PciDevice> = Vec::new();
    'scan: for window in &windows {
        let function = |bus: u8, dev: u8, func: u8| {
            let mmio = window.function(bus, dev, func).expect("the scan reads only buses its window decodes");
            PciDevice { mmio, bus, dev, func }
        };
        for bus in window.ecam.buses() {
            for dev in 0..32u8 {
                let root = function(bus, dev, 0);
                if root.vendor_id() == INVALID_VENDOR { continue; }

                let funcs = if root.read_config_u8(HEADER_TYPE) & MULTI_FUNCTION != 0 { 8 } else { 1 };
                for func in 0..funcs {
                    let pci = function(bus, dev, func);
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
    }

    log!("PCI: Enumeration complete, {} functions.", found.len());
    found
}

fn print_device(pci: &PciDevice) {
    log!(
        "  PCI {:02x}:{:02x}.{} [{:02x}{:02x}] vendor={:04x} device={:04x} prog_if={:02x} bars=[{}]",
        pci.bus, pci.dev, pci.func,
        pci.read_config_u8(CLASS), pci.read_config_u8(SUBCLASS),
        pci.vendor_id(), pci.device_id(),
        pci.read_config_u8(PROG_IF),
        assigned_bars(pci),
    );
}

/// The Memory Space windows firmware assigned this function, as `bar<n>=<addr>`
/// in slot order; an unassigned register contributes nothing.
///
/// **Read, never probed.** [`PciDevice::bar_size`]'s write-ones dance turns
/// memory decode off for the duration, and an inventory that only describes a
/// function must not disturb one.
fn assigned_bars(pci: &PciDevice) -> alloc::string::String {
    use core::fmt::Write;

    let slots = pci.bar_slots();
    if slots == 0 {
        return alloc::string::String::from("header type this kernel does not decode");
    }
    let mut out = alloc::string::String::new();
    let mut index = 0;
    while index < slots {
        let slot = index;
        let low = pci.read_config_u32(bar::BASE + slot as u64 * 4);
        let named = match bar::decode(slot, low) {
            Ok(bar::Width::Narrow(memory)) => Ok(memory.address()),
            Ok(bar::Width::Wide(wide)) => {
                let high = pci.read_config_u32(bar::BASE + (slot as u64 + 1) * 4);
                // The pair is one BAR; its high half is not a slot of its own.
                index += 1;
                wide.with_high(high).map(|memory| memory.address())
            }
            Err(why) => Err(why),
        };
        index += 1;
        // An unassigned register is the ordinary case and says nothing; the
        // other refusals are a device describing a space this kernel has not.
        if named == Err(bar::Unusable::Unassigned) {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        // Infallible: writing into a `String` fails only where the formatter does, and none of these do.
        let _ = match named {
            Ok(address) => write!(out, "bar{slot}={address:#x}"),
            Err(why) => write!(out, "bar{slot}=none({why})"),
        };
    }
    out
}

/// Drive the real capability walk over config space no device produces: a
/// cyclic or forbidden link, a head the specification forbids, and a
/// capability reached before a link that ends the walk early.
#[cfg(feature = "boot-actuators")]
pub fn cap_selftest() {
    use crate::mm::{pmm, Mmio};
    use alloc::vec::Vec;

    const CASES: usize = 6;
    const SPLITS: usize = 13;
    /// PCI 3.0 §6.7: a vendor-specific capability, which the walk yields like any other.
    const VENDOR: u8 = 0x09;
    // No device is told it: it stands in for config space.
    let frame = pmm::alloc_frame().expect("pci-cap-selftest: no frame to stand in for config space");
    // SAFETY: `frame` owns this page until it drops at the end of this
    // function, and no `Mmio` built over it escapes.
    let cfg = unsafe { Mmio::over_phys(frame.direct_map(), crate::mm::PAGE_SIZE) };
    let device = PciDevice::over_config(cfg);

    let mut passed = 0usize;
    let mut split_passed = 0usize;
    let mut walk_case = |name: &str,
                         head: u8,
                         links: &[(u64, u8, u8)],
                         want: &[u64],
                         early: bool,
                         reached: Option<u8>| {
        for off in 0..0x100u64 {
            cfg.write_u8(off, 0);
        }
        cfg.write_u8(CAPABILITIES_PTR, head);
        for &(at, id, next) in links {
            cfg.write_u8(at, id);
            cfg.write_u8(at + 1, next);
        }
        // Bounded, so a walk that fails to terminate is a wrong length here, not a hang.
        let got: Vec<u64> = device.capabilities().take(64).map(|c| c.offset()).collect();
        if got.as_slice() == want {
            passed += 1;
        } else {
            log!("pci cap selftest FAILED on {name}: yielded {got:?}, want {want:?}");
        }
        // The split a claimed function's arming turns on: only a walk that
        // reached the list's terminator may answer a capability it did not
        // yield as absent rather than as never read.
        let classify = |id: u8| match device.capability(id) {
            Ok(_) => "found",
            Err(NoCapability::Absent) => "absent",
            Err(NoCapability::Truncated) => "truncated",
        };
        let want_split = if early { "truncated" } else { "absent" };
        let split = classify(msix::CAP_ID);
        if split == want_split {
            split_passed += 1;
        } else {
            log!("pci cap selftest FAILED on {name}: a capability the walk never \
                  reaches reads {split}, want {want_split}");
        }
        // An early end is a fact about the rest of the list, never about what
        // was read.
        if let Some(id) = reached {
            let found = classify(id);
            if found == "found" {
                split_passed += 1;
            } else {
                log!("pci cap selftest FAILED on {name}: a capability it publishes \
                      before the link that ends the walk reads {found}, want found");
            }
        }
        // No layout here publishes an MSI-X capability the walk reaches, so
        // this returns at the capability lookup and touches no MMIO.
        let armed = match device.msix_entry() {
            Ok(_) => "found",
            Err(NoEntry::NoTable(NoCapability::Absent)) => "absent",
            Err(NoEntry::NoTable(NoCapability::Truncated)) => "truncated",
            Err(NoEntry::Unusable) => "unusable",
        };
        if armed == want_split {
            split_passed += 1;
        } else {
            log!("pci cap selftest FAILED on {name}: the arming reads {armed}, \
                  want {want_split}");
        }
    };

    let chain: &[(u64, u8, u8)] = &[(0x40, VENDOR, 0x50), (0x50, VENDOR, 0)];
    walk_case("a well-formed chain", 0x40, chain, &[0x40, 0x50], false, None);
    walk_case("a list that cycles on itself", 0x40, &[(0x40, VENDOR, 0x40)], &[0x40], true, None);
    walk_case("a link that is not dword-aligned", 0x40, &[(0x40, VENDOR, 0x43)], &[0x40], true, None);
    walk_case("a link below the standard header", 0x40, &[(0x40, VENDOR, 0x10)], &[0x40], true, None);
    walk_case("a head the spec forbids", 0x41, &[], &[], true, None);
    walk_case(
        "a capability reached before a link the spec forbids",
        0x40,
        &[(0x40, msi::CAP_ID, 0x43), (0x44, msix::CAP_ID, 0)],
        &[0x40],
        true,
        Some(msi::CAP_ID),
    );
    log!("pci cap split {split_passed}/{SPLITS}");
    log!("pci cap selftest {passed}/{CASES}");
}
