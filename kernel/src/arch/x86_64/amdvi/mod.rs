//! AMD-Vi, the AMD I/O Virtualization Technology (IOMMU Specification 48882).
//! Every register layout and every AMD name of the unit is at or below this
//! module.
//!
//! Finds every unit the IVRS describes, logs what each was handed over with,
//! and switches off whatever firmware left on, each field confirmed before
//! the next, so DMA reaches memory untranslated. No unit is programmed: no
//! domain is given and no interrupt is remapped on an AMD-Vi machine
//! (`issues/an-amd-vi-machine-isolates-no-device.md`).

use alloc::format;
use alloc::string::String;

use crate::drivers::pci::PciDevice;
use crate::iommu::StreamId;
use crate::log;
use crate::time::{Duration, Tripwire};

use super::iommu_unit::{register_window, yn};
use toyos_acpi::{DeviceEntry, Features, Ivrs, IvrsBlock, IvrsRefused, Phys, Requesters, TableError, Uid, Unit};

/// A unit's register window: everything below the performance counters,
/// which nothing here reads.
const REGISTER_WINDOW: u64 = 0x4000;

const EXTENDED_FEATURES: u64 = 0x0030;
const CONTROL: u64 = 0x0018;
const STATUS: u64 = 0x2020;

const IOMMU_EN: u64 = 1 << 0;
const EVENT_LOG_EN: u64 = 1 << 2;
const CMD_BUF_EN: u64 = 1 << 12;
const PPR_LOG_EN: u64 = 1 << 13;
const GA_LOG_EN: u64 = 1 << 28;

/// `CONTROL` fields firmware may leave on, each with the `STATUS` bit that
/// says it stopped: the logs before the unit, which logs nothing once off.
const FIRMWARE_LEFT: [(u64, Option<u32>, &str); 5] = [
    (CMD_BUF_EN, Some(1 << 4), "CmdBufEn"),
    (EVENT_LOG_EN, Some(1 << 3), "EventLogEn"),
    (GA_LOG_EN, Some(1 << 8), "GALogEn"),
    (PPR_LOG_EN, Some(1 << 7), "PPRLogEn"),
    (IOMMU_EN, None, "IommuEn"),
];

/// How long a `CONTROL` write is given to show before the kernel panics: a
/// unit that will not stop is one whose reach nothing can state.
const COMMAND_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "a unit that does not stop within a second of being told to is one whose reach nothing can state",
);

/// Every unit the IVRS describes, switched off; `false` where firmware
/// published no IVRS.
#[cfg_attr(not(feature = "boot-actuators"), allow(unused_variables))]
pub fn init(rsdp_addr: u64, devices: &[PciDevice]) -> bool {
    let ivrs = match toyos_acpi::ivrs(crate::drivers::acpi::direct_phys(), rsdp_addr) {
        Ok(ivrs) => ivrs,
        Err(IvrsRefused::Table(TableError::Absent)) => return false,
        Err(e) => {
            log!("iommu: IVRS unusable: {e:?} — no unit is found, and one firmware left on stays on");
            return true;
        }
    };
    let info = ivrs.info();
    log!(
        "iommu: IVRS rev={} ivinfo={:#010x} efr_images={} preboot_dma_protection={} pa={} va={}",
        ivrs.revision(),
        info.raw,
        yn(info.efr_images),
        yn(info.dma_remap),
        info.physical_bits,
        info.virtual_bits,
    );
    let mut units = 0usize;
    for block in ivrs.blocks() {
        match block {
            IvrsBlock::Unit(unit) => {
                describe(units, &unit);
                for entry in ivrs.devices(&unit) {
                    match entry {
                        Ok(entry) => log!("iommu: amdvi unit{units} serves {}", served(&ivrs, &entry)),
                        Err(e) => log!("iommu: amdvi unit{units} device entries refused: {e:?}"),
                    }
                }
                switch_off(units, &unit, devices);
                units += 1;
            }
            IvrsBlock::Superseded(unit) => log!(
                "iommu: IVRS IVHD {:#04x} at +{} describes {} again, which is read at its higher type",
                unit.kind,
                unit.at,
                requester(unit.device)
            ),
            IvrsBlock::Memory(m) => {
                let requesters = match m.requesters {
                    Requesters::All => String::from("every requester"),
                    Requesters::One(id) => format!("{}", requester(id)),
                    Requesters::Range { first, last } => format!("{}..={}", requester(first), requester(last)),
                };
                log!(
                    "iommu: IVRS IVMD seg={} {requesters} {:#018x}+{:#x} unity={} r={} w={} exclusion={}",
                    m.segment,
                    m.start,
                    m.length,
                    yn(m.flags & 1 != 0),
                    yn(m.flags & 2 != 0),
                    yn(m.flags & 4 != 0),
                    yn(m.flags & 8 != 0),
                );
            }
            IvrsBlock::Other { kind, at, len } => {
                log!("iommu: IVRS block type {kind:#04x} at +{at}, {len} bytes — not used by this kernel")
            }
        }
    }
    log!(
        "iommu: AMD-Vi isolates no device this boot: {units} unit(s) described, each switched off, so DMA \
         reaches memory untranslated"
    );
    true
}

/// A requester id, printed as `pci::enumerate` prints a function.
fn requester(id: u16) -> StreamId {
    let [devfn, bus] = id.to_le_bytes();
    StreamId::pci(bus, devfn >> 3, devfn & 7)
}

fn describe(index: usize, unit: &Unit) {
    let image = match unit.features {
        Features::Reported(word) => format!("features={word:#010x}"),
        Features::Image { attributes, efr, efr2 } => {
            format!("attributes={attributes:#010x} efr_image={efr:#018x} efr2_image={efr2:#018x}")
        }
    };
    log!(
        "iommu: amdvi unit{index} @{:#x} seg={} dev={} ivhd={:#04x} flags={:#04x} cap={:#x} info={:#06x} {image}",
        unit.base,
        unit.segment,
        requester(unit.device),
        unit.kind,
        unit.flags,
        unit.capability,
        unit.info,
    );
}

/// What one device entry names, and its DTE setting byte.
fn served(ivrs: &Ivrs<impl Phys>, entry: &DeviceEntry) -> String {
    let id = requester;
    match *entry {
        DeviceEntry::All { data } => format!("every requester data={data:#04x}"),
        DeviceEntry::Select { id: one, data } => format!("{} data={data:#04x}", id(one)),
        DeviceEntry::Range { first, last, data } => format!("{}..={} data={data:#04x}", id(first), id(last)),
        DeviceEntry::Alias { id: one, used, data } => format!("{} as {} data={data:#04x}", id(one), id(used)),
        DeviceEntry::AliasRange { first, last, used, data } => {
            format!("{}..={} as {} data={data:#04x}", id(first), id(last), id(used))
        }
        DeviceEntry::Extended { id: one, data, extended } => {
            format!("{} data={data:#04x} extended={extended:#010x}", id(one))
        }
        DeviceEntry::ExtendedRange { first, last, data, extended } => {
            format!("{}..={} data={data:#04x} extended={extended:#010x}", id(first), id(last))
        }
        DeviceEntry::Special { handle, used, variety, data } => {
            format!("special variety={variety} handle={handle} as {} data={data:#04x}", id(used))
        }
        DeviceEntry::Hid { id: one, data, hid, cid, uid } => {
            let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).trim_end_matches('\0').into();
            let uid: String = match uid {
                Uid::Absent => String::from("none"),
                Uid::Integer(value) => format!("{value}"),
                Uid::String { at, len } => text(&ivrs.bytes(at, len).collect::<alloc::vec::Vec<u8>>()),
            };
            let (hid, cid): (String, String) = (text(&hid), text(&cid));
            format!("acpi {hid:?} cid={cid:?} uid={uid:?} as {} data={data:#04x}", id(one))
        }
        DeviceEntry::Other(kind) => format!("an entry of type {kind:#04x} not used by this kernel"),
    }
}

/// Every field of [`FIRMWARE_LEFT`] that is on, off, each confirmed before
/// the next.
#[cfg_attr(not(feature = "boot-actuators"), allow(unused_variables))]
fn switch_off(index: usize, unit: &Unit, devices: &[PciDevice]) {
    let Some(regs) = register_window(unit.base, REGISTER_WINDOW) else {
        log!(
            "iommu: amdvi unit{index} register base {:#x} is not a {REGISTER_WINDOW:#x}-aligned physical \
             address — not mapped, and whatever firmware left on stays on",
            unit.base
        );
        return;
    };
    // A described unit whose window does not decode: firmware bug, or the unit is powered down.
    if regs.read_u32(STATUS) == u32::MAX {
        log!("iommu: amdvi unit{index} @{:#x}: STATUS reads all ones, the unit is described but not present", unit.base);
        return;
    }
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::iommu_firmware_left() {
        leave_on(regs, devices);
    }
    log!(
        "iommu: amdvi unit{index} handed over control={:#018x} status={:#010x} efr={:#018x}",
        regs.read_u64(CONTROL),
        regs.read_u32(STATUS),
        regs.read_u64(EXTENDED_FEATURES),
    );
    for (bit, run, what) in FIRMWARE_LEFT {
        if regs.read_u64(CONTROL) & bit == 0 {
            continue;
        }
        log!("iommu: amdvi unit{index} was handed over with {what} on; it goes off");
        regs.write_u64(CONTROL, regs.read_u64(CONTROL) & !bit);
        let stopped = || match run {
            Some(run) => regs.read_u32(STATUS) & run == 0,
            None => regs.read_u64(CONTROL) & bit == 0,
        };
        let deadline = crate::clock::nanos_since_boot() + COMMAND_TIMEOUT.nanos();
        while !stopped() {
            assert!(
                crate::clock::nanos_since_boot() < deadline,
                "iommu: amdvi unit{index} never stopped {what}: CONTROL={:#018x} STATUS={:#010x}",
                regs.read_u64(CONTROL),
                regs.read_u32(STATUS)
            );
            core::hint::spin_loop();
        }
    }
    log!(
        "iommu: amdvi unit{index} @{:#x} switched off control={:#018x} status={:#010x}",
        unit.base,
        regs.read_u64(CONTROL),
        regs.read_u32(STATUS)
    );
}

/// [`crate::actuator::iommu_firmware_left`]: the unit left on as firmware
/// protecting memory before the operating system runs may leave it — every
/// requester's device table entry translating through a table that maps
/// nothing, so every DMA is blocked, with its command buffer and event log
/// running. Each enumerated function's entry is invalidated, which is what a
/// unit reads a changed entry on, and a completion wait says each was.
///
/// The tables are leaked, as firmware's are its own: the unit reads them
/// until it is switched off, and freed they could read back as an entry that
/// passes DMA untranslated.
#[cfg(feature = "boot-actuators")]
fn leave_on(regs: crate::mm::Mmio, devices: &[PciDevice]) {
    use crate::mm::{pmm, DirectMap, Mmio};

    const DEVICE_TABLE: u64 = 0x0000;
    const COMMAND_BASE: u64 = 0x0008;
    const EVENT_BASE: u64 = 0x0010;
    const COMMAND_HEAD: u64 = 0x2000;
    const COMMAND_TAIL: u64 = 0x2008;
    const EVENT_HEAD: u64 = 0x2010;
    const EVENT_TAIL: u64 = 0x2018;
    /// 2^8 entries of 16 bytes, 4 KiB, in bits 59:56 of either base.
    const LOG_LEN: u64 = 8 << 56;
    const RING: usize = 1 << 8;
    const DEVICE_IDS: u64 = 1 << 16;
    const ENTRY: u64 = 32;
    /// `V`, `TV`, and a four-level table: `Mode` 100b at 11:9; `IR` and `IW`
    /// clear.
    const BLOCKED: u64 = 1 | (1 << 1) | (4 << 9);
    const INVALIDATE_ENTRY: u64 = 2 << 60;
    /// The opcode, and `S`: store the second word at the address.
    const COMPLETION_WAIT: u64 = (1 << 60) | 1;
    const DONE: u64 = 0x600d_f00d;

    // A ring holds one entry fewer than it has, and the wait takes one.
    assert!(devices.len() < RING - 1, "amdvi: {} functions overflow the actuator's {RING}-entry ring", devices.len());
    let window = |phys: u64, bytes: u64| {
        // SAFETY: `phys` is inside a page `pmm::alloc_page` handed out and
        // this function leaks, which the direct map covers for the machine's life.
        unsafe { Mmio::over_phys(DirectMap::from_phys(phys), bytes) }
    };
    let table = pmm::alloc_page().expect("amdvi: no memory for the device table the actuator leaves on");
    let rest = pmm::alloc_page().expect("amdvi: no memory for the logs the actuator leaves on");
    let table_at = table.direct_map().phys();
    let at = rest.direct_map().phys();
    let (commands_at, events_at, root_at, done_at) = (at, at + 0x1000, at + 0x2000, at + 0x3000);
    #[expect(clippy::disallowed_methods, reason = "the unit reads them until it is switched off, as firmware's own tables")]
    core::mem::forget((table, rest));

    let entries = window(table_at, DEVICE_IDS * ENTRY);
    for id in 0..DEVICE_IDS {
        entries.write_u64(id * ENTRY, BLOCKED | root_at);
        entries.write_u64(id * ENTRY + 8, 1);
    }
    regs.write_u64(DEVICE_TABLE, table_at | (DEVICE_IDS * ENTRY / 0x1000 - 1));
    regs.write_u64(COMMAND_BASE, commands_at | LOG_LEN);
    regs.write_u64(EVENT_BASE, events_at | LOG_LEN);
    for at in [COMMAND_HEAD, COMMAND_TAIL, EVENT_HEAD, EVENT_TAIL] {
        regs.write_u64(at, 0);
    }
    regs.write_u64(CONTROL, regs.read_u64(CONTROL) | CMD_BUF_EN | EVENT_LOG_EN | IOMMU_EN);

    let commands = window(commands_at, RING as u64 * 16);
    let mut tail = 0;
    let mut push = |low: u64, high: u64| {
        commands.write_u64(tail, low);
        commands.write_u64(tail + 8, high);
        tail += 16;
    };
    for device in devices {
        push(INVALIDATE_ENTRY | u64::from(StreamId::pci(device.bus, device.dev, device.func).requester()), 0);
    }
    push(COMPLETION_WAIT | done_at, DONE);
    regs.write_u64(COMMAND_TAIL, tail);
    let done = window(done_at, 8);
    let deadline = crate::clock::nanos_since_boot() + COMMAND_TIMEOUT.nanos();
    while done.read_u64(0) != DONE {
        assert!(
            crate::clock::nanos_since_boot() < deadline,
            "amdvi: the actuator's completion wait never landed: STATUS={:#010x}",
            regs.read_u32(STATUS)
        );
        core::hint::spin_loop();
    }
}
