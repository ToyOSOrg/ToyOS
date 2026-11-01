//! A device's address space: one context descriptor, tagged with an ASID of
//! its own, over stage 1 tables that map 2 MiB blocks
//! (`toyos_smmu::table`), and the device addresses it hands out, over the
//! unit's 48-bit input (`crate::iommu::domains`).
//!
//! **And the ITS's doorbell, the one 4 KiB page at its own address**: the unit
//! translates a function's message as it does any write it makes, so a domain
//! without that page is one whose function's messages it refuses. The page
//! holds `GITS_TRANSLATER` and nothing else a device may write, and below
//! memory it is no address a domain hands out.
//!
//! A new leaf needs no invalidation — the unit caches no translation that
//! faulted — and a removed one is invalidated by the domain's ASID, behind a
//! `CMD_SYNC`, before [`unmap`] returns.

use toyos_phys::Phys;
use toyos_smmu::config::{Cd, Ste};
use toyos_smmu::queue::Command;
use toyos_smmu::table::{entry, next, plan, Access, Entry, Leaf, INPUT_BITS};
use toyos_smmu::Asid;

use super::{window, Live, UNIT};
use crate::iommu::domains;
use crate::iommu::{DomainId, IommuError, Iova, Limit, StreamId};
use crate::log;
use crate::mm::PAGE_SIZE;

/// One 2 MiB block of memory at `phys`, which a device reads and writes.
fn block(phys: u64) -> Leaf {
    Leaf::Memory(Phys::<21>::new(phys).expect("SMMU: a 2 MiB-aligned page of memory"), Access::ReadWrite)
}

/// Domain ids start at 1: `DomainId` is never 0.
pub(super) const FIRST: u16 = 1;

#[derive(Clone, Copy)]
pub(super) struct Space {
    asid: Asid,
    root: Phys<12>,
    context: Phys<6>,
}

impl Live {
    /// `leaf` at `at`, its tables grown on the way.
    fn put(&mut self, root: Phys<12>, at: u64, leaf: Leaf) {
        let path = plan(at, leaf, &self.unit).expect("SMMU: a handed-out address");
        let mut table = root;
        let (leaf, walk) = path.indices().split_last().expect("a path names at least its leaf's table");
        for (level, index) in walk.iter().enumerate() {
            let slot = window(table.get(), PAGE_SIZE).subregion(*index as u64 * 8, 8);
            table = match entry(slot.read_u64(0), level) {
                Entry::Table(below) => below,
                Entry::Invalid => {
                    let below = Phys::new(self.memory.alloc(PAGE_SIZE)).expect("SMMU: a table below the unit's output size");
                    slot.write_u64(0, next(below, &self.unit).expect("SMMU: a table below the unit's output size"));
                    below
                }
                Entry::Mapped => panic!("SMMU: {at:#x}'s level {level} entry is a block, where a table goes"),
            };
        }
        let slot = window(table.get(), PAGE_SIZE).subregion(*leaf as u64 * 8, 8);
        // A live leaf here is memory a holder still reaches: the caller takes
        // a mapping back before it puts another at its address.
        assert!(entry(slot.read_u64(0), walk.len()) == Entry::Invalid, "SMMU: {at:#x} was still mapped when a new leaf was written there");
        slot.write_u64(0, path.descriptor);
    }

    /// Clear the 2 MiB block at `at`; `false` where nothing is mapped there.
    fn take(&mut self, root: Phys<12>, at: u64) -> bool {
        let any = Phys::<21>::new(0).expect("0 is aligned");
        let Some(path) = plan(at, Leaf::Memory(any, Access::Read), &self.unit) else { return false };
        let mut table = root;
        let (leaf, walk) = path.indices().split_last().expect("a path names at least its leaf's table");
        for (level, index) in walk.iter().enumerate() {
            match entry(window(table.get(), PAGE_SIZE).read_u64(*index as u64 * 8), level) {
                Entry::Table(below) => table = below,
                Entry::Invalid | Entry::Mapped => return false,
            }
        }
        let slot = window(table.get(), PAGE_SIZE).subregion(*leaf as u64 * 8, 8);
        if entry(slot.read_u64(0), walk.len()) != Entry::Mapped {
            return false;
        }
        slot.write_u64(0, 0);
        true
    }
}

/// What firmware reserved for a function, mapped at its own addresses: no
/// firmware table this kernel reads on AArch64 reserves any.
pub fn keep_reserved(_id: DomainId, _function: StreamId) {}

/// Why `function` keeps the machine's own addresses rather than a domain of
/// its own: never, since nothing this kernel reads of the IORT says so.
pub fn untranslated(_function: StreamId) -> Option<&'static str> {
    None
}

/// Whether `function`, given no domain of its own, reaches nothing: the IORT
/// routes it through the armed unit, whose stream table aborts every stream
/// no claim gave a domain.
pub fn aborts_undomained(function: StreamId) -> bool {
    UNIT.lock().as_ref().is_some_and(|live| live.routes.iter().any(|(stream, ..)| *stream == function))
}

/// Why no claim may have `function`: it arrived after the unit was
/// programmed, and its IORT route was read only for the functions enumerated
/// then.
pub fn unclaimable(function: StreamId) -> Option<&'static str> {
    let arrived = UNIT.lock().as_ref().is_some_and(|live| !live.enumerated.contains(&function));
    arrived.then_some("it arrived after the SMMUv3 was programmed, and its IORT route is read for the functions enumerated then")
}

/// A new domain, with `room` bytes of it handed out first, clear of the root
/// bridges' declared `windows`, `None` where none are held: refused, with no
/// id spent, where it would have less than that.
pub fn create(room: u64, windows: Option<&[toyos_abi::acpi::RootBridgeWindow]>) -> Result<(DomainId, Iova), IommuError> {
    let doorbell = super::super::irqchip::its::doorbell_page();
    let mut held = UNIT.lock();
    let live = held.as_mut().ok_or(IommuError::NoUnit)?;
    let ceiling = if live.unit.asid(0x100).is_some() { 1 << 16 } else { 1 << 8 };
    let next = live.domains.prepare(INPUT_BITS as u8, ceiling, room, windows)?;
    let asid = live.unit.asid(next.id).expect("SMMU: an id below the unit's ASIDs");
    let root = Phys::new(live.memory.alloc(PAGE_SIZE)).expect("SMMU: a table below the unit's output size");
    let context = Phys::new(live.memory.alloc(64)).expect("SMMU: a descriptor below the unit's output size");
    let words = Cd::new(root, asid, &live.unit).expect("SMMU: a table below the unit's output size").words();
    let descriptor = window(context.get(), 64);
    // `V` is in the first doubleword, so it goes last.
    for (i, word) in words.iter().enumerate().skip(1) {
        descriptor.write_u64(8 * i as u64, *word);
    }
    descriptor.write_u64(0, words[0]);
    if let Some(page) = doorbell {
        assert!(
            !next.addresses.allowed().iter().any(|&(start, end)| start <= page && page < end),
            "SMMU: the doorbell at {page:#x} is inside the addresses a domain hands out"
        );
        live.put(root, page, Leaf::Doorbell(Phys::new(page).expect("SMMU: a 4 KiB page below 2^48")));
    }
    let what = alloc::format!(
        "root={:#x} context={:#x} asid={} the doorbell {}",
        root.get(),
        context.get(),
        asid.get(),
        match doorbell {
            Some(page) => alloc::format!("page at {page:#x},"),
            None => "page nowhere: no ITS is armed,".into(),
        },
    );
    Ok(live.domains.add(next, Space { asid, root, context }, what))
}

pub fn map(id: DomainId, phys: u64, bytes: u64, limit: Limit) -> Result<Iova, IommuError> {
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let (space, at) = live.domains.map(id, phys, bytes, limit)?;
    for (at, phys) in domains::leaves(at, phys, bytes) {
        live.put(space.root, at, block(phys));
    }
    Ok(at)
}

/// Put `bytes` at `phys` at `at` again, room this domain handed out before
/// and whose mapping was taken back.
pub fn map_at(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    place(id, at, phys, bytes)?;
    domains::placed_again(id, at, phys, bytes);
    Ok(())
}

/// [`map_at`] without its record line, for a mapping a holder makes and takes
/// back as often as it likes.
pub fn place(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let space = live.domains.place(id, at, phys, bytes)?;
    for (at, phys) in domains::leaves(at, phys, bytes) {
        live.put(space.root, at, block(phys));
    }
    Ok(())
}

/// Take `bytes` at `at` back: no device reaches them once this returns.
pub fn unmap(id: DomainId, at: Iova, bytes: u64) -> Result<(), IommuError> {
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let space = live.domains.space(id);
    let result = domains::leaves(at, 0, bytes)
        .try_for_each(|(here, _)| if live.take(space.root, here) { Ok(()) } else { Err(IommuError::NotMapped(Iova::translated(here))) });
    // Whatever was cleared before a refusal is gone from the unit too.
    live.issue(&[Command::InvalidateAsid(space.asid)]);
    result
}

/// Move `function` onto domain `id`: translating through it, and only it,
/// once this returns.
pub fn attach(function: StreamId, id: DomainId) {
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let stream = live.stream(function);
    let ste = Ste::stage1(live.domains.space(id).context, &live.unit).expect("SMMU: a descriptor below the unit's output size");
    live.write_entry(stream, ste);
    crate::iommu::fault::attached(function, id.raw());
    log!("iommu: {function} moves to domain{}", id.raw());
}
