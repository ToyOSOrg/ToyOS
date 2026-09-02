//! Intel VT-d. Every register layout and every Intel name in this subsystem is
//! at or below this module: nothing above `vtd/` may say `Dmar`, `Sagaw` or
//! `SourceId`.
//!
//! Finds units, decodes capabilities, gives every enumerated PCI function a
//! context entry naming one identity-mapped domain, turns translation on, and
//! points every unit at one interrupt remapping table. A capability the kernel
//! cannot use leaves the unit switched off rather than halting, logged by
//! register value rather than a bare "unsupported".

pub mod dmar;
pub mod domain;
pub mod fault;
pub mod interrupt;
mod queue;
mod table;

use alloc::vec::Vec;

use crate::drivers::acpi::TableError;
use crate::drivers::pci::PciDevice;
use crate::iommu::{AddressWidth, StreamId};
use crate::mm::paging::MmioPolicy;
use crate::mm::Mmio;
use crate::sync::Lock;
use crate::time::{Duration, Tripwire};

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
const IQT_REG: u64 = 0x88;
const IQA_REG: u64 = 0x90;

/// `GCMD` bits; `GSTS` confirms each at the same bit position.
const TRANSLATION_ENABLE: u32 = 1 << 31;
const SET_ROOT_TABLE_POINTER: u32 = 1 << 30;
const QUEUED_INVALIDATION_ENABLE: u32 = 1 << 26;
/// `GSTS.RTPS`: the root table pointer has been taken; shares `GCMD`'s `SRTP` bit position.
const ROOT_TABLE_SET: u32 = 1 << 30;

/// How long a `GCMD` write is given to appear in `GSTS` before the kernel
/// panics: a half-enabled unit's reach cannot be stated.
const COMMAND_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "a unit half-way through being enabled is a unit whose reach nothing can state",
);

/// x86-64's 52-bit physical-address ceiling: a register base at or above this
/// is not an address at all, and would otherwise wrap `DirectMap::as_ptr`'s
/// unchecked offset into the user half.
const MAX_PHYS: u64 = 1 << 52;

/// Ceiling on units inventoried; a machine past it is told, not silently truncated.
pub(super) const MAX_UNITS: usize = 16;

/// Remapping tables outlive `init`: the unit keeps reading them while it is on.
static TABLES: Lock<Tables> = Lock::new(Tables::new());

/// Every unit that is translating, in the order they were enabled.
///
/// The single owner of a unit's invalidation queue: a queue has one tail, so a
/// second holder would submit into the same ring behind the first's back.
static UNITS: Lock<Vec<Live>> = Lock::new(Vec::new());

/// A unit past `enable`: its window, its queue, and the root table its context
/// entries live in.
pub(super) struct Live {
    regs: Mmio,
    queue: Queue,
    root: Table,
    /// A descriptor type a unit does not implement stalls its queue, so the
    /// interrupt-entry-cache type goes only to a unit that remaps.
    remaps: bool,
}

impl Live {
    pub(super) fn root(&self) -> Table {
        self.root
    }

    pub(super) fn invalidate_domain(&mut self, domain: u16) {
        self.queue.invalidate_domain(self.regs, domain);
    }

    pub(super) fn invalidate_context(&mut self, domain: u16, stream: u16) {
        self.queue.invalidate_context(self.regs, domain, stream);
    }
}

/// Every remapping unit's interrupt-entry cache, gone.
pub(super) fn invalidate_interrupt_entries() {
    for unit in UNITS.lock().iter_mut().filter(|u| u.remaps) {
        unit.queue.invalidate_interrupts(unit.regs);
    }
}

pub fn init(rsdp_addr: u64, devices: &[PciDevice]) {
    let dmar = match Dmar::open(rsdp_addr) {
        Ok(dmar) => dmar,
        // ACPI cannot distinguish "no VT-d silicon" from "VT-d disabled in
        // firmware"; the line names both rather than guess.
        Err(TableError::Absent) => {
            log!(
                "iommu: no DMAR table — this platform has no IOMMU, or VT-d is disabled in \
                 firmware setup (look for \"VT-d\" / \"Intel Virtualization Technology for \
                 Directed I/O\")"
            );
            return;
        }
        Err(e) => {
            log!("iommu: DMAR unusable: {e:?} — this machine has no IOMMU the kernel can use");
            return;
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
    fault::describe(devices);

    let mut units = 0usize;
    let mut regions = 0usize;
    // Described and planned before any unit is armed: whether sources may move
    // to the remappable format is one decision, taken before the first `IRE`.
    let mut ready: Vec<(Unit, Plan)> = Vec::new();
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
            // Kernel devices get RMRR regions for free and userspace handoff of
            // one is refused elsewhere; QEMU publishes none, so this arm is
            // untested by the harness.
            Ok(Structure::Rmrr(rmrr)) => {
                log!(
                    "iommu: rmrr{regions} seg={} {:#018x}..{:#018x}",
                    rmrr.segment(),
                    rmrr.base(),
                    rmrr.limit()
                );
                describe_scopes("rmrr", regions, rmrr.scopes());
                regions += 1;
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

    if units == 0 {
        log!("iommu: DMAR describes no remapping unit");
    }

    let remap = remappable(&ready, units);
    // One identity-domain table set per address width: units may disagree on
    // `CAP.SAGAW`, and a shared set would be programmed at the wrong depth for one.
    let mut domains: [Option<Table>; 2] = [None, None];
    for (unit, plan) in ready {
        enable(unit, plan, devices, &mut domains, remap);
    }
}

/// Whether every interrupt source may be moved to the remappable format, and
/// with `EIME` if so; `None` leaves the machine exactly as it boots with no unit.
///
/// A source writes one address, so a unit left without `IRE` beside one that has
/// it would read that address as a compatibility message and deliver the
/// interrupt to whatever the handle bits spell. Every condition below therefore
/// refuses for the machine, not for the unit that failed it.
fn remappable(ready: &[(Unit, Plan)], described: usize) -> Option<bool> {
    if ready.is_empty() {
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
    let apics = crate::drivers::ioapic::ids();
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
    Some(ready.iter().all(|(u, _)| u.caps.extended_interrupt_mode()))
}

/// A unit whose register window decodes, with its capabilities and `GCMD` state.
struct Unit {
    index: usize,
    base: u64,
    regs: Mmio,
    caps: Capabilities,
    /// Bits switched on in `GCMD`; the register is not read-modify-write safe, so this is the only record.
    gcmd: u32,
}

impl Unit {
    /// Set one `GCMD` bit and wait for `GSTS` to agree — one bit at a time.
    fn command(&mut self, bit: u32, persistent: bool, status: u32, what: &str) {
        self.regs.write_u32(GCMD_REG, self.gcmd | bit);
        if persistent {
            self.gcmd |= bit;
        }
        let deadline = crate::clock::nanos_since_boot() + COMMAND_TIMEOUT.nanos();
        loop {
            let gsts = self.regs.read_u32(GSTS_REG);
            if gsts & status != 0 {
                return;
            }
            assert!(
                crate::clock::nanos_since_boot() < deadline,
                "iommu: unit{} never reported {what}, GSTS={gsts:#010x}",
                self.index
            );
            core::hint::spin_loop();
        }
    }
}

/// Firmware's register base, mapped only if 4 KiB-aligned and within the
/// physical range — never clamped to fit, since a base in usable RAM would
/// decode as a plausible capability register until a write lands in
/// somebody's heap.
fn window(base: u64) -> Option<Mmio> {
    if base == 0 || !base.is_multiple_of(REGISTER_WINDOW) || base >= MAX_PHYS {
        return None;
    }
    Some(
        crate::mm::paging::map_mmio(base, REGISTER_WINDOW, MmioPolicy::Uncacheable),
    )
}

fn describe_unit(index: usize, drhd: &dmar::Drhd) -> Option<Unit> {
    let base = drhd.register_base();
    let Some(regs) = window(base) else {
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
         aw={} sagaw={:#04x} mgaw={} nd={} sps2m={} cm={} psi={} nfr={} fro={:#x} qi={} ir={} \
         eim={} pt={} coherent={} sc={}",
        drhd.segment(),
        yn(drhd.include_pci_all()),
        (version >> 4) & 0xF,
        version & 0xF,
        caps.cap,
        caps.ecap,
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
    );

    describe_scopes("unit", index, drhd.scopes());
    Some(Unit { index, base, regs, caps, gcmd: 0 })
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

/// Programs the unit in its own order — queue, root pointer, interrupt remap
/// pointer, global invalidation, then `TE` — each confirmed in `GSTS` before
/// the next.
fn enable(
    mut unit: Unit,
    plan: Plan,
    devices: &[PciDevice],
    domains: &mut [Option<Table>; 2],
    remap: Option<bool>,
) {
    let index = unit.index;
    let Plan { width, records } = plan;
    // Before any table is built: what a domain of a driver's own can be is the
    // narrowest thing every translating unit on this machine agrees to.
    domain::unit_agrees(width, unit.caps.domains());

    let (root, queue) = {
        let mut tables = TABLES.lock();
        let domain = match domains[domain_slot(width)] {
            Some(domain) => domain,
            None => {
                let top = crate::mm::pmm::top();
                let (domain, frames) = table::identity_domain(&mut tables, width, top);
                log!(
                    "iommu: identity domain aw={} covers 0x0..{top:#x} in {frames} 2 MiB leaves",
                    width.bits()
                );
                domains[domain_slot(width)] = Some(domain);
                domain
            }
        };

        // Every enumerated function needs a context entry before `TE`: one
        // missing bricks the boot disk instead of merely faulting later.
        let root = tables.alloc();
        for device in devices {
            let stream = StreamId::pci(device.bus, device.dev, device.func);
            // Unreachable from the host side, so these actuators substitute for
            // it; both are answered on the device's first *read*, since a
            // first-write access would cache write permission and never fault.
            if crate::actuator::iommu_context_absent()
                && device.matches_class(NVME_CLASS, NVME_SUBCLASS, None)
            {
                log!("iommu: unit{index} leaves {stream} out of the root table (actuator)");
                continue;
            }
            // A present context entry naming an empty domain, distinct from a
            // missing entry: passthrough would fault identically either way.
            if crate::actuator::iommu_empty_domain()
                && device.matches_class(NVME_CLASS, NVME_SUBCLASS, None)
            {
                let empty = tables.alloc();
                log!("iommu: unit{index} gives {stream} a domain with no mappings (actuator)");
                table::bind_identity(&mut tables, root, stream, empty, width);
                continue;
            }
            table::bind_identity(&mut tables, root, stream, domain, width);
        }

        let mut queue = Queue::new(&mut tables, unit.regs);
        drop(tables);
        // Outside the `TABLES` lock: `interrupt::arm` takes its own lock and
        // then that one, and the order this subsystem holds is the reverse.
        let irta = remap.map(interrupt::arm);

        // Before `TE`: the first blocked transaction must be reportable, not merely counted.
        fault::arm(index, unit.regs, records, crate::arch::idt::DMA_FAULT_VECTOR);

        unit.command(
            QUEUED_INVALIDATION_ENABLE,
            true,
            QUEUED_INVALIDATION_ENABLE,
            "queued invalidation",
        );
        unit.regs.write_u64(RTADDR_REG, root.phys());
        unit.command(SET_ROOT_TABLE_POINTER, false, ROOT_TABLE_SET, "the root table pointer");
        if let Some(irta) = irta {
            unit.regs.write_u64(interrupt::IRTA_REG, irta);
            unit.command(
                interrupt::SET_TABLE_POINTER,
                false,
                interrupt::SET_TABLE_POINTER,
                "the interrupt remap table pointer",
            );
            queue.invalidate_interrupts(unit.regs);
        }
        queue.invalidate_all(unit.regs);
        (root, queue)
    };

    unit.command(TRANSLATION_ENABLE, true, TRANSLATION_ENABLE, "translation");
    // `GCMD.CFI` is never among the bits `command` writes, so every write leaves
    // it clear and `GSTS.CFIS` reads clear: a compatibility-format message is
    // blocked from here on, which is the whole point of the step.
    if remap.is_some() {
        unit.command(
            interrupt::INTERRUPT_REMAPPING_ENABLE,
            true,
            interrupt::INTERRUPT_REMAPPING_ENABLE,
            "interrupt remapping",
        );
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

/// Device class the actuators target, not a bus/device/function: QEMU's slot
/// choice is not this kernel's business, and the harness reads the same
/// class independently out of `pci::enumerate`.
const NVME_CLASS: u8 = 0x01;
const NVME_SUBCLASS: u8 = 0x08;

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

    /// `ECAP.QI`; absent falls back to `CCMD_REG`/`IOTLB_REG`, correct but slower.
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
    /// overriding a device's own no-snoop setting — the HDA driver relies on
    /// this to override vendor no-snoop with no config-write syscall.
    fn snoop_control(&self) -> bool {
        self.ecap & (1 << 7) != 0
    }

    /// `ECAP.PT`; absent falls back to identity-mapped translated domains for
    /// kernel-owned devices — same protection, more page tables.
    fn passthrough(&self) -> bool {
        self.ecap & (1 << 6) != 0
    }
}

/// One character per boolean; `n` is printed rather than omitted, since an
/// absent field would look like a forgotten one.
fn yn(v: bool) -> char {
    if v {
        'y'
    } else {
        'n'
    }
}
