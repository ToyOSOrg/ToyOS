//! The GICv3 Interrupt Translation Service (IHI 0069H.b chapter 5), driven
//! with `toyos_its`'s encodings: what turns a claimed function's message into
//! that function's own LPI, and into no other.
//!
//! **A message is translated by the DeviceID of the function that wrote it**,
//! which the bus says and the function cannot (§5.1, §5.2.1): a claim's
//! function is mapped to a translation table of its own (`MAPD`) whose one
//! event is its function's LPI (`MAPTI`), so whatever a function writes to the
//! doorbell, it raises its own LPI or nothing. A DeviceID no claim holds is
//! mapped to no table, and its writes are dropped (§5.3.10).
//!
//! **`pcidev` function `n`'s LPI is INTID 8192 + `n`**, so the LPIs bound the
//! functions a claim can be given one to, and the handler finds the function
//! by arithmetic.
//!
//! **Every LPI is collection 0's, on the boot CPU's redistributor**, as every
//! claimed function's interrupt on x86-64 is cpu0's.
//!
//! **What the ITS and the redistributor read is handed over before it is
//! named:** each table is written whole through [`Mmio`], whose write orders
//! it before the register write or command that names it, and the
//! configuration of every LPI is written, enabled, before the redistributor's
//! `EnableLPIs`, so no `INV` is owed (§5.1.1).
//!
//! An ITS this kernel cannot drive is left disabled, and every claim is
//! refused on its account ([`is_armed`]); a refusal found once the
//! redistributor's `EnableLPIs` is set, a command the ITS refuses, or one it
//! does not consume within [`TIMEOUT_PER_SECOND`]'s bound, is a kernel
//! defect, and panics.
//!
//! **A claim's DeviceID is the one the SMMUv3's own routes carry**
//! (`super::super::smmu::its_device`): a function the unit would not put on a
//! domain is refused before its claim reaches the unit.

use alloc::vec::Vec;

use toyos_its::command::Command;
use toyos_its::lpi::{self, Layout, Lpi};
use toyos_its::{Collection, CommandQueue, EventBits, Its, Table, Target};
use toyos_phys::Phys;

use crate::iommu::StreamId;
use crate::log;
use crate::mm::pmm::{self, PhysPage};
use crate::mm::policy::MmioPolicy;
use crate::mm::{DirectMap, Mmio, PAGE_2M};
use crate::sync::Lock;

/// A command is consumed within a second, and a register settles in one:
/// `settles` takes the bound as a rate.
const TIMEOUT_PER_SECOND: u64 = 1;

/// The ITS's control frame; the translation frame is the next 64 KiB.
const FRAME: u64 = 0x1_0000;

/// `GITS_CTLR.Quiescent` [31] (§12.19.4): the ITS has finished with every
/// table and may be reprogrammed.
const CTLR_QUIESCENT: u32 = 1 << 31;

/// Offsets in a redistributor's `RD_base` frame (§12.10).
const GICR_CTLR: u64 = 0x0000;
const GICR_TYPER: u64 = 0x0008;

/// INTID bits the LPI tables are laid out for: the least that names an LPI
/// (§5.1.1), 8192 of them.
const ID_BITS: u8 = 14;

/// The widest DeviceID a claim is given: a PCI requester ID's, which is what
/// the IORT maps each function from. The device table holds no more.
const DEVICE_BITS: u8 = 16;

/// One event per claim, so the least table there is: two events.
const EVENT_BITS: u8 = 1;

/// A claim's one event: what its message carries as data.
const EVENT: u32 = 0;

/// Every LPI a claim raises runs at the one priority every SGI and PPI does
/// (`super::PRIORITY`).
const PRIORITY: u8 = super::PRIORITY;

/// `GITS_BASER<n>`'s `Type` [58:56] and `Entry_Size` [52:48], which the ITS
/// states and a write leaves as they are (§12.19.1).
const BASER_READ_ONLY: u64 = 0b111 << 56 | 0x1F << 48;

/// The translation table entries one claim's function writes are read
/// through: `MAPD`'s `ITT_addr` is 256-byte aligned (§5.3.10).
const ITT_ALIGN: u64 = 256;

static ITS: Lock<Option<Live>> = Lock::new(None);

/// The ITS this kernel drives, armed.
struct Live {
    regs: Mmio,
    /// The MADT's id for it, which the IORT names it by.
    id: u32,
    /// `GITS_TRANSLATER`'s physical address: every claim's message address.
    doorbell: u32,
    queue: Mmio,
    commands: CommandQueue,
    /// `GITS_CWRITER` as last written.
    cwriter: u64,
    /// The boot CPU's redistributor, as this ITS names it.
    target: Target,
    collection: Collection,
    events: EventBits,
    space: lpi::Space,
    /// Each DeviceID's translation table, made on its first claim and kept.
    itts: Vec<(u32, Phys<8>)>,
    itt_bytes: u64,
    /// The DeviceIDs a claim has mapped.
    held: Vec<u32>,
    /// What every table is in: never given back, since the ITS and the
    /// redistributor may read any of it for the machine's life.
    memory: Memory,
}

impl Live {
    /// `device`'s translation table, made on its first claim: `None` where
    /// there is no memory for one.
    fn table_of(&mut self, device: u32) -> Option<Phys<8>> {
        if let Some((_, table)) = self.itts.iter().find(|(held, _)| *held == device) {
            return Some(*table);
        }
        let at = self.memory.alloc(self.itt_bytes, ITT_ALIGN)?;
        let table = Phys::<8>::new(at).expect("256-byte aligned below 2^48");
        self.itts.push((device, table));
        Some(table)
    }

    /// `commands`, then a `SYNC` to the boot CPU's redistributor, consumed:
    /// every one has taken effect when this returns (§5.3.15).
    fn issue(&mut self, commands: &[Command]) {
        let (regs, queue) = (self.regs, self.commands);
        for command in commands.iter().copied().chain([Command::Sync(self.target)]) {
            let cwriter = self.cwriter;
            super::settles(TIMEOUT_PER_SECOND, "ITS: room in its command queue", || {
                !queue.is_full(cwriter, regs.read_u64(toyos_its::GITS_CREADR as u64))
            });
            let at = u64::from(queue.offset(cwriter));
            for (i, word) in command.words().iter().enumerate() {
                self.queue.write_u64(at + 8 * i as u64, *word);
            }
            self.cwriter = queue.after(cwriter);
        }
        let cwriter = self.cwriter;
        regs.write_u64(toyos_its::GITS_CWRITER as u64, cwriter);
        super::settles(TIMEOUT_PER_SECOND, "ITS: its commands consumed", || {
            let creadr = regs.read_u64(toyos_its::GITS_CREADR as u64);
            assert!(
                creadr & toyos_its::CREADR_STALLED == 0,
                "ITS: stalled on the command at {:#x} of {commands:?}",
                queue.offset(creadr)
            );
            queue.is_empty(cwriter, creadr)
        });
    }
}

/// Bump-allocated zeroed memory, aligned as asked, out of 2 MiB pages.
struct Memory {
    pages: Vec<PhysPage>,
    used: u64,
}

impl Memory {
    /// `None` where the allocator has no page left.
    fn alloc(&mut self, bytes: u64, align: u64) -> Option<u64> {
        assert!(align.is_power_of_two() && bytes <= PAGE_2M, "ITS: {bytes:#x} bytes aligned to {align:#x}");
        let mut at = self.used.next_multiple_of(align);
        if self.pages.is_empty() || at + bytes > PAGE_2M {
            // Zeroed by the allocator.
            self.pages.push(pmm::alloc_page()?);
            at = 0;
        }
        self.used = at + bytes;
        Some(self.pages.last().expect("a page was just pushed").direct_map().phys() + at)
    }
}

/// `bytes` of [`Memory`] at `phys`.
fn window(phys: u64, bytes: u64) -> Mmio {
    // SAFETY: every caller names memory `Memory::alloc` handed out, which
    // `Live` keeps for the machine's life and the direct map covers.
    unsafe { Mmio::over_phys(DirectMap::from_phys(phys), bytes) }
}

/// The `pcidev` function LPI `intid` is raised for, for the interrupt's
/// handler: `None` for an INTID that is no LPI.
pub(in crate::arch::aarch64) fn function_of(intid: u32) -> Option<usize> {
    let function = intid.checked_sub(lpi::FIRST)?;
    (function < (1 << ID_BITS) - lpi::FIRST).then_some(function as usize)
}

/// Bring the one ITS the MADT names up, on the boot CPU's redistributor at
/// `frame`, once that CPU's GIC is, from the `(id, base)` of every ITS the
/// MADT names: every refusal is logged and leaves no claim a message.
pub(super) fn init(named: &[(u32, u64)], frame: u64) {
    let (id, base) = match *named {
        [] => {
            log!("ITS: the MADT names none, so no claimed function is given an interrupt");
            return;
        }
        [one] => one,
        _ => {
            log!("ITS: the MADT names {}, and this kernel drives one: no claimed function is given an interrupt", named.len());
            return;
        }
    };
    match bring_up(id, base, frame) {
        Ok(live) => {
            log!(
                "ITS: {id} at {base:#x} armed: doorbell {:#x}, collection 0 on the boot CPU's redistributor at {frame:#x}, \
                 every LPI enabled",
                live.doorbell,
            );
            *ITS.lock() = Some(live);
        }
        Err(why) => log!("ITS: {id} at {base:#x} is left disabled, so no claimed function is given an interrupt: {why}"),
    }
}

fn bring_up(id: u32, base: u64, frame: u64) -> Result<Live, alloc::string::String> {
    use alloc::format;
    let doorbell = base + FRAME + toyos_its::GITS_TRANSLATER;
    let doorbell = u32::try_from(doorbell)
        .map_err(|_| format!("GITS_TRANSLATER at {doorbell:#x} is past the 32 bits a message address this kernel writes holds"))?;
    let regs = crate::mm::paging::map_mmio(base, FRAME, MmioPolicy::Uncacheable);
    let typer = regs.read_u64(toyos_its::GITS_TYPER as u64);
    let its: Its = toyos_its::probe(typer).map_err(|lacks| format!("it lacks {lacks:?} (GITS_TYPER {typer:#x})"))?;
    let events = its.events(EVENT_BITS).ok_or_else(|| format!("it has no EventID (GITS_TYPER {typer:#x})"))?;
    if its.device_bits < DEVICE_BITS {
        return Err(format!("its DeviceIDs are {} bits, short of a requester ID's {DEVICE_BITS}", its.device_bits));
    }

    // Disabled and quiescent before any table is named (§12.19.4).
    let ctlr = regs.read_u32(toyos_its::GITS_CTLR as u64);
    if ctlr & toyos_its::CTLR_ENABLED != 0 {
        regs.write_u32(toyos_its::GITS_CTLR as u64, ctlr & !toyos_its::CTLR_ENABLED);
    }
    super::settles(TIMEOUT_PER_SECOND, "ITS: GITS_CTLR.Quiescent", || {
        regs.read_u32(toyos_its::GITS_CTLR as u64) & CTLR_QUIESCENT != 0
    });

    // The redistributor first: an ITS whose LPIs no redistributor takes
    // would translate into nothing.
    let rd = Mmio::new(DirectMap::from_phys(frame), FRAME);
    let rd_typer = rd.read_u64(GICR_TYPER);
    if rd_typer & lpi::TYPER_PLPIS == 0 {
        return Err(format!("the boot CPU's redistributor takes no physical LPI (GICR_TYPER {rd_typer:#x})"));
    }
    if rd.read_u32(GICR_CTLR) & lpi::CTLR_ENABLE_LPIS != 0 {
        return Err("the boot CPU's redistributor was handed over with its LPIs enabled, whose tables nothing may move".into());
    }
    let gicd_typer = super::distributor_typer();
    let layout = Layout::new(gicd_typer, ID_BITS)
        .ok_or_else(|| format!("the distributor takes no LPI of {ID_BITS} bits (GICD_TYPER {gicd_typer:#x})"))?;

    let mut memory = Memory { pages: Vec::new(), used: 0 };
    let mut alloc = |bytes, align| memory.alloc(bytes, align).ok_or("there is no memory for its tables");
    let configuration = alloc(layout.configuration_bytes(), 1 << 12)?;
    let pending = alloc(layout.pending_bytes(), 1 << 16)?;
    let queue_at = alloc(4096, 1 << 16)?;
    let commands = CommandQueue::new(1).expect("one page is a queue");

    // Each `GITS_BASER<n>` that asks for a table this kernel needs is given
    // one; the others back nothing (§12.19.1).
    let mut collections_in_table = 0;
    for n in 0..8 {
        let at = toyos_its::gits_baser(n).expect("eight registers") as u64;
        let backing = toyos_its::table(regs.read_u64(at));
        let entries = match backing.table {
            Table::Unimplemented | Table::Other(_) => continue,
            Table::Devices => 1u64 << DEVICE_BITS,
            // One collection: the boot CPU's.
            Table::Collections => 1,
        };
        let pages = backing
            .pages(entries)
            .ok_or_else(|| format!("GITS_BASER{n} cannot hold {entries} entries in a flat table"))?;
        let bytes = u64::from(pages) * backing.page.bytes();
        if bytes > PAGE_2M {
            return Err(format!("GITS_BASER{n}'s table of {entries} entries is {bytes:#x} bytes, past a page"));
        }
        let table = Phys::<16>::new(alloc(bytes, 1 << 16)?).expect("a 64 KiB-aligned page below 2^48");
        let value = backing.baser(table, pages).expect("pages inside the field");
        regs.write_u64(at, value);
        let read = regs.read_u64(at);
        if read & !BASER_READ_ONLY != value {
            return Err(format!("GITS_BASER{n} reads {read:#x} after {value:#x} was written"));
        }
        if backing.table == Table::Collections {
            collections_in_table = backing.entries(pages);
        }
    }
    let collection = its
        .collections(collections_in_table)
        .collection(0)
        .ok_or_else(|| format!("it holds no collection, and no GITS_BASER backs a table of them (GITS_TYPER {typer:#x})"))?;

    // Each base register is read back whole: one that reduced the
    // shareability or cacheability asked for would read a table this kernel
    // writes through the cacheable direct map with no maintenance.
    let queue_phys = Phys::<16>::new(queue_at).expect("a 64 KiB-aligned page below 2^48");
    let cbaser = commands.cbaser(queue_phys);
    regs.write_u64(toyos_its::GITS_CBASER as u64, cbaser);
    let read = regs.read_u64(toyos_its::GITS_CBASER as u64);
    if read != cbaser {
        return Err(format!("GITS_CBASER reads {read:#x} after {cbaser:#x} was written"));
    }
    regs.write_u64(toyos_its::GITS_CWRITER as u64, 0);

    let propbaser = layout.propbaser(Phys::new(configuration).expect("a 4 KiB-aligned page below 2^48"));
    rd.write_u64(lpi::GICR_PROPBASER as u64, propbaser);
    let read = rd.read_u64(lpi::GICR_PROPBASER as u64);
    let space = layout
        .space(read)
        .filter(|_| read == propbaser)
        .ok_or_else(|| format!("GICR_PROPBASER reads {read:#x} after {propbaser:#x} was written"))?;
    // Every LPI enabled, before the redistributor reads the table: only a
    // claim's mapped event raises one.
    let config = window(configuration, layout.configuration_bytes());
    for lpi in (lpi::FIRST..).map_while(|intid| space.lpi(intid)) {
        config.write_u8(lpi.configuration_index() as u64, lpi::configuration(PRIORITY, true));
    }
    let pendbaser = lpi::pendbaser(Phys::new(pending).expect("a 64 KiB-aligned page below 2^48"));
    rd.write_u64(lpi::GICR_PENDBASER as u64, pendbaser);
    let read = rd.read_u64(lpi::GICR_PENDBASER as u64);
    if read & !lpi::PENDBASER_PTZ != pendbaser & !lpi::PENDBASER_PTZ {
        return Err(format!("GICR_PENDBASER reads {read:#x} after {pendbaser:#x} was written"));
    }

    // From `EnableLPIs` on, the redistributor may read and write both tables
    // for the machine's life, and whether it can be cleared again is
    // IMPLEMENTATION DEFINED (`GICR_CTLR.EnableLPIs`): no refusal past here
    // can give their pages back, so each is a panic.
    rd.write_u32(GICR_CTLR, rd.read_u32(GICR_CTLR) | lpi::CTLR_ENABLE_LPIS);
    assert!(
        rd.read_u32(GICR_CTLR) & lpi::CTLR_ENABLE_LPIS != 0,
        "ITS: the boot CPU's redistributor reads EnableLPIs clear after it was set"
    );
    regs.write_u32(toyos_its::GITS_CTLR as u64, toyos_its::CTLR_ENABLED);
    assert!(
        regs.read_u32(toyos_its::GITS_CTLR as u64) & toyos_its::CTLR_ENABLED != 0,
        "ITS: GITS_CTLR reads Enabled clear after it was set"
    );

    let target = its.target(lpi::processor_number(rd_typer), Phys::new(frame).expect("a redistributor frame is 64 KiB aligned"));
    let mut live = Live {
        regs,
        id,
        doorbell,
        queue: window(queue_at, 4096),
        commands,
        cwriter: 0,
        target,
        collection,
        events,
        space,
        itts: Vec::new(),
        itt_bytes: its.itt_bytes(events).next_multiple_of(ITT_ALIGN),
        held: Vec::new(),
        memory,
    };
    live.issue(&[Command::MapCollection { collection, target }]);
    Ok(live)
}

/// The 4 KiB page `GITS_TRANSLATER` is in, where an ITS is armed: what every
/// claimed function's domain maps for its messages to reach it.
pub(in crate::arch::aarch64) fn doorbell_page() -> Option<u64> {
    ITS.lock().as_ref().map(|live| u64::from(live.doorbell) & !(crate::mm::PAGE_SIZE - 1))
}

/// Whether an ITS is armed to give a claimed function its own LPI.
/// Whether a message a holder writes into its function's MSI-X table reaches
/// nothing but its claim's interrupt: always, since the ITS translates by the
/// device id the bridge stamps, through that device's own table.
pub fn fences_tables() -> bool {
    true
}

pub fn is_armed() -> bool {
    ITS.lock().is_some()
}

pub struct Msi {
    pub address: u32,
    pub data: u32,
}

/// Why a source could not be given a message.
#[derive(Clone, Copy)]
pub enum Refused {
    /// It is a driver in this kernel's.
    KernelDriver,
    /// The SMMUv3 translates it to no DeviceID at this ITS.
    NoDeviceId,
    /// Its DeviceID is past the device table's.
    DeviceIdTooWide(u32),
    /// Its DeviceID is another claim's: one table translates both, so each
    /// could raise the other's interrupt.
    DeviceIdHeld(u32),
    /// Its `pcidev` function number names no LPI the redistributor has.
    NoLpi(usize),
    /// There is no memory for its translation table.
    NoMemory,
}

impl Refused {
    /// Whether this machine has run out of something a claim spends, rather
    /// than this function being one no claim can be given.
    pub fn exhausted(self) -> bool {
        matches!(self, Self::NoLpi(_) | Self::NoMemory)
    }
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::KernelDriver => write!(
                f,
                "a driver in this kernel is given no LPI: the ITS maps only a claimed function's \
                 DeviceID, and such a driver has no address space for its DMA"
            ),
            Self::NoDeviceId => write!(
                f,
                "the IORT gives it no DeviceID at this kernel's ITS behind the SMMUv3, so no \
                 table of its own would translate its message and no domain of its own would \
                 confine its writes"
            ),
            Self::DeviceIdTooWide(id) => write!(f, "its DeviceID {id:#x} is past the ITS's device table"),
            Self::DeviceIdHeld(id) => write!(
                f,
                "the IORT gives it DeviceID {id:#x}, which another claimed function's messages \
                 are already translated by"
            ),
            Self::NoLpi(function) => write!(
                f,
                "function {function}'s LPI {} is past the redistributor's LPIs",
                lpi::FIRST as usize + function
            ),
            Self::NoMemory => write!(f, "there is no memory for its translation table"),
        }
    }
}

/// A driver in this kernel is given no LPI: only a claimed function is
/// mapped, and a driver in this kernel on this machine has no address space
/// to do DMA in (`super::super::iommu_unit`).
pub fn msi(_source: StreamId, _irq: crate::arch::DriverIrq) -> Result<Msi, Refused> {
    Err(Refused::KernelDriver)
}

/// The DeviceID `routed` names at this ITS: what the SMMUv3's own routes
/// map the function's stream on to, so a function the unit would not put on
/// a domain is refused here, before its claim reaches `attach`.
fn device_of(live: &Live, routed: Option<toyos_acpi::ItsDevice>) -> Result<u32, Refused> {
    match routed {
        Some(device) if device.its == live.id && device.device >> DEVICE_BITS == 0 => Ok(device.device),
        Some(device) if device.its == live.id => Err(Refused::DeviceIdTooWide(device.device)),
        _ => Err(Refused::NoDeviceId),
    }
}

/// A claimed function's DeviceID and the LPI its message raises, held until
/// [`release`].
pub struct Claimed {
    pub msi: Msi,
    device: u32,
    lpi: Lpi,
}

impl Claimed {
    /// What the message raises, for the record.
    pub fn interrupt(&self) -> impl core::fmt::Display {
        alloc::format!("LPI {}", self.lpi.intid())
    }
}

/// Map `source`'s DeviceID to its translation table, its one event to `pcidev`
/// function `function`'s LPI, and answer the message that raises it.
pub fn claim(function: usize, source: StreamId) -> Result<Claimed, Refused> {
    // Before this lock, so it never nests with the unit's.
    let routed = super::super::smmu::its_device(source);
    let mut held = ITS.lock();
    let live = held.as_mut().unwrap_or_else(|| panic!("ITS: {source} claimed with no ITS armed"));
    let device = device_of(live, routed)?;
    if live.held.contains(&device) {
        return Err(Refused::DeviceIdHeld(device));
    }
    let lpi = u32::try_from(function)
        .ok()
        .and_then(|function| lpi::FIRST.checked_add(function))
        .and_then(|intid| live.space.lpi(intid))
        .ok_or(Refused::NoLpi(function))?;
    let table = live.table_of(device).ok_or(Refused::NoMemory)?;
    let event = live.events.event(EVENT).expect("event 0 is in every table");
    let (collection, events) = (live.collection, live.events);
    live.issue(&[
        Command::MapDevice { device, table, events },
        Command::MapEvent { device, event, lpi, collection },
    ]);
    live.held.push(device);
    log!("ITS: {source}, DeviceID {device:#x}, event {EVENT} is LPI {}", lpi.intid());
    Ok(Claimed { msi: Msi { address: live.doorbell, data: EVENT }, device, lpi })
}

/// A claim's DeviceID unmapped, and anything it left pending dropped, before
/// this returns.
pub fn release(claimed: &Claimed, source: StreamId) {
    let device = claimed.device;
    let mut held = ITS.lock();
    let live = held.as_mut().unwrap_or_else(|| panic!("ITS: {source} released with no ITS armed"));
    let at = live.held.iter().position(|held| *held == device);
    live.held.swap_remove(at.unwrap_or_else(|| panic!("ITS: DeviceID {device:#x} released unmapped")));
    let event = live.events.event(EVENT).expect("event 0 is in every table");
    live.issue(&[Command::Discard { device, event }, Command::UnmapDevice { device }]);
    log!("ITS: {source}, DeviceID {device:#x}, unmapped from LPI {}", claimed.lpi.intid());
}
