//! One PCI function, driven by a process instead of by this kernel.
//!
//! The line through the device is **who can name an address**. This module
//! keeps config space — there is no write path to it from userland — puts the
//! function in an address space of its own at the unit *before* it enables bus
//! mastering, programs the interrupt vector into whichever of the function's two
//! message mechanisms it has, and hands out every device address a descriptor
//! may carry. Nothing the holder writes into a descriptor can make the device
//! touch memory the kernel did not grant it: the domain maps the grants and
//! nothing else, and an address outside them is refused at the unit and
//! recorded against that claim.
//!
//! **A window is 2 MiB because that is the only page this kernel maps**, so a
//! BAR a process may see is re-assigned onto a 2 MiB boundary of its own.
//!
//! **Nothing here reads an address this kernel cannot name as routed**, because
//! a load no bridge forwards does not answer all-ones on real hardware: it does
//! not complete, and the CPU cannot be interrupted out of it. The name is a
//! window [`toyos_abi::boot::KernelArgs::root_bridge_windows`] carries: the
//! address firmware put in the BAR is held against those windows here, and no
//! other address is ever offered ([`placement::reserve`]).
//!
//! **Inside one, the function itself is the proof.** [`place_bar`] reads one
//! dword through the BAR where firmware put it, moves the BAR onto the
//! candidate, and reads the same dword there: the function answering its own
//! value is what says it decodes the new address. A placement that does not is
//! undone — the register back to what firmware left in it, the address back to
//! the run it came out of — and the next candidate tried.
//! [`alone_in_its_page`] is the assertion that it worked, never the other way
//! round.
//!
//! **A BAR nothing can settle is not handed over, and its function still is.**
//! An empty window answers the same value at every dword of it, so no read
//! through one proves it decodes anywhere; the holder is given no window for
//! that BAR rather than nothing at all, and a function this kernel could settle
//! no BAR of ends at [`Refusal::NoMappableBar`].
//!
//! **The BAR holding the MSI-X table or PBA is never mapped**: a holder that
//! could rewrite the table could point the device's message at any address the
//! LAPIC decodes. **So a function is armed on MSI only where a walk that
//! reached its capability list's terminator found no MSI-X**: its message is
//! then a word of config space, which has no write path from userland, and it
//! has no table in a BAR for [`msix_bar`] to keep back.
//!
//! **A function with no address space of its own is not handed over**, because
//! every grant would answer with a physical address and a descriptor holding
//! one is an arbitrary read and write over all of memory.
//!
//! **A function masters the bus only once it has memory it may reach.** What
//! comes back from a process still holds the device addresses of a domain that
//! no longer maps them, so bus mastering is not started at hand-over: it starts
//! on the claim's first grant, after that grant is in the function's domain.
//!
//! **A function no mechanism resets is still aimed at what its last holder
//! granted.** Nothing but that function's own driver can stop its queues, and
//! no register write retracts a transfer it had taken in before its holder
//! died: the first grant that starts it mastering again lets that transfer out.
//! So [`release`] takes those grants back like any other — mastering is off,
//! so the function reaches nothing until its next claim's first grant — and
//! keeps only their device addresses, as the slot's [`RESIDUE`]: the domain
//! never hands an address out twice, and the slot is that function's alone
//! (`toyos_pci::slot`), for the rest of the boot if it is never claimed again.
//! The next claim's grant of a range's size is fresh pages placed at that
//! range; a range it places nothing at stays unmapped, so a transfer aimed there
//! faults, and its own release drops it. No page is ever two holders'.
//!
//! **That rests on the next holder laying its grants out as the last one
//! did.** The stale transfer — data and descriptor write-back — lands in
//! whatever the new holder keeps at that address, after it has set up its own
//! rings or not: harmless for a netd succeeding a netd, a silent write into its
//! own memory for a holder with another layout.
//!
//! **A reset returns a function's configuration to its defaults, BARs
//! included, and the next claim puts back what it needs.** [`release`] resets
//! the function by the first mechanism it advertises ([`reset`]) while the old
//! grants are still mapped; the next claim of the slot waits the reset out and
//! writes back what the reset cleared and [`bring_up`] does not write itself
//! ([`Kept`]) before it reads a register of the function.

/// No `crate::` reference, so `kernel-loom` compiles it and models the
/// interleaving no guest test lands on.
mod record;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;

use record::Interrupt;
use toyos_abi::boot::{MemoryMapEntry, RootBridgeWindow};
use toyos_abi::pci::{DeviceIrqRecord, PciFunctionInfo, BARS};
use toyos_abi::syscall::{PciId, RegWidth, SyscallError};
use toyos_dma::Register;
use toyos_pci::bridge::Window;
use toyos_pci::slot::{self, Slot};
use toyos_pci::{af, aperture, bar, express, msix, placement, pm, probe};

use crate::device::{Claim, ClaimError};
use crate::drivers::pci::{NoCapability, PciDevice, Unarmed};
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

/// The unit every run in this module's records is said in.
const MIB: u64 = 1024 * 1024;

/// How much address space the windows may take, above what firmware assigned:
/// every BAR of every function this machine can hand out, at one 2 MiB page
/// each.
const WINDOW_SPAN: u64 = (MAX_FUNCTIONS * BARS) as u64 * PAGE_2M;

/// A function's own configuration space, which is all a claim may read of it
/// (PCIe base spec §7.2.2: 4 KiB per function under ECAM).
const CONFIG_BYTES: u64 = 4096;

/// How many addresses one BAR may be offered before its claim is refused.
///
/// **A refused claim may not spend the machine's free space**: each address the
/// walk takes costs a mapped page and a read, and every one it took goes back
/// to its run when the walk ends in a refusal — so this is the bound on what a
/// function that answers nowhere costs, however many times it is claimed.
const MAX_CANDIDATES: usize = 8;

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
    /// Placed at a range of [`Bound::residue`]: taken back, the range returns
    /// there.
    residual: bool,
}

/// Device addresses a slot's domain handed out and no longer maps.
#[derive(Clone, Copy)]
struct Aimed {
    at: u64,
    bytes: u64,
}

/// Where a function [`release`] could not reset was left aimed: its last
/// holder's grants, by address alone, until that function is claimed again.
static RESIDUE: [Lock<Vec<Aimed>>; MAX_FUNCTIONS] =
    [const { Lock::new(Vec::new()) }; MAX_FUNCTIONS];

/// How a claimed function was made to speak. Both deliver [`VECTORS`]`[slot]`
/// into the same [`Interrupt`] and the claim answers the same handle either way.
enum Armed {
    /// This function's one MSI-X table entry, mapped for the kernel alone.
    Msix(Mmio),
    Msi,
}

/// What a live slot drives. The ISR never reads this.
struct Bound {
    pci: PciDevice,
    space: DeviceSpace,
    armed: Armed,
    id: PciId,
    /// Where each mappable BAR was put, and how much of it the function
    /// advertises; 0 bytes is a slot with no BAR this claim may map.
    bar_at: [u64; BARS],
    bar_bytes: [u64; BARS],
    /// Minted on the first map of that BAR, so a second answers the same
    /// object rather than a second handle to one window.
    bars: [Option<Arc<SharedMemObject>>; BARS],
    grants: Vec<Grant>,
    /// The [`RESIDUE`] this claim took over and has placed no grant at: mapped
    /// to nothing and counted against nothing, and where a grant of a range's
    /// size is placed.
    residue: Vec<Aimed>,
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
    /// What each of [`Self::functions`] said it was, read once in [`publish`]:
    /// the inventory is answered from here and never by reading config space
    /// of a function a reset may be in the middle of. The requester id is not
    /// stored beside it — a `Pci`'s own `at` already names the function, and
    /// [`requester_of`] is its inverse.
    identities: Vec<toyos_abi::inventory::Pci>,
    /// The PCI segment group every one of [`Self::functions`] is on: the one
    /// the ECAM window they were enumerated through serves.
    segment: u16,
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
    /// Where this module may ask the machine about an address: runs below
    /// 4 GiB for a 32-bit BAR, and above everything firmware assigned for a
    /// 64-bit one. Separate because a 32-bit BAR cannot hold an address a
    /// 64-bit one can, and each shortens as [`placement::reserve`] hands an
    /// address out of it.
    ///
    /// **A run is where the machine may be asked, never where a BAR is put.**
    /// What the firmware map, this bus's assigned BARs and its bridges'
    /// forwarded ranges leave over is address space nothing *said* it decodes,
    /// which is not the same claim as reaching the bus.
    low: Vec<Window>,
    high: Vec<Window>,
    /// The memory the root bridges decode, as firmware declared it.
    ///
    /// **The necessary condition for any read this module issues**, so an
    /// address inside none of them is never touched; a machine whose firmware
    /// declared nothing is one where nothing may be.
    firmware: Vec<RootBridgeWindow>,
    /// Windows this module has cut, as `(requester, BAR index, at, span)`.
    ///
    /// **A window belongs to the BAR it was cut for, not to the claim that
    /// asked for it.** The BAR keeps the address across a release, so a window
    /// returned to a free list would let a later claim put a second function on
    /// top of a live one; and one cut per claim would let a process spawn and
    /// die its way through the whole span. Taken once and reused, which is
    /// [`SPACE`]'s treatment for the same reason.
    windows: Vec<(u16, u8, u64, u64)>,
    /// Functions this module has reset, when each may be touched again (PCIe
    /// §6.6.2), and what the reset cleared that the next claim puts back. One
    /// entry per function ever released, so a re-claim cannot start reading a
    /// register the reset has not finished with.
    resetting: Vec<(u16, Resetting, Kept)>,
}

static MACHINE: Lock<Machine> = Lock::new(Machine {
    functions: Vec::new(),
    identities: Vec::new(),
    segment: 0,
    decoded: Vec::new(),
    kernel_driven: Vec::new(),
    low: Vec::new(),
    high: Vec::new(),
    firmware: Vec::new(),
    windows: Vec::new(),
    resetting: Vec::new(),
});

/// Which requester holds each slot.
///
/// **One lock over the whole array, because this is where a function's
/// exclusivity is decided.** `Claim::acquire`'s per-class flag does not answer
/// for a class that names several devices, and `src/build.rs`'s
/// `one_claimant_per_device` compares `system.toml` strings, and a host-side
/// gate is not the capability boundary.
///
/// **Taken alone**: nothing is held while this is, and it is held across
/// nothing — `reserve` runs after `claim` has dropped `MACHINE`, and `release`
/// takes it once the teardown is over.
static SLOTS: Lock<[Slot; MAX_FUNCTIONS]> = Lock::new([Slot::Free; MAX_FUNCTIONS]);

/// Take a slot for `who`, or say why not.
///
/// The scan and the take are one critical section: two claims arriving together
/// must not both find the function unheld.
fn reserve(who: u16) -> Result<usize, ClaimError> {
    slot::reserve(&mut *SLOTS.lock(), who).map_err(|refused| match refused {
        slot::Refused::Owned => ClaimError::Owned,
        slot::Refused::Exhausted => ClaimError::Exhausted,
    })
}

/// Every function this machine enumerated, and who drives it now: a kernel
/// driver, a process holding a claim, or nobody.
pub fn inventory() -> Vec<toyos_abi::inventory::Pci> {
    use toyos_abi::inventory::Driven;
    let (identities, kernel_driven) = {
        let machine = MACHINE.lock();
        (machine.identities.clone(), machine.kernel_driven.clone())
    };
    let slots = *SLOTS.lock();
    let mut out = Vec::with_capacity(identities.len());
    for mut pci in identities {
        let who = requester_of(pci.at);
        pci.driven = if kernel_driven.contains(&who) {
            Driven::Kernel
        } else if slots.contains(&Slot::Held(who)) {
            Driven::Claimed
        } else {
            Driven::Free
        };
        out.push(pci);
    }
    out
}

/// The PCI segment group every enumerated function is on.
pub fn segment() -> u16 {
    MACHINE.lock().segment
}

/// The function a claim's slot holds, or `None` for a slot nobody holds.
pub fn held_at(slot: usize) -> Option<toyos_abi::inventory::PciAddr> {
    let Some(Slot::Held(who)) = SLOTS.lock().get(slot).copied() else { return None };
    Some(addr_of(MACHINE.lock().segment, who))
}

pub(crate) fn requester(pci: &PciDevice) -> u16 {
    ((pci.bus as u16) << 8) | ((pci.dev as u16) << 3) | pci.func as u16
}

/// The function a [`requester`] ID names on `segment`: its inverse.
pub(crate) fn addr_of(segment: u16, who: u16) -> toyos_abi::inventory::PciAddr {
    toyos_abi::inventory::PciAddr {
        segment,
        bus: (who >> 8) as u8,
        dev: ((who >> 3) & 0x1f) as u8,
        func: (who & 7) as u8,
    }
}

/// [`addr_of`]'s inverse: the requester id `at` names, dropping the segment
/// [`requester`] never carried either.
pub(crate) fn requester_of(at: toyos_abi::inventory::PciAddr) -> u16 {
    ((at.bus as u16) << 8) | ((at.dev as u16) << 3) | at.func as u16
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

/// Take the enumeration, and derive where a BAR a process maps may be asked
/// about.
///
/// **Before any driver `init`**, because the sizing probe below takes memory
/// decode off the function it is probing for the length of the probe, and a
/// driver mid-transfer must not meet that.
///
/// **The high run is above everything firmware described**, which is every BAR
/// it assigned *and* every entry of the memory map it handed the loader — RAM,
/// its own runtime services, the ACPI regions and the fixed platform apertures
/// alike. Below 4 GiB there is no such address: the platform's fixed MMIO is at
/// [`PLATFORM_MMIO`] and the memory map reaches it, so the low runs are the
/// gaps *between* what those sources describe ([`free_runs_below_4g`]).
pub fn publish(devices: &[PciDevice], segment: u16, maps: &[MemoryMapEntry], firmware: &[RootBridgeWindow]) {
    let mut wide_end = 0u64;
    let mut decoded = Vec::new();
    for entry in maps {
        wide_end = wide_end.max(entry.end);
    }
    for device in devices {
        // Bounded by the header's own declaration: a bridge has two BAR slots
        // and four registers past them that are not BARs, and `bar_size` below
        // write-ones-probes whatever it is given.
        let slots = device.bar_slots();
        let mut index = 0u8;
        while index < slots {
            let low = device.read_config_u32(bar::BASE + index as u64 * 4);
            let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
            if let Ok(memory) = device.memory_bar(index) {
                // At least one byte for a BAR that answers no size: a window
                // may not start on top of an address something decodes, and
                // an unknown length is not an empty one.
                let size = device.bar_size(index).unwrap_or(0).max(1);
                let end = memory.address().saturating_add(size);
                decoded.push((requester(device), memory.address(), end));
                wide_end = wide_end.max(end);
            }
            // A 64-bit BAR's high half is the next register and is not a BAR:
            // decoding it would read an address out of address bits.
            index += if wide { 2 } else { 1 };
        }
    }
    account_for(firmware, &decoded);
    let low = free_runs_below_4g(devices, maps, &decoded);
    let high = match window(wide_end, u64::MAX) {
        (0, _) => Vec::new(),
        (start, end) => alloc::vec![Window { start, end }],
    };
    log!(
        "pcidev: {} functions; {} run(s) of {} MiB or more below {PLATFORM_MMIO:#x} and {} above \
         everything firmware described",
        devices.len(),
        low.len(),
        PAGE_2M / MIB,
        high.len(),
    );
    for run in low.iter().chain(high.iter()) {
        log!("pcidev:   {:#x}..{:#x} ({} MiB)", run.start, run.end, (run.end - run.start) / MIB);
    }
    let mut machine = MACHINE.lock();
    machine.functions = devices.to_vec();
    machine.identities =
        devices.iter().map(|d| d.identity(addr_of(segment, requester(d)))).collect();
    machine.segment = segment;
    machine.decoded = decoded;
    machine.firmware = firmware.to_vec();
    machine.low = low;
    machine.high = high;
}

/// Say what memory firmware declared the root bridges decode, and which
/// assigned BAR lies inside none of it.
///
/// **What this answers about is the necessary condition every read below is
/// checked against**, so it says the answer once and counts nothing from it.
fn account_for(firmware: &[RootBridgeWindow], decoded: &[(u16, u64, u64)]) {
    if firmware.is_empty() {
        log!(
            "pcidev: firmware declared no root bridge memory, so no address on this machine may \
             be read"
        );
        return;
    }
    let mut said = String::new();
    for window in firmware {
        if !said.is_empty() {
            said.push_str(", ");
        }
        let _ = write!(said, "mem {:#x}..{:#x}", window.base, window.end());
    }
    log!("pcidev: firmware declared root bridge memory: {said}");

    for (who, base, end) in decoded {
        if aperture::decode(firmware, *base, *end) == aperture::Decode::Unrouted {
            log!(
                "pcidev: requester {who:#06x}'s {base:#x}..{end:#x} is inside none of it, so this \
                 kernel has no declaration to read it by"
            );
        }
    }
}

/// What is left below 4 GiB, after everything this machine could be asked about
/// itself.
///
/// **Below 4 GiB there is no address above everything firmware described** —
/// the platform's fixed MMIO is at [`PLATFORM_MMIO`] and the memory map reaches
/// it — so a 32-bit window is a run *between* things rather than a span above
/// them, and this is the subtraction that finds one.
///
/// It accounts for exactly three things and each is *read*: the firmware memory
/// map, the BARs this bus has assigned, and every range a bridge forwards to a
/// secondary bus. None of the three says an address reaches the bus, which is
/// why a run here is only where [`place_bar`] *may* ask.
fn free_runs_below_4g(
    devices: &[PciDevice],
    maps: &[MemoryMapEntry],
    decoded: &[(u16, u64, u64)],
) -> Vec<Window> {
    let mut taken: Vec<(u64, u64)> = Vec::new();
    let mut note = |start: u64, end: u64| {
        let (start, end) = (start.min(PLATFORM_MMIO), end.min(PLATFORM_MMIO));
        if start < end {
            taken.push((start, end));
        }
    };
    for entry in maps {
        note(entry.start, entry.end);
    }
    for (_, start, end) in decoded {
        note(*start, *end);
    }
    for device in devices {
        for forwarded in device.forwarded_below_4g() {
            log!(
                "pcidev: PCI {:02x}:{:02x}.{} forwards {:#x}..{:#x} to its secondary bus",
                device.bus,
                device.dev,
                device.func,
                forwarded.start,
                forwarded.end,
            );
            note(forwarded.start, forwarded.end);
        }
    }
    taken.sort_unstable();
    let mut free: Vec<Window> = Vec::new();
    let mut at = 0u64;
    for (start, end) in taken {
        if start > at {
            free.push(Window { start: at, end: start });
        }
        at = at.max(end);
    }
    if at < PLATFORM_MMIO {
        free.push(Window { start: at, end: PLATFORM_MMIO });
    }
    free.retain(|run| run.end - run.start >= PAGE_2M);
    free
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
    NoInterrupt,
    MsixUnusable,
    CapsTruncated,
    Untranslated(IommuError),
    /// This machine offered the BAR no address of that width at all: no free
    /// run holds a span-aligned one inside a window firmware declared.
    NoRun { wide: bool },
    /// Every address this machine offered was tried and the function answered
    /// at none of them.
    NoPlacement { wide: bool, asked: usize },
    /// The function publishes nothing this claim may map — no memory BAR, or
    /// only the one holding its own MSI-X table.
    NoMappableBar,
    BarUnsizable(u8),
    BarUnplaceable(u8),
    BarResized(u8),
    /// The BAR no longer holds the window this module cut for it.
    BarMoved(u8),
    /// Firmware assigned this BAR no address, so the function answers nowhere
    /// and there is nothing to hold a candidate's answer against.
    BarUnassigned(u8),
    /// The address firmware assigned this BAR is inside no window firmware
    /// declared, so this kernel may not read it.
    BarUnrouted(u8),
    /// The dword this function answers where firmware put it is one of the two
    /// a read nobody answered comes back as, so it settles no candidate.
    BarReferenceEmpty(u8),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoInterrupt => write!(
                f,
                "neither its MSI-X nor its MSI could be armed, and a claim with no interrupt \
                 is a driver that would never be told anything"
            ),
            Self::MsixUnusable => write!(
                f,
                "it publishes MSI-X and this kernel could not arm it, and MSI is not a fallback \
                 for a function that has a table"
            ),
            Self::CapsTruncated => write!(
                f,
                "its capability list ends at a link the PCI spec forbids, so whether it holds \
                 an MSI-X table in a BAR was never read, and MSI is not armed on a guess"
            ),
            Self::Untranslated(why) => write!(
                f,
                "it would have no address space of its own — {why} — and a process driving \
                 it would be given physical addresses to put in descriptors"
            ),
            Self::NoRun { wide: true } => write!(
                f,
                "this machine has no 2 MiB-aligned 64-bit address space both above what firmware \
                 assigned and inside a window firmware declared to offer its BAR"
            ),
            Self::NoRun { wide: false } => write!(
                f,
                "its BAR is 32-bit and nothing below 4 GiB is both free of the firmware map, \
                 this bus's assigned BARs and its bridges' forwarded ranges and inside a window \
                 firmware declared"
            ),
            Self::NoPlacement { wide, asked } => write!(
                f,
                "its BAR was moved onto {asked} {}-bit address(es) inside the windows firmware \
                 declared and the function answered at none of them",
                if *wide { 64 } else { 32 }
            ),
            Self::NoMappableBar => write!(
                f,
                "it publishes no memory BAR this claim may map, so its holder would have no \
                 registers to drive it through"
            ),
            Self::BarUnsizable(i) => write!(f, "BAR {i} answers no size to bound a window by"),
            Self::BarUnplaceable(i) => write!(f, "BAR {i} did not take the address it was given"),
            Self::BarResized(i) => write!(
                f,
                "BAR {i} answers a different size than the window it already holds was cut for, \
                 so the function changed under this kernel"
            ),
            Self::BarMoved(i) => write!(
                f,
                "BAR {i} no longer holds the window it was cut for, so the function does not \
                 decode where its holder would map it"
            ),
            Self::BarUnassigned(i) => write!(
                f,
                "firmware assigned BAR {i} no address, so this function answers nowhere and \
                 nothing says what it would answer through a BAR moved anywhere else"
            ),
            Self::BarUnrouted(i) => write!(
                f,
                "BAR {i} holds an address inside no window firmware declared, so nothing says a \
                 read of it would come back"
            ),
            Self::BarReferenceEmpty(i) => write!(
                f,
                "BAR {i} answers all-zeroes or all-ones where firmware put it, which is what a \
                 read nobody answers comes back as, so it settles no candidate"
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

    // `Owned` before `Exhausted`: a second claim on a function somebody holds
    // is a different fact from a machine with no slot left.
    let slot = reserve(requester(&pci))?;
    // Dropped by every refusal below, and `release` is what gives the slot back.
    let claim = Claim::pci(slot);

    match bring_up(pci, id, slot) {
        Ok(mut bound) => {
            bound.residue = take_residue(slot);
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
    // What the slot's previous holder left mapped goes before anything attaches
    // to its domain, and whatever is left of a reset [`release`] started on this
    // function before a register of it is read (PCIe §6.6.2).
    finish_retired(slot);
    settle_after_reset(&pci);

    // **Every refusal that can be taken without touching the function is taken
    // first.** An address space holding this function's grants and nothing else
    // — and a refusal where this machine has none to give, because the
    // alternative is handing a process physical addresses to write into
    // descriptors. It is asked for here rather than after the BARs because a
    // refusal that had already armed a vector and moved a function's BARs would
    // leave the machine changed by a hand-over that did not happen.
    let space = slot_space(slot).map_err(Refusal::Untranslated)?;

    // Held across every walk this hand-over makes of the function's own list —
    // both readers below and the MSI fallback between them — so the staged
    // shape is the device's and not one reader's view of it.
    #[cfg(feature = "boot-actuators")]
    let _staged = crate::drivers::pci::StagedCaps::armed_for(&pci);

    // The table's own BAR, so it can be left where it is and kept out of what
    // the holder maps.
    let table_bar = msix_bar(&pci);
    // Decode must be on for a BAR to answer, and off across each move.
    pci.enable_memory_space();

    // Then the interrupt, still before a window is cut: a function neither
    // mechanism can be armed on is one no holder could ever be told anything
    // about.
    let armed = match pci.enable_msix(VECTORS[slot]) {
        Ok(entry) => Armed::Msix(entry),
        Err(Unarmed::Unusable) => return Err(Refusal::MsixUnusable),
        Err(Unarmed::Blocked) => return Err(Refusal::NoInterrupt),
        Err(Unarmed::NoTable(NoCapability::Truncated)) => return Err(Refusal::CapsTruncated),
        Err(Unarmed::NoTable(NoCapability::Absent)) => {
            pci.enable_msi(VECTORS[slot]).then_some(Armed::Msi).ok_or(Refusal::NoInterrupt)?
        }
    };

    // From here a refusal has to undo: a vector is armed, and the arms below
    // move the function's BARs.
    match place_bars(&pci, id, table_bar) {
        Ok((bar_at, bar_bytes)) => {
            space.attach(pci.bus, pci.dev, pci.func);
            Ok(Bound {
                pci,
                space,
                armed,
                id,
                bar_at,
                bar_bytes,
                bars: [const { None }; BARS],
                grants: Vec::new(),
                residue: Vec::new(),
                mastering: false,
            })
        }
        Err(why) => {
            match armed {
                Armed::Msix(_) => pci.disable_msix(),
                Armed::Msi => pci.disable_msi(),
            }
            Err(why)
        }
    }
}

/// Move every BAR this claim may map, and answer where each one went.
///
/// A BAR that was moved before this refusal keeps its address: the window is
/// that BAR's for the life of the boot ([`Machine::windows`]), so a later claim
/// on the same function takes the same one back rather than putting a second
/// function on top of it.
fn place_bars(
    pci: &PciDevice,
    id: PciId,
    table_bar: Option<u8>,
) -> Result<([u64; BARS], [u64; BARS]), Refusal> {
    let mut bar_at = [0u64; BARS];
    let mut bar_bytes = [0u64; BARS];
    let slots = pci.bar_slots();
    let mut index = 0u8;
    while index < slots {
        let low = pci.read_config_u32(bar::BASE + index as u64 * 4);
        let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
        let step = if wide { 2 } else { 1 };
        // A BAR this module cut a window for is [`place_bar`]'s to judge
        // whatever its register holds now: one a reset returned to zero reads
        // as unassigned, and it is the window that moved, not a BAR that is
        // not there.
        if Some(index) == table_bar || (pci.memory_bar(index).is_err() && !was_cut(pci, index)) {
            index += step;
            continue;
        }
        let size = pci.bar_size(index).map_err(|_| Refusal::BarUnsizable(index))?;
        let at = match place_bar(pci, id, index, size) {
            Ok(at) => at,
            // Each of these says only that nothing could settle this BAR, which
            // is that window's loss and not the function's.
            Err(why @ (Refusal::BarUnassigned(_)
            | Refusal::BarUnrouted(_)
            | Refusal::BarReferenceEmpty(_))) => {
                log!(
                    "pcidev: PCI {:02x}:{:02x}.{} keeps BAR {index} where firmware put it and \
                     hands it to nobody — {why}",
                    pci.bus,
                    pci.dev,
                    pci.func
                );
                index += step;
                continue;
            }
            Err(why) => return Err(why),
        };
        bar_at[index as usize] = at;
        bar_bytes[index as usize] = size;
        index += step;
    }
    // Its own refusal and not a window one: nothing about this machine's
    // address space is wrong, and a reader sent to the window allocator would
    // find it healthy.
    if bar_bytes.iter().all(|bytes| *bytes == 0) {
        return Err(Refusal::NoMappableBar);
    }
    Ok((bar_at, bar_bytes))
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

/// What a reset returns to its default that the next hand-over needs back and
/// does not write itself, read off the function before its reset.
///
/// An FLR returns every register of the function to its initial value except
/// the sticky and hardware-initialised ones (PCIe base spec 6.0 §6.6.2), and a
/// D3hot → D0 transition without `No_Soft_Reset` does the same (PCI PM 1.2
/// §5.4). Of what that clears, [`bring_up`] writes the Command register's
/// decode and mastering bits, MSI or MSI-X, and — through [`place_bar`] only
/// on a claim's first cut — the BARs it moves. So this keeps:
///
/// - **every BAR**: the ones this module moved, which [`cut_already`] answers
///   with an address the function would otherwise no longer decode; and the
///   ones it left where firmware put them, the MSI-X table's among them, which
///   [`PciDevice::enable_msix`] reads its table's address out of.
/// - **Device Control, and Device Control 2 where the structure has one**:
///   Max_Payload_Size has to agree with the link partner's (§7.5.3.4), and the
///   rest is what firmware configured the hierarchy with (§7.5.3.16).
#[derive(Clone, Copy)]
struct Kept {
    bars: [u32; BARS],
    slots: u8,
    control: Option<(u16, Option<u16>)>,
}

impl Kept {
    fn read(pci: &PciDevice) -> Self {
        let slots = pci.bar_slots();
        let mut bars = [0u32; BARS];
        for (index, bar) in bars.iter_mut().enumerate().take(slots as usize) {
            *bar = pci.read_config_u32(bar::BASE + index as u64 * 4);
        }
        let control = pci.capability(express::CAP_ID).ok().map(|cap| {
            let second = express::has_control_2(cap.read_u16(express::CAPABILITIES))
                .then(|| cap.read_u16(express::DEVICE_CONTROL_2));
            (cap.read_u16(express::DEVICE_CONTROL), second)
        });
        Self { bars, slots, control }
    }

    /// Put it back, with memory decode off so nothing reads through a BAR that
    /// is half-written. [`bring_up`] turns decode on again.
    fn restore(&self, pci: &PciDevice) {
        pci.set_memory_decode(false);
        let lost = crate::actuator::pcidev_bar_lost_on_reset().then(|| msix_bar(pci));
        for (index, bar) in self.bars.iter().enumerate().take(self.slots as usize) {
            if lost.is_some_and(|table| table != Some(index as u8)) {
                continue;
            }
            pci.write_config_u32(bar::BASE + index as u64 * 4, *bar);
        }
        if let (Some((control, second)), Ok(cap)) = (self.control, pci.capability(express::CAP_ID)) {
            cap.write_u16(express::DEVICE_CONTROL, express::restored(control));
            if let Some(second) = second {
                cap.write_u16(express::DEVICE_CONTROL_2, second);
            }
        }
    }
}

/// How a released function is being put back into its reset state, and when
/// it may be touched again.
#[derive(Clone, Copy)]
enum Resetting {
    /// A function level reset, through the Express capability or the AF one:
    /// nothing may touch the function before `at` (PCIe §6.6.2).
    Flr { at: u64 },
    /// The Power Management round trip: the function was put in D3hot and may
    /// be moved back to D0 at `at`, and touched a transition time after that.
    D3hot { at: u64 },
}

impl Resetting {
    /// When nothing the function started before its reset can still land.
    fn quiet_at(self) -> u64 {
        match self {
            Self::Flr { at } | Self::D3hot { at } => at,
        }
    }
}

/// Why one of the three mechanisms did not reset a function.
#[derive(Clone, Copy)]
enum Declined {
    /// The walk reached the list's terminator without the capability.
    Absent,
    /// The walk ended at a link the spec forbids before finding it.
    Unread,
    /// The capability is there and does not advertise a function level reset.
    NoFlr,
    /// The PM capability says `No_Soft_Reset`: D3hot → D0 keeps its state.
    NoSoftReset,
    /// The function is not in D0, so a round trip from it is not one this
    /// kernel knows the timing of.
    NotInD0,
    /// `pcidev-reset-nothing` declined it without asking the function.
    Staged,
}

impl core::fmt::Display for Declined {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Absent => "no capability",
            Self::Unread => "the capability list ends at a forbidden link before it",
            Self::NoFlr => "no function level reset advertised",
            Self::NoSoftReset => "No_Soft_Reset set",
            Self::NotInD0 => "not in D0",
            Self::Staged => "declined unasked, as staged",
        })
    }
}

impl From<NoCapability> for Declined {
    fn from(why: NoCapability) -> Self {
        match why {
            NoCapability::Absent => Self::Absent,
            NoCapability::Truncated => Self::Unread,
        }
    }
}

/// Which reset a release started, or why none of the three could be.
enum How {
    Express,
    Af { pending: bool },
    D3hot,
    Nothing { express: Declined, af: Declined, pm: Declined },
}

impl core::fmt::Display for How {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Express => f.write_str("a function level reset (Express)"),
            Self::Af { pending: false } => f.write_str("a function level reset (AF)"),
            Self::Af { pending: true } => {
                f.write_str("a function level reset (AF), with transactions still pending")
            }
            Self::D3hot => f.write_str("the D3hot round trip"),
            Self::Nothing { express, af, pm } => write!(
                f,
                "nothing (Express: {express}; AF: {af}; PM: {pm}), so where it may still be aimed \
                 is kept for its next claim"
            ),
        }
    }
}

/// Start this function's reset, by the first of the three mechanisms it
/// advertises, and answer which and how long it takes.
///
/// The Express reset first, then the AF one a conventional function publishes
/// in its place, then the D3hot round trip, which resets any function that
/// does not say `No_Soft_Reset`.
fn reset(pci: &PciDevice) -> (How, Option<Resetting>) {
    if crate::actuator::pcidev_reset_nothing() {
        let staged = Declined::Staged;
        return (How::Nothing { express: staged, af: staged, pm: staged }, None);
    }
    let now = crate::clock::nanos_since_boot();
    let express = match pci.capability(express::CAP_ID) {
        Ok(cap) if express::resets(cap.read_u32(express::DEVICE_CAPABILITIES)) => {
            let control = cap.read_u16(express::DEVICE_CONTROL);
            cap.write_u16(express::DEVICE_CONTROL, express::initiate(control));
            return (How::Express, Some(Resetting::Flr { at: now + express::SETTLE_NANOS }));
        }
        Ok(_) => Declined::NoFlr,
        Err(why) => why.into(),
    };
    let af = match pci.capability(af::CAP_ID) {
        Ok(cap) if af::resets(cap.read_u8(af::AF_CAPABILITIES)) => {
            let pending = af::pending(cap.read_u8(af::AF_STATUS));
            cap.write_u8(af::AF_CONTROL, af::INITIATE_FLR);
            return (How::Af { pending }, Some(Resetting::Flr { at: now + af::SETTLE_NANOS }));
        }
        Ok(_) => Declined::NoFlr,
        Err(why) => why.into(),
    };
    let pm = match pci.capability(pm::CAP_ID) {
        Ok(cap) => {
            let pmcsr = cap.read_u16(pm::PMCSR);
            if !pm::resets(pmcsr) {
                Declined::NoSoftReset
            } else if pm::state(pmcsr) != pm::D0 {
                Declined::NotInD0
            } else {
                cap.write_u16(pm::PMCSR, pm::to(pmcsr, pm::D3HOT));
                return (How::D3hot, Some(Resetting::D3hot { at: now + pm::TRANSITION_NANOS }));
            }
        }
        Err(why) => why.into(),
    };
    (How::Nothing { express, af, pm }, None)
}

/// Finish whatever is left of a reset this kernel started on this function,
/// and put back what it cleared, before a register of it is read.
///
/// Spent here rather than in [`release`], where the process is already dying
/// and the drain that runs its teardown may be the idle loop's: a re-claim is
/// the only thing that may touch the function again.
fn settle_after_reset(pci: &PciDevice) {
    let who = requester(pci);
    let pending = {
        let mut machine = MACHINE.lock();
        let at = machine.resetting.iter().position(|(id, _, _)| *id == who);
        at.map(|at| machine.resetting.swap_remove(at))
    };
    let Some((_, resetting, kept)) = pending else { return };
    wait_until(resetting.quiet_at());
    if let Resetting::D3hot { .. } = resetting {
        if let Ok(cap) = pci.capability(pm::CAP_ID) {
            let pmcsr = cap.read_u16(pm::PMCSR);
            cap.write_u16(pm::PMCSR, pm::to(pmcsr, pm::D0));
        }
        wait_until(crate::clock::nanos_since_boot() + pm::TRANSITION_NANOS);
    }
    kept.restore(pci);
    if crate::actuator::pcidev_bar_moved_on_reset() && matches!(resetting, Resetting::Flr { .. }) {
        move_inside_its_window(pci);
    }
}

/// [`crate::actuator::pcidev_bar_moved_on_reset`]: BAR 0 one BAR's size above
/// the window it was cut, so the register decodes an address that is not the
/// cut and that nothing else decodes.
fn move_inside_its_window(pci: &PciDevice) {
    let (at, span) = {
        let who = requester(pci);
        let machine = MACHINE.lock();
        let &(_, _, at, span) = machine
            .windows
            .iter()
            .find(|(w, i, _, _)| *w == who && *i == 0)
            .expect("pcidev-bar-moved-on-reset: BAR 0 of a reset function was never cut");
        (at, span)
    };
    let size = pci.bar_size(0).expect("pcidev-bar-moved-on-reset: BAR 0 does not size");
    assert!(size < span, "pcidev-bar-moved-on-reset: BAR 0 fills its window, so no address inside it is not the cut");
    let low = pci.read_config_u32(bar::BASE);
    assert_eq!(u64::from(low & !0xf), at & 0xffff_ffff, "pcidev-bar-moved-on-reset: BAR 0 was not restored to its cut");
    pci.write_config_u32(bar::BASE, low + size as u32);
}

fn wait_until(at: u64) {
    while crate::clock::nanos_since_boot() < at {
        core::hint::spin_loop();
    }
}

/// A released function's grants, still mapped in its slot's domain until its
/// reset has finished: a write the function had already issued lands in the
/// released holder's own pages rather than faulting, or landing in pages the
/// allocator has handed somebody else.
struct Retired {
    space: DeviceSpace,
    grants: Vec<Grant>,
    quiet_at: u64,
}

static RETIRED: [Lock<Option<Retired>>; MAX_FUNCTIONS] =
    [const { Lock::new(None) }; MAX_FUNCTIONS];

/// Unmap and free what a slot's previous holder left mapped, once its function
/// can no longer write into it. Before anything else attaches to the domain.
fn finish_retired(slot: usize) {
    let retired = RETIRED[slot].lock().take();
    let Some(retired) = retired else { return };
    wait_until(retired.quiet_at);
    for grant in retired.grants.iter() {
        if let Err(why) = retired.space.unmap(grant.at, grant.bytes) {
            panic!("pcidev: slot {slot} could not take {:#x} back: {why}", grant.at);
        }
    }
    // The pages go back with this drop, after the unmap above.
    drop(retired);
}

/// The slot's [`RESIDUE`], for the claim that now holds its function.
fn take_residue(slot: usize) -> Vec<Aimed> {
    let residue = core::mem::take(&mut *RESIDUE[slot].lock());
    if !residue.is_empty() {
        let bytes: u64 = residue.iter().map(|aimed| aimed.bytes).sum();
        log!(
            "pcidev: slot {slot} holds {} range(s), {bytes} bytes, its function was left aimed \
             at: a grant of a range's size is placed there, on fresh pages",
            residue.len()
        );
    }
    residue
}

/// Which BAR holds this function's MSI-X table or PBA, if any.
///
/// Both, because both are the unit's structures and neither is the holder's:
/// the two live in one BAR on every device in reach, and a device that split
/// them costs the second BAR too rather than publishing one of them.
fn msix_bar(pci: &PciDevice) -> Option<u8> {
    let cap = pci.capability(msix::CAP_ID).ok()?;
    let control = cap.read_u16(msix::MESSAGE_CONTROL);
    let table = msix::Msix::decode(control, cap.read_u32(msix::TABLE)).ok()?;
    Some(table.bir())
}

/// What this machine answers at `at + offset`.
///
/// **One dword and no more.** This is a read of a register file this kernel
/// does not know: a device's first page holds read-to-clear causes and
/// pop-on-read queues, and a sweep of it would drive whatever is there. So the
/// probe touches the one dword [`probe::reference`] names, and the same one
/// before the BAR is moved onto the address and after.
///
/// **Every caller has named `at` routed first.** A load no bridge forwards does
/// not come back on real hardware.
///
/// **A refused candidate leaves the direct map's entries over its range
/// uncacheable, and that is the whole of what it leaves**: the boot map already
/// covers every physical address, so this takes no address space there is any
/// giving back of, and a run holds no memory the firmware map described.
fn probe_dword(at: u64, span: u64, offset: u64) -> u32 {
    crate::mm::paging::map_mmio(at, span, MmioPolicy::Uncacheable).read_u32(offset)
}

/// Move BAR `index` onto a 2 MiB boundary inside a window firmware declared,
/// and answer where.
///
/// **Only an address inside a window firmware declared is ever read**, because
/// a load no bridge forwards does not come back on real hardware — the address
/// firmware itself put in the BAR included.
///
/// Memory decode is off across every write, so nothing can read through a BAR
/// that is half-programmed, and the address is read back off the register
/// rather than assumed. A candidate the machine refuses leaves the register
/// holding what firmware left in it, and a walk that ends in a refusal gives
/// every address it took back to the run it came out of.
fn place_bar(pci: &PciDevice, id: PciId, index: u8, size: u64) -> Result<u64, Refusal> {
    let offset = bar::BASE + index as u64 * 4;
    let low = pci.read_config_u32(offset);
    let wide = matches!(bar::decode(index, low), Ok(bar::Width::Wide(_)));
    let high = pci.read_config_u32(offset + 4);
    let span = align_2m(size as usize) as u64;
    if let Some(at) = cut_already(pci, index, span)? {
        // The register is the proof and the record is not: a function that
        // lost its address since the cut decodes nowhere its holder maps.
        return match pci.memory_bar(index) {
            Ok(memory) if memory.address() == at => Ok(at),
            _ => Err(Refusal::BarMoved(index)),
        };
    }
    let restore = || {
        pci.set_memory_decode(false);
        pci.write_config_u32(offset, low);
        if wide {
            pci.write_config_u32(offset + 4, high);
        }
        pci.set_memory_decode(true);
    };

    let windows = MACHINE.lock().firmware.clone();
    // **What this function answers, read where firmware put it.** It is the
    // reference every candidate is settled against, and it has to be taken
    // here: from the first write below the BAR is somewhere this kernel chose
    // and the device's own address is gone.
    //
    // A BAR firmware assigned no address has no such reference, and reading one
    // at zero would take low RAM for the function's answer — so it is refused
    // by name rather than settled against a number that is nothing's.
    let was = pci.memory_bar(index).map_err(|_| Refusal::BarUnplaceable(index))?.address();
    if was == 0 {
        return Err(Refusal::BarUnassigned(index));
    }
    let inside = aperture::decode(&windows, was, was.saturating_add(size));
    if !matches!(inside, aperture::Decode::Inside(_)) {
        return Err(Refusal::BarUnrouted(index));
    }
    let reference = probe::reference(id);
    let signature = probe_dword(was, size, reference);
    if probe::degenerate(signature) {
        return Err(Refusal::BarReferenceEmpty(index));
    }

    let who = alloc::format!("PCI {:02x}:{:02x}.{}", pci.bus, pci.dev, pci.func);
    let mut refused = [None; MAX_CANDIDATES];
    let mut asked = 0usize;
    while asked < MAX_CANDIDATES {
        let Some(candidate) = with_runs(wide, |runs| placement::reserve(runs, &windows, span))
        else {
            break;
        };
        let at = candidate.at;
        let Ok(placed) = bar::placement(index, low, at, size) else {
            refused[asked] = Some(candidate);
            give_back(wide, &refused, span);
            return Err(Refusal::BarUnplaceable(index));
        };
        pci.set_memory_decode(false);
        pci.write_config_u32(offset, placed.low);
        if let Some(high) = placed.high {
            pci.write_config_u32(offset + 4, high);
        }
        pci.set_memory_decode(true);
        // Read back off the register rather than assumed: a function that did
        // not take the address decodes somewhere else and says nothing about it.
        if !matches!(pci.memory_bar(index), Ok(memory) if memory.address() == at) {
            restore();
            refused[asked] = Some(candidate);
            give_back(wide, &refused, span);
            return Err(Refusal::BarUnplaceable(index));
        }
        let after = probe_dword(at, span, reference);
        if after == signature {
            alone_in_its_page(pci, index, at, span);
            cut(pci, index, at, span);
            log!(
                "pcidev: {who} BAR {index} ({size:#x} bytes) placed at {at:#x} — inside \
                 firmware's mem {:#x}; its +{reference:#x} dword answers {after:#010x} there, \
                 and answered {signature:#010x} from {was:#x}, where firmware put it",
                candidate.window
            );
            return Ok(at);
        }

        restore();
        log!(
            "pcidev: {who} BAR {index} left {at:#x}: with the BAR moved onto it and decode on it \
             answers {after:#010x}, and this function answers {signature:#010x} at {was:#x}, so \
             nothing routes it"
        );
        refused[asked] = Some(candidate);
        asked += 1;
    }
    give_back(wide, &refused, span);
    Err(if asked == 0 { Refusal::NoRun { wide } } else { Refusal::NoPlacement { wide, asked } })
}

/// This machine's free runs of one width, under the lock that hands them out.
///
/// **Choosing an address and taking it out of the runs are one critical
/// section**, because two claims arriving together must not be offered one
/// address: the loser would either trip [`alone_in_its_page`] or land on top of
/// the winner. The probe that follows runs outside the lock and against a
/// reservation no other claim can take.
fn with_runs<T>(wide: bool, f: impl FnOnce(&mut Vec<Window>) -> T) -> T {
    let mut machine = MACHINE.lock();
    f(if wide { &mut machine.high } else { &mut machine.low })
}

/// Put back every address a refused walk took, newest first: a run gives back
/// only the address it handed out last.
fn give_back(wide: bool, refused: &[Option<placement::Reservation>], span: u64) {
    with_runs(wide, |runs| {
        for candidate in refused.iter().rev().flatten() {
            placement::release(runs, *candidate, span);
        }
    });
}

/// Where this BAR was already put, if a claim before this one put it there.
///
/// A window belongs to the BAR it was cut for, not to the claim that asked for
/// it ([`Machine::windows`]), so a later claim on the same function takes the
/// same address back rather than asking the machine a second time.
fn cut_already(pci: &PciDevice, index: u8, span: u64) -> Result<Option<u64>, Refusal> {
    let who = requester(pci);
    let machine = MACHINE.lock();
    let Some(&(_, _, at, cut)) = machine.windows.iter().find(|(w, i, _, _)| *w == who && *i == index)
    else {
        return Ok(None);
    };
    // Refused by its own name and not as an address-space refusal: this machine
    // has the room, and what changed is the BAR.
    if cut == span {
        Ok(Some(at))
    } else {
        Err(Refusal::BarResized(index))
    }
}

/// Whether this module has cut a window for BAR `index` of this function.
fn was_cut(pci: &PciDevice, index: u8) -> bool {
    let who = requester(pci);
    MACHINE.lock().windows.iter().any(|(w, i, _, _)| *w == who && *i == index)
}

/// Record `at .. at + span` as this BAR's for the life of the boot.
///
/// The address is already out of the runs: [`placement::reserve`] took it there
/// under [`with_runs`], which is what keeps a second claim from being offered
/// it.
fn cut(pci: &PciDevice, index: u8, at: u64, span: u64) {
    MACHINE.lock().windows.push((requester(pci), index, at, span));
}

/// Nothing else on this machine decodes inside the page that is about to be
/// mapped into a process.
///
/// The assertion that [`place_bar`] worked, never the mechanism. Two things
/// this module says are outside a window: what firmware assigned to some other
/// function, and the windows this module has itself cut — an overlap between
/// them is one process given another's registers.
fn alone_in_its_page(claimed: &PciDevice, index: u8, at: u64, span: u64) {
    let machine = MACHINE.lock();
    let mine = requester(claimed);
    let firmware = machine
        .decoded
        .iter()
        .filter(|(who, _, _)| *who != mine)
        .map(|(who, address, end)| (*who, *address, *end));
    let cut = machine
        .windows
        .iter()
        .filter(|(who, i, _, _)| (*who, *i) != (mine, index))
        .map(|(who, _, address, span)| (*who, *address, *address + *span));
    for (who, address, end) in firmware.chain(cut) {
        // The whole extent, not the base: a BAR that starts below the window
        // and reaches into it is the overlap this is for.
        assert!(
            end <= at || address >= at + span,
            "pcidev: the {span:#x}-byte window at {at:#x} holds requester {who:#06x}'s \
             {address:#x}..{end:#x} as well, and its holder would be given that \
             function's registers",
        );
    }
}

/// Give up a slot: the device stops mastering the bus, then loses its
/// addresses, and only then do the pages behind them go back — and where
/// nothing reset it, the addresses are kept as the slot's [`RESIDUE`].
///
/// The order is what makes a dying driver safe. A page freed while the function
/// could still reach it is a device writing into memory the allocator has
/// already handed to somebody else.
pub fn release(slot: usize) {
    // Two statements, because edition 2021 keeps an `if let`'s scrutinee
    // temporaries alive to the end of its block: `BOUND[slot]`'s guard would be
    // held across the unmaps, the reset and `WATCHERS`.
    let bound = BOUND[slot].lock().take();
    if let Some(bound) = bound {
        tear_down(slot, bound);
    }
    // Unconditional and last: a hand-over refused inside [`bring_up`] bound
    // nothing and still holds its reservation, and the slot comes back only
    // once the function that was in it can no longer reach memory — or, with
    // a residue, stays that function's.
    let residue = !RESIDUE[slot].lock().is_empty();
    slot::release(&mut *SLOTS.lock(), slot, residue);
}

fn tear_down(slot: usize, mut bound: Bound) {
    bound.pci.disable_bus_master();
    match &bound.armed {
        Armed::Msix(entry) => entry.write_u32(msix::ENTRY_VECTOR_CONTROL, msix::ENTRY_MASKED),
        Armed::Msi => bound.pci.disable_msi(),
    }
    crate::iommu::note_user_owned(bound.pci.bus, bound.pci.dev, bound.pci.func, None);
    // **The reset before the domain gives anything back.** With mastering off
    // the function starts nothing new; its grants stay mapped until it is
    // quiet, so nothing it had already issued lands in a page the allocator
    // has handed on. The wait is the next claim's, never this teardown's: it
    // can run on the idle loop's drain.
    // Read before the reset returns it to its defaults.
    let kept = Kept::read(&bound.pci);
    let (how, resetting) = reset(&bound.pci);
    let grants = core::mem::take(&mut bound.grants);
    match resetting {
        Some(resetting) => {
            let who = requester(&bound.pci);
            {
                let mut machine = MACHINE.lock();
                match machine.resetting.iter_mut().find(|(id, _, _)| *id == who) {
                    Some(entry) => *entry = (who, resetting, kept),
                    None => machine.resetting.push((who, resetting, kept)),
                }
            }
            let parked = Retired { space: bound.space, grants, quiet_at: resetting.quiet_at() };
            let previous = RETIRED[slot].lock().replace(parked);
            assert!(previous.is_none(), "pcidev: slot {slot} was claimed with a retired holder left");
        }
        // Nothing reset it, so whatever it had taken in is still aimed at these
        // addresses; mastering is off, so it reaches none of them until the
        // next claim's first grant.
        None => {
            for grant in grants.iter() {
                if let Err(why) = bound.space.unmap(grant.at, grant.bytes) {
                    panic!("pcidev: slot {slot} could not take {:#x} back: {why}", grant.at);
                }
            }
            let mut residue = RESIDUE[slot].lock();
            assert!(residue.is_empty(), "pcidev: slot {slot} was claimed with a residue left untaken");
            *residue = grants.iter().map(|grant| Aimed { at: grant.at, bytes: grant.bytes }).collect();
        }
    }
    IRQ[slot].clear();
    WATCHERS[slot].lock().clear();
    log!(
        "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] released from slot {slot}; reset by {how}",
        bound.pci.bus,
        bound.pci.dev,
        bound.pci.func,
        bound.id.vendor,
        bound.id.device,
    );
    // Last: the rest of the claim goes with this drop.
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
        let phys = memory.phys().phys();
        // The pages are the allocator's own and 2 MiB, so a domain that cannot
        // take them is a kernel bug rather than the device's answer.
        let refused = |why| -> ! { panic!("pcidev: slot {slot} could not map a grant: {why}") };
        let (at, residual) = match bound.residue.iter().position(|aimed| aimed.bytes == span) {
            // The function may still be aimed here, so this holder is the one
            // whatever it had in flight lands on.
            Some(index) => {
                let aimed = bound.residue.remove(index);
                bound.space.map_at(aimed.at, phys, span).unwrap_or_else(|why| refused(why));
                (aimed.at, true)
            }
            None => (bound.space.map(phys, span).unwrap_or_else(|why| refused(why)), false),
        };
        let first = bound.grants.is_empty();
        bound.grants.push(Grant { memory: Arc::clone(&memory), at, bytes: span, residual });
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
        if grant.residual {
            bound.residue.push(Aimed { at: grant.at, bytes: grant.bytes });
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

/// The interrupts since the last read, or `None` for none; `Io` once the unit
/// has refused the function, which is the one answer its holder cannot take
/// for a quiet device.
pub fn take_record(slot: usize) -> Result<Option<DeviceIrqRecord>, SyscallError> {
    if IRQ[slot].faulted() {
        return Err(SyscallError::Io);
    }
    Ok(IRQ[slot].take().map(|count| DeviceIrqRecord { count }))
}

/// Whether a read of the claim answers at once: a message is waiting, or the
/// refusal is.
pub fn has_irq(slot: usize) -> bool {
    IRQ[slot].armed() || IRQ[slot].faulted()
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
        if !irq.take_pending() {
            continue;
        }
        // A fault's wake is no message.
        if !irq.faulted() && irq.take_unannounced() {
            log!(
                "pcidev: slot {slot} took its first message on vector {:#x}",
                VECTORS[slot]
            );
        }
        crate::inbox::Source::PciFunction(slot as u8).wake();
    }
}

/// The unit refused this function an access.
///
/// Called from the fault handler, which takes no lock: every call the claim
/// answers refuses from here on, its interrupt read included, and this CPU's
/// next scheduler pass wakes whoever waits on the claim to read that refusal —
/// the pass a message earns, posted the way its ISR posts it.
pub fn note_fault(slot: usize) {
    IRQ[slot].fault();
    crate::irq_ring::isr_publish(crate::irq_ring::IrqSource::UserDev, crate::clock::nanos_since_boot());
    crate::preempt::set_need_resched();
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
