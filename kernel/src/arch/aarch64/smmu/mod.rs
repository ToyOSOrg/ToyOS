//! The IOMMU: the SMMUv3 the IORT names (Arm IHI 0070 H.a), programmed with
//! `toyos_smmu`'s encodings. Every register and Arm name of the unit is at or
//! below this module.
//!
//! [`init`] arms the unit with every stream aborting and no bypass anywhere:
//! `GBPA` aborts before anything else is written, so nothing passes while the
//! unit is programmed; every enumerated function's stream has an entry that
//! aborts, and the unit is enabled. A stream translates only once
//! [`domain::attach`] puts it on a domain of its own, through stage 1 alone;
//! no domain maps memory by identity, so a function no driver attached
//! reaches nothing. A unit this kernel cannot program is left aborting every
//! transaction, and no domain is given.
//!
//! The unit reads what it is given coherently — `toyos_smmu::unit::probe`
//! refuses one that does not — so a store it reads is published by the order
//! [`Mmio`] gives it ahead of the register write or the command that names it.
//!
//! One lock, [`UNIT`], over the unit's registers, queues, tables and domains.
//! The event interrupt takes none ([`fault`]).

pub mod domain;
pub mod fault;
#[cfg(feature = "boot-actuators")]
mod selftest;

use alloc::vec::Vec;

use toyos_acpi::{IortRefused, Node, Route, TableError};
use toyos_phys::Phys;
use toyos_smmu::config::Ste;
use toyos_smmu::queue::{Command, Commands, Signal};
use toyos_smmu::unit::{self as reg, Unit};

use crate::drivers::acpi::direct_phys;
use crate::drivers::pci::PciDevice;
use crate::iommu::StreamId;
use crate::log;
use crate::mm::pmm::{self, PhysPage};
use crate::mm::policy::MmioPolicy;
use crate::mm::{DirectMap, Mmio, PAGE_2M};
use crate::sync::Lock;
use crate::time::{Duration, Tripwire};

/// Register pages 0 and 1, 64 KiB each (§6.1).
const REGISTER_PAGE: u64 = 0x1_0000;

/// The largest queues this kernel makes, as log2 of their entries: 4 KiB each.
const COMMANDS_LOG2: u8 = 8;
const EVENTS_LOG2: u8 = 7;

/// A stream table no larger than one 2 MiB page.
const STREAMS_LOG2_MAX: u32 = 15;

/// How long the unit is given to take a register write or consume its
/// commands before the kernel panics: a unit half-way through either is one
/// whose reach nothing can state.
const UNIT_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "a unit that takes no register write or command for a second is one whose reach nothing can state",
);

static UNIT: Lock<Option<Live>> = Lock::new(None);

/// The unit's two register pages, addressed by `toyos_smmu::unit`'s offsets.
#[derive(Clone, Copy)]
struct Registers(Mmio);

impl Registers {
    fn read(self, at: usize) -> u32 {
        self.0.read_u32(at as u64)
    }

    fn write(self, at: usize, value: u32) {
        self.0.write_u32(at as u64, value);
    }

    fn write64(self, at: usize, value: u64) {
        self.0.write_u64(at as u64, value);
    }

    /// Wait until `done`, panicking past [`UNIT_TIMEOUT`] or on a command the
    /// unit stopped its queue at.
    fn wait(self, what: &str, done: impl Fn() -> bool) {
        let deadline = crate::clock::nanos_since_boot() + UNIT_TIMEOUT.nanos();
        while !done() {
            let errors = reg::active_errors(self.read(reg::GERROR), self.read(reg::GERRORN));
            if errors & reg::GERROR_CMDQ != 0 {
                let cons = self.read(reg::CMDQ_CONS);
                panic!("SMMU: the command queue stopped at CONS {cons:#x}: {:?}", reg::command_error(cons));
            }
            assert!(
                crate::clock::nanos_since_boot() < deadline,
                "SMMU: {what} not seen within {UNIT_TIMEOUT}; GERROR {errors:#x}"
            );
            core::hint::spin_loop();
        }
    }

    /// Write `value` to the control register at `at` and wait for its
    /// acknowledgement register, the word above it, to read it back.
    fn control(self, at: usize, value: u32, what: &str) {
        self.write(at, value);
        self.wait(what, || self.read(at + 4) == value);
    }
}

/// The armed unit and everything it reads.
struct Live {
    regs: Registers,
    unit: Unit,
    commands: Commands,
    queue: Mmio,
    /// `SMMU_CMDQ_PROD` as last written.
    prod: u32,
    streams: Mmio,
    memory: Memory,
    /// Each enumerated function the IORT routes through this unit, and its
    /// StreamID.
    routes: Vec<(StreamId, u32)>,
    domains: Vec<domain::Domain>,
    /// What no domain's addresses may reach, as `(start, end)`.
    reserved: Vec<(u64, u64)>,
}

impl Live {
    /// `commands`, then a `CMD_SYNC`, consumed: every one has taken effect
    /// when this returns.
    fn issue(&mut self, commands: &[Command]) {
        let (regs, queue) = (self.regs, self.commands);
        for command in commands.iter().copied().chain([Command::Sync(Signal::None)]) {
            let prod = self.prod;
            regs.wait("room in its command queue", || !queue.is_full(prod, regs.read(reg::CMDQ_CONS)));
            let at = queue.slot(prod) as u64 * 16;
            let [low, high] = command.words();
            self.queue.write_u64(at, low);
            self.queue.write_u64(at + 8, high);
            self.prod = queue.after(prod);
        }
        let prod = self.prod;
        regs.write(reg::CMDQ_PROD, prod);
        regs.wait("its CMD_SYNC consumed", || queue.is_empty(prod, regs.read(reg::CMDQ_CONS)));
    }

    /// The StreamID the IORT gives `function`'s requests through this unit.
    fn stream(&self, function: StreamId) -> u32 {
        let route = self.routes.iter().find(|(rid, _)| *rid == function);
        route.unwrap_or_else(|| panic!("SMMU: {function} is no function the IORT routes through this unit")).1
    }

    /// Point stream `stream`'s entry at `ste` and have the unit forget what
    /// it held for the old one. The first doubleword goes last: it holds `V`,
    /// `Config` and the context pointer, and the others are read only under
    /// the configuration it names.
    fn write_entry(&mut self, stream: u32, ste: Ste) {
        let words = ste.words();
        let at = u64::from(stream) * 64;
        for (i, word) in words.iter().enumerate().skip(1) {
            self.streams.write_u64(at + 8 * i as u64, *word);
        }
        self.streams.write_u64(at, words[0]);
        self.issue(&[Command::ForgetStream(stream)]);
    }
}

/// What the unit reads, carved from 2 MiB pages and never given back: a page
/// the unit may still walk is not a page to free.
struct Memory {
    pages: Vec<PhysPage>,
    /// Bytes taken from the newest page; full to begin with.
    used: u64,
}

impl Memory {
    const fn new() -> Self {
        Self { pages: Vec::new(), used: PAGE_2M }
    }

    /// `bytes` of zeroes, aligned to their own size: a power of two from 64
    /// bytes to 2 MiB.
    fn alloc(&mut self, bytes: u64) -> u64 {
        assert!(bytes.is_power_of_two() && (64..=PAGE_2M).contains(&bytes), "SMMU: {bytes:#x} bytes of tables");
        let mut at = self.used.next_multiple_of(bytes);
        if at + bytes > PAGE_2M {
            self.pages.push(pmm::alloc_page().expect("SMMU: no memory for the unit's tables"));
            at = 0;
        }
        self.used = at + bytes;
        let phys = self.pages.last().expect("a page was just pushed").direct_map().phys() + at;
        let zeroed = window(phys, bytes);
        for offset in (0..bytes).step_by(8) {
            zeroed.write_u64(offset, 0);
        }
        phys
    }
}

/// `bytes` of [`Memory`] at `phys`.
fn window(phys: u64, bytes: u64) -> Mmio {
    // SAFETY: every caller names memory `Memory::alloc` handed out, which is
    // never freed and which the direct map covers for the machine's life.
    unsafe { Mmio::over_phys(DirectMap::from_phys(phys), bytes) }
}

/// The SMMUv3, armed; a refusal leaves it aborting what it is given, or says
/// why it could not be told to.
pub fn init(rsdp_addr: u64, devices: &[PciDevice], windows: &[toyos_abi::boot::RootBridgeWindow]) {
    let iort = match toyos_acpi::iort(direct_phys(), rsdp_addr) {
        Ok(iort) => iort,
        Err(IortRefused::Table(TableError::Absent)) => {
            log!("IOMMU: no IORT, so no SMMUv3: no device is translated this boot");
            return;
        }
        Err(why) => {
            log!("IOMMU: the IORT is refused: {why:?}; no device is translated this boot");
            return;
        }
    };
    let mut units = iort.nodes().filter_map(|node| match node {
        Node::Smmuv3(smmu) => Some(smmu),
        _ => None,
    });
    let Some(smmu) = units.next() else {
        log!("IOMMU: the IORT names no SMMUv3: no device is translated this boot");
        return;
    };
    if units.next().is_some() {
        log!("IOMMU: the IORT names more than one SMMUv3, and this kernel drives one: none is touched");
        return;
    }
    if !smmu.base.is_multiple_of(REGISTER_PAGE) {
        log!("IOMMU: the SMMUv3's registers at {:#x} are not on a 64 KiB page: not touched", smmu.base);
        return;
    }
    let regs = Registers(crate::mm::paging::map_mmio(smmu.base, 2 * REGISTER_PAGE, MmioPolicy::Uncacheable));
    abort_unprogrammed(regs, smmu.base);

    let segment = u32::from(crate::pcidev::segment());
    let mut routes = Vec::new();
    for device in devices {
        let function = StreamId::pci(device.bus, device.dev, device.func);
        match iort.route(segment, function.requester()) {
            Ok(Route::Translated { smmu: by, stream, .. }) if by.base == smmu.base => routes.push((function, stream)),
            Ok(route) => log!("IOMMU: {function} is not routed through the SMMUv3: {route:?}"),
            Err(why) => log!("IOMMU: {function}'s IORT route is refused: {why:?}"),
        }
    }
    let reserved = windows.iter().map(|w| (w.base, w.end())).collect();
    let Some(live) = program(regs, smmu.base, smmu.coherent_override, smmu.event, routes, reserved, devices) else {
        return;
    };
    *UNIT.lock() = Some(live);
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::smmu_selftest() {
        selftest::run(devices);
    }
}

/// `GBPA` aborting and the unit disabled: from here until `SMMUEN`, and for
/// good if it is never set, every transaction aborts.
fn abort_unprogrammed(regs: Registers, base: u64) {
    let gbpa = || regs.read(reg::GBPA);
    regs.wait("GBPA free to update", || gbpa() & reg::GBPA_UPDATE == 0);
    regs.write(reg::GBPA, gbpa() | reg::GBPA_ABORT | reg::GBPA_UPDATE);
    regs.wait("GBPA updated", || gbpa() & reg::GBPA_UPDATE == 0);
    assert!(
        gbpa() & reg::GBPA_ABORT != 0,
        "SMMU: GBPA reads {:#x} after ABORT was written: transactions bypass while SMMUEN is clear",
        gbpa()
    );
    let cr0 = regs.read(reg::CR0);
    if cr0 != 0 {
        log!("IOMMU: the SMMUv3 at {base:#x} was handed over with CR0 {cr0:#x}; it goes off first");
        regs.control(reg::CR0, 0, "CR0 cleared");
    }
    log!("IOMMU: SMMUv3 at {base:#x}: GBPA {:#x}, every transaction aborts while SMMUEN is clear", gbpa());
}

/// The unit's tables, queues and interrupt, and `SMMUEN`; `None`, with the
/// reason logged, where it cannot be, the unit left as
/// [`abort_unprogrammed`] left it.
fn program(
    regs: Registers,
    base: u64,
    coherent_override: bool,
    event: Option<core::num::NonZeroU32>,
    routes: Vec<(StreamId, u32)>,
    reserved: Vec<(u64, u64)>,
    devices: &[PciDevice],
) -> Option<Live> {
    let refused = |why: core::fmt::Arguments<'_>| log!("IOMMU: the SMMUv3 at {base:#x} is left aborting: {why}");
    let (idr0, idr1, idr5) = (regs.read(reg::IDR0), regs.read(reg::IDR1), regs.read(reg::IDR5));
    let unit = match reg::probe(idr0, idr1, idr5, coherent_override) {
        Ok(unit) => unit,
        Err(lacks) => {
            refused(format_args!("it lacks {lacks:?} (IDR0 {idr0:#x}, IDR1 {idr1:#x}, IDR5 {idr5:#x})"));
            return None;
        }
    };
    // Every table and leaf below is in memory this bounds, so none of
    // `toyos_smmu`'s answers past the unit's output size can come back.
    let top = pmm::top();
    let last_page = Phys::new((top - 1) & !(crate::mm::PAGE_SIZE - 1));
    if last_page.and_then(|page| toyos_smmu::table::next(page, &unit)).is_none() {
        refused(format_args!("memory reaches {top:#x}, past what it outputs (IDR5 {idr5:#x})"));
        return None;
    }
    let Some(event) = event else {
        refused(format_args!("the IORT gives its event queue no wired interrupt"));
        return None;
    };
    let highest = routes.iter().map(|(_, stream)| *stream).max().unwrap_or(0);
    let log2 = u32::BITS - highest.leading_zeros();
    if log2 > STREAMS_LOG2_MAX || log2 > u32::from(unit.stream_bits) {
        refused(format_args!("StreamID {highest:#x} needs a table of 2^{log2} entries"));
        return None;
    }
    if let Err(limit) = super::irqchip::route_iommu_events(event.get()) {
        refused(format_args!("its event interrupt {event} is no SPI the distributor takes below {limit}"));
        return None;
    }

    let mut memory = Memory::new();
    let table = memory.alloc(64 << log2);
    let streams = window(table, 64 << log2);
    for stream in 0..1u64 << log2 {
        for (i, word) in Ste::ABORT.words().iter().enumerate() {
            streams.write_u64(stream * 64 + 8 * i as u64, *word);
        }
    }
    let (commands_log2, events_log2) =
        (unit.command_queue_log2.min(COMMANDS_LOG2), unit.event_queue_log2.min(EVENTS_LOG2));
    let commands_at = memory.alloc(16 << commands_log2);
    let events_at = memory.alloc(32 << events_log2);
    let (Some((table_base, table_cfg)), Some((commands_base, commands)), Some((events_base, events))) = (
        Phys::new(table).and_then(|at| unit.stream_table(at, log2 as u8)),
        Phys::new(commands_at).and_then(|at| unit.command_queue(at, commands_log2)),
        Phys::new(events_at).and_then(|at| unit.event_queue(at, events_log2)),
    ) else {
        unreachable!("SMMU: each is aligned to its size, inside the unit's sizes, and below its output size");
    };

    regs.write(reg::CR1, reg::CR1_WRITE_BACK);
    regs.write(reg::CR2, reg::CR2_RECORD_PRIVATE);
    regs.write64(reg::STRTAB_BASE, table_base);
    regs.write(reg::STRTAB_BASE_CFG, table_cfg);
    regs.write64(reg::CMDQ_BASE, commands_base);
    regs.write(reg::CMDQ_PROD, 0);
    regs.write(reg::CMDQ_CONS, 0);
    regs.control(reg::CR0, reg::CR0_CMDQEN, "its command queue enabled");
    let mut live = Live {
        regs,
        unit,
        commands,
        queue: window(commands_at, 16 << commands_log2),
        prod: 0,
        streams,
        memory,
        routes,
        domains: Vec::new(),
        reserved,
    };
    live.issue(&[Command::ForgetAll, Command::InvalidateAll]);

    regs.write64(reg::EVENTQ_BASE, events_base);
    regs.write(reg::EVENTQ_PROD, 0);
    regs.write(reg::EVENTQ_CONS, 0);
    fault::arm(regs, window(events_at, 32 << events_log2), events, devices, &live.routes);
    regs.control(reg::CR0, reg::CR0_CMDQEN | reg::CR0_EVENTQEN, "its event queue enabled");
    regs.control(reg::IRQ_CTRL, reg::IRQ_EVENTQ, "its event interrupt enabled");
    regs.control(reg::CR0, reg::CR0_CMDQEN | reg::CR0_EVENTQEN | reg::CR0_SMMUEN, "SMMUEN");

    log!(
        "IOMMU: SMMUv3 at {base:#x} armed, CR0ACK {:#x}: {} functions' streams in a table of {}, every entry \
         aborting; a CMD_SYNC consumed; events on SPI {event}",
        regs.read(reg::CR0ACK),
        live.routes.len(),
        1u32 << log2,
    );
    Some(live)
}

pub mod interrupt {
    use crate::iommu::{Refused, StreamId};

    pub struct Msi {
        pub address: u32,
        pub data: u32,
    }

    pub struct Pin {
        pub low: u32,
        pub high: u32,
    }

    /// The SMMUv3 remaps no interrupt: a message is an ITS's to translate,
    /// and nothing here drives one.
    pub fn is_armed() -> bool {
        false
    }

    pub fn msi(_source: StreamId, _vector: u8, _dest: u32) -> Result<Msi, Refused> {
        unreachable!("no interrupt remapping on this machine, and `is_armed` said so")
    }

    pub fn claim(_slot: usize, _source: StreamId, _vector: u8) -> Msi {
        unreachable!("no interrupt remapping on this machine, and `is_armed` said so")
    }

    pub fn release(_slot: usize, _source: StreamId) {
        unreachable!("no interrupt remapping on this machine, and `is_armed` said so")
    }

    pub fn pin(_apic_id: u8, _vector: u8, _dest: u32, _level: bool) -> Result<Pin, Refused> {
        unreachable!("no interrupt remapping on this machine, and `is_armed` said so")
    }
}
