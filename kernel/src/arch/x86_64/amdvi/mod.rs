//! AMD-Vi, the AMD I/O Virtualization Technology (AMD 48882 rev 3.11). Every
//! register access and every AMD name of the unit is at or below this module;
//! the words the unit and this driver exchange are `toyos_amdvi`'s.
//!
//! Finds every unit the IVRS describes, logs what each was handed over with,
//! switches off whatever firmware left on, and programs each it can use over
//! one device table every unit reads (§2.2.1): an entry per requester id,
//! blocked unless it names a domain, every enumerated function on the
//! identity domain until a driver moves it to one of its own ([`domain`]),
//! and the event log feeding `iommu::fault` ([`fault`]). A unit it cannot use
//! is logged by register value and left off. Where every unit programmed
//! can remap, every message is remapped through its requester's own table
//! ([`interrupt`]); where one cannot, every entry's interrupt half stays
//! invalid and messages pass as written (Table 9).

pub mod domain;
pub mod fault;
pub mod interrupt;
#[cfg(feature = "boot-actuators")]
pub(super) mod selftest;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::drivers::pci::PciDevice;
use crate::iommu::StreamId;
use crate::log;
use crate::mm::pmm::{self, PhysPage};
use crate::mm::{DirectMap, Mmio, PAGE_2M, PAGE_SIZE};
use crate::sync::Lock;
use crate::time::{Duration, Tripwire};
use toyos_amdvi::dte::{interrupts_blocked, Dte, Setting, DEVICE_IDS, ENTRY_BYTES};
use toyos_amdvi::queue::{Command, ENTRIES};
use toyos_amdvi::reg::{self, control, status, Features, SmiFilter};
use toyos_phys::Phys as Address;

use super::iommu_unit::{register_window, yn, MAX_UNITS};
use toyos_acpi::{
    DeviceEntry, Features as Image, Ivmd, Ivrs, IvrsBlock, IvrsRefused, Phys, Requesters, TableError, Uid, Unit,
};

/// A unit's register window: everything below the performance counters,
/// which nothing here reads.
const REGISTER_WINDOW: u64 = 0x4000;

/// What an untranslated request in it means is fixed by the platform —
/// interrupts, system management, I/O and configuration — and not by a page
/// table (48882 Rev. 3.09 §2.1.2, Table 3), so no domain's addresses enter it.
const SPECIAL_RANGE: (u64, u64) = (0xFD_0000_0000, 0x100_0000_0000);

/// `CONTROL` fields firmware may leave on, each with the `STATUS` bit that
/// says it stopped, or `None` where `CONTROL` reading back is the answer: each
/// log stopped before its interrupt and the unit last, since a log's
/// registers move only once its run bit is clear (§2.4, §2.5.1) and a unit
/// switched off logs nothing.
const FIRMWARE_LEFT: [(u64, Option<u32>, &str); 8] = [
    (control::CMD_BUF_EN, Some(status::CMD_BUF_RUN), "CmdBufEn"),
    (control::EVENT_INT_EN, None, "EventIntEn"),
    (control::EVENT_LOG_EN, Some(status::EVENT_LOG_RUN), "EventLogEn"),
    (control::GA_LOG_EN, Some(status::GA_LOG_RUN), "GALogEn"),
    (control::GA_INT_EN, None, "GAIntEn"),
    (control::PPR_LOG_EN, Some(status::PPR_LOG_RUN), "PPRLogEn"),
    (control::PPR_INT_EN, None, "PPRIntEn"),
    (control::IOMMU_EN, None, "IommuEn"),
];

/// Why a requester the SMI filter passes, which no special entry names,
/// takes no claim.
const SMI_SOURCE: &str = "firmware's SMI filter passes its SMIs";

/// IVMD flags (Table 113).
const IVMD_UNITY: u8 = 1 << 0;
const IVMD_EXCLUSION: u8 = 1 << 3;

/// How long the unit is given to show a `CONTROL` write or finish a command
/// before the kernel panics: a unit half-way through either is one whose
/// reach nothing can state.
const COMMAND_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "a unit that does not act on a register write or a command within a second is one whose reach nothing can state",
);

/// The 4 KiB tables every unit reads, carved out of 2 MiB pages and never
/// freed: a unit may still be walking any of them.
struct Tables {
    /// The device table's own page.
    device_table: Option<PhysPage>,
    pages: Vec<PhysPage>,
    /// Bytes taken from the newest page; starts full so the first call allocates one.
    used: u64,
}

impl Tables {
    const fn new() -> Self {
        Self { device_table: None, pages: Vec::new(), used: PAGE_2M }
    }

    /// One zeroed 4 KiB table.
    fn alloc(&mut self) -> Address<12> {
        if self.used == PAGE_2M {
            self.pages.push(pmm::alloc_page().expect("amdvi: no physical memory for a table"));
            self.used = 0;
        }
        let at = self.pages.last().expect("amdvi: the page just pushed").direct_map().phys() + self.used;
        self.used += PAGE_SIZE;
        Address::new(at).expect("amdvi: a table below 2^48")
    }
}

static TABLES: Lock<Tables> = Lock::new(Tables::new());

/// Every translating unit, and the single owner of each one's command buffer:
/// a buffer has one tail, so a second holder would write behind its back.
static UNITS: Lock<Vec<Live>> = Lock::new(Vec::new());

/// `bytes` at `phys`, through the direct map.
fn memory(phys: u64, bytes: u64) -> Mmio {
    // SAFETY: every caller names a table, ring or page this module took from
    // `pmm` and never frees, which the direct map covers for the machine's life.
    unsafe { Mmio::over_phys(DirectMap::from_phys(phys), bytes) }
}

/// The physical address of the device table every unit reads, one 2 MiB
/// page of 65,536 entries kept in [`TABLES`]; 0 until [`init`] made it.
static DEVICE_TABLE: AtomicU64 = AtomicU64::new(0);

/// The entry for requester `id`, its translation half.
fn entry(id: u16) -> Mmio {
    let table = DEVICE_TABLE.load(Ordering::Acquire);
    assert!(table != 0, "amdvi: an entry written before the device table was made");
    memory(table + u64::from(id) * ENTRY_BYTES as u64, 16)
}

/// A unit described, switched off and found usable, not yet programmed.
struct Ready {
    index: usize,
    base: u64,
    regs: Mmio,
    function: PciDevice,
    features: Features,
    /// What [`settle`] writes to the unit's SMI filter registers before it
    /// turns the filter on; `None` where the unit filters no SMI.
    smi: Option<Vec<SmiFilter>>,
}

/// Every unit the IVRS describes, switched off, and each this kernel can use
/// programmed; `false` where firmware published no IVRS. Every IVMD range and
/// the unit's special ranges are what no domain's addresses reach.
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
    let enumerated = |id: u16| devices.iter().any(|d| StreamId::pci(d.bus, d.dev, d.func).requester() == id);
    // What firmware declares each requester needs, combined where entries
    // overlap: a special entry names in its byte what the range around it
    // does not.
    let mut settings = alloc::vec![Setting(0); DEVICE_IDS];
    let mut ready: Vec<Ready> = Vec::new();
    let mut ivmds: Vec<Ivmd> = Vec::new();
    // The ids a unit that will be programmed serves: a function outside them
    // is behind a unit left off, and no domain of its own would translate.
    let mut translated: Vec<(u16, u16)> = Vec::new();
    // The I/O APICs a unit that will be programmed names, by MADT id.
    let mut apics: Vec<(u8, u16)> = Vec::new();
    // Requester ids no claim may have: the platform speaks as them.
    let mut platform: Vec<(u16, &'static str)> = Vec::new();
    let mut units = 0usize;
    for block in ivrs.blocks() {
        match block {
            IvrsBlock::Unit(unit) => {
                if units == MAX_UNITS {
                    log!("iommu: more than {MAX_UNITS} AMD-Vi units described; the rest are not inventoried");
                    break;
                }
                describe(units, &unit);
                let function = describe_function(units, &unit, devices);
                let mut usable = Ok(());
                let mut serves: Vec<(u16, u16)> = Vec::new();
                let mut specials: Vec<(u8, u8, u16)> = Vec::new();
                for entry in ivrs.devices(&unit) {
                    match entry {
                        Ok(entry) => {
                            log!("iommu: amdvi unit{units} serves {}", served(&ivrs, &entry));
                            // Variety 01h: the handle is an I/O APIC's MADT id;
                            // 02h, an HPET's number (Table 109).
                            if let DeviceEntry::Special { handle, used, variety, .. } = entry {
                                specials.push((variety, handle, used));
                            }
                            match declare(&entry, &mut settings, enumerated) {
                                Ok(ids) => serves.extend(ids),
                                Err(why) => usable = Err(why),
                            }
                        }
                        Err(e) => usable = Err(format!("its device entries are refused: {e:?}")),
                    }
                }
                if let Some(regs) = present(units, &unit) {
                    #[cfg(feature = "boot-actuators")]
                    if crate::actuator::iommu_firmware_left() {
                        leave_on(regs);
                    }
                    switch_off(units, &unit, regs);
                    match usable.and_then(|()| usable_unit(&unit, regs, function)) {
                        Ok(function) => {
                            translated.extend(serves);
                            apics.extend(specials.iter().filter(|(variety, ..)| *variety == 1).map(|&(_, handle, used)| (handle, used)));
                            // An I/O APIC's or an HPET's messages are the platform's (Table 109).
                            platform.extend(specials.iter().map(|&(variety, _, used)| {
                                let what = match variety {
                                    1 => "the IVRS names it as an I/O APIC",
                                    2 => "the IVRS names it as an HPET",
                                    _ => "the IVRS names it as a platform source",
                                };
                                (used, what)
                            }));
                            let features = Features(regs.read_u64(reg::EXTENDED_FEATURES));
                            let chipset: Vec<u16> = specials.iter().map(|&(.., used)| used).collect();
                            let smi = smi_filter(units, regs, features, &chipset);
                            for id in smi.iter().flatten().filter_map(|register| register.passes()) {
                                if !chipset.contains(&id) {
                                    platform.push((id, SMI_SOURCE));
                                }
                            }
                            ready.push(Ready { index: units, base: unit.base, regs, function, features, smi });
                        }
                        Err(why) => log!("iommu: amdvi unit{units} is left off: {why}"),
                    }
                }
                units += 1;
            }
            IvrsBlock::Superseded(unit) => log!(
                "iommu: IVRS IVHD {:#04x} at +{} describes {} again, which is read at its higher type",
                unit.kind,
                unit.at,
                requester(unit.device)
            ),
            IvrsBlock::Memory(m) => {
                log!(
                    "iommu: IVRS IVMD seg={} {} {:#018x}+{:#x} unity={} r={} w={} exclusion={}",
                    m.segment,
                    requesters(m.requesters),
                    m.start,
                    m.length,
                    yn(m.flags & IVMD_UNITY != 0),
                    yn(m.flags & 2 != 0),
                    yn(m.flags & 4 != 0),
                    yn(m.flags & IVMD_EXCLUSION != 0),
                );
                ivmds.push(m);
            }
            IvrsBlock::Other { kind, at, len } => {
                log!("iommu: IVRS block type {kind:#04x} at +{at}, {len} bytes — not used by this kernel")
            }
        }
    }
    if let Some(m) = ivmds.iter().find(|m| m.flags & IVMD_EXCLUSION != 0 || m.flags & IVMD_UNITY == 0) {
        log!(
            "iommu: AMD-Vi isolates no device this boot: firmware's IVMD at {:#x}+{:#x} is {}, which this kernel \
             does not honour, so every unit is left off",
            m.start,
            m.length,
            // Table 113: `ExclusionRange` overrides the rest; `Unity` clear
            // lets the range sit at any device address.
            if m.flags & IVMD_EXCLUSION != 0 { "an exclusion range" } else { "a range without Unity" },
        );
        return true;
    }
    if ready.is_empty() {
        log!("iommu: AMD-Vi isolates no device this boot: {units} unit(s) described, none usable");
        return true;
    }

    // Before any unit is armed: the handler reaches a faulting function's
    // config space through this and cannot take a lock to find it.
    crate::iommu::fault::describe(devices);
    let top = pmm::top();
    for m in &ivmds {
        let end = m.start + m.length;
        log!(
            "iommu: IVMD {} {:#018x}..{end:#018x} {} the identity domain 0x0..{top:#x}",
            requesters(m.requesters),
            m.start,
            if end <= top { "inside" } else { "OUTSIDE" }
        );
    }
    let x2apic = super::control_regs::apic_mode() == super::control_regs::ApicMode::X2apic;
    let remap = interrupt::remappable(
        ready.iter().all(|unit| unit.features.guest_apic()),
        ready.iter().all(|unit| unit.features.x2apic()),
        x2apic,
        &apics,
        &super::ioapic::ids(),
    );
    let identity = build(devices, &settings, remap.is_some());
    let reserved = ivmds
        .iter()
        .map(|m| (m.start, m.start + m.length))
        .chain([super::iommu_unit::INTERRUPT_RANGE, SPECIAL_RANGE])
        .collect();
    let mut ids: Vec<u16> = platform.iter().map(|&(id, _)| id).collect();
    ids.sort_unstable();
    ids.dedup();
    fault::platform(ids.into_iter());
    domain::start(reserved, settings, ivmds.iter().map(|m| m.requesters).collect(), translated, platform);
    log!("iommu: amdvi identity domain covers 0x0..{top:#x}, root={:#x}", identity.get());
    if let Some(extended) = remap {
        log!(
            "iommu: amdvi remaps every message through its requester's table: 128-bit entries, x2apic \
             destinations={}, {} I/O APIC(s) named",
            yn(extended),
            apics.len(),
        );
        interrupt::start(extended, apics);
    }
    for unit in ready {
        program(unit, devices, remap);
    }
    true
}

/// The ids `entry` declares a DTE setting for, combined into `settings`, and
/// the ids whose requests reach the unit under them, which it serves;
/// refused where it makes an enumerated function issue its requests under
/// another's id, which would share that id's domain.
fn declare(
    entry: &DeviceEntry,
    settings: &mut [Setting],
    enumerated: impl Fn(u16) -> bool,
) -> Result<Option<(u16, u16)>, String> {
    let mut set = |first: u16, last: u16, data: u8| {
        for id in first..=last {
            settings[usize::from(id)].0 |= data;
        }
    };
    let (first, last, data) = match *entry {
        DeviceEntry::All { data } => (0, u16::MAX, data),
        DeviceEntry::Select { id, data } | DeviceEntry::Extended { id, data, .. } | DeviceEntry::Hid { id, data, .. } => {
            (id, id, data)
        }
        DeviceEntry::Range { first, last, data } | DeviceEntry::ExtendedRange { first, last, data, .. } => {
            (first, last, data)
        }
        DeviceEntry::Special { used, data, .. } => (used, used, data),
        DeviceEntry::Alias { id: first, used, data } | DeviceEntry::AliasRange { first, used, data, .. } => {
            let last = match *entry {
                DeviceEntry::AliasRange { last, .. } => last,
                _ => first,
            };
            if let Some(id) = (first..=last).find(|&id| id != used && enumerated(id)) {
                return Err(format!("the IVRS has {} issue its requests as {}", requester(id), requester(used)));
            }
            (used, used, data)
        }
        DeviceEntry::Other(_) => return Ok(None),
    };
    set(first, last, data);
    Ok(Some((first, last)))
}

/// The unit's SMI filter registers as [`settle`] is to write them: passing
/// what firmware left valid and every requester id the IVRS names the
/// chipset's own interrupt sources by, `chipset`, which are taken to raise
/// the platform's SMIs too, since §2.1.5.3 leaves naming those to firmware;
/// `None`, said, where the unit has no filter or too few registers left.
fn smi_filter(index: usize, regs: Mmio, features: Features, chipset: &[u16]) -> Option<Vec<SmiFilter>> {
    let Some(count) = features.smi_filters() else {
        log!("iommu: amdvi unit{index} has no SMI filter (EFR {:#018x}), so every SMI a device sends passes", features.0);
        return None;
    };
    let mut registers: Vec<SmiFilter> =
        (0..count as u64).map(|i| SmiFilter(regs.read_u64(reg::SMI_FILTER + 8 * i))).collect();
    for (i, register) in registers.iter().enumerate() {
        log!(
            "iommu: amdvi unit{index} SMI filter {i} = {:#010x} as firmware left it: passes {}, {}",
            register.0,
            match register.passes() {
                Some(id) => format!("{}", requester(id)),
                None => String::from("nothing"),
            },
            if register.locked() { "locked" } else { "unlocked" }
        );
    }
    match reg::smi_filter(&mut registers, chipset) {
        Ok(()) => Some(registers),
        Err(id) => {
            log!(
                "iommu: amdvi unit{index} SMI filter has no register left to pass {}, a chipset source, so it \
                 stays off and every SMI a device sends passes",
                requester(id)
            );
            None
        }
    }
}

/// The unit's function, where this driver can program the unit: segment 0,
/// `INVALIDATE_IOMMU_ALL`, a host table depth it can name, and a function
/// with an MSI capability to report faults on.
fn usable_unit(unit: &Unit, regs: Mmio, function: Option<PciDevice>) -> Result<PciDevice, String> {
    let features = Features(regs.read_u64(reg::EXTENDED_FEATURES));
    if unit.segment != 0 {
        return Err(format!("it serves segment {}, and a requester id here names segment 0", unit.segment));
    }
    if !features.invalidate_all() {
        return Err(format!("EFR {:#018x} has IASup clear", features.0));
    }
    if features.host_levels().is_none() {
        return Err(format!("EFR {:#018x} has HATS 11b, a reserved depth", features.0));
    }
    let function = function.ok_or_else(|| String::from("its own function is not among the enumerated ones"))?;
    function
        .capability(toyos_pci::msi::CAP_ID)
        .map_err(|_| String::from("its function has no MSI capability to report a fault on"))?;
    Ok(function)
}

/// The device table, every entry blocked but each enumerated function's,
/// which names the identity domain; answers the identity domain's root.
fn build(devices: &[PciDevice], settings: &[Setting], remap: bool) -> Address<12> {
    let mut tables = TABLES.lock();
    let page = pmm::alloc_page().expect("amdvi: no physical memory for the device table");
    let at = page.direct_map().phys();
    // Kept, never freed: every unit reads it for the machine's life.
    tables.device_table = Some(page);
    DEVICE_TABLE.store(at, Ordering::Release);
    for (id, setting) in (0..=u16::MAX).zip(settings) {
        let words = Dte::blocked(domain::BLOCKED, *setting).words();
        let slot = entry(id);
        slot.write_u64(8, words[1]);
        slot.write_u64(0, words[0]);
        // Where every unit remaps, every requester's fixed and arbitrated
        // messages are aborted until an entry of its own is written.
        if remap {
            interrupt::interrupt_half(id).write_u64(0, interrupts_blocked(*setting));
        }
    }
    let root = tables.alloc();
    let mut phys = 0;
    while phys < pmm::top() {
        domain::put(&mut tables, root, phys, phys);
        phys += PAGE_2M;
    }
    for device in devices {
        let id = StreamId::pci(device.bus, device.dev, device.func).requester();
        let words = Dte::translated(root, domain::IDENTITY, settings[usize::from(id)]).words();
        let slot = entry(id);
        slot.write_u64(8, words[1]);
        slot.write_u64(0, words[0]);
    }
    root
}

/// A unit past [`program`]: its window, its command buffer, and the word its
/// completion waits store.
struct Live {
    index: usize,
    regs: Mmio,
    commands: Mmio,
    tail: usize,
    done: Address<12>,
    waited: u64,
    /// [`Ready::smi`], until [`settle`] writes it.
    smi: Option<Vec<SmiFilter>>,
}

impl Live {
    /// `commands`, then a `COMPLETION_WAIT`, finished: every one has taken
    /// effect when this returns (§2.4.1).
    fn submit(&mut self, commands: &[Command]) {
        for chunk in commands.chunks(ENTRIES - 2) {
            for command in chunk {
                self.push(*command);
            }
            self.waited += 1;
            let (index, regs, waited) = (self.index, self.regs, self.waited);
            self.push(Command::CompletionWait { store: Address::new(self.done.get()).expect("aligned"), data: waited });
            regs.write_u64(reg::COMMAND_TAIL, reg::offset(self.tail));
            let done = memory(self.done.get(), 8);
            spin_until(
                || done.read_u64(0) == waited,
                || {
                    format!(
                        "iommu: amdvi unit{index} never finished a completion wait: STATUS={:#010x} head={:#x} tail={:#x}",
                        regs.read_u32(reg::STATUS),
                        regs.read_u64(reg::COMMAND_HEAD),
                        regs.read_u64(reg::COMMAND_TAIL)
                    )
                },
            );
        }
    }

    fn push(&mut self, command: Command) {
        let [low, high] = command.words();
        let at = reg::offset(self.tail);
        self.commands.write_u64(at + 8, high);
        self.commands.write_u64(at, low);
        self.tail = (self.tail + 1) % ENTRIES;
    }
}

/// `commands` on every translating unit, finished on each before this returns.
fn submit(commands: &[Command]) {
    for unit in UNITS.lock().iter_mut() {
        unit.submit(commands);
    }
}

/// Programs the unit in the order §2.3.1 gives — the device table, the
/// command buffer and the event log, then `CONTROL` — with the event
/// interrupt armed and the exclusion range cleared before the unit is on;
/// then empties every cache it holds and names each enumerated
/// function's entry.
fn program(unit: Ready, devices: &[PciDevice], remap: Option<bool>) {
    let Ready { index, base, regs, function, smi, .. } = unit;
    let (commands, events, done) = {
        let mut tables = TABLES.lock();
        (tables.alloc(), tables.alloc(), tables.alloc())
    };
    let table = Address::new(DEVICE_TABLE.load(Ordering::Acquire)).expect("amdvi: a device table below 2^48");
    regs.write_u64(reg::DEVICE_TABLE, reg::device_table(table));
    // Writing a base resets the ring's head and tail (§2.4, §2.5).
    regs.write_u64(reg::COMMAND_BASE, reg::ring(commands));
    regs.write_u64(reg::EVENT_BASE, reg::ring(events));
    // `ExEn` and `Allow` clear: no address is passed untranslated (MMIO 0020h).
    regs.write_u64(reg::EXCLUSION_BASE, 0);
    regs.write_u64(reg::EXCLUSION_LIMIT, 0);
    fault::arm(index, regs, events.get());
    function.enable_bus_master();
    let (address, data) = super::apic::msi_message(crate::arch::idt::DMA_FAULT_VECTOR);
    assert!(function.arm_unit_msi(address, data), "amdvi: unit{index}'s function lost its MSI capability");
    // `Coherent`, its reset value and the one Table 97 recommends, has the
    // unit snoop its device table fetches: nothing here flushes a table.
    // Written whole: a field firmware left, `DevTblSegEn` among them, would
    // otherwise change how the unit reads this table. `switch_off` logged
    // what it was handed over with.
    let remapping = match remap {
        None => 0,
        Some(false) => control::GA_EN,
        Some(true) => control::GA_EN | control::XT_EN,
    };
    let on = control::COHERENT
        | control::EVENT_LOG_EN
        | control::EVENT_INT_EN
        | control::CMD_BUF_EN
        | control::IOMMU_EN
        | remapping;
    regs.write_u64(reg::CONTROL, on);
    let running = status::CMD_BUF_RUN | status::EVENT_LOG_RUN;
    spin_until(
        || regs.read_u32(reg::STATUS) & running == running,
        || format!("iommu: amdvi unit{index} never ran its command buffer and event log: STATUS={:#010x}", regs.read_u32(reg::STATUS)),
    );
    let mut live = Live { index, regs, commands: memory(commands.get(), PAGE_SIZE), tail: 0, done, waited: 0, smi };
    // §2.4.8 needs `INVALIDATE_IOMMU_ALL` alone. QEMU 11.1.1 moves a function
    // off pass-through only on an `INVALIDATE_DEVTAB_ENTRY` that names it
    // (`issues/qemus-invalidate-iommu-all-leaves-every-function-untranslated.md`),
    // so each enumerated function is named too, at one command each.
    let reread = devices.iter().map(|d| Command::InvalidateDevice(StreamId::pci(d.bus, d.dev, d.func).requester()));
    live.submit(&[Command::InvalidateAll].into_iter().chain(reread).collect::<Vec<_>>());
    log!(
        "iommu: amdvi unit{index} @{base:#x} translating control={:#018x} status={:#010x} table={:#x} \
         commands={:#x} events={:#x} msi={address:#010x}/{data:#06x} streams={}",
        regs.read_u64(reg::CONTROL),
        regs.read_u32(reg::STATUS),
        table.get(),
        commands.get(),
        events.get(),
        devices.len(),
    );
    UNITS.lock().push(live);
}

/// Every unit's SMI filter on, once every function this kernel drives is out
/// of firmware's hands. A refused SMI is logged as an `IO_PAGE_FAULT`
/// (§2.1.5.1) for [`fault`].
pub fn settle() {
    let mut units = UNITS.lock();
    let mut everywhere = !units.is_empty();
    for unit in units.iter_mut() {
        let Some(filter) = unit.smi.take() else {
            everywhere = false;
            continue;
        };
        for (i, register) in (0u64..).zip(&filter) {
            if !SmiFilter(unit.regs.read_u64(reg::SMI_FILTER + 8 * i)).locked() {
                unit.regs.write_u64(reg::SMI_FILTER + 8 * i, register.0);
            }
        }
        unit.regs.write_u64(reg::CONTROL, unit.regs.read_u64(reg::CONTROL) | control::SMIF_EN | control::SMIF_LOG_EN);
        let passes: Vec<String> = (0..filter.len() as u64)
            .filter_map(|i| SmiFilter(unit.regs.read_u64(reg::SMI_FILTER + 8 * i)).passes())
            .map(|id| format!("{}", requester(id)))
            .collect();
        log!(
            "iommu: amdvi unit{} SMI filter on control={:#018x}, passing {}",
            unit.index,
            unit.regs.read_u64(reg::CONTROL),
            passes.join(" ")
        );
    }
    interrupt::smi_filtered(everywhere);
}

/// A requester id, printed as `pci::enumerate` prints a function.
fn requester(id: u16) -> StreamId {
    let [devfn, bus] = id.to_le_bytes();
    StreamId::pci(bus, devfn >> 3, devfn & 7)
}

fn requesters(named: Requesters) -> String {
    match named {
        Requesters::All => String::from("every requester"),
        Requesters::One(id) => format!("{}", requester(id)),
        Requesters::Range { first, last } => format!("{}..={}", requester(first), requester(last)),
    }
}

fn describe(index: usize, unit: &Unit) {
    let image = match unit.features {
        Image::Reported(word) => format!("features={word:#010x}"),
        Image::Image { attributes, efr, efr2 } => {
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

/// The unit's own PCI function, said: its capability ids, which say whether
/// its event interrupt is an MSI or an MSI-X one (§3.1), and its
/// Miscellaneous Information Register 0 (Capability Offset 10h, §3.2): the
/// address widths and the message number that interrupt is raised with.
fn describe_function(index: usize, unit: &Unit, devices: &[PciDevice]) -> Option<PciDevice> {
    let at = requester(unit.device);
    let Some(function) = devices.iter().find(|d| StreamId::pci(d.bus, d.dev, d.func) == at) else {
        log!("iommu: amdvi unit{index} function {at} is not among the enumerated functions");
        return None;
    };
    let ids: String = function.capabilities().map(|cap| format!(" {:#04x}", cap.id())).collect();
    let misc = function.read_config_u32(u64::from(unit.capability) + 0x10);
    log!(
        "iommu: amdvi unit{index} function {at} caps=[{}] misc0={misc:#010x} va={} pa={} msinum={}",
        ids.trim_start(),
        (misc >> 15) & 0x7F,
        (misc >> 8) & 0x7F,
        misc & 0x1F,
    );
    Some(*function)
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

/// The unit's registers, or `None`, said, where its base is refused or its
/// window does not decode.
fn present(index: usize, unit: &Unit) -> Option<Mmio> {
    let Some(regs) = register_window(unit.base, REGISTER_WINDOW) else {
        log!(
            "iommu: amdvi unit{index} register base {:#x} is not a {REGISTER_WINDOW:#x}-aligned physical \
             address — not mapped, and whatever firmware left on stays on",
            unit.base
        );
        return None;
    };
    // A described unit whose window does not decode: firmware bug, or the unit is powered down.
    if regs.read_u32(reg::STATUS) == u32::MAX {
        log!(
            "iommu: amdvi unit{index} @{:#x}: STATUS reads all ones, the unit is described but not present, \
             and whatever firmware left on stays on",
            unit.base
        );
        return None;
    }
    Some(regs)
}

/// Spins until `done`, panicking with what `stuck` says once
/// [`COMMAND_TIMEOUT`] has passed.
fn spin_until(mut done: impl FnMut() -> bool, stuck: impl Fn() -> String) {
    let deadline = crate::clock::nanos_since_boot() + COMMAND_TIMEOUT.nanos();
    while !done() {
        assert!(crate::clock::nanos_since_boot() < deadline, "{}", stuck());
        core::hint::spin_loop();
    }
}

/// Every field of [`FIRMWARE_LEFT`] that is on, off, each confirmed before
/// the next.
fn switch_off(index: usize, unit: &Unit, regs: Mmio) {
    log!(
        "iommu: amdvi unit{index} handed over control={:#018x} status={:#010x} efr={:#018x} \
         exclusion={:#018x}..{:#018x}",
        regs.read_u64(reg::CONTROL),
        regs.read_u32(reg::STATUS),
        regs.read_u64(reg::EXTENDED_FEATURES),
        regs.read_u64(reg::EXCLUSION_BASE),
        regs.read_u64(reg::EXCLUSION_LIMIT),
    );
    for (bit, run, what) in FIRMWARE_LEFT {
        if regs.read_u64(reg::CONTROL) & bit == 0 {
            continue;
        }
        log!("iommu: amdvi unit{index} was handed over with {what} on; it goes off");
        regs.write_u64(reg::CONTROL, regs.read_u64(reg::CONTROL) & !bit);
        let stopped = || match run {
            Some(run) => regs.read_u32(reg::STATUS) & run == 0,
            None => regs.read_u64(reg::CONTROL) & bit == 0,
        };
        spin_until(stopped, || {
            format!(
                "iommu: amdvi unit{index} never stopped {what}: CONTROL={:#018x} STATUS={:#010x}",
                regs.read_u64(reg::CONTROL),
                regs.read_u32(reg::STATUS)
            )
        });
    }
    log!(
        "iommu: amdvi unit{index} @{:#x} switched off control={:#018x} status={:#010x}",
        unit.base,
        regs.read_u64(reg::CONTROL),
        regs.read_u32(reg::STATUS)
    );
}

/// [`crate::actuator::iommu_firmware_left`]: the unit left on as firmware
/// protecting memory before the operating system runs may leave it — every
/// requester's entry translating through a table that maps nothing, so every
/// DMA is blocked, with its command buffer and event log running. One
/// `INVALIDATE_IOMMU_ALL` empties every cache the unit holds, so it reads
/// every entry as written, and a completion wait says it did.
///
/// The tables are leaked, as firmware's are its own: the unit reads them
/// until it is switched off, and freed they could read back as an entry that
/// passes DMA untranslated.
#[cfg(feature = "boot-actuators")]
fn leave_on(regs: Mmio) {
    let features = Features(regs.read_u64(reg::EXTENDED_FEATURES));
    assert!(
        features.invalidate_all(),
        "amdvi: the unit's EFR {:#018x} has IASup clear, so it takes no INVALIDATE_IOMMU_ALL and the actuator \
         cannot make it read the entries it leaves",
        features.0
    );
    let table = pmm::alloc_page().expect("amdvi: no memory for the device table the actuator leaves on");
    let rest = pmm::alloc_page().expect("amdvi: no memory for the logs the actuator leaves on");
    let (table_at, at) = (table.direct_map().phys(), rest.direct_map().phys());
    #[expect(clippy::disallowed_methods, reason = "the unit reads them until it is switched off, as firmware's own tables")]
    core::mem::forget((table, rest));
    let address = |at: u64| Address::<12>::new(at).expect("amdvi: a page below 2^48");
    let (commands, events, root, done) = (address(at), address(at + 0x1000), address(at + 0x2000), address(at + 0x3000));

    let entries = memory(table_at, (DEVICE_IDS * ENTRY_BYTES) as u64);
    let blocking = Dte::translated(root, 0, Setting(0)).words();
    for id in 0..DEVICE_IDS as u64 {
        entries.write_u64(id * ENTRY_BYTES as u64 + 8, blocking[1]);
        entries.write_u64(id * ENTRY_BYTES as u64, blocking[0]);
    }
    regs.write_u64(reg::DEVICE_TABLE, reg::device_table(address(table_at)));
    regs.write_u64(reg::COMMAND_BASE, reg::ring(commands));
    regs.write_u64(reg::EVENT_BASE, reg::ring(events));
    let on = control::CMD_BUF_EN | control::EVENT_LOG_EN | control::IOMMU_EN;
    regs.write_u64(reg::CONTROL, regs.read_u64(reg::CONTROL) | on);

    const DONE: u64 = 0x600d_f00d;
    let wait = Command::CompletionWait { store: Address::new(done.get()).expect("aligned"), data: DONE };
    let ring = memory(commands.get(), PAGE_SIZE);
    for (slot, command) in [Command::InvalidateAll, wait].into_iter().enumerate() {
        let [low, high] = command.words();
        ring.write_u64(reg::offset(slot) + 8, high);
        ring.write_u64(reg::offset(slot), low);
    }
    regs.write_u64(reg::COMMAND_TAIL, reg::offset(2));
    let stored = memory(done.get(), 8);
    spin_until(
        || stored.read_u64(0) == DONE,
        || format!("amdvi: the actuator's completion wait never landed: STATUS={:#010x}", regs.read_u32(reg::STATUS)),
    );
}
