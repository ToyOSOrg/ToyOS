//! One PCI function, driven by a process instead of by this kernel.
//!
//! The line through the device is **who can name an address**. This module
//! keeps config space — there is no write path to it from userland — puts the
//! function in an address space of its own at the unit *before* it enables bus
//! mastering, programs a message raising an interrupt of its own into whichever
//! of the function's two message mechanisms it has, translated for that
//! function alone and no other, and hands out every device address a descriptor
//! may carry. Nothing the holder writes into a descriptor can make the device
//! touch memory the kernel did not grant it: the domain maps the grants — the
//! claim's own, and the regions of ordinary memory its holder lends it
//! ([`dma_map`]) until [`dma_unmap`] or the claim's end — and nothing else, and
//! an address outside them is refused at the unit and recorded against that
//! claim.
//!
//! **What a holder lends is placed in a window of its function's domain
//! reserved for lending, [`LEND_WINDOW`] long, and nowhere else**, so
//! lending and taking back as often as a holder likes spends no address and no
//! remapping table past that window's, and a window with no room left is a
//! refusal ([`SyscallError::ResourceExhausted`]). An address in it is lent
//! again once it is taken back. Within a claim, what the function still had in
//! flight there lands in memory the same holder lent, which is authority it
//! already had over both regions; across claims, only once the function was
//! reset and is quiet ([`Retired`]), and never while the address is the
//! function's [`Function::residue`].
//!
//! **A BAR is handed over where firmware put it, in the 4 KiB pages it
//! covers**, and only where no other function decodes inside them
//! ([`pages::of_bar`]): this kernel moves no BAR, and of one it hands over
//! touches only the MSI-X entry it arms.
//!
//! **A BAR is handed over only inside the memory the root bridges are declared
//! to decode** ([`Apertures`]), because a load no bridge forwards does not
//! answer all-ones on real hardware: it does not complete, and the CPU cannot
//! be interrupted out of it. With no declaration held no function is handed
//! over.
//!
//! **A BAR that cannot be handed over is withheld, and its function still is**:
//! its holder is given no window for that BAR, and a function with none left
//! ends at [`Refusal::NoMappableBar`].
//!
//! **The BAR holding the MSI-X table is handed over like any other**: a claim
//! exists only where every message its function sends, whatever its holder
//! writes there or aims a DMA at, reaches only its own interrupt
//! ([`crate::iommu::remapping`]). **A function is armed on MSI only where a
//! walk that reached its capability list's terminator found no MSI-X.**
//!
//! **A function with no address space of its own is not handed over**, because
//! every grant would answer with a physical address and a descriptor holding
//! one is an arbitrary read and write over all of memory. **Nor is one whose
//! interrupts this machine does not remap**, because its message would be
//! compatibility format, which raises any vector on any CPU.
//!
//! **Every grant ends below the address limit its holder stated**
//! ([`dma_limit`]), and no grant or lend is made before one is: a function
//! that drops its upper address bits would issue a grant above its limit as
//! another grant's address.
//!
//! **A function masters the bus only when its holder starts it**
//! ([`dma_start`]), never at hand-over: what comes back from a process, or
//! from firmware, may still hold addresses and run queues. Its holder has the
//! registers first, with mastering off, to stop and reset the function by its
//! own means and lay out its grants; then it starts it. The kernel takes that
//! as no proof of anything: whatever the function then does reaches only its
//! domain.
//!
//! **A function no mechanism resets is still aimed at what its last holder
//! granted.** Nothing but that function's own driver can stop its queues, and
//! no register write retracts a transfer it had taken in before its holder
//! died: the start that lets it master again lets that transfer out.
//! So [`release`] takes those grants back like any other — mastering is off, so
//! the function reaches nothing until its next holder starts it — and keeps
//! only their device addresses, as the function's [`Function::residue`], for
//! the rest of the boot if it is never claimed again. A residue address is
//! never lent ([`Bound::lend_room`]) and never a fresh grant's, so the next
//! claim's grant of a range's size is fresh pages placed at that range; a range
//! it places nothing at stays unmapped, so a transfer aimed there faults, and
//! stays residue across its release until a release resets the function. No
//! page is ever two holders'.
//!
//! **That rests on the next holder laying its grants out as the last one did.**
//! The stale transfer — data and descriptor write-back — lands in whatever the
//! new holder keeps at that address, after it has set up its own rings or not:
//! harmless for a netstack succeeding a netstack, a silent write into its own
//! memory for a holder with another layout.
//!
//! **A reset returns a function's configuration to its defaults, BARs
//! included, and the next claim puts back what it needs**, its BARs where
//! firmware put them. [`release`] resets
//! the function by the first mechanism it advertises ([`reset`]) while the old
//! grants are still mapped; the function's next claim waits the reset out and
//! writes back what the reset cleared and [`bring_up`] does not write itself
//! ([`Kept`]) before it reads a register of the function.
//!
//! **A call names its function by its claim's [`Binding`] and never by the
//! function's number.** A function is the next claim's once its holder's
//! release is over, so a number carried from a claim to a call would be another
//! holder's claim by the time it was used. The binding is lent by the claim
//! under the lock its release takes it with (`object::Held`): a call runs
//! wholly before the release or finds no binding. Outside a claim's own mint
//! and release, only the two handlers name a function, by the interrupt and the
//! requester the hardware gave them.
//!
//! **A poll is on a function's watch only while its claim holds it.** It is
//! registered with the binding ([`add_poll`]), so before the release, whose
//! own answer to every poll on the watch comes before the function can be
//! claimed again; a registration that finds the claim released is ended by
//! its registrant (`DeviceClaim::add_poll`). So a function's next holder's
//! interrupt fires no poll of the last one's, in whatever ring.

/// No `crate::` reference, so `kernel-loom` compiles it and models the
/// interleaving no guest test lands on.
pub(crate) mod record;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use record::Interrupt;
use toyos_abi::boot::RootBridgeWindow;
use toyos_abi::inventory::Arrival;
use toyos_abi::pci::{DeviceIrqRecord, PciFunctionInfo, BARS, LEND_WINDOW};
use toyos_abi::syscall::{PciId, RegWidth, SyscallError};
use toyos_register::Register;
use toyos_pci::{af, aperture, bar, express, pages, pm};

use crate::device::{Claim, ClaimError};
use crate::drivers::pci::{arm_claimed_msi, arm_claimed_msix, mask_claimed_msix, NoCapability, NoEntry, PciDevice};
use crate::iommu::{IommuError, Limit, OwnSpace, Refused, Remapped};
use crate::mm::policy::CachePolicy;
use crate::mm::{align_2m, PAGE_2M};
use crate::object::shm::SharedMemObject;
use crate::sync::Lock;
use crate::inbox::PollEntry;
use crate::watch::IrqWatch;

/// The most one grant may be. A driver's rings and buffers are kilobytes to a
/// megabyte; this is room for a queue depth nothing in reach uses, and a bound
/// so a refused-but-plausible request is a refusal rather than the machine's
/// memory.
const MAX_GRANT_BYTES: u64 = 8 * 1024 * 1024;

/// And the most one claim may hold across every grant. What it lends is
/// bounded apart, by the window it is placed in ([`Space::lend`]).
const MAX_GRANT_TOTAL: u64 = 32 * 1024 * 1024;

/// A function's own configuration space, which is all a claim may read of it
/// (PCIe base spec §7.2.2: 4 KiB per function under ECAM).
const CONFIG_BYTES: u64 = 4096;

/// One enumerated function's claims, for the life of the boot: what its holder
/// drives, and what outlives a holder.
struct Function {
    who: u16,
    /// Its configuration space, which the reset's stop writes with no lock.
    pci: PciDevice,
    /// Whether a claim holds it: taken by [`claim`] before the function is
    /// touched, and given back last by [`release`].
    ///
    /// **This is where a function's exclusivity is decided.**
    /// `Claim::acquire`'s per-class flag does not answer for a class that
    /// names several devices, and the manifest's refusal of two matchers on
    /// one function is a host-side gate, not the capability boundary.
    held: AtomicBool,
    irq: Interrupt,
    /// What its holder's poll waits on: two processes each driving a function
    /// must not learn when the other's device is busy.
    watch: IrqWatch,
    /// Made on its first claim and kept, because a domain id is never given
    /// back (`arch/x86_64/vtd/domain.rs`). [`release`] empties it, so the next
    /// holder attaches to one that maps nothing.
    space: Lock<Option<Space>>,
    bound: Lock<Option<Bound>>,
    retired: Lock<Option<Retired>>,
    /// Where [`release`] could not reset it was left aimed: its last holder's
    /// grants, by address alone, until it is claimed again.
    residue: Lock<Vec<Aimed>>,
}

impl core::fmt::Display for Function {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PCI {:02x}:{:02x}.{}", self.who >> 8, (self.who >> 3) & 0x1f, self.who & 7)
    }
}

/// Exactly the functions [`publish`] took, in [`Machine::functions`]' order,
/// leaked once: null until then and never written again, so a handler reaches
/// a function's record with no lock. A function's place here is its
/// [`Arrival`]'s ([`arrival_of`]), which is therefore never another's.
static FUNCTIONS: AtomicPtr<&'static [Function]> = AtomicPtr::new(core::ptr::null_mut());

fn functions() -> &'static [Function] {
    let published = FUNCTIONS.load(Ordering::Acquire);
    if published.is_null() {
        return &[];
    }
    // SAFETY: non-null only as `publish` stored it: a leaked box, never freed
    // or written again, published with Release once it was whole.
    unsafe { *published }
}

/// Every function a claim holds, stopped before the machine resets or powers
/// off with its holder alive: Bus Master Enable cleared, which ends its DMA
/// and its messages, since a message is a write it masters (PCIe Base
/// §7.5.1.1); MSI Enable cleared (§7.7.1) and MSI-X's Function Mask set
/// (§7.7.2) as well. Registers alone, with no lock and no allocation: a panic
/// and a wedge reach it.
pub fn stop_before_reset() {
    for function in functions().iter().filter(|f| f.held.load(Ordering::Acquire)) {
        function.pci.disable_bus_master();
        function.pci.disable_msi();
        function.pci.mask_msix_function();
    }
}

/// The arrival of function `number`, counted from 1 so a zeroed record names
/// none.
fn arrival_of(number: usize) -> Arrival {
    Arrival(u32::try_from(number + 1).expect("pcidev: fewer functions than a u32 counts"))
}

/// Function `number`, which a claim or an interrupt named.
fn function(number: usize) -> &'static Function {
    functions().get(number).unwrap_or_else(|| panic!("pcidev: function {number} was never enumerated"))
}

/// A function's address space, and the room in it every holder's lent regions go.
#[derive(Clone, Copy)]
struct Space {
    space: OwnSpace,
    /// [`LEND_WINDOW`] of addresses handed out with the domain and never
    /// mapped by anything but [`dma_map`].
    lend: u64,
}

/// One granted buffer: the memory, and where the device reaches it.
struct Grant {
    /// Held so the pages outlive every handle the process had: they are freed
    /// by [`release`], after bus mastering is off and the domain has given the
    /// address back — or, for a [`Origin::Mapped`] region, by [`dma_unmap`].
    memory: Arc<SharedMemObject>,
    at: u64,
    bytes: u64,
    origin: Origin,
}

/// Where a grant's memory came from, which decides who may take it back.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// [`dma_alloc`]'s fresh pages, the claim's for its whole life. `residual`
    /// is a grant placed at a range of [`Bound::residue`]: taken back, the
    /// range returns there.
    Allocated { residual: bool },
    /// A region the holder already had, put in [`Space::lend`] by
    /// [`dma_map`]: never at a residue range, because what a stale transfer
    /// lands in there must be this holder's own fresh memory and not a region
    /// some other process still reads. Its address can itself become residue,
    /// and a [`dma_alloc`] grant of its size is then placed there like at any
    /// other — fresh pages, which is what residue is for.
    Mapped,
}

/// Device addresses a function's domain handed out and no longer maps.
#[derive(Clone, Copy)]
struct Aimed {
    at: u64,
    bytes: u64,
}

/// How a claimed function was made to speak, and the message it speaks with.
/// Both deliver the function's own interrupt into the same [`Interrupt`] and
/// the claim answers the same handle either way.
enum Armed {
    /// This function's MSI-X table, where it was found at arming.
    Msix(u64, Remapped),
    Msi(Remapped),
}

impl Armed {
    fn message(&self) -> &Remapped {
        match self {
            Self::Msix(_, message) | Self::Msi(message) => message,
        }
    }
}

/// What a live claim drives. The ISR never reads this.
struct Bound {
    pci: PciDevice,
    space: OwnSpace,
    /// [`Space::lend`].
    lend: u64,
    armed: Armed,
    id: PciId,
    /// Where firmware put each BAR its holder may map, and how much of it the
    /// function advertises; 0 bytes is a slot with no BAR this claim may map.
    bar_at: [u64; BARS],
    bar_bytes: [u64; BARS],
    grants: Vec<Grant>,
    /// The [`Function::residue`] this claim took over and has placed no grant at: mapped
    /// to nothing and counted against nothing, and where a grant of a range's
    /// size is placed.
    residue: Vec<Aimed>,
    /// What the holder stated the function issues ([`dma_limit`]); no grant
    /// or lend is made before it is.
    limit: Option<Limit>,
    /// Whether this function may issue a transaction yet: false until its
    /// holder starts it ([`dma_start`]), so a function carrying a previous
    /// holder's or firmware's queue addresses acts on none of them before its
    /// holder has stopped it.
    mastering: bool,
}

impl Bound {
    /// The first run of `span` in [`Space::lend`] that no grant and no residue
    /// range touches.
    fn lend_room(&self, span: u64) -> Option<u64> {
        let taken = |at: u64| {
            let end = at + span;
            let grants = self.grants.iter().map(|grant| (grant.at, grant.bytes));
            let residue = self.residue.iter().map(|aimed| (aimed.at, aimed.bytes));
            grants.chain(residue).any(|(from, bytes)| from < end && at < from + bytes)
        };
        (0..LEND_WINDOW / PAGE_2M)
            .map(|leaf| self.lend + leaf * PAGE_2M)
            .take_while(|at| at + span <= self.lend + LEND_WINDOW)
            .find(|at| !taken(*at))
    }

    /// Let the function master the bus, once.
    fn start_mastering(&mut self) {
        if !self.mastering {
            self.pci.start_bus_mastering();
            self.mastering = true;
        }
    }
}

/// Every function this machine enumerated.
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
    /// Every memory BAR every function decodes where firmware put it.
    ///
    /// Recorded in [`publish`] and never re-derived: reading a BAR's *size*
    /// means writing all-ones into it and reading back, with memory decode off
    /// for the length of the probe — safe before any driver `init` and nothing
    /// to do to a live function while a claim is being minted.
    decoded: Vec<Decoded>,
    /// Requester ids the kernel's own drivers bound. A claim on one of them
    /// would be two drivers on one device.
    kernel_driven: Vec<u16>,
    /// What this kernel holds of the memory the root bridges decode: the
    /// necessary condition for any BAR this module hands over.
    apertures: Apertures,
    /// Functions this module has reset, when each may be touched again (PCIe
    /// §6.6.2), and what the reset cleared that the next claim puts back. One
    /// entry per function ever released, so a re-claim cannot start reading a
    /// register the reset has not finished with.
    resetting: Vec<(u16, Resetting, Kept)>,
}

/// One memory BAR firmware assigned, as [`publish`] read it.
#[derive(Clone, Copy)]
struct Decoded {
    who: u16,
    index: u8,
    base: u64,
    /// What its sizing probe answered.
    bytes: Result<u64, bar::BadSize>,
}

impl Decoded {
    /// Where it decodes; at least one byte for a BAR that answers no size,
    /// since an unknown length is not an empty one.
    fn extent(&self) -> (u16, u64, u64) {
        (self.who, self.base, self.base.saturating_add(self.bytes.unwrap_or(0).max(1)))
    }
}

/// The memory the root bridges decode, as far as this kernel has been told.
///
/// **Only a declaration is ever read from**, so an address inside none of its
/// windows is never touched, and a function is handed over on no other state:
/// a machine that has not declared, or whose declaration was refused, is one
/// where nothing is known to be routed.
enum Apertures {
    Unknown,
    /// Never replaced.
    Declared(aperture::Declaration<Vec<RootBridgeWindow>>),
    Refused(aperture::Refused),
}

static MACHINE: Lock<Machine> = Lock::new(Machine {
    functions: Vec::new(),
    identities: Vec::new(),
    segment: 0,
    decoded: Vec::new(),
    kernel_driven: Vec::new(),
    apertures: Apertures::Unknown,
    resetting: Vec::new(),
});

/// One claim's hold on a function, from its reservation to the release its
/// drop is: what every call on the claim is made with.
///
/// Neither `Copy` nor `Clone` and made only by [`claim`], so there is one per
/// reservation, and a reference to it is a function no other claim holds.
pub struct Binding {
    /// Its index in [`functions`].
    number: usize,
}

impl Binding {
    fn function(&self) -> &'static Function {
        function(self.number)
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        release(self.number);
    }
}

/// Every function this machine enumerated, and who drives it now: a kernel
/// driver, a process holding a claim, or nobody.
pub fn inventory() -> Vec<toyos_abi::inventory::Pci> {
    use toyos_abi::inventory::Driven;
    let (identities, kernel_driven) = {
        let machine = MACHINE.lock();
        (machine.identities.clone(), machine.kernel_driven.clone())
    };
    let mut out = Vec::with_capacity(identities.len());
    for (mut pci, function) in identities.into_iter().zip(functions()) {
        pci.driven = if kernel_driven.contains(&function.who) {
            Driven::Kernel
        } else if function.held.load(Ordering::Acquire) {
            Driven::Claimed
        } else {
            Driven::Free
        };
        out.push(pci);
    }
    out
}

/// What the kernel enumerated at a function, for the `acpi` claim's holder's
/// configuration write (`toyos_userbound::firmware::ConfigAt::write`): `None`
/// where nothing answered. Held where a kernel driver drives it or a claim
/// holds it.
pub fn enumerated(bus: u8, dev: u8, func: u8) -> Option<toyos_userbound::firmware::Enumerated> {
    let who = ((bus as u16) << 8) | ((dev as u16) << 3) | func as u16;
    let (pci, driven) = {
        let machine = MACHINE.lock();
        let pci = *machine.identities.iter().find(|pci| requester_of(pci.at) == who)?;
        (pci, machine.kernel_driven.contains(&who))
    };
    let held = driven || functions().iter().any(|f| f.who == who && f.held.load(Ordering::Acquire));
    Some(toyos_userbound::firmware::Enumerated { vendor: pci.vendor, device: pci.device, class: pci.class, subclass: pci.subclass, held })
}

/// Every range a memory BAR of an enumerated function decodes, as `(start,
/// end)`, where firmware put it. What the `acpi` claim's holder is refused
/// (`toyos_userbound::firmware`), whoever drives the function. `f` runs under
/// [`MACHINE`]; the mediation takes `paging`'s record of windows inside it,
/// and nothing holding that record's lock takes this one.
pub fn with_bar_memory<T>(f: impl FnOnce(&mut dyn Iterator<Item = (u64, u64)>) -> T) -> T {
    let machine = MACHINE.lock();
    f(&mut machine.decoded.iter().map(|bar| {
        let (_, start, end) = bar.extent();
        (start, end)
    }))
}

/// The function a claim holds.
pub fn held_at(binding: &Binding) -> toyos_abi::inventory::PciAddr {
    addr_of(MACHINE.lock().segment, binding.function().who)
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

/// Take the enumeration, and record every memory BAR where firmware put it.
///
/// **Before any driver `init`**, because the sizing probe below takes memory
/// decode off the function it is probing for the length of the probe, and a
/// driver mid-transfer must not meet that.
pub fn publish(devices: &[PciDevice], segment: u16, firmware: &[RootBridgeWindow]) {
    let mut decoded = Vec::new();
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
                decoded.push(Decoded { who: requester(device), index, base: memory.address(), bytes: device.bar_size(index) });
            }
            // A 64-bit BAR's high half is the next register and is not a BAR:
            // decoding it would read an address out of address bits.
            index += if wide { 2 } else { 1 };
        }
    }
    let apertures = match aperture::check(firmware.to_vec()) {
        Ok(declaration) => {
            survey(declaration.windows(), &decoded);
            Apertures::Declared(declaration)
        }
        Err(why) => {
            log!("pcidev: the root bridges' memory is refused as declared: {why}, so no function is handed over");
            Apertures::Refused(why)
        }
    };
    log!("pcidev: {} functions", devices.len());
    let records: Vec<Function> = devices
        .iter()
        .map(|device| Function {
            who: requester(device),
            pci: *device,
            held: AtomicBool::new(false),
            irq: Interrupt::new(),
            watch: IrqWatch::new(),
            space: Lock::new(None),
            bound: Lock::new(None),
            retired: Lock::new(None),
            residue: Lock::new(Vec::new()),
        })
        .collect();
    let records: &'static [Function] = Box::leak(records.into_boxed_slice());
    let first = FUNCTIONS.compare_exchange(
        core::ptr::null_mut(),
        Box::leak(Box::new(records)),
        Ordering::Release,
        Ordering::Relaxed,
    );
    assert!(first.is_ok(), "pcidev: the functions were published twice");
    let mut machine = MACHINE.lock();
    machine.functions = devices.to_vec();
    machine.identities = devices
        .iter()
        .enumerate()
        .map(|(number, d)| d.identity(arrival_of(number), addr_of(segment, requester(d))))
        .collect();
    machine.segment = segment;
    machine.decoded = decoded;
    machine.apertures = apertures;
}

/// The root bridges' memory, where a declaration of it is held.
pub fn declared() -> Option<aperture::Declaration<Vec<RootBridgeWindow>>> {
    match &MACHINE.lock().apertures {
        Apertures::Declared(declaration) => Some(declaration.clone()),
        Apertures::Unknown | Apertures::Refused(_) => None,
    }
}

/// Say what memory firmware declared the root bridges decode, and every
/// memory BAR no claim could be given: one inside none of it, and one whose
/// pages another function decodes in, with how many of those there are.
fn survey(firmware: &[RootBridgeWindow], decoded: &[Decoded]) {
    let mut said = String::new();
    for window in firmware {
        if !said.is_empty() {
            said.push_str(", ");
        }
        let _ = write!(said, "mem {:#x}..{:#x}", window.base, window.end());
    }
    log!("pcidev: firmware declared root bridge memory: {said}");

    let mut withheld = 0;
    for bar in decoded {
        let (who, base, end) = bar.extent();
        if aperture::decode(firmware, base, end) == aperture::Decode::Unrouted {
            log!(
                "pcidev: requester {who:#06x}'s {base:#x}..{end:#x} is inside none of it, so no \
                 claim is handed it"
            );
        }
        if let Err(why) = pages::of_bar(base, end - base, others(decoded, who)) {
            withheld += 1;
            log!("pcidev: requester {who:#06x}'s BAR {} at {base:#x} is handed to no claim: {why}", bar.index);
        }
    }
    log!(
        "pcidev: {} memory BAR(s), {withheld} of them sharing a {}-byte page with another function \
         or starting inside one",
        decoded.len(),
        pages::PAGE,
    );
}

/// Every extent a function other than `who` decodes.
fn others(decoded: &[Decoded], who: u16) -> impl Iterator<Item = (u16, u64, u64)> + '_ {
    decoded.iter().filter(move |bar| bar.who != who).map(Decoded::extent)
}

/// Why a function could not be handed over. Carried rather than collapsed: one
/// message for all of them sends whoever reads the log looking in the wrong
/// place.
#[derive(Clone, Copy)]
enum Refusal {
    /// Nothing has declared the memory the root bridges decode yet.
    Undeclared,
    /// The declaration of the memory the root bridges decode was refused.
    AperturesRefused(aperture::Refused),
    NotRemapped(crate::iommu::NotRemapped),
    /// This machine remaps and translates, and not for this function.
    NotIsolated(&'static str),
    /// The machine translates messages, and none could be made for this function.
    NoMessage(Refused),
    NoInterrupt,
    MsixUnusable,
    CapsTruncated,
    Untranslated(IommuError),
    /// The function publishes no memory BAR.
    NoMappableBar,
    BarUnsizable(u8),
    /// The BAR no longer holds the address firmware put in it.
    BarMoved(u8),
    /// The address firmware assigned this BAR is inside no window firmware
    /// declared, so this kernel may not read it.
    BarUnrouted(u8),
    /// This BAR's pages are not its own ([`pages::of_bar`]).
    BarPages(u8, pages::Refused),
    /// With mastering stopped, its Transactions Pending stayed set past
    /// [`express::QUIESCE_NANOS`].
    StillTransacting,
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Undeclared => write!(
                f,
                "nothing has declared the memory the root bridges decode, so no address on this \
                 machine is known to be routed"
            ),
            Self::AperturesRefused(why) => write!(
                f,
                "the declaration of the memory the root bridges decode was refused ({why}), so no \
                 address on this machine is known to be routed"
            ),
            Self::NotRemapped(crate::iommu::NotRemapped::Unremapped) => write!(
                f,
                "its interrupts would not be remapped on this machine, and a message that is not \
                 remapped can raise any vector on any CPU"
            ),
            Self::NotRemapped(crate::iommu::NotRemapped::SmiUnfiltered) => write!(
                f,
                "an IOMMU unit on this machine filters no SMI, so a write its holder aimed at the \
                 interrupt range could send one to every CPU"
            ),
            Self::NotIsolated(why) => {
                write!(f, "no address space or message of its own can be made for it: {why}")
            }
            Self::NoMessage(why) => write!(f, "no message of its own could be made for it — {why}"),
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
            Self::NoMappableBar => write!(
                f,
                "it publishes no memory BAR this claim may map, so its holder would have no \
                 registers to drive it through"
            ),
            Self::BarUnsizable(i) => write!(f, "BAR {i} answers no size to bound a window by"),
            Self::BarMoved(i) => write!(
                f,
                "BAR {i} no longer holds the address firmware put in it, so the function does not \
                 decode where its holder would map it"
            ),
            Self::BarUnrouted(i) => write!(
                f,
                "BAR {i} holds an address inside no window firmware declared, so nothing says a \
                 read of it would come back"
            ),
            Self::BarPages(i, why) => write!(f, "BAR {i}: {why}"),
            Self::StillTransacting => write!(
                f,
                "with its mastering stopped, a request it had issued was still outstanding after {} ms, \
                 and it would land in a domain that maps none of it",
                express::QUIESCE_NANOS / 1_000_000
            ),
        }
    }
}

/// Claim the function that arrived as `arrival`, bring it up to the point a
/// driver takes over, and answer what that driver needs to know.
pub fn claim(arrival: Arrival) -> Result<(PciFunctionInfo, Claim), ClaimError> {
    let (number, pci, id, driven) = {
        let machine = MACHINE.lock();
        let number = machine.identities.iter().position(|pci| pci.arrival == arrival).ok_or(ClaimError::Absent)?;
        let (pci, identity) = (machine.functions[number], &machine.identities[number]);
        let driven = machine.kernel_driven.contains(&requester(&pci));
        (number, pci, PciId { vendor: identity.vendor, device: identity.device }, driven)
    };
    if driven {
        log!("pcidev: PCI {:02x}:{:02x}.{} is driven by this kernel and cannot be claimed", pci.bus, pci.dev, pci.func);
        return Err(ClaimError::KernelDriven);
    }

    // Before anything is taken or touched: with no declaration held, no
    // read this claim would make is known to reach the bus.
    let declared = match &MACHINE.lock().apertures {
        Apertures::Declared(declaration) => Ok(declaration.clone()),
        Apertures::Unknown => Err(Refusal::Undeclared),
        Apertures::Refused(why) => Err(Refusal::AperturesRefused(*why)),
    };
    let declaration = match declared {
        Ok(declaration) => declaration,
        Err(why) => {
            log!("pcidev: PCI {:02x}:{:02x}.{} NOT HANDED OVER — {why}", pci.bus, pci.dev, pci.func);
            return Err(ClaimError::Unusable);
        }
    };

    // The test and the take are one exchange: two claims arriving together
    // must not both find the function unheld.
    let held = &function(number).held;
    if held.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return Err(ClaimError::Owned);
    }
    // Dropped by every refusal below, which is what gives the function back.
    let binding = Binding { number };

    match bring_up(pci, id, number, declaration.windows()) {
        Ok(mut bound) => {
            bound.residue = take_residue(number);
            let info = PciFunctionInfo {
                bar_bytes: bound.bar_bytes,
                bus: pci.bus,
                dev: pci.dev,
                func: pci.func,
                _pad: [0; 5],
            };
            for (index, (at, bytes)) in bound.bar_at.iter().zip(bound.bar_bytes).enumerate().filter(|(_, (_, bytes))| *bytes != 0) {
                log!(
                    "pcidev: PCI {:02x}:{:02x}.{} BAR {index} ({bytes:#x} bytes) is handed over where \
                     firmware put it, {at:#x}..{:#x}",
                    pci.bus,
                    pci.dev,
                    pci.func,
                    at + bytes.next_multiple_of(pages::PAGE),
                );
            }
            log!(
                "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] handed over, {}",
                pci.bus,
                pci.dev,
                pci.func,
                id.vendor,
                id.device,
                bound.armed.message().interrupt(),
            );
            let function = binding.function();
            *function.bound.lock() = Some(bound);
            function.irq.clear();
            Ok((info, Claim::pci(binding)))
        }
        Err(why) => {
            log!(
                "pcidev: PCI {:02x}:{:02x}.{} NOT HANDED OVER — {why}",
                pci.bus,
                pci.dev,
                pci.func
            );
            match why {
                Refusal::NoMessage(refused) if refused.exhausted() => Err(ClaimError::Exhausted),
                _ => Err(ClaimError::Unusable),
            }
        }
    }
}

/// Choose the BARs its holder is given, give the function an address space,
/// arm the interrupt, stop its mastering and let what it had in flight finish,
/// then attach the domain. Its holder starts it ([`dma_start`]).
fn bring_up(pci: PciDevice, id: PciId, number: usize, windows: &[RootBridgeWindow]) -> Result<Bound, Refusal> {
    let remapping = crate::iommu::remapping().map_err(Refusal::NotRemapped)?;
    crate::iommu::isolates(&pci).map_err(Refusal::NotIsolated)?;

    // What the function's previous holder left mapped goes before anything
    // attaches to its domain, and whatever is left of a reset [`release`]
    // started on this function before a register of it is read (PCIe §6.6.2).
    finish_retired(number);
    settle_after_reset(&pci);

    // **Every refusal that can be taken without changing the function is
    // taken first**, so a hand-over that does not happen leaves the machine as
    // it found it. An address space holding this function's grants and nothing
    // else — and a refusal where this machine has none to give, because the
    // alternative is handing a process physical addresses to write into
    // descriptors.
    let Space { space, lend } = function_space(number).map_err(Refusal::Untranslated)?;
    let (bar_at, bar_bytes) = bars(&pci, windows)?;

    // Decode must be on for the table's BAR to take the entry, and for the
    // holder's registers to answer.
    pci.enable_memory_space();

    // Then the interrupt: a function neither mechanism can be armed on is one
    // no holder could ever be told anything about. A refusal here drops
    // `message`, which gives its translation and its interrupt back.
    let message = crate::iommu::claim_msi(remapping, number, &pci).map_err(Refusal::NoMessage)?;
    let armed = match arm_claimed_msix(&message) {
        Ok(table) => Armed::Msix(table, message),
        Err(NoEntry::Unusable) => return Err(Refusal::MsixUnusable),
        Err(NoEntry::NoTable(NoCapability::Truncated)) => return Err(Refusal::CapsTruncated),
        Err(NoEntry::NoTable(NoCapability::Absent)) if arm_claimed_msi(&message) => Armed::Msi(message),
        Err(NoEntry::NoTable(NoCapability::Absent)) => return Err(Refusal::NoInterrupt),
    };
    quiesce(&pci)?;
    space.attach(pci.bus, pci.dev, pci.func);
    Ok(Bound {
        pci,
        space,
        lend,
        armed,
        id,
        bar_at,
        bar_bytes,
        grants: Vec::new(),
        residue: Vec::new(),
        limit: None,
        mastering: false,
    })
}

/// Where firmware put each BAR this claim's holder may map, and its size,
/// read off what [`publish`] recorded and the registers, writing nothing.
///
/// A BAR whose pages are not its own, or that lies outside the root bridges'
/// declared memory, is withheld by name, and the function still handed over.
fn bars(pci: &PciDevice, windows: &[RootBridgeWindow]) -> Result<([u64; BARS], [u64; BARS]), Refusal> {
    let who = requester(pci);
    let decoded = MACHINE.lock().decoded.clone();
    let mut bar_at = [0u64; BARS];
    let mut bar_bytes = [0u64; BARS];
    for bar in decoded.iter().filter(|bar| bar.who == who) {
        let index = bar.index;
        let bytes = bar.bytes.map_err(|_| Refusal::BarUnsizable(index))?;
        // The register and not the record: a function that lost its address
        // since decodes nowhere its holder maps.
        if !matches!(pci.memory_bar(index), Ok(memory) if memory.address() == bar.base) {
            return Err(Refusal::BarMoved(index));
        }
        let handed = pages::of_bar(bar.base, bytes, others(&decoded, who))
            .map_err(|why| Refusal::BarPages(index, why))
            .and_then(|pages| match aperture::decode(windows, pages.at, pages.end()) {
                aperture::Decode::Inside(_) => Ok(pages),
                _ => Err(Refusal::BarUnrouted(index)),
            });
        match handed {
            Ok(_) => {
                bar_at[usize::from(index)] = bar.base;
                bar_bytes[usize::from(index)] = bytes;
            }
            Err(why) => log!(
                "pcidev: PCI {:02x}:{:02x}.{} keeps BAR {index} where firmware put it and hands it \
                 to nobody — {why}",
                pci.bus,
                pci.dev,
                pci.func
            ),
        }
    }
    // Its own refusal and not a BAR's: nothing about this machine's address
    // space is wrong.
    if bar_bytes.iter().all(|bytes| *bytes == 0) {
        return Err(Refusal::NoMappableBar);
    }
    Ok((bar_at, bar_bytes))
}

/// Function `number`'s address space, made on its first claim with what
/// firmware reserved for the function already mapped, so a function firmware
/// left running against that memory, a scan-out among them, keeps reaching it
/// across the attach.
fn function_space(number: usize) -> Result<Space, IommuError> {
    let function = function(number);
    let mut held = function.space.lock();
    match *held {
        Some(space) => Ok(space),
        None => {
            let who = function.who;
            let (space, lend) = OwnSpace::create(LEND_WINDOW, (who >> 8) as u8, ((who >> 3) & 0x1f) as u8, (who & 7) as u8)?;
            *held = Some(Space { space, lend });
            Ok(Space { space, lend })
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
/// decode and mastering bits and MSI or MSI-X. So this keeps:
///
/// - **every BAR**, where firmware put it, which is where [`bars`] hands it
///   over and where the MSI-X table's address is read from.
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
        for (index, bar) in self.bars.iter().enumerate().take(self.slots as usize) {
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
}

impl core::fmt::Display for Declined {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Absent => "no capability",
            Self::Unread => "the capability list ends at a forbidden link before it",
            Self::NoFlr => "no function level reset advertised",
            Self::NoSoftReset => "No_Soft_Reset set",
            Self::NotInD0 => "not in D0",
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
}

/// Stop a function mastering and let everything it already issued finish
/// before it moves onto a domain that maps none of those addresses. Firmware
/// may have left it running queues at physical addresses, and what lands after
/// the attach would fault against its holder.
///
/// Clearing Bus Master Enable stops new requests. A config read's completion
/// passes none of the function's earlier posted writes (PCIe Base §2.4.1), but
/// Non-Posted Requests, its DMA reads, are only known finished once Transactions
/// Pending clears: Device Status' (§7.5.3.5), or on a conventional function
/// the Advanced Features capability's, under the same bound as a hang ceiling,
/// conventional PCI having no Completion Timeout. A function with neither has
/// no pending signal: its posted writes are flushed, and a read it still had in
/// flight may fault in the new domain, against its holder; isolation holds,
/// since that domain maps only the function's own.
fn quiesce(pci: &PciDevice) -> Result<(), Refusal> {
    pci.disable_bus_master();
    pci.drain_messages();
    let pending: Box<dyn Fn() -> bool + '_> = match (pci.capability(express::CAP_ID), pci.capability(af::CAP_ID)) {
        (Err(NoCapability::Truncated), _) | (_, Err(NoCapability::Truncated)) => return Err(Refusal::CapsTruncated),
        (Ok(cap), _) => Box::new(move || express::pending(cap.read_u16(express::DEVICE_STATUS))),
        (Err(NoCapability::Absent), Ok(cap)) if af::reports_pending(cap.read_u8(af::AF_CAPABILITIES)) => {
            Box::new(move || af::pending(cap.read_u8(af::AF_STATUS)))
        }
        (Err(NoCapability::Absent), _) => return Ok(()),
    };
    let deadline = crate::clock::nanos_since_boot() + express::QUIESCE_NANOS;
    while pending() {
        if crate::clock::nanos_since_boot() >= deadline {
            return Err(Refusal::StillTransacting);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

fn wait_until(at: u64) {
    while crate::clock::nanos_since_boot() < at {
        core::hint::spin_loop();
    }
}

/// A released function's grants, still mapped in its domain until its
/// reset has finished: a write the function had already issued lands in the
/// released holder's own pages rather than faulting, or landing in pages the
/// allocator has handed somebody else.
struct Retired {
    space: OwnSpace,
    grants: Vec<Grant>,
    quiet_at: u64,
}

/// Unmap and free what a function's previous holder left mapped, once the
/// function can no longer write into it. Before anything else attaches to the
/// domain.
fn finish_retired(number: usize) {
    let function = function(number);
    let retired = function.retired.lock().take();
    let Some(retired) = retired else { return };
    wait_until(retired.quiet_at);
    for grant in retired.grants.iter() {
        if let Err(why) = retired.space.unmap(grant.at, grant.bytes) {
            panic!("pcidev: {function} could not take {:#x} back: {why}", grant.at);
        }
    }
    // The pages go back with this drop, after the unmap above.
    drop(retired);
}

/// The function's [`Function::residue`], for the claim that now holds it.
fn take_residue(number: usize) -> Vec<Aimed> {
    let function = function(number);
    let residue = core::mem::take(&mut *function.residue.lock());
    if !residue.is_empty() {
        let bytes: u64 = residue.iter().map(|aimed| aimed.bytes).sum();
        log!(
            "pcidev: {function} was left aimed at {} range(s), {bytes} bytes: a grant of a \
             range's size is placed there, on fresh pages",
            residue.len()
        );
    }
    residue
}

/// Give up a function: it stops mastering the bus, then loses its addresses,
/// and only then do the pages behind them go back — and where nothing reset
/// it, the addresses are kept as its [`Function::residue`].
///
/// The order is what makes a dying driver safe. A page freed while the function
/// could still reach it is a device writing into memory the allocator has
/// already handed to somebody else.
fn release(number: usize) {
    let function = function(number);
    // Two statements, because edition 2021 keeps an `if let`'s scrutinee
    // temporaries alive to the end of its block: the bound guard would be
    // held across the unmaps, the reset and the watch's answer.
    let bound = function.bound.lock().take();
    if let Some(bound) = bound {
        tear_down(function, bound);
    }
    // Unconditional and last: a hand-over refused inside [`bring_up`] bound
    // nothing and still holds its reservation, and the function comes back
    // only once it can no longer reach memory.
    function.held.store(false, Ordering::Release);
}

fn tear_down(function: &Function, mut bound: Bound) {
    bound.pci.disable_bus_master();
    let message = match bound.armed {
        Armed::Msix(table, message) => {
            mask_claimed_msix(table);
            message
        }
        Armed::Msi(message) => {
            bound.pci.disable_msi();
            message
        }
    };
    // Once the function no longer speaks, and before it can be claimed again:
    // its translation goes, and its interrupt is free.
    bound.pci.drain_messages();
    drop(message);
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
            let previous = function.retired.lock().replace(parked);
            assert!(previous.is_none(), "pcidev: {function} was claimed with a retired holder left");
        }
        // Nothing reset it, so whatever it had taken in is still aimed at these
        // addresses; mastering is off, so it reaches none of them until the
        // next holder starts it.
        None => {
            for grant in grants.iter() {
                if let Err(why) = bound.space.unmap(grant.at, grant.bytes) {
                    panic!("pcidev: {function} could not take {:#x} back: {why}", grant.at);
                }
            }
            let mut residue = function.residue.lock();
            assert!(residue.is_empty(), "pcidev: {function} was claimed with a residue left untaken");
            // With what this claim took over and placed nothing at: the function may
            // still be aimed there too, until something resets it.
            *residue = grants
                .iter()
                .map(|grant| Aimed { at: grant.at, bytes: grant.bytes })
                .chain(bound.residue.iter().copied())
                .collect();
        }
    }
    function.irq.clear();
    // The holder is gone, so a poll on it is answered rather than left for the next holder's interrupts.
    function.watch.cancel_polls();
    log!(
        "pcidev: PCI {:02x}:{:02x}.{} [{:04x}:{:04x}] released; reset by {how}",
        bound.pci.bus,
        bound.pci.dev,
        bound.pci.func,
        bound.id.vendor,
        bound.id.device,
    );
}

/// What every call a claim answers checks first, over what that claim bound.
fn with_bound<T>(
    binding: &Binding,
    f: impl FnOnce(&mut Bound) -> Result<T, SyscallError>,
) -> Result<T, SyscallError> {
    let function = binding.function();
    if function.irq.faulted() {
        return Err(SyscallError::Io);
    }
    let mut guard = function.bound.lock();
    // Bound before `claim` hands the binding out, and unbound only by its drop.
    f(guard.as_mut().expect("pcidev: a claim's binding has its function bound"))
}

/// One memory BAR as an object to map, a fresh one for every ask.
///
/// **The claim keeps where the window is and never an object over it**: an
/// object ends with its last handle, which its holder closes whenever it
/// likes, and the claim outlives that.
pub fn bar_object(binding: &Binding, index: u64) -> Result<Arc<SharedMemObject>, SyscallError> {
    with_bound(binding, |bound| {
        let index = usize::try_from(index).map_err(|_| SyscallError::InvalidArgument)?;
        if index >= BARS || bound.bar_bytes[index] == 0 {
            return Err(SyscallError::InvalidArgument);
        }
        // Registers, and the memory type is not firmware's to decide.
        Ok(SharedMemObject::aperture(
            bound.bar_at[index],
            bound.bar_bytes[index].next_multiple_of(pages::PAGE),
            CachePolicy::Uncacheable,
        )?)
    })
}

/// Memory this function may reach, and nothing else may.
pub fn dma_alloc(
    binding: &Binding,
    bytes: u64,
) -> Result<(Arc<SharedMemObject>, u64, u64), SyscallError> {
    let function = binding.function();
    with_bound(binding, |bound| {
        let limit = bound.limit.ok_or(SyscallError::InvalidArgument)?;
        if bytes == 0 || bytes > MAX_GRANT_BYTES {
            return Err(SyscallError::InvalidArgument);
        }
        let held: u64 = bound
            .grants
            .iter()
            .filter(|grant| grant.origin != Origin::Mapped)
            .map(|grant| grant.bytes)
            .sum();
        let span = align_2m(bytes as usize) as u64;
        if held + span > MAX_GRANT_TOTAL {
            return Err(SyscallError::ResourceExhausted);
        }
        let memory = SharedMemObject::create_run(&crate::mm::charge::current(), span)?;
        let (phys, _) = memory.run().expect("pcidev: a grant is one run");
        // The pages are the allocator's own and 2 MiB, so a domain that cannot
        // take them is a kernel bug rather than the device's answer.
        let refused = |why| -> ! { panic!("pcidev: {function} could not map a grant: {why}") };
        let (at, residual) = match bound.residue.iter().position(|aimed| aimed.bytes == span && aimed.at + span <= limit.end()) {
            // The function may still be aimed here, so this holder is the one
            // whatever it had in flight lands on.
            Some(index) => {
                let aimed = bound.residue.remove(index);
                bound.space.map_at(aimed.at, phys, span).unwrap_or_else(|why| refused(why));
                (aimed.at, true)
            }
            // A domain its claims have spent is the machine's limit and not
            // a kernel bug: the holder is refused, and the pages go back.
            None => match bound.space.map(phys, span, limit) {
                Ok(at) => (at, false),
                Err(IommuError::AddressesExhausted(_)) => return Err(SyscallError::ResourceExhausted),
                Err(why) => refused(why),
            },
        };
        let origin = Origin::Allocated { residual };
        bound.grants.push(Grant { memory: Arc::clone(&memory), at, bytes: span, origin });
        Ok((memory, at, span))
    })
}

/// The function issues device addresses of `bits` bits, as its holder states
/// it: once per claim and before any grant or lend, which are all placed
/// below it. `InvalidArgument` outside 32 to 64 bits, `AlreadyExists` for a
/// second statement.
pub fn dma_limit(binding: &Binding, bits: u64) -> Result<(), SyscallError> {
    let limit = u8::try_from(bits).ok().and_then(Limit::bits).ok_or(SyscallError::InvalidArgument)?;
    with_bound(binding, |bound| {
        if bound.limit.is_some() {
            return Err(SyscallError::AlreadyExists);
        }
        // `OwnSpace::create` hands the lend window out below 4 GiB, which
        // every limit reaches.
        assert!(bound.lend + LEND_WINDOW <= limit.end(), "pcidev: a lend window at {:#x} above 4 GiB", bound.lend);
        bound.limit = Some(limit);
        Ok(())
    })
}

/// Let the function master the bus, as its holder asks once it has stopped
/// and laid out what the function may reach; nothing on a second call.
pub fn dma_start(binding: &Binding) -> Result<(), SyscallError> {
    with_bound(binding, |bound| {
        bound.start_mastering();
        Ok(())
    })
}

/// Take a grant back, for a caller that could not be given a handle to it.
///
/// **A partial success is not left behind.** Without this the grant would stay
/// mapped in the function's domain and counted against [`MAX_GRANT_TOTAL`] with
/// nothing naming it, so a caller that hit a full handle table once would be
/// refused every later grant with no way back but dying.
///
/// Found by its memory and not by position: a [`dma_map`] on another thread of
/// the same holder can push between the grant and this, and the address the
/// caller was *told* is not always the address the grant is at.
pub fn dma_undo(binding: &Binding, memory: &Arc<SharedMemObject>) {
    let function = binding.function();
    let _ = with_bound(binding, |bound| {
        let Some(index) = bound.grants.iter().position(|grant| Arc::ptr_eq(&grant.memory, memory))
        else {
            return Ok(());
        };
        let grant = bound.grants.remove(index);
        if let Err(why) = bound.space.unmap(grant.at, grant.bytes) {
            panic!("pcidev: {function} could not take {:#x} back: {why}", grant.at);
        }
        if grant.origin == (Origin::Allocated { residual: true }) {
            bound.residue.push(Aimed { at: grant.at, bytes: grant.bytes });
        }
        Ok(())
    });
}

/// Put a region the holder already has into its function's domain, and answer
/// where the function reaches it and how much of it there is.
///
/// **The domain is still the whole gate.** What is added is exactly the
/// region's pages, in [`Space::lend`] where no grant and no residue is, so the
/// function reaches this memory, the claim's own grants and what this holder
/// lent it before, and nothing else. Only ordinary memory this kernel
/// allocated in one run qualifies ([`SharedMemObject::run`]): a BAR window mapped here
/// would be one device aimed at another's registers, and firmware's
/// framebuffer is not the kernel's to lend.
///
/// Bounded by [`LEND_WINDOW`] alone: a window with no run of the region's
/// length left free is a refusal.
pub fn dma_map(binding: &Binding, region: &Arc<SharedMemObject>) -> Result<(u64, u64), SyscallError> {
    let function = binding.function();
    with_bound(binding, |bound| {
        bound.limit.ok_or(SyscallError::InvalidArgument)?;
        let (phys, span) = region.run().ok_or(SyscallError::InvalidArgument)?;
        // One region, one address: a second mapping of it would be two grants
        // naming one set of pages, and taking either back would leave the
        // function aimed at what the other still says is lent.
        if bound.grants.iter().any(|grant| Arc::ptr_eq(&grant.memory, region)) {
            return Err(SyscallError::InvalidArgument);
        }
        let at = bound.lend_room(span).ok_or(SyscallError::ResourceExhausted)?;
        // Inside room the domain handed out with the function, and the pages are
        // the allocator's own whole 2 MiB ones: a refusal here is a kernel bug.
        if let Err(why) = bound.space.place(at, phys, span) {
            panic!("pcidev: {function} could not lend a region at {at:#x}: {why}");
        }
        bound.grants.push(Grant { memory: Arc::clone(region), at, bytes: span, origin: Origin::Mapped });
        Ok((at, span))
    })
}

/// Take back a region [`dma_map`] put at `at`.
///
/// The unmap invalidates the unit's cached translations before it returns
/// (`vtd::domain::unmap`), so from here an access the function still makes
/// there is refused at the unit and recorded against this claim — and only
/// after that is the region's `Arc` let go, so its pages can never be freed
/// under a translation that still names them.
///
/// **Answers the region, for the caller to let go with nothing held**: the
/// last `Arc` of one can run its zero-handle teardown, which takes other
/// objects' locks, and the caller holds its claim's.
pub fn dma_unmap(binding: &Binding, at: u64) -> Result<Arc<SharedMemObject>, SyscallError> {
    let function = binding.function();
    with_bound(binding, |bound| {
        let index = bound
            .grants
            .iter()
            .position(|grant| grant.at == at && grant.origin == Origin::Mapped)
            .ok_or(SyscallError::NotFound)?;
        let grant = bound.grants.remove(index);
        if let Err(why) = bound.space.unmap(grant.at, grant.bytes) {
            panic!("pcidev: {function} could not take {:#x} back: {why}", grant.at);
        }
        Ok(grant.memory)
    })
}

/// The window a claim's configuration reads are checked against, for the one
/// caller that turns an offset from userland into a [`Register`].
pub fn config_window(offset: u64, width: RegWidth) -> Result<Register, toyos_register::RefusedRegister> {
    toyos_register::register(offset, width.bytes(), CONFIG_BYTES)
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
pub fn config_read(binding: &Binding, at: Register, width: RegWidth) -> Result<u32, SyscallError> {
    with_bound(binding, |bound| {
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
pub fn take_record(binding: &Binding) -> Result<Option<DeviceIrqRecord>, SyscallError> {
    let function = binding.function();
    if function.irq.faulted() {
        return Err(SyscallError::Io);
    }
    let taken = function.irq.take();
    if taken.is_some() && function.irq.take_unannounced() {
        let bound = function.bound.lock();
        let bound = bound.as_ref().expect("pcidev: a claim's binding has its function bound");
        log!("pcidev: {function} took its first message on {}", bound.armed.message().interrupt());
    }
    Ok(taken)
}

/// Whether a read of the claim answers at once: a message is waiting, or the
/// refusal is.
pub fn has_irq(binding: &Binding) -> bool {
    let function = binding.function();
    function.irq.armed() || function.irq.faulted()
}

/// Records one message and posts the claim's watch. Called from the
/// interrupt's handler for function `number`, so it allocates nothing;
/// `record.rs` owns the counting, and `kernel-loom` models it against a
/// concurrent reader.
///
/// **The handler holds the preempt count raised across it**: the post lets go
/// of a count of its own, and at depth zero that would run a scheduler pass
/// inside the handler, before its end of interrupt.
pub fn isr(number: usize) {
    let function = function(number);
    assert!(crate::preempt::count() > 0, "pcidev: {function}'s interrupt arrived with the preempt count at zero");
    function.irq.took(crate::clock::nanos_since_boot());
    function.watch.post_in_place();
}

/// The unit refused requester `who` an access: `true` where a claim holds it.
///
/// Called from the fault handler, so it takes no lock: every call the claim
/// answers refuses from here on, its interrupt read included, and the post
/// wakes whoever waits on the claim to read that refusal, as a message's does.
pub fn note_fault(who: u16) -> bool {
    let Some(function) = held(who) else {
        return false;
    };
    function.irq.fault();
    function.watch.post_in_place();
    crate::preempt::set_need_resched();
    true
}

/// Requester `who`'s function, where a claim holds it; takes no lock.
fn held(who: u16) -> Option<&'static Function> {
    functions().iter().find(|function| function.who == who && function.held.load(Ordering::Acquire))
}

/// Whether a claim holds requester `who`; takes no lock.
pub fn is_held(who: u16) -> bool {
    held(who).is_some()
}

/// Register a poll on the watch of the function a claim holds.
pub(crate) fn add_poll(binding: &Binding, entry: PollEntry) {
    binding.function().watch.add_poll(entry);
}
