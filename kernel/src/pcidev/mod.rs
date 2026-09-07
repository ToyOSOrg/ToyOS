//! One PCI function, driven by a process instead of by this kernel.
//!
//! The line through the device is **who can name an address**. This module
//! keeps config space — there is no write path to it from userland — puts the
//! function in an address space of its own at the unit *before* it enables bus
//! mastering, programs the interrupt vector into its MSI-X table, and hands out
//! every device address a descriptor may carry. Nothing the holder writes into
//! a descriptor can make the device touch memory the kernel did not grant it:
//! the domain maps the grants and nothing else, and an address outside them is
//! refused at the unit and recorded against that claim.
//!
//! **A window is 2 MiB because that is the only page this kernel maps.** A BAR
//! a process may see is re-assigned onto a 2 MiB boundary above everything
//! firmware described; [`place_bar`] is the mechanism and [`alone_in_its_page`]
//! is the assertion that it worked, never the other way round.
//!
//! **The BAR holding the MSI-X table or PBA is never mapped**: a holder that
//! could rewrite the table could point the device's message at any address the
//! LAPIC decodes.
//!
//! **A function with no address space of its own is not handed over**, because
//! every grant would answer with a physical address and a descriptor holding
//! one is an arbitrary read and write over all of memory.
//!
//! **A function masters the bus only once it has memory it may reach.** What
//! comes back from a process still holds the device addresses of a domain that
//! no longer maps them, so bus mastering is not started at hand-over: it starts
//! on the claim's first grant, after that grant is in the function's domain.
//! Between the two the function can issue no transaction at all, whatever its
//! registers still say. `release` also asks the function for a reset where it
//! advertises one (PCIe §6.6.2), which no device in reach does — so the order
//! above is the mechanism and the reset is the belt.
//!
//! Nothing here is specific to what a function *is*.

/// No `crate::` reference, so `kernel-loom` compiles it and models the edge
/// x86's TSO hides.
mod record;

use alloc::sync::Arc;
use alloc::vec::Vec;

use record::Interrupt;
use toyos_abi::boot::MemoryMapEntry;
use toyos_abi::pci::{DeviceIrqRecord, PciFunctionInfo, BARS};
use toyos_abi::syscall::{PciId, RegWidth, SyscallError};
use toyos_dma::Register;
use toyos_pci::{bar, express, msix};

use crate::device::{Claim, ClaimError};
use crate::drivers::pci::PciDevice;
use crate::inbox::InboxId;
use crate::iommu::{DeviceSpace, IommuError};
use crate::mm::paging::{CachePolicy, MmioPolicy};
use crate::mm::{align_2m, DirectMap, Mmio, PAGE_2M};
use crate::object::shm::{Region, SharedMemObject};
use crate::sync::Lock;

/// How many functions this machine can hand out at once.
///
/// A small fixed number because each one costs an interrupt vector, and a
/// vector in `arch::idt` is declared rather than allocated.
pub const MAX_FUNCTIONS: usize = 4;

/// The vectors those functions' messages carry, one per slot. Declared here and
/// in `arch::idt`'s table; `VECTORS.len() == MAX_FUNCTIONS` is what keeps the
/// two from disagreeing.
pub const VECTORS: [u8; MAX_FUNCTIONS] = [0x28, 0x29, 0x2A, 0x2B];

/// The most one grant may be. A driver's rings and buffers are kilobytes to a
/// megabyte; this is room for a queue depth nothing in reach uses, and a bound
/// so a refused-but-plausible request is a refusal rather than the machine's
/// memory.
const MAX_GRANT_BYTES: u64 = 8 * 1024 * 1024;

/// And the most one claim may hold across every grant.
const MAX_GRANT_TOTAL: u64 = 32 * 1024 * 1024;

/// Where the fixed platform devices start on a PC: the I/O APIC, the HPET and
/// the LAPIC window are at and above this, so a 32-bit window may not reach it.
const PLATFORM_MMIO: u64 = 0xFEC0_0000;

/// How much address space the windows may take, above what firmware assigned:
/// every BAR of every function this machine can hand out, at one 2 MiB page
/// each.
const WINDOW_SPAN: u64 = (MAX_FUNCTIONS * BARS) as u64 * PAGE_2M;

/// A function's own configuration space, which is all a claim may read of it
/// (PCIe base spec §7.2.2: 4 KiB per function under ECAM).
const CONFIG_BYTES: u64 = 4096;

static IRQ: [Interrupt; MAX_FUNCTIONS] = [const { Interrupt::new() }; MAX_FUNCTIONS];

/// One address space per slot, made on that slot's first claim and kept.
///
/// Kept because a domain id is never given back (`iommu/vtd/domain.rs`), so a
/// domain per claim would let a process spawn and die its way through every id
/// the units report. [`release`] empties it, so the next holder attaches to one
/// that maps nothing.
static SPACE: [Lock<Option<DeviceSpace>>; MAX_FUNCTIONS] =
    [const { Lock::new(None) }; MAX_FUNCTIONS];

/// One granted buffer: the memory, and where the device reaches it.
struct Grant {
    /// Held so the pages outlive every handle the process had: they are freed
    /// by [`release`], after bus mastering is off and the domain has given the
    /// address back.
    #[expect(dead_code, reason = "the Arc is what keeps the pages alive past the process")]
    memory: Arc<SharedMemObject>,
    at: u64,
    bytes: u64,
}

/// What a live slot drives. The ISR never reads this.
struct Bound {
    pci: PciDevice,
    space: DeviceSpace,
    /// This function's one MSI-X table entry, mapped for the kernel alone.
    entry: Mmio,
    id: PciId,
    /// Where each mappable BAR was put, and how much of it the function
    /// advertises; 0 bytes is a slot with no BAR this claim may map.
    bar_at: [u64; BARS],
    bar_bytes: [u64; BARS],
    /// Minted on the first map of that BAR, so a second answers the same
    /// object rather than a second handle to one window.
    bars: [Option<Arc<SharedMemObject>>; BARS],
    grants: Vec<Grant>,
    /// Whether this function may issue a transaction yet. False until its first
    /// grant is in its domain, so a function carrying a previous holder's queue
    /// addresses can act on none of them.
    mastering: bool,
}

static BOUND: [Lock<Option<Bound>>; MAX_FUNCTIONS] =
    [const { Lock::new(None) }; MAX_FUNCTIONS];

static WATCHERS: [Lock<Vec<InboxId>>; MAX_FUNCTIONS] =
    [const { Lock::new(Vec::new()) }; MAX_FUNCTIONS];

/// Every function this machine enumerated, and the two windows a BAR may be
/// moved into.
struct Machine {
    functions: Vec<PciDevice>,
    /// Every memory BAR every function decodes, as `(requester, base, end)`.
    ///
    /// Recorded in [`publish`] and never re-derived: reading a BAR's *size*
    /// means writing all-ones into it and reading back, with memory decode off
    /// for the length of the probe — safe before any driver `init` and nothing
    /// to do to a live function while a claim is being minted.
    decoded: Vec<(u16, u64, u64)>,
    /// Requester ids the kernel's own drivers bound. A claim on one of them
    /// would be two drivers on one device.
    kernel_driven: Vec<u16>,
    /// Next free 2 MiB boundary, and the first address past the window, for a
    /// 32-bit BAR and for a 64-bit one. They are separate because a 32-bit BAR
    /// cannot hold an address a 64-bit one can.
    narrow: (u64, u64),
    wide: (u64, u64),
    /// Functions this module has reset, and when each may be touched again
    /// (PCIe §6.6.2). One entry per function ever released, so a re-claim
    /// cannot start reading a register the reset has not finished with.
    resetting: Vec<(u16, u64)>,
}

static MACHINE: Lock<Machine> = Lock::new(Machine {
    functions: Vec::new(),
    decoded: Vec::new(),
    kernel_driven: Vec::new(),
    narrow: (0, 0),
    wide: (0, 0),
    resetting: Vec::new(),
});

fn requester(pci: &PciDevice) -> u16 {
    ((pci.bus as u16) << 8) | ((pci.dev as u16) << 3) | pci.func as u16
}

/// Record that a kernel driver has taken this function.
///
/// Called from [`PciDevice::enable_bus_master`], which is what every kernel
/// driver that masters the bus does and nothing else does: the set that matters
/// is exactly the set that can reach memory, so this needs no list to be kept
/// in step by hand.
pub fn note_kernel_driver(pci: &PciDevice) {
    let mut machine = MACHINE.lock();
    let who = requester(pci);
    if !machine.kernel_driven.contains(&who) {
        machine.kernel_driven.push(who);
    }
}

/// Take the enumeration, and derive where a BAR a process maps may be put.
///
/// **Before any driver `init`**, because the sizing probe below takes memory
/// decode off the function it is probing for the length of the probe, and a
/// driver mid-transfer must not meet that.
///
/// **Both floors are above everything firmware described**, which is every BAR
/// it assigned *and* every entry of the memory map it handed the loader — RAM,
/// its own runtime services, the ACPI regions and the fixed platform apertures
/// alike. A floor derived from BARs alone would put a holder's 2 MiB window on
/// whatever firmware had put there instead, and the only thing that would catch
/// it is [`Refusal::Dead`], which cannot tell unrouted space from RAM.
pub fn publish(devices: &[PciDevice], maps: &[MemoryMapEntry]) {
    let mut narrow_end = 0u64;
    let mut wide_end = 0u64;
    let mut decoded = Vec::new();
    for entry in maps {
        wide_end = wide_end.max(entry.end);
        // Clamped rather than skipped: a region that starts below the platform's
        // fixed MMIO and ends above it still covers every address a 32-bit
        // window could take, and clamping is what makes that answer "no room".
        if entry.start < PLATFORM_MMIO {
            narrow_end = narrow_end.max(entry.end.min(PLATFORM_MMIO));
        }
    }
    for device in devices {
        let mut index = 0u8;
        while index <= bar::MAX_INDEX {
            let low = device.read_config_u32(bar::BASE + index as u64 * 4);
            let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
            if let Ok(memory) = device.memory_bar(index) {
                // At least one byte for a BAR that answers no size: a window
                // may not start on top of an address something decodes, and
                // an unknown length is not an empty one.
                let size = device.bar_size(index).unwrap_or(0).max(1);
                let end = memory.address().saturating_add(size);
                decoded.push((requester(device), memory.address(), end));
                if memory.address() < 1 << 32 {
                    narrow_end = narrow_end.max(end);
                } else {
                    wide_end = wide_end.max(end);
                }
            }
            // A 64-bit BAR's high half is the next register and is not a BAR:
            // decoding it would read an address out of address bits.
            index += if wide { 2 } else { 1 };
        }
    }
    let mut machine = MACHINE.lock();
    machine.functions = devices.to_vec();
    machine.decoded = decoded;
    machine.narrow = window(narrow_end, PLATFORM_MMIO);
    machine.wide = window(wide_end, u64::MAX);
    log!(
        "pcidev: {} functions; a 32-bit window comes from {:#x}..{:#x}, a 64-bit one from \
         {:#x}..{:#x}",
        devices.len(),
        machine.narrow.0,
        machine.narrow.1,
        machine.wide.0,
        machine.wide.1,
    );
}

/// The span above `assigned` this module may hand out, or an empty one where
/// there is no room under `ceiling`.
fn window(assigned: u64, ceiling: u64) -> (u64, u64) {
    if assigned == 0 {
        return (0, 0);
    }
    let base = align_2m(assigned as usize) as u64;
    let top = base.saturating_add(WINDOW_SPAN);
    if base >= ceiling || top > ceiling {
        return (0, 0);
    }
    (base, top)
}

/// Why a function could not be handed over. Carried rather than collapsed: one
/// message for all of them sends whoever reads the log looking in the wrong
/// place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Refusal {
    NoMsix,
    Untranslated(IommuError),
    NoWindow,
    BarUnsizable(u8),
    BarUnplaceable(u8),
    Dead(u64),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoMsix => write!(
                f,
                "its MSI-X could not be armed, and a claim with no interrupt is a driver \
                 that would never be told anything"
            ),
            Self::Untranslated(why) => write!(
                f,
                "it would have no address space of its own — {why} — and a process driving \
                 it would be given physical addresses to put in descriptors"
            ),
            Self::NoWindow => write!(
                f,
                "this machine has no 2 MiB-aligned address space above what firmware \
                 assigned to put a BAR in"
            ),
            Self::BarUnsizable(i) => write!(f, "BAR {i} answers no size to bound a window by"),
            Self::BarUnplaceable(i) => write!(f, "BAR {i} did not take the address it was given"),
            Self::Dead(at) => write!(
                f,
                "the window it was moved to at {at:#x} reads ones, so nothing routes it"
            ),
        }
    }
}

/// Claim `id`, bring the function up to the point a driver takes over, and
/// answer what that driver needs to know.
pub fn claim(id: PciId) -> Result<(PciFunctionInfo, u8, Claim), ClaimError> {
    let (pci, driven) = {
        let machine = MACHINE.lock();
        let mut found: Option<PciDevice> = None;
        for device in machine.functions.iter() {
            if !device.is_id(id.vendor, id.device) {
                continue;
            }
            if found.is_some() {
                // Refused by name rather than resolved to the first match: a
                // config that names a card this machine has two of is asking
                // for one of them and cannot say which.
                log!(
                    "pcidev: {:04x}:{:04x} names more than one function on this machine",
                    id.vendor,
                    id.device
                );
                return Err(ClaimError::Ambiguous);
            }
            found = Some(*device);
        }
        let pci = found.ok_or(ClaimError::Absent)?;
        (pci, machine.kernel_driven.contains(&requester(&pci)))
    };
    if driven {
        log!(
            "pcidev: PCI {:02x}:{:02x}.{} is driven by this kernel and cannot be claimed",
            pci.bus,
            pci.dev,
            pci.func
        );
        return Err(ClaimError::KernelDriven);
    }

    let slot = (0..MAX_FUNCTIONS)
        .find(|slot| BOUND[*slot].lock().is_none())
        .ok_or(ClaimError::Exhausted)?;
    // The guard exists from here on, so every refusal below frees the slot.
    let claim = Claim::pci(slot);

    match bring_up(pci, id, slot) {
        Ok(bound) => {
            let info = PciFunctionInfo {
                bar_bytes: bound.bar_bytes,
                bus: pci.bus,
                dev: pci.dev,
                func: pci.func,
                _pad: [0; 5],
            };
            *BOUND[slot].lock() = Some(bound);
            IRQ[slot].clear();
            crate::iommu::note_user_owned(pci.bus, pci.dev, pci.func, Some(slot));
            log!(
                "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] handed over on slot {slot}, \
                 vector {:#x}",
                pci.bus,
                pci.dev,
                pci.func,
                id.vendor,
                id.device,
                VECTORS[slot],
            );
            Ok((info, slot as u8, claim))
        }
        Err(why) => {
            log!(
                "pcidev: PCI {:02x}:{:02x}.{} NOT HANDED OVER — {why}",
                pci.bus,
                pci.dev,
                pci.func
            );
            Err(ClaimError::Unusable)
        }
    }
}

/// Move the BARs, arm the interrupt, give the function an address space, and
/// only then let it master the bus.
///
/// The order is the whole security argument: a function that could master the
/// bus before its domain existed would be reaching physical memory with
/// whatever addresses its registers still held.
fn bring_up(pci: PciDevice, id: PciId, slot: usize) -> Result<Bound, Refusal> {
    // Whatever is left of a reset [`release`] started on this function, before
    // a register of it is read (PCIe §6.6.2).
    settle_after_reset(&pci);
    // The table's own BAR, so it can be left where it is and kept out of what
    // the holder maps.
    let table_bar = msix_bar(&pci);
    // Decode must be on for a BAR to answer, and off across each move.
    pci.enable_memory_space();

    // **The interrupt first, before a window is spent.** A function whose
    // MSI-X cannot be armed is one no holder could ever be told anything
    // about, so it is refused here — and the 2 MiB of address space each of
    // its BARs would take stays unspent, which is what `virtio_net_no_msix`
    // reads back off the console.
    let entry = pci.enable_msix(VECTORS[slot]).ok_or(Refusal::NoMsix)?;

    let mut bar_at = [0u64; BARS];
    let mut bar_bytes = [0u64; BARS];
    let mut index = 0u8;
    while index <= bar::MAX_INDEX {
        let low = pci.read_config_u32(bar::BASE + index as u64 * 4);
        let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
        let step = if wide { 2 } else { 1 };
        if pci.memory_bar(index).is_err() || Some(index) == table_bar {
            index += step;
            continue;
        }
        let size = pci.bar_size(index).map_err(|_| Refusal::BarUnsizable(index))?;
        let at = place_bar(&pci, index, size)?;
        bar_at[index as usize] = at;
        bar_bytes[index as usize] = size;
        index += step;
    }
    if bar_bytes.iter().all(|bytes| *bytes == 0) {
        return Err(Refusal::NoWindow);
    }

    // An address space holding this function's grants and nothing else,
    // attached before it can issue a transaction of its own — and a *refusal*
    // where this machine has none to give, because the alternative is handing
    // a process physical addresses to write into descriptors.
    let space = slot_space(slot).map_err(Refusal::Untranslated)?;
    space.attach(pci.bus, pci.dev, pci.func);

    // **Bus mastering is deliberately not started here.** A function this
    // kernel handed out before may still hold the queue addresses its last
    // holder programmed, and this machine's devices advertise no reset to
    // clear them with; a function that cannot master the bus cannot act on
    // them. It starts on the first grant, which is the first moment there is
    // anything it may legally reach.
    Ok(Bound {
        pci,
        space,
        entry,
        id,
        bar_at,
        bar_bytes,
        bars: [const { None }; BARS],
        grants: Vec::new(),
        mastering: false,
    })
}

/// This slot's address space, made on its first claim.
fn slot_space(slot: usize) -> Result<DeviceSpace, IommuError> {
    let mut held = SPACE[slot].lock();
    match *held {
        Some(space) => Ok(space),
        None => {
            let space = DeviceSpace::own()?;
            *held = Some(space);
            Ok(space)
        }
    }
}

/// Start this function's own reset, where it advertises one (PCIe §7.5.3.3),
/// and answer when it may be touched again.
///
/// `None` for a function that advertises none — which every device this project
/// has in reach does, so nothing rests on this: what makes a re-claim safe is
/// that bus mastering starts on the first grant and not at hand-over.
fn reset(pci: &PciDevice) -> Option<u64> {
    let cap = pci.capabilities().find(|c| c.id() == express::CAP_ID)?;
    if !express::resets(cap.read_u32(express::DEVICE_CAPABILITIES)) {
        return None;
    }
    let control = cap.read_u16(express::DEVICE_CONTROL);
    cap.write_u16(express::DEVICE_CONTROL, express::initiate(control));
    Some(crate::clock::nanos_since_boot() + express::SETTLE_NANOS)
}

/// Wait out whatever is left of a reset this kernel started on this function.
///
/// Spent here rather than in [`release`], where the process is already dying:
/// a re-claim is the only thing that may touch the function again, and on every
/// boot in reach the deadline is long past by the time one happens.
fn settle_after_reset(pci: &PciDevice) {
    let who = requester(pci);
    let deadline = {
        let machine = MACHINE.lock();
        machine.resetting.iter().find(|(id, _)| *id == who).map(|(_, at)| *at)
    };
    let Some(deadline) = deadline else { return };
    while crate::clock::nanos_since_boot() < deadline {
        core::hint::spin_loop();
    }
}

/// Which BAR holds this function's MSI-X table or PBA, if any.
///
/// Both, because both are the unit's structures and neither is the holder's:
/// the two live in one BAR on every device in reach, and a device that split
/// them costs the second BAR too rather than publishing one of them.
fn msix_bar(pci: &PciDevice) -> Option<u8> {
    let cap = pci.capabilities().find(|c| c.id() == msix::CAP_ID)?;
    let control = cap.read_u16(msix::MESSAGE_CONTROL);
    let table = msix::Msix::decode(control, cap.read_u32(msix::TABLE)).ok()?;
    Some(table.bir())
}

/// Move BAR `index` onto a 2 MiB boundary of its own and answer where.
///
/// Memory decode is off across the write, so nothing can read through a BAR
/// that is half-programmed, and the address is read back off the register
/// rather than assumed.
fn place_bar(pci: &PciDevice, index: u8, size: u64) -> Result<u64, Refusal> {
    let offset = bar::BASE + index as u64 * 4;
    let low = pci.read_config_u32(offset);
    let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
    let span = align_2m(size as usize) as u64;
    let at = take_window(wide, span).ok_or(Refusal::NoWindow)?;
    let placed =
        bar::placement(index, low, at, size).map_err(|_| Refusal::BarUnplaceable(index))?;

    pci.set_memory_decode(false);
    pci.write_config_u32(offset, placed.low);
    if let Some(high) = placed.high {
        pci.write_config_u32(offset + 4, high);
    }
    pci.set_memory_decode(true);

    match pci.memory_bar(index) {
        Ok(memory) if memory.address() == at => {}
        _ => return Err(Refusal::BarUnplaceable(index)),
    }
    alone_in_its_page(pci, at, span);

    // The one read of the function's registers this kernel does: evidence that
    // the address the BAR was moved to is one the bridge actually routes. A
    // window nothing decodes answers ones, and handing that to a driver would
    // be handing it a device that is not there.
    let window = crate::mm::paging::map_mmio(at, span, MmioPolicy::Uncacheable);
    if window.read_u32(0) == u32::MAX {
        return Err(Refusal::Dead(at));
    }
    log!(
        "pcidev: PCI {:02x}:{:02x}.{} BAR {index} ({size:#x} bytes) moved to {at:#x}",
        pci.bus,
        pci.dev,
        pci.func
    );
    Ok(at)
}

/// The next window of `span` bytes, or `None` where this machine has no room.
///
/// Aligned to `span` and not merely to a page: a BAR's low address bits are
/// hardwired to zero, so a window wider than 2 MiB has to start on its own
/// size or the device decodes somewhere else (PCIe §7.5.1.2.1).
fn take_window(wide: bool, span: u64) -> Option<u64> {
    let mut machine = MACHINE.lock();
    let (next, top) = if wide { &mut machine.wide } else { &mut machine.narrow };
    if *next == 0 {
        return None;
    }
    let at = next.checked_next_multiple_of(span)?;
    let end = at.checked_add(span)?;
    if end > *top {
        return None;
    }
    *next = end;
    Some(at)
}

/// Nothing else this machine enumerated decodes inside the page that is about
/// to be mapped into a process.
///
/// The assertion that [`take_window`] worked, never the mechanism: the window
/// is taken above every address firmware assigned, so an overlap here is this
/// module handing out an address it did not own.
fn alone_in_its_page(claimed: &PciDevice, at: u64, span: u64) {
    let machine = MACHINE.lock();
    let mine = requester(claimed);
    for (who, address, end) in machine.decoded.iter() {
        if *who == mine {
            continue;
        }
        // The whole extent, not the base: a BAR that starts below the window
        // and reaches into it is the overlap this is for.
        assert!(
            *end <= at || *address >= at + span,
            "pcidev: the {span:#x}-byte window at {at:#x} holds requester {who:#06x}'s \
             {address:#x}..{end:#x} as well, and its holder would be given that \
             function's registers",
        );
    }
}

/// Give up a slot: the device stops mastering the bus, then loses its
/// addresses, and only then do the pages behind them go back.
///
/// The order is what makes a dying driver safe. A page freed while the function
/// could still reach it is a device writing into memory the allocator has
/// already handed to somebody else.
pub fn release(slot: usize) {
    let Some(bound) = BOUND[slot].lock().take() else { return };
    bound.pci.disable_bus_master();
    bound.entry.write_u32(msix::ENTRY_VECTOR_CONTROL, msix::ENTRY_MASKED);
    crate::iommu::note_user_owned(bound.pci.bus, bound.pci.dev, bound.pci.func, None);
    for grant in bound.grants.iter() {
        if let Err(why) = bound.space.unmap(grant.at, grant.bytes) {
            panic!("pcidev: slot {slot} could not take {:#x} back: {why}", grant.at);
        }
    }
    // After the domain is empty and bus mastering is gone, so nothing the reset
    // disturbs can reach memory: the function goes back to the state the next
    // holder's `bring_up` expects, and the deadline is recorded rather than
    // waited out here — this runs on a dying process's teardown.
    if let Some(at) = reset(&bound.pci) {
        let who = requester(&bound.pci);
        let mut machine = MACHINE.lock();
        match machine.resetting.iter_mut().find(|(id, _)| *id == who) {
            Some(entry) => entry.1 = at,
            None => machine.resetting.push((who, at)),
        }
    }
    IRQ[slot].clear();
    WATCHERS[slot].lock().clear();
    log!(
        "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] released from slot {slot}",
        bound.pci.bus,
        bound.pci.dev,
        bound.pci.func,
        bound.id.vendor,
        bound.id.device,
    );
    // Last: the grants' pages are freed by this drop, after the unmap above.
    drop(bound);
}

/// What every call a claim answers checks first.
fn with_bound<T>(
    slot: usize,
    f: impl FnOnce(&mut Bound) -> Result<T, SyscallError>,
) -> Result<T, SyscallError> {
    if IRQ[slot].faulted() {
        return Err(SyscallError::Io);
    }
    let mut guard = BOUND[slot].lock();
    let bound = guard.as_mut().ok_or(SyscallError::NotFound)?;
    f(bound)
}

/// One memory BAR as an object to map; the same object every time.
pub fn bar_object(slot: usize, index: u64) -> Result<Arc<SharedMemObject>, SyscallError> {
    with_bound(slot, |bound| {
        let index = usize::try_from(index).map_err(|_| SyscallError::InvalidArgument)?;
        if index >= BARS || bound.bar_bytes[index] == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        if let Some(object) = bound.bars[index].as_ref() {
            return Ok(Arc::clone(object));
        }
        let object = SharedMemObject::over(Region {
            phys: DirectMap::from_phys(bound.bar_at[index]),
            size: align_2m(bound.bar_bytes[index] as usize) as u64,
            // Registers, and the memory type is not firmware's to decide.
            cache: CachePolicy::Uncacheable,
            // The kernel owns no pages here: this is a device's aperture.
            pages: None,
        });
        bound.bars[index] = Some(Arc::clone(&object));
        Ok(object)
    })
}

/// Memory this function may reach, and nothing else may.
pub fn dma_alloc(
    slot: usize,
    bytes: u64,
) -> Result<(Arc<SharedMemObject>, u64, u64), SyscallError> {
    with_bound(slot, |bound| {
        if bytes == 0 || bytes > MAX_GRANT_BYTES {
            return Err(SyscallError::InvalidArgument);
        }
        let held: u64 = bound.grants.iter().map(|grant| grant.bytes).sum();
        let span = align_2m(bytes as usize) as u64;
        if held + span > MAX_GRANT_TOTAL {
            return Err(SyscallError::ResourceExhausted);
        }
        let memory = SharedMemObject::create(span)?;
        // The pages are the allocator's own and 2 MiB, so a domain that cannot
        // take them is a kernel bug rather than the device's answer.
        let at = bound
            .space
            .map(memory.phys().phys(), span)
            .unwrap_or_else(|why| panic!("pcidev: slot {slot} could not map a grant: {why}"));
        let first = bound.grants.is_empty();
        bound.grants.push(Grant { memory: Arc::clone(&memory), at, bytes: span });
        // After the mapping and never before: the first thing this function may
        // reach has to exist before it may reach anything.
        if !bound.mastering {
            bound.pci.start_bus_mastering();
            bound.mastering = true;
        }
        Ok((memory, foreign_if_armed(first, at), span))
    })
}

/// Take a grant back, for a caller that could not be given a handle to it.
///
/// **A partial success is not left behind.** Without this the grant would stay
/// mapped in the function's domain and counted against [`MAX_GRANT_TOTAL`] with
/// nothing naming it, so a caller that hit a full handle table once would be
/// refused every later grant with no way back but dying.
pub fn dma_undo(slot: usize) {
    let _ = with_bound(slot, |bound| {
        // The most recent, which is the one [`dma_alloc`] just pushed: the
        // slot's lock is what makes "just" mean it, and the address the caller
        // was *told* is not always the address the grant is at.
        let Some(grant) = bound.grants.pop() else { return Ok(()) };
        if let Err(why) = bound.space.unmap(grant.at, grant.bytes) {
            panic!("pcidev: slot {slot} could not take {:#x} back: {why}", grant.at);
        }
        Ok(())
    });
}

/// The address a grant answers with, or — for a claim's first grant, with the
/// actuator armed — another driver's pool.
///
/// The grant is real and mapped; only the address the driver is *told* is one
/// this function's domain does not have, so what the device is pointed at is a
/// wrong descriptor rather than a driver written to misbehave.
fn foreign_if_armed(first: bool, at: u64) -> u64 {
    #[cfg(feature = "boot-actuators")]
    if first && crate::actuator::iommu_userdev_foreign_dma() {
        let foreign =
            crate::drivers::nvme::FOREIGN_PROBE.load(core::sync::atomic::Ordering::Relaxed);
        if foreign != 0 {
            return foreign;
        }
    }
    #[cfg(not(feature = "boot-actuators"))]
    let _ = first;
    at
}

/// The window a claim's configuration reads are checked against, for the one
/// caller that turns an offset from userland into a [`Register`].
pub fn config_window(offset: u64, width: RegWidth) -> Result<Register, toyos_dma::RefusedRegister> {
    toyos_dma::register(offset, width.bytes(), CONFIG_BYTES)
}

/// One dword, word or byte of this function's own config space.
///
/// Read-only and there is no writing counterpart: a driver cannot find its own
/// registers without its capability chain, while every write config space takes
/// — bus mastering, the BARs, the MSI-X control word — is a decision this module
/// keeps.
///
/// **Takes the witness and not an offset.** The number came from a caller, and
/// [`Register`]'s only constructor is the check, so there is no way to reach
/// the register file here with one nobody bounded.
pub fn config_read(slot: usize, at: Register, width: RegWidth) -> Result<u32, SyscallError> {
    with_bound(slot, |bound| {
        Ok(match width {
            RegWidth::U8 => bound.pci.read_config_u8(at.offset()) as u32,
            RegWidth::U16 => bound.pci.read_config_u16(at.offset()) as u32,
            RegWidth::U32 => bound.pci.read_config_u32(at.offset()),
        })
    })
}

/// The interrupts since the last read, or `None` for none.
pub fn take_record(slot: usize) -> Option<DeviceIrqRecord> {
    IRQ[slot].take().map(|count| DeviceIrqRecord { count })
}

pub fn has_irq(slot: usize) -> bool {
    IRQ[slot].armed()
}

/// Records one message. Called from the vector's ISR, so it takes no lock and
/// allocates nothing; `record.rs` owns the counting, and `kernel-loom` models
/// it against a concurrent reader.
pub fn isr(slot: usize) {
    IRQ[slot].took();
}

/// Turn every message taken since the last pass into a wake.
///
/// On the scheduler pass rather than in the ISR, like every other device in
/// this kernel: a wake takes the inbox lock and an ISR may not.
pub fn drain_pending() {
    for (slot, irq) in IRQ.iter().enumerate() {
        if irq.take_pending() {
            crate::inbox::Source::PciFunction(slot as u8).wake();
        }
    }
}

/// The unit refused this function an access.
///
/// Called from the fault handler, which takes no lock: one store, and every
/// call the claim answers refuses from here on.
pub fn note_fault(slot: usize) {
    IRQ[slot].fault();
}

pub fn add_inbox_watcher(slot: usize, id: InboxId) {
    let mut watchers = WATCHERS[slot].lock();
    if !watchers.contains(&id) {
        watchers.push(id);
    }
}

pub fn remove_inbox_watcher(slot: usize, id: InboxId) {
    WATCHERS[slot].lock().retain(|held| *held != id);
}

pub fn inbox_watchers(slot: usize) -> Vec<InboxId> {
    WATCHERS[slot].lock().clone()
}
