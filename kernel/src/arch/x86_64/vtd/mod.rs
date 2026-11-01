//! Intel VT-d. Every register layout and every Intel name in this subsystem is
//! at or below this module: nothing above `vtd/` may say `Dmar`, `Sagaw` or
//! `SourceId`.
//!
//! Finds units, decodes capabilities, gives every enumerated PCI function a
//! context entry naming one identity-mapped domain, turns translation on, and
//! points every unit at one interrupt remapping table. A capability the kernel
//! cannot use leaves the unit switched off rather than halting, logged by
//! register value rather than a bare "unsupported".
//!
//! Every unit is handed over with translation off, and where every unit has
//! protected memory regions, inside them (`pmr`).

pub mod dmar;
pub mod domain;
pub mod fault;
pub mod interrupt;
mod pmr;
mod queue;
mod table;

use crate::log;
use alloc::vec::Vec;

use crate::drivers::acpi::TableError;
use crate::drivers::pci::PciDevice;
use crate::iommu::{AddressWidth, StreamId};
use crate::mm::Mmio;
use crate::sync::Lock;
use crate::time::{Duration, Tripwire};

use super::iommu_unit::{yn, MAX_UNITS};
use dmar::{Dmar, Malformed, Scope, Scopes, Structure};
use queue::Queue;
use table::{Table, Tables};

/// A unit's 4 KiB register window; every read and write stays inside it.
const REGISTER_WINDOW: u64 = 4096;

const VER_REG: u64 = 0x00;
const CAP_REG: u64 = 0x08;
const ECAP_REG: u64 = 0x10;
const GCMD_REG: u64 = 0x18;
const GSTS_REG: u64 = 0x1C;
const RTADDR_REG: u64 = 0x20;
const FSTS_REG: u64 = 0x34;
const FECTL_REG: u64 = 0x38;
const FEDATA_REG: u64 = 0x3C;
const FEADDR_REG: u64 = 0x40;
const FEUADDR_REG: u64 = 0x44;
const IQH_REG: u64 = 0x80;
const IQT_REG: u64 = 0x88;
const IQA_REG: u64 = 0x90;
/// Register-based invalidation (§6.5.1); `IOTLB_REG` sits `ECAP.IRO` past the base.
const CCMD_REG: u64 = 0x28;
const IOTLB_REG_AFTER_IRO: u64 = 0x08;

/// `GCMD` bits; `GSTS` confirms each at the same bit position.
const TRANSLATION_ENABLE: u32 = 1 << 31;
const SET_ROOT_TABLE_POINTER: u32 = 1 << 30;
const QUEUED_INVALIDATION_ENABLE: u32 = 1 << 26;

/// `GSTS` less its one-shot bits: what §11.4.4.1's recipe carries into every
/// `GCMD` write, so that each write changes exactly one field.
const PERSISTENT: u32 = 0x96FF_FFFF;

/// What firmware may leave on, switched off one field per write before the
/// unit is pointed at anything: §6.6 and §6.7 move a live unit only onto
/// tables that translate alike, §6.5.2 points a queue only while queued
/// invalidation is off, and `CFI` would ride every later write into the `IRE`
/// this kernel sets. `CFI` before `IRE`, while `GSTS` still reports it
/// (§11.4.4.2).
const FIRMWARE_LEFT: [(u32, &str); 4] = [
    (TRANSLATION_ENABLE, "translation"),
    (interrupt::COMPATIBILITY_FORMAT, "compatibility-format pass-through"),
    (interrupt::INTERRUPT_REMAPPING_ENABLE, "interrupt remapping"),
    (QUEUED_INVALIDATION_ENABLE, "queued invalidation"),
];

/// How long a `GCMD` write is given to appear in `GSTS` before the kernel
/// panics: a half-enabled unit's reach cannot be stated.
const COMMAND_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "a unit half-way through being enabled is a unit whose reach nothing can state",
);


/// Remapping tables outlive `init`: the unit keeps reading them while it is on.
static TABLES: Lock<Tables> = Lock::new(Tables::new());

/// Every translating unit, and the single owner of each one's invalidation
/// queue: a queue has one tail, so a second holder would submit behind its back.
static UNITS: Lock<Vec<Live>> = Lock::new(Vec::new());

/// A unit past its hand-over: its window, its queue, and the root table its
/// context entries live in.
pub(super) struct Live {
    regs: Mmio,
    queue: Queue,
    root: Table,
    /// A descriptor type a unit does not implement stalls its queue, so the
    /// interrupt-entry-cache type goes only to a unit that remaps.
    remaps: bool,
}

impl Live {
    pub(super) fn invalidate_domain(&mut self, domain: u16) {
        self.queue.invalidate_domain(self.regs, domain);
    }

    /// Moves `stream` onto `domain` in this unit's tables, and out of
    /// whatever this unit still holds for it under its old one.
    pub(super) fn attach(&mut self, stream: StreamId, id: u16, space: table::Space) {
        let displaced = table::bind(&mut TABLES.lock(), self.root, stream, id, space);
        self.queue.invalidate_context(self.regs, displaced);
    }
}

/// Every remapping unit's interrupt-entry cache, gone.
pub(super) fn invalidate_interrupt_entries() {
    for unit in UNITS.lock().iter_mut().filter(|u| u.remaps) {
        unit.queue.invalidate_interrupts(unit.regs);
    }
}

/// Every region firmware reserved and the interrupt range are what no domain's
/// addresses reach. `false` where firmware published no DMAR.
pub fn init(rsdp_addr: u64, devices: &[PciDevice], kernel: (u64, u64)) -> bool {
    let dmar = match Dmar::open(rsdp_addr) {
        Ok(dmar) => dmar,
        Err(TableError::Absent) => return false,
        Err(e) => {
            log!("iommu: DMAR unusable: {e:?} — this machine has no IOMMU the kernel can use");
            return true;
        }
    };

    log!(
        "iommu: DMAR haw={} flags={:#04x} intr_remap={} x2apic_opt_out={} dma_ctrl_opt_in={}",
        dmar.host_address_width,
        dmar.flags,
        yn(dmar.flags & dmar::FLAG_INTR_REMAP != 0),
        yn(dmar.flags & dmar::FLAG_X2APIC_OPT_OUT != 0),
        yn(dmar.flags & dmar::FLAG_DMA_CTRL_OPT_IN != 0),
    );

    // Before any unit is armed: the handler reaches a faulting function's
    // config space through this and cannot take a lock to find it.
    crate::iommu::fault::describe(devices);

    let mut units = 0usize;
    let mut regions = 0usize;
    let mut held_regions = 0usize;
    // Read once, before the walk, so every region is judged against the same
    // bound the domains below are built over.
    let identity_top = crate::mm::pmm::top();
    // Described and planned before any unit is armed: whether sources may move
    // to the remappable format is one decision, taken before the first `IRE`.
    let mut ready: Vec<(Unit, Plan)> = Vec::new();
    // Every RMRR, which no protected region may cover during the hand-over.
    let mut rmrrs: Vec<(u64, u64)> = Vec::new();
    // Every RMRR with its scopes resolved, classified once the walk is whole.
    let mut reservations: Vec<kernel::rmrr::Region> = Vec::new();
    // What no domain's addresses may reach beside the declared windows, RMRRs
    // added as the walk meets them.
    let mut reserved: Vec<(u64, u64)> = alloc::vec![super::iommu_unit::INTERRUPT_RANGE];
    for structure in dmar.structures() {
        match structure {
            Ok(Structure::Drhd(drhd)) => {
                // Counted before the ceiling refuses it: a unit left uninventoried
                // leaves `units` above `ready`, which is what stops `remappable`.
                units += 1;
                if units > MAX_UNITS {
                    log!("iommu: more than {MAX_UNITS} units described; the rest are not inventoried");
                    break;
                }
                if let Some(unit) = describe_unit(units - 1, &drhd) {
                    if let Some(plan) = plan(&unit) {
                        ready.push((unit, plan));
                    }
                }
            }
            // A kernel device reaches its RMRRs through the identity domain; a
            // claimed one only where [`classify`] keeps them, in its own domain.
            // QEMU publishes none, so this arm is untested by the harness.
            //
            // **The identity domain's half is checked here.** It is built over
            // `[0, top)` exactly (`table::identity_domain`), so a region whose
            // limit falls at or above `top` is one firmware requires stay
            // mapped and this kernel does not map — a device in its scope would
            // fault on the access firmware reserved the range for. No domain
            // hands these addresses out (`domain::avoid`); the only one that
            // maps them is the own domain of the one function [`classify`]
            // keeps them for, at their own address.
            Ok(Structure::Rmrr(rmrr)) => {
                let held = rmrr.limit() < identity_top;
                // Spelled whole in each arm rather than with the verdict as a
                // field: the host profile holds the refusing word against this
                // file's source, and a literal assembled at run time is one no
                // reader of the source can find.
                let (seg, base, limit) = (rmrr.segment(), rmrr.base(), rmrr.limit());
                if held {
                    log!(
                        "iommu: rmrr{regions} seg={seg} {base:#018x}..{limit:#018x} inside the \
                         identity domain 0x0..{identity_top:#x}"
                    );
                } else {
                    log!(
                        "iommu: rmrr{regions} seg={seg} {base:#018x}..{limit:#018x} OUTSIDE the \
                         identity domain 0x0..{identity_top:#x}"
                    );
                }
                reserved.push((base, limit.saturating_add(1)));
                rmrrs.push((base, limit.saturating_add(1)));
                describe_scopes("rmrr", regions, rmrr.scopes());
                // Functions are enumerated on one segment group: a region on
                // another names devices this kernel cannot tell from its own.
                let scopes = if seg == toyos_acpi::SEGMENT_GROUP {
                    resolved(regions, &rmrr.scopes().map(|scope| scope.ok()).collect::<Vec<_>>(), devices)
                } else {
                    log!("iommu: rmrr{regions} is on segment {seg}, which this kernel enumerates no function on");
                    alloc::vec![kernel::rmrr::Scope::Unknown]
                };
                reservations.push(kernel::rmrr::Region { start: base, end: limit.saturating_add(1), scopes });
                regions += 1;
                held_regions += usize::from(held);
            }
            Ok(Structure::Skipped { kind, at, len }) => {
                log!(
                    "iommu: DMAR structure type {kind} ({}) at +{at}, {len} bytes — not used by \
                     this kernel",
                    dmar::structure_name(kind)
                );
            }
            Err(Malformed { at, declared }) => {
                log!(
                    "iommu: DMAR structure at +{at} declares {declared} bytes the table cannot \
                     hold — stopping"
                );
            }
        }
    }

    classify(&reservations, devices);

    if units == 0 {
        log!("iommu: DMAR describes no remapping unit");
    }
    log!(
        "iommu: DMAR describes {units} units and {regions} reserved regions, {held_regions} of \
         them inside the identity domain"
    );

    domain::avoid(reserved);
    let remap = remappable(&ready, units, dmar.flags);
    // One identity-domain table set per address width: units may disagree on
    // `CAP.SAGAW`, and a shared set would be programmed at the wrong depth for one.
    let mut domains: [Option<Table>; 2] = [None, None];
    // Before any table is built: what a domain of a driver's own can be is the
    // narrowest thing every translating unit on this machine agrees to.
    for (unit, plan) in &ready {
        domain::unit_agrees(plan.width, unit.caps.domains(), unit.caps.mgaw());
    }
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::iommu_firmware_left() {
        for (unit, plan) in &ready {
            let domain = identity_domain(&mut TABLES.lock(), &mut domains, plan.width);
            unit.leave_on(devices, domain, plan.width);
        }
    }
    for (unit, _) in &ready {
        pmr::hand_over(unit.index, unit.regs);
    }
    // Built before the regions are chosen, so they can be chosen to hold them.
    let ready: Vec<(Unit, Plan, Table)> = ready
        .into_iter()
        .map(|(unit, plan)| {
            let mut tables = TABLES.lock();
            let domain = identity_domain(&mut tables, &mut domains, plan.width);
            let root = identity_root(&mut tables, devices, domain, plan.width);
            (unit, plan, root)
        })
        .collect();
    let guard = {
        let candidates: Vec<pmr::Candidate> = ready
            .iter()
            .map(|(unit, _, _)| pmr::Candidate {
                index: unit.index,
                regs: unit.regs,
                regions: unit.caps.protected_memory(),
                registers: unit.invalidates_through_registers(),
            })
            .collect();
        // What the OS owns once firmware is done with it: the pmm's memory and
        // ACPI's reclaimable tables, never firmware's NVS.
        let ram: Vec<(u64, u64)> = crate::mm::firmware_map()
            .iter()
            .filter(|e| toyos_bootmap::is_usable_type(e.uefi_type) || e.uefi_type == toyos_bootmap::EFI_ACPI_RECLAIM_MEMORY)
            .map(|e| (e.start, e.end))
            .collect();
        let cover: Vec<(u64, u64)> = TABLES.lock().pages().chain([kernel]).collect();
        if candidates.is_empty() {
            None
        } else {
            pmr::program(&candidates, units, &ram, &rmrrs, &cover, crate::mm::pmm::top())
        }
    };
    if let Some(guard) = &guard {
        log_bus_masters(devices);
        guard.arm();
    }
    let via = if guard.is_some() { Invalidation::Registers } else { Invalidation::Queue };
    let translating: Vec<Translating> =
        ready.into_iter().map(|(unit, plan, root)| translate(unit, plan, root, via)).collect();
    if let Some(guard) = guard {
        guard.release();
    }
    for translating in translating {
        finish(translating, devices, remap);
    }
    true
}

/// Every function mastering the bus as the regions go on: what they block,
/// they block without a record (§11.4.8.1).
fn log_bus_masters(devices: &[PciDevice]) {
    const BUS_MASTER: u16 = 1 << 2;
    let masters: Vec<alloc::string::String> = devices
        .iter()
        .filter(|d| d.read_config_u16(0x04) & BUS_MASTER != 0)
        .map(|d| alloc::format!("{}", StreamId::pci(d.bus, d.dev, d.func)))
        .collect();
    log!("iommu: functions mastering the bus as protected memory goes on: [{}]", masters.join(" "));
}

/// The identity domain at `width`, built the first time it is asked for.
fn identity_domain(tables: &mut Tables, domains: &mut [Option<Table>; 2], width: AddressWidth) -> Table {
    *domains[domain_slot(width)].get_or_insert_with(|| {
        let top = crate::mm::pmm::top();
        let (domain, frames) = table::identity_domain(tables, width, top);
        log!("iommu: identity domain aw={} covers 0x0..{top:#x} in {frames} 2 MiB leaves", width.bits());
        domain
    })
}

/// Whether every interrupt source may be moved to the remappable format, and
/// with `EIME` if so; `None` leaves the machine exactly as it boots with no unit.
///
/// A source writes one address, so a unit left without `IRE` beside one that has
/// it would read that address as a compatibility message and deliver the
/// interrupt to whatever the handle bits spell. Every condition below therefore
/// refuses for the machine, not for the unit that failed it.
fn remappable(ready: &[(Unit, Plan)], described: usize, flags: u8) -> Option<bool> {
    if ready.is_empty() {
        return None;
    }
    if flags & dmar::FLAG_INTR_REMAP == 0 {
        log!(
            "iommu: DMAR flags={flags:#04x} leave INTR_REMAP clear, so firmware says this \
             platform does not remap — every source stays in compatibility format"
        );
        return None;
    }
    if ready.len() != described {
        log!(
            "iommu: {} of {described} units are programmed, so no source may use the remappable \
             format — one left without IRE would misread it",
            ready.len()
        );
        return None;
    }
    if let Some((unit, _)) = ready.iter().find(|(u, _)| !u.caps.interrupt_remapping()) {
        log!(
            "iommu: unit{} cannot remap interrupts (ECAP={:#018x}) — every source stays in \
             compatibility format",
            unit.index,
            unit.caps.ecap
        );
        return None;
    }
    let apics = crate::arch::ioapic::ids();
    if !interrupt::apics_are_named(&apics) {
        log!(
            "iommu: firmware named a requester id for only some of this machine's {} I/O APICs, \
             so no pin could be source-id-verified — every source stays in compatibility format",
            apics.len()
        );
        return None;
    }
    // Without `ECAP.EIM` on every unit an entry's destination is eight bits
    // wide, which is a bound on the ids in use and not a refusal of x2APIC.
    // xAPIC local APICs take the eight-bit form alone (Section 9.9), so
    // `EIME` waits on both.
    let x2apic = super::control_regs::apic_mode() == super::control_regs::ApicMode::X2apic;
    Some(x2apic && ready.iter().all(|(u, _)| u.caps.extended_interrupt_mode()))
}

/// A unit whose register window decodes, with its capabilities.
struct Unit {
    index: usize,
    base: u64,
    regs: Mmio,
    caps: Capabilities,
    /// `VER_REG`'s major version.
    major: u32,
}

impl Unit {
    /// Turn one `GCMD` field on or off by §11.4.4.1's recipe — every other
    /// field as `GSTS` reports it — and wait for `GSTS` to agree.
    fn command(&self, bit: u32, on: bool, what: &str) {
        let status = self.regs.read_u32(GSTS_REG) & PERSISTENT;
        self.regs.write_u32(GCMD_REG, if on { status | bit } else { status & !bit });
        let state = if on { "" } else { " off" };
        self.wait(what, state, || (self.regs.read_u32(GSTS_REG) & bit != 0) == on);
    }

    fn wait(&self, what: &str, state: &str, done: impl Fn() -> bool) {
        let deadline = crate::clock::nanos_since_boot() + COMMAND_TIMEOUT.nanos();
        while !done() {
            assert!(
                crate::clock::nanos_since_boot() < deadline,
                "iommu: unit{} never reported {what}{state}, GSTS={:#010x}",
                self.index,
                self.regs.read_u32(GSTS_REG)
            );
            core::hint::spin_loop();
        }
    }

    /// [`crate::actuator::iommu_firmware_left`]: the state [`Self::hand_over`]
    /// is for, left the way this kernel leaves a unit, each batch ended by a
    /// wait. `CFI` is written as firmware writes it, unconfirmed: whether
    /// `GSTS` reports it is the unit's to say.
    ///
    /// Through the identity domain, never an empty root: a unit left
    /// translating through tables that miss a reserved region faults its
    /// device's DMA for as long as it is left so, which firmware may not do.
    #[cfg(feature = "boot-actuators")]
    fn leave_on(&self, devices: &[PciDevice], domain: Table, width: AddressWidth) {
        let (mut queue, root, remap) = {
            let mut tables = TABLES.lock();
            let queue = Queue::new(&mut tables, self.regs);
            let root = identity_root(&mut tables, devices, domain, width);
            (queue, root, tables.alloc())
        };
        self.command(QUEUED_INVALIDATION_ENABLE, true, "queued invalidation");
        self.regs.write_u64(RTADDR_REG, root.phys());
        self.command(SET_ROOT_TABLE_POINTER, true, "the root table pointer");
        queue.invalidate_all(self.regs);
        self.command(TRANSLATION_ENABLE, true, "translation");
        if self.caps.interrupt_remapping() {
            self.regs.write_u64(interrupt::IRTA_REG, remap.phys());
            self.command(interrupt::SET_TABLE_POINTER, true, "the interrupt remap table pointer");
            queue.invalidate_interrupts(self.regs);
            self.command(interrupt::INTERRUPT_REMAPPING_ENABLE, true, "interrupt remapping");
            let status = self.regs.read_u32(GSTS_REG) & PERSISTENT;
            self.regs.write_u32(GCMD_REG, status | interrupt::COMPATIBILITY_FORMAT);
        }
    }

    /// Whether this unit takes register-based invalidation: a unit of major
    /// version 6 or later refuses every request (§11.4.6.1), and the IOTLB
    /// registers must lie inside the window this kernel maps.
    fn invalidates_through_registers(&self) -> bool {
        self.major < 6 && self.caps.iotlb_registers() + 16 <= REGISTER_WINDOW
    }

    /// A global context-cache invalidation, then a global IOTLB one draining
    /// reads and writes, through the unit's registers (§6.5.1); valid only with
    /// queued invalidation off, whatever `TES` says. A unit that reports either
    /// done at no granularity refused it.
    fn invalidate_through_registers(&self) {
        const START: u64 = 1 << 63;
        const GLOBAL_CONTEXT: u64 = 0b01 << 61;
        const CONTEXT_DONE: u64 = 0b11 << 59;
        const GLOBAL_IOTLB: u64 = 0b01 << 60;
        const DRAIN: u64 = (1 << 49) | (1 << 48);
        const IOTLB_DONE: u64 = 0b11 << 57;
        let iotlb = self.caps.iotlb_registers() + IOTLB_REG_AFTER_IRO;
        for (at, request, done, what) in [
            (CCMD_REG, START | GLOBAL_CONTEXT, CONTEXT_DONE, "the context-cache invalidation"),
            (iotlb, START | GLOBAL_IOTLB | DRAIN, IOTLB_DONE, "the IOTLB invalidation"),
        ] {
            self.regs.write_u64(at, request);
            self.wait(what, " done", || self.regs.read_u64(at) & START == 0);
            let read = self.regs.read_u64(at);
            assert!(read & done != 0, "iommu: unit{} refused {what}: {read:#018x}", self.index);
        }
    }

    /// Every field of [`FIRMWARE_LEFT`] that is on, off.
    fn hand_over(&self) {
        for (bit, what) in FIRMWARE_LEFT {
            if self.regs.read_u32(GSTS_REG) & bit == 0 {
                continue;
            }
            log!("iommu: unit{} was handed over with {what} on; it goes off first", self.index);
            if bit == QUEUED_INVALIDATION_ENABLE {
                // §6.5.2: a queue is switched off only once it is empty and its
                // last descriptor was a wait, which only firmware could have sent.
                self.wait("its invalidation queue", " empty", || {
                    self.regs.read_u64(IQH_REG) == self.regs.read_u64(IQT_REG)
                });
            }
            self.command(bit, false, what);
        }
    }
}

fn describe_unit(index: usize, drhd: &dmar::Drhd) -> Option<Unit> {
    let base = drhd.register_base();
    let Some(regs) = super::iommu_unit::register_window(base, REGISTER_WINDOW) else {
        log!(
            "iommu: unit{index} register base {base:#x} is not a 4 KiB-aligned physical address \
             — not mapped"
        );
        return None;
    };

    let version = regs.read_u32(VER_REG);
    // A described unit whose window does not decode: firmware bug, or the unit is powered down.
    if version == u32::MAX || (version >> 4) & 0xF == 0 {
        log!(
            "iommu: unit{index} @{base:#x}: register window reads ver={version:#010x}, the unit \
             is described but not present"
        );
        return None;
    }

    let caps = Capabilities { cap: regs.read_u64(CAP_REG), ecap: regs.read_u64(ECAP_REG) };
    log!(
        "iommu: unit{index} @{base:#x} seg={} pci_all={} ver={}.{} cap={:#018x} ecap={:#018x} \
         gsts={:#010x} aw={} sagaw={:#04x} mgaw={} nd={} sps2m={} cm={} psi={} nfr={} fro={:#x} \
         qi={} ir={} eim={} pt={} coherent={} sc={} pmr={}",
        drhd.segment(),
        yn(drhd.include_pci_all()),
        (version >> 4) & 0xF,
        version & 0xF,
        caps.cap,
        caps.ecap,
        regs.read_u32(GSTS_REG),
        // Widest of 48/39 the unit advertises; a unit offering neither is refused.
        match caps.address_width() {
            Some(aw) => aw.bits(),
            None => 0,
        },
        caps.sagaw(),
        caps.mgaw(),
        caps.domains(),
        yn(caps.superpage_2m()),
        yn(caps.caching_mode()),
        yn(caps.page_selective_invalidation()),
        caps.fault_recording_registers(),
        caps.fault_recording_offset(),
        yn(caps.queued_invalidation()),
        yn(caps.interrupt_remapping()),
        yn(caps.extended_interrupt_mode()),
        yn(caps.passthrough()),
        yn(caps.coherent()),
        yn(caps.snoop_control()),
        yn(caps.protected_memory()),
    );

    describe_scopes("unit", index, drhd.scopes());
    Some(Unit { index, base, regs, caps, major: (version >> 4) & 0xF })
}

/// What a unit's capabilities let this kernel program, or `None` with the
/// register value that refused it.
struct Plan {
    width: AddressWidth,
    records: fault::Records,
}

fn plan(unit: &Unit) -> Option<Plan> {
    let index = unit.index;
    let Some(width) = unit.caps.address_width() else {
        log!(
            "iommu: unit{index} supports no address width this kernel implements \
             (CAP={:#018x}) — not programmed",
            unit.caps.cap
        );
        return None;
    };
    if !unit.caps.superpage_2m() {
        log!(
            "iommu: unit{index} cannot map 2 MiB pages (CAP={:#018x}) — not programmed",
            unit.caps.cap
        );
        return None;
    }
    if !unit.caps.queued_invalidation() {
        log!(
            "iommu: unit{index} has no queued invalidation (ECAP={:#018x}) — not programmed",
            unit.caps.ecap
        );
        return None;
    }
    if unit.caps.domains() <= table::KERNEL_DOMAIN as u32 {
        log!(
            "iommu: unit{index} supports {} domains, too few to name one (CAP={:#018x}) — not \
             programmed",
            unit.caps.domains(),
            unit.caps.cap
        );
        return None;
    }
    let records =
        fault::Records { offset: unit.caps.fault_recording_offset(), count: unit.caps.fault_recording_registers() };
    if !records.fit(REGISTER_WINDOW) {
        log!(
            "iommu: unit{index} puts {} fault records at {:#x}, past its own {REGISTER_WINDOW}-byte \
             window (CAP={:#018x}) — not programmed",
            records.count,
            records.offset,
            unit.caps.cap
        );
        return None;
    }
    Some(Plan { width, records })
}

/// How a hand-over invalidates after pointing the unit at this kernel's root.
#[derive(Clone, Copy)]
enum Invalidation {
    /// Through the unit's registers, inside protected memory: the queue's
    /// memory is no remapping structure (Appendix A, `ECAP.C`), so the regions
    /// may cover it.
    Registers,
    /// Through the queue, enabled first.
    Queue,
}

/// A unit translating through this kernel's root, its interrupt remapping not
/// yet programmed.
struct Translating {
    unit: Unit,
    width: AddressWidth,
    root: Table,
    /// Enabled already where the hand-over invalidated through it.
    queue: Option<Queue>,
}

/// Whatever firmware left on, off; then `root` pointed, the global
/// invalidation §6.6 owes it, and translation on.
fn translate(unit: Unit, plan: Plan, root: Table, via: Invalidation) -> Translating {
    let Plan { width, records } = plan;
    // After the tables are built, so a unit firmware left translating passes
    // DMA untranslated only while it is programmed, and before the queue is
    // pointed, which §6.5.2 does only with it off.
    unit.hand_over();
    // Before `TE`: the first blocked transaction must be reportable, not merely counted.
    fault::arm(unit.index, unit.regs, records, crate::arch::idt::DMA_FAULT_VECTOR);
    let mut queue = match via {
        Invalidation::Registers => None,
        Invalidation::Queue => Some(enable_queue(&unit)),
    };
    unit.regs.write_u64(RTADDR_REG, root.phys());
    unit.command(SET_ROOT_TABLE_POINTER, true, "the root table pointer");
    match queue.as_mut() {
        Some(queue) => queue.invalidate_all(unit.regs),
        None => unit.invalidate_through_registers(),
    }
    unit.command(TRANSLATION_ENABLE, true, "translation");
    Translating { unit, width, root, queue }
}

fn enable_queue(unit: &Unit) -> Queue {
    let queue = Queue::new(&mut TABLES.lock(), unit.regs);
    unit.command(QUEUED_INVALIDATION_ENABLE, true, "queued invalidation");
    queue
}

/// The queue where it is not on yet, then interrupt remapping, each confirmed
/// in `GSTS` before the next.
fn finish(translating: Translating, devices: &[PciDevice], remap: Option<bool>) {
    let Translating { unit, width, root, queue } = translating;
    let index = unit.index;
    let mut queue = queue.unwrap_or_else(|| enable_queue(&unit));
    // Outside the `TABLES` lock: `interrupt::arm` takes its own lock and
    // then that one, and the order this subsystem holds is the reverse.
    let irta = remap.map(interrupt::arm);
    if let Some(irta) = irta {
        unit.regs.write_u64(interrupt::IRTA_REG, irta);
        unit.command(interrupt::SET_TABLE_POINTER, true, "the interrupt remap table pointer");
        queue.invalidate_interrupts(unit.regs);
        unit.command(interrupt::INTERRUPT_REMAPPING_ENABLE, true, "interrupt remapping");
    }
    // After `IRE`, so nothing is invalidated on a unit that is not yet reading
    // the table, and after `TE`, so nothing reaches a unit that is not on.
    UNITS.lock().push(Live { regs: unit.regs, queue, root, remaps: remap.is_some() });

    let gsts = unit.regs.read_u32(GSTS_REG);
    log!(
        "iommu: unit{index} @{:#x} translating gsts={gsts:#010x} tes={} qies={} ires={} cfis={} \
         irt={:#x} irta={:#x} eime={} root={:#x} domain={} aw={} streams={}",
        unit.base,
        yn(gsts & TRANSLATION_ENABLE != 0),
        yn(gsts & QUEUED_INVALIDATION_ENABLE != 0),
        yn(gsts & interrupt::INTERRUPT_REMAPPING_ENABLE != 0),
        yn(gsts & interrupt::COMPATIBILITY_FORMAT != 0),
        interrupt::table_address(),
        unit.regs.read_u64(interrupt::IRTA_REG),
        yn(remap == Some(true)),
        root.phys(),
        table::KERNEL_DOMAIN,
        width.bits(),
        devices.len(),
    );
}

/// A root table giving every enumerated function a context entry in `domain`:
/// one missing before `TE` bricks the boot disk instead of merely faulting later.
fn identity_root(tables: &mut Tables, devices: &[PciDevice], domain: Table, width: AddressWidth) -> Table {
    let root = tables.alloc();
    for device in devices {
        let stream = StreamId::pci(device.bus, device.dev, device.func);
        table::bind_identity(tables, root, stream, domain, width);
    }
    root
}

/// Slot of the per-width domain cache; exhaustive match so a new `AddressWidth` fails to compile here.
fn domain_slot(width: AddressWidth) -> usize {
    match width {
        AddressWidth::Bits39 => 0,
        AddressWidth::Bits48 => 1,
    }
}

fn describe_scopes(owner: &str, index: usize, scopes: Scopes) {
    for scope in scopes {
        match scope {
            Ok(scope) => log_scope(owner, index, &scope),
            Err(Malformed { at, declared }) => log!(
                "iommu: {owner}{index} device scope at +{at} declares {declared} bytes the \
                 structure cannot hold — stopping"
            ),
        }
    }
}

/// Device scope type 1, a PCI endpoint, whose path names one function, and
/// type 2, a bridge and every function below it (VT-d Rev. 4.0 §8.3.1).
const SCOPE_ENDPOINT: u8 = 1;
const SCOPE_SUBHIERARCHY: u8 = 2;

/// Each scope of RMRR `index` resolved against the functions this kernel
/// enumerated. A malformed entry stands for itself and every entry after it.
fn resolved(index: usize, scopes: &[Option<Scope>], devices: &[PciDevice]) -> Vec<kernel::rmrr::Scope> {
    use kernel::rmrr::Scope as Resolved;
    let mut out = Vec::new();
    for scope in scopes {
        let Some(scope) = scope else {
            log!("iommu: rmrr{index}'s scope list is malformed past here; what it names is unknown");
            out.push(Resolved::Unknown);
            break;
        };
        let resolved = match (scope.kind(), resolve(scope, devices)) {
            (SCOPE_ENDPOINT, Some(at)) => Resolved::Endpoint(stream_of(at).requester()),
            (SCOPE_SUBHIERARCHY, Some(at)) if bridged(at) => {
                let (secondary, subordinate) = (at.read_config_u8(0x19), at.read_config_u8(0x1A));
                Resolved::SubHierarchy(
                    devices
                        .iter()
                        .filter(|d| (secondary..=subordinate).contains(&d.bus))
                        .chain(core::iter::once(at))
                        .map(|d| stream_of(d).requester())
                        .collect(),
                )
            }
            _ => {
                log!("iommu: rmrr{index} names a {} this kernel cannot follow to a function", scope.kind_name());
                Resolved::Unknown
            }
        };
        out.push(resolved);
    }
    out
}

/// Every function the RMRRs name, kept or refused by `kernel::rmrr::classify`,
/// which no claim of it then crosses (`domain::unclaimable`).
fn classify(reservations: &[kernel::rmrr::Region], devices: &[PciDevice]) {
    let class = |id: u16| devices.iter().find(|d| stream_of(d).requester() == id).map(|d| d.read_config_u32(0x08) >> 8);
    for (id, verdict) in kernel::rmrr::classify(reservations, class) {
        let stream = StreamId::pci((id >> 8) as u8, ((id >> 3) & 0x1F) as u8, (id & 7) as u8);
        match &verdict {
            kernel::rmrr::Verdict::Kept(ranges) => log!("iommu: {stream}'s reserved memory is its alone, kept in its own domain: {ranges:x?}"),
            kernel::rmrr::Verdict::Refused(why) => log!("iommu: {stream} is unclaimable: {why}"),
        }
        domain::reserve(stream, verdict);
    }
}

fn stream_of(device: &PciDevice) -> StreamId {
    StreamId::pci(device.bus, device.dev, device.func)
}

/// A configured PCI-to-PCI bridge: header type 1 (PCI Local Bus 3.0 §6.1,
/// 0Eh bits 6:0) with a secondary bus assigned (19h). Anything else names
/// no bus this kernel can follow.
fn bridged(device: &PciDevice) -> bool {
    device.read_config_u8(0x0E) & 0x7F == 1 && device.read_config_u8(0x19) != 0
}

/// The function a scope's path ends at: each step but the last a configured
/// bridge this kernel enumerated, whose secondary bus the next step is on
/// (VT-d Rev. 4.0 §8.3.1).
fn resolve<'a>(scope: &Scope, devices: &'a [PciDevice]) -> Option<&'a PciDevice> {
    let mut bus = scope.start_bus();
    let mut path = scope.path().peekable();
    while let Some((device, function)) = path.next() {
        let found = devices.iter().find(|d| (d.bus, d.dev, d.func) == (bus, device, function))?;
        if path.peek().is_none() {
            return Some(found);
        }
        if !bridged(found) {
            return None;
        }
        bus = found.read_config_u8(0x19);
    }
    None
}

/// Device scope type 3, whose enumeration id is an I/O APIC's MADT id.
const SCOPE_IOAPIC: u8 = 3;

fn log_scope(owner: &str, index: usize, scope: &Scope) {
    match scope.stream_id() {
        Some(stream) => {
            log!(
                "iommu: {owner}{index} scope {} {stream} id={}",
                scope.kind_name(),
                scope.enumeration_id()
            );
            // The only place an I/O APIC's requester id is stated: it sits on a
            // pseudo-bus no PCI walk reaches, so firmware naming it here is the
            // whole of what a source-id check on a pin can be built from.
            if scope.kind() == SCOPE_IOAPIC {
                interrupt::describe_apic(scope.enumeration_id(), stream);
            }
        }
        // No requester id for a device behind a bridge; print what firmware
        // wrote instead.
        None => log!(
            "iommu: {owner}{index} scope {} bus={:#04x} path={} id={} — requester id needs a \
             bridge walk",
            scope.kind_name(),
            scope.start_bus(),
            scope.path().count(),
            scope.enumeration_id()
        ),
    }
}

/// `CAP` and `ECAP`, read once at init and decoded on demand.
struct Capabilities {
    cap: u64,
    ecap: u64,
}

impl Capabilities {
    /// `CAP.ND`: concurrent domains, encoded as the exponent.
    fn domains(&self) -> u32 {
        1u32 << (4 + 2 * (self.cap & 0x7))
    }

    /// `CAP.CM`: read for the log only; the kernel invalidates unconditionally,
    /// since an arm no machine here exercises is presumed wrong.
    fn caching_mode(&self) -> bool {
        self.cap & (1 << 7) != 0
    }

    /// `CAP.SAGAW`, raw: bit *n* covers `30 + 9n` address bits (bit 1 = 39-bit, bit 2 = 48-bit).
    fn sagaw(&self) -> u8 {
        ((self.cap >> 8) & 0x1F) as u8
    }

    /// Widest depth this kernel implements that the unit advertises; `None`
    /// refuses. 57-bit is never considered: an unused level is untested.
    fn address_width(&self) -> Option<AddressWidth> {
        let sagaw = self.sagaw();
        if sagaw & (1 << 2) != 0 {
            Some(AddressWidth::Bits48)
        } else if sagaw & (1 << 1) != 0 {
            Some(AddressWidth::Bits39)
        } else {
            None
        }
    }

    /// `CAP.MGAW`, encoded one less than it is: bounds every IOVA.
    fn mgaw(&self) -> u8 {
        (((self.cap >> 16) & 0x3F) + 1) as u8
    }

    /// `CAP.SPS` bit 0: 2 MiB leaf entries, required because the kernel is 2 MiB-page-only.
    fn superpage_2m(&self) -> bool {
        self.cap & (1 << 34) != 0
    }

    /// `CAP.PSI`; absent means invalidation falls back to domain-wide, correct but coarser.
    fn page_selective_invalidation(&self) -> bool {
        self.cap & (1 << 39) != 0
    }

    /// `CAP.PLMR` and `CAP.PHMR` (bits 5 and 6): both protected memory regions.
    fn protected_memory(&self) -> bool {
        self.cap & (0b11 << 5) == 0b11 << 5
    }

    /// `ECAP.IRO`, in 16-byte units: where the IOTLB registers start.
    fn iotlb_registers(&self) -> u64 {
        ((self.ecap >> 8) & 0x3FF) * 16
    }

    /// `CAP.NFR`, encoded one less than it is.
    fn fault_recording_registers(&self) -> u32 {
        (((self.cap >> 40) & 0xFF) + 1) as u32
    }

    /// `CAP.FRO`, in 16-byte units, from the start of the register window.
    fn fault_recording_offset(&self) -> u64 {
        ((self.cap >> 24) & 0x3FF) * 16
    }

    /// `ECAP.C`: page-table walks snoop the cache; read for the log line only, the flush stays unconditional.
    fn coherent(&self) -> bool {
        self.ecap & (1 << 0) != 0
    }

    /// `ECAP.QI`; a unit without it is left unprogrammed (`plan`).
    fn queued_invalidation(&self) -> bool {
        self.ecap & (1 << 1) != 0
    }

    /// `ECAP.IR`; absence refuses rather than degrades — without it a driver with a mapped BAR can inject an arbitrary vector.
    fn interrupt_remapping(&self) -> bool {
        self.ecap & (1 << 3) != 0
    }

    /// `ECAP.EIM`: 32-bit APIC ids in interrupt remap table entries.
    fn extended_interrupt_mode(&self) -> bool {
        self.ecap & (1 << 4) != 0
    }

    /// `ECAP.SC`: a second-level entry can force DMA to snoop the cache,
    /// overriding a device's own no-snoop setting; read for the log line only.
    fn snoop_control(&self) -> bool {
        self.ecap & (1 << 7) != 0
    }

    /// `ECAP.PT`; absent falls back to identity-mapped translated domains for
    /// kernel-owned devices — same protection, more page tables.
    fn passthrough(&self) -> bool {
        self.ecap & (1 << 6) != 0
    }
}
