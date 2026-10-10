//! A device's address space: one context descriptor, tagged with an ASID of
//! its own, over stage 1 tables that map 2 MiB blocks and nothing else
//! (`toyos_smmu::table`), and the device addresses it hands out.
//!
//! A domain's addresses start a quarter of the way up the 48-bit input, above
//! all memory, so a descriptor still carrying one names nothing a domain maps
//! by accident; they end below the first root-bridge window over that, which
//! a bridge may route peer-to-peer before the unit sees the request. An
//! address is handed out once: a device holding a stale one reaches whatever
//! took its place, so nothing takes it.
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
use crate::iommu::{DomainId, IommuError, Iova, StreamId};
use crate::log;
use crate::mm::{PAGE_2M, PAGE_SIZE};

/// Domain ids start at 1: `DomainId` is never 0.
const FIRST: u16 = 1;

/// A quarter of the way up the input.
const FLOOR: u64 = 1 << (INPUT_BITS - 2);

pub(super) struct Domain {
    asid: Asid,
    root: Phys<12>,
    context: Phys<6>,
    addresses: Addresses,
}

/// A domain's device addresses, from [`FLOOR`] to its ceiling.
#[derive(Clone, Copy)]
struct Addresses {
    ceiling: u64,
    /// The first not yet handed out.
    next: u64,
}

impl Addresses {
    /// `bytes`, rounded up to whole leaves, of addresses never handed out.
    const fn reserve(&mut self, bytes: u64) -> Option<Iova> {
        let Some(end) = bytes.checked_next_multiple_of(PAGE_2M) else { return None };
        let Some(end) = self.next.checked_add(end) else { return None };
        if end > self.ceiling {
            return None;
        }
        let at = Iova::translated(self.next);
        self.next = end;
        Some(at)
    }

    /// Whether `bytes` at `at` is room [`Self::reserve`] handed out, starting
    /// on a leaf it could have returned.
    const fn handed_out(&self, at: Iova, bytes: u64) -> bool {
        if !at.raw().is_multiple_of(PAGE_2M) || at.raw() < FLOOR {
            return false;
        }
        match bytes.checked_next_multiple_of(PAGE_2M) {
            Some(span) => match at.raw().checked_add(span) {
                Some(end) => end <= self.next,
                None => false,
            },
            None => false,
        }
    }
}

/// Where a domain's addresses end: under the input, and under the first of
/// `reserved` reaching above [`FLOOR`]; at or below it where one covers it.
const fn ceiling(reserved: &[(u64, u64)]) -> u64 {
    let mut ceiling = 1 << INPUT_BITS;
    let mut i = 0;
    while i < reserved.len() {
        let (start, end) = reserved[i];
        if end > FLOOR && start < ceiling {
            ceiling = start;
        }
        i += 1;
    }
    ceiling
}

/// [`Addresses`] and [`ceiling`] at their boundaries, at compile time: the
/// binary has no test harness.
const _: () = {
    let mut one = Addresses { ceiling: FLOOR + 2 * PAGE_2M, next: FLOOR };
    assert!(matches!(one.reserve(1), Some(at) if at.raw() == FLOOR));
    assert!(one.handed_out(Iova::translated(FLOOR), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR - PAGE_2M), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR + PAGE_2M), PAGE_2M));
    assert!(!one.handed_out(Iova::translated(FLOOR), PAGE_2M + 1));
    assert!(!one.handed_out(Iova::translated(FLOOR + 1), 0));
    assert!(!one.handed_out(Iova::translated(u64::MAX - PAGE_2M + 1), PAGE_2M));
    // Past the ceiling, and past the end of the address space, nothing.
    assert!(one.reserve(PAGE_2M + 1).is_none());
    assert!(one.reserve(u64::MAX).is_none());
    assert!(matches!(one.reserve(PAGE_2M), Some(at) if at.raw() == FLOOR + PAGE_2M));
    assert!(one.reserve(1).is_none());
    // `virt`'s windows, all below the floor, leave the whole input.
    assert!(ceiling(&[(0x1000_0000, 0x3f00_0000), (0x80_0000_0000, 0x100_0000_0000)]) == 1 << INPUT_BITS);
    // A window over the floor ends the domain where it starts; one across it leaves nothing.
    assert!(ceiling(&[(FLOOR + 4 * PAGE_2M, FLOOR + 8 * PAGE_2M)]) == FLOOR + 4 * PAGE_2M);
    assert!(ceiling(&[(FLOOR - PAGE_2M, FLOOR + PAGE_2M)]) < FLOOR);
};

impl Live {
    fn domain(&mut self, id: DomainId) -> &mut Domain {
        &mut self.domains[usize::from(id.raw() - FIRST)]
    }

    /// One 2 MiB block of `phys` at `at`, its tables grown on the way.
    fn put(&mut self, root: Phys<12>, at: u64, phys: u64) {
        let block = Phys::<21>::new(phys).expect("SMMU: a 2 MiB-aligned page of memory");
        let path = plan(at, Leaf::Memory(block, Access::ReadWrite), &self.unit).expect("SMMU: a handed-out address");
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
    let mut held = UNIT.lock();
    let live = held.as_mut().ok_or(IommuError::NoUnit)?;
    let domains = if live.unit.asid(0x100).is_some() { 1 << 16 } else { 1 << 8 };
    let id = u16::try_from(usize::from(FIRST) + live.domains.len()).map_err(|_| IommuError::DomainsExhausted(domains))?;
    let asid = live.unit.asid(id).ok_or(IommuError::DomainsExhausted(domains))?;
    let top = crate::mm::pmm::top();
    if FLOOR <= top {
        return Err(IommuError::WindowBelowMemory { translatable: INPUT_BITS as u8, floor: FLOOR, top });
    }
    let ceiling = ceiling(&live.reserved);
    let mut addresses = Addresses { ceiling, next: FLOOR };
    // At least one leaf, whatever was asked: a domain with none is no domain.
    let first = match addresses.reserve(room) {
        Some(first) if ceiling >= FLOOR + PAGE_2M => first,
        _ => return Err(IommuError::NoRoom { floor: FLOOR, ceiling, room }),
    };
    let root = Phys::new(live.memory.alloc(PAGE_SIZE)).expect("SMMU: a table below the unit's output size");
    let context = Phys::new(live.memory.alloc(64)).expect("SMMU: a descriptor below the unit's output size");
    let words = Cd::new(root, asid, &live.unit).expect("SMMU: a table below the unit's output size").words();
    let descriptor = window(context.get(), 64);
    // `V` is in the first doubleword, so it goes last.
    for (i, word) in words.iter().enumerate().skip(1) {
        descriptor.write_u64(8 * i as u64, *word);
    }
    descriptor.write_u64(0, words[0]);
    let domain = Domain { asid, root, context, addresses };
    log!(
        "iommu: domain{id} root={:#x} context={:#x} asid={} addresses from {FLOOR:#x} to {ceiling:#x}",
        root.get(),
        context.get(),
        asid.get()
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
    let at = domain.addresses.reserve(bytes).ok_or(IommuError::AddressesExhausted(domain.addresses.ceiling))?;
    let root = domain.root;
    for offset in (0..bytes).step_by(PAGE_2M as usize) {
        live.put(root, at.raw() + offset, phys + offset);
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
        live.put(root, at.raw() + offset, phys + offset);
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
    super::fault::attached(stream, id.raw());
    log!("iommu: {function} moves to domain{}", id.raw());
}
