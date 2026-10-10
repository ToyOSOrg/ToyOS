//! A device's address space: one context descriptor, tagged with an ASID of
//! its own, over stage 1 tables that map 2 MiB blocks
//! (`toyos_smmu::table`), and the device addresses it hands out, over the
//! unit's 48-bit input (`crate::iommu::window`).
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
use crate::iommu::window::Window;
use crate::iommu::{DomainId, IommuError, Iova, StreamId};
use crate::log;
use crate::mm::{PAGE_2M, PAGE_SIZE};

/// One 2 MiB block of memory at `phys`, which a device reads and writes.
fn block(phys: u64) -> Leaf {
    Leaf::Memory(Phys::<21>::new(phys).expect("SMMU: a 2 MiB-aligned page of memory"), Access::ReadWrite)
}

/// Domain ids start at 1: `DomainId` is never 0.
const FIRST: u16 = 1;

pub(super) struct Domain {
    asid: Asid,
    root: Phys<12>,
    context: Phys<6>,
    addresses: Window,
}

impl Live {
    fn domain(&mut self, id: DomainId) -> &mut Domain {
        &mut self.domains[usize::from(id.raw() - FIRST)]
    }

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

/// A new domain, with `room` bytes of it handed out first: refused, with no id
/// spent, where it would have less than that.
pub fn create(room: u64) -> Result<(DomainId, Iova), IommuError> {
    let doorbell = super::super::irqchip::its::doorbell_page();
    let mut held = UNIT.lock();
    let live = held.as_mut().ok_or(IommuError::NoUnit)?;
    let domains = if live.unit.asid(0x100).is_some() { 1 << 16 } else { 1 << 8 };
    let id = u16::try_from(usize::from(FIRST) + live.domains.len()).map_err(|_| IommuError::DomainsExhausted(domains))?;
    let asid = live.unit.asid(id).ok_or(IommuError::DomainsExhausted(domains))?;
    let (addresses, first) = Window::new(INPUT_BITS as u8, &live.reserved, room)?;
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
        assert!(page < addresses.floor(), "SMMU: the doorbell at {page:#x} is inside the addresses a domain hands out");
        live.put(root, page, Leaf::Doorbell(Phys::new(page).expect("SMMU: a 4 KiB page below 2^48")));
    }
    let domain = Domain { asid, root, context, addresses };
    log!(
        "iommu: domain{id} root={:#x} context={:#x} asid={} addresses from {:#x} to {:#x}, the doorbell {}",
        root.get(),
        context.get(),
        asid.get(),
        addresses.floor(),
        addresses.ceiling(),
        match doorbell {
            Some(page) => alloc::format!("page at {page:#x}"),
            None => "page nowhere: no ITS is armed".into(),
        },
    );
    live.domains.push(domain);
    Ok((DomainId::new(id), first))
}

pub fn map(id: DomainId, phys: u64, bytes: u64) -> Result<Iova, IommuError> {
    if !phys.is_multiple_of(PAGE_2M) {
        return Err(IommuError::Unaligned(phys));
    }
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let domain = live.domain(id);
    let at = domain.addresses.reserve(bytes).ok_or(IommuError::AddressesExhausted(domain.addresses.ceiling()))?;
    let root = domain.root;
    for offset in (0..bytes).step_by(PAGE_2M as usize) {
        live.put(root, at.raw() + offset, block(phys + offset));
    }
    log!("iommu: domain{} maps {phys:#x}..{:#x} at {:#x}", id.raw(), phys + bytes.next_multiple_of(PAGE_2M), at.raw());
    Ok(at)
}

/// Put `bytes` at `phys` at `at` again, room this domain handed out before
/// and whose mapping was taken back.
pub fn map_at(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    place(id, at, phys, bytes)?;
    log!("iommu: domain{} maps {phys:#x}..{:#x} at {:#x} again", id.raw(), phys + bytes.next_multiple_of(PAGE_2M), at.raw());
    Ok(())
}

/// [`map_at`] without its record line, for a mapping a holder makes and takes
/// back as often as it likes. Room never handed out is a kernel bug: [`map`]
/// may yet hand it out.
pub fn place(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<u16, IommuError> {
    if !phys.is_multiple_of(PAGE_2M) {
        return Err(IommuError::Unaligned(phys));
    }
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let domain = live.domain(id);
    assert!(domain.addresses.handed_out(at, bytes), "iommu: domain{} never handed out {:#x}+{bytes:#x}", id.raw(), at.raw());
    let root = domain.root;
    for offset in (0..bytes).step_by(PAGE_2M as usize) {
        live.put(root, at.raw() + offset, block(phys + offset));
    }
    Ok(id.raw())
}

/// Take `bytes` at `at` back: no device reaches them once this returns.
pub fn unmap(id: DomainId, at: Iova, bytes: u64) -> Result<(), IommuError> {
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let (root, asid) = (live.domain(id).root, live.domain(id).asid);
    let mut result = Ok(());
    for offset in (0..bytes).step_by(PAGE_2M as usize) {
        let here = at.raw() + offset;
        if !live.take(root, here) {
            result = Err(IommuError::NotMapped(Iova::translated(here)));
            break;
        }
    }
    // Whatever was cleared before a refusal is gone from the unit too.
    live.issue(&[Command::InvalidateAsid(asid)]);
    result
}

/// Move `function` onto domain `id`: translating through it, and only it,
/// once this returns.
pub fn attach(function: StreamId, id: DomainId) {
    let mut held = UNIT.lock();
    let live = held.as_mut().expect("a domain exists only on an armed unit");
    let stream = live.stream(function);
    let context = live.domain(id).context;
    let ste = Ste::stage1(context, &live.unit).expect("SMMU: a descriptor below the unit's output size");
    live.write_entry(stream, ste);
    crate::iommu::fault::attached(function, id.raw());
    log!("iommu: {function} moves to domain{}", id.raw());
}
