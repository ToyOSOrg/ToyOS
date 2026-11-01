//! Address spaces a requester is put in: four-level host tables mapping
//! 2 MiB pages (`toyos_amdvi::table`), tagged with a domain id of their own,
//! over the device addresses `iommu::domains` hands out.
//!
//! Every change is published before it returns: a leaf put in or taken out
//! by `INVALIDATE_IOMMU_PAGES` for the domain, since a unit may keep a
//! translation that faulted (§2.1.3), and a requester moved by
//! `INVALIDATE_DEVTAB_ENTRY` and then the pages of the domain it left
//! (§2.4.2), each behind a completion wait (§2.4.1).
//!
//! A requester an IVMD names stays on the identity domain: firmware declares
//! memory it must keep reaching, which a domain of its own would not map.
//!
//! `DOMAINS` is taken before `UNITS` or `TABLES`, and those two are never held together.

use alloc::vec::Vec;

use crate::iommu::domains::{self, Domains};
use crate::iommu::{DomainId, IommuError, Iova, Limit, StreamId};
use crate::log;
use crate::sync::Lock;
use toyos_acpi::Requesters;
use toyos_amdvi::dte::{Dte, Setting};
use toyos_amdvi::table::{directory, entry, index, leaf, Entry, LEAF_LEVEL, ROOT_LEVEL};
use toyos_phys::Phys as Address;

use super::{memory, submit, Tables, TABLES};
use toyos_amdvi::queue::Command;

/// The id every blocked entry is tagged with, which Table 8 requires valid
/// wherever `V` is set; no translating entry uses it.
pub(super) const BLOCKED: u16 = 0;
/// The identity domain's id: every enumerated function until a driver moves it.
pub(super) const IDENTITY: u16 = 1;

/// What a translating entry names: its domain's root table.
#[derive(Clone, Copy)]
pub struct Space {
    root: Address<12>,
}

struct Amd {
    domains: Domains<Space>,
    /// Firmware's DTE setting for each requester id, which an entry keeps
    /// whichever domain it names; [`Setting::CLAIMED`] from a function's first
    /// claim on, for the boot.
    settings: Vec<Setting>,
    /// The requesters an IVMD names.
    kept: Vec<Requesters>,
    /// The ids a programmed unit serves, as inclusive ranges.
    translated: Vec<(u16, u16)>,
    /// Requester ids the platform speaks as, and why no claim may have one.
    platform: Vec<(u16, &'static str)>,
}

static DOMAINS: Lock<Option<Amd>> = Lock::new(None);

/// Before any domain is made, once a unit will translate: every one is built
/// clear of `reserved`, `kept`'s requesters stay on the identity domain, and
/// only a requester in `translated` is given one.
pub(super) fn start(
    reserved: Option<Vec<(u64, u64)>>,
    settings: Vec<Setting>,
    kept: Vec<Requesters>,
    translated: Vec<(u16, u16)>,
    platform: Vec<(u16, &'static str)>,
) {
    let mut domains = Domains::new(IDENTITY + 1);
    domains.avoid(reserved);
    *DOMAINS.lock() = Some(Amd { domains, settings, kept, translated, platform });
}

/// Why no claim may have `stream`, if none may: [`untranslated`]'s reasons,
/// and a requester id the platform speaks as, an I/O APIC's, an HPET's, or
/// one whose SMIs the unit's filter passes.
pub fn unclaimable(stream: StreamId) -> Option<&'static str> {
    untranslated(stream).or_else(|| {
        let id = stream.requester();
        DOMAINS.lock().as_ref()?.platform.iter().find(|(platform, _)| *platform == id).map(|(_, why)| *why)
    })
}

/// What requester `id`'s entry carries of firmware's declaration.
pub(super) fn setting(id: u16) -> Setting {
    DOMAINS.lock().as_ref().expect("amdvi: an entry is written only once a unit translates").settings[usize::from(id)]
}

/// Requester `id` is driven by a process: from now on its entry passes
/// nothing firmware declared and gives no `SysMgt`, whichever domain it names.
pub(super) fn claimed(id: u16) {
    let mut held = DOMAINS.lock();
    let amd = held.as_mut().expect("amdvi: a claim exists only once a unit translates");
    amd.settings[usize::from(id)] = Setting::CLAIMED;
}

/// Why `stream` keeps the machine's own addresses rather than a domain of its
/// own, if it does: no programmed unit serves it, so nothing would translate
/// one; or an IVMD names it, so firmware reserves memory it must keep
/// reaching, which the identity domain maps and a domain of its own would not.
fn untranslated(stream: StreamId) -> Option<&'static str> {
    let held = DOMAINS.lock();
    let amd = held.as_ref()?;
    let id = stream.requester();
    if !amd.translated.iter().any(|&(first, last)| (first..=last).contains(&id)) {
        Some("no unit this kernel programmed serves it")
    } else if keeps_named(amd, id) {
        Some("firmware reserves memory it must keep reaching")
    } else {
        None
    }
}

/// A new domain, with `room` bytes of it handed out first: refused, with no id
/// spent, where the domain would have less than that.
pub fn create(room: u64) -> Result<(DomainId, Iova), IommuError> {
    let mut held = DOMAINS.lock();
    let amd = held.as_mut().ok_or(IommuError::NoUnit)?;
    // Mode-4 host tables translate 48 bits of device address (48882 §2.2.3).
    let next = amd.domains.prepare(9 * ROOT_LEVEL + 12, 1 << 16, room)?;
    let space = Space { root: TABLES.lock().alloc() };
    Ok(amd.domains.add(next, space, alloc::format!("root={:#x} mode={ROOT_LEVEL}", space.root.get())))
}

pub fn map(id: DomainId, phys: u64, bytes: u64, limit: Limit) -> Result<Iova, IommuError> {
    let mut held = DOMAINS.lock();
    let amd = held.as_mut().expect("amdvi: a domain exists only once a unit translates");
    let (space, at) = amd.domains.map(id, phys, bytes, limit)?;
    place_leaves(id, space, at, phys, bytes);
    Ok(at)
}

/// Put `bytes` at `phys` at `at`, room this domain handed out before and whose
/// mapping was taken back: a device still aimed there reaches these pages.
pub fn map_at(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    place(id, at, phys, bytes)?;
    domains::placed_again(id, at, phys, bytes);
    Ok(())
}

/// [`map_at`] without its record line, for a mapping a holder makes and
/// takes back as often as it likes.
pub fn place(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    let held = DOMAINS.lock();
    let amd = held.as_ref().expect("amdvi: a domain exists only once a unit translates");
    let space = amd.domains.place(id, at, phys, bytes)?;
    place_leaves(id, space, at, phys, bytes);
    Ok(())
}

/// Under `DOMAINS`: the leaves, then every unit's cache of the domain.
fn place_leaves(id: DomainId, space: Space, at: Iova, phys: u64, bytes: u64) {
    {
        let mut tables = TABLES.lock();
        for (at, phys) in domains::leaves(at, phys, bytes) {
            put(&mut tables, space.root, at, phys);
        }
    }
    submit(&[Command::InvalidateDomain(id.raw())]);
}

/// Take `bytes` at `at` back; whatever was cleared before a refusal is gone
/// from every unit too.
pub fn unmap(id: DomainId, at: Iova, bytes: u64) -> Result<(), IommuError> {
    let held = DOMAINS.lock();
    let amd = held.as_ref().expect("amdvi: a domain exists only once a unit translates");
    let space = amd.domains.space(id);
    let result = domains::leaves(at, 0, bytes)
        .try_for_each(|(here, _)| if take(space.root, here) { Ok(()) } else { Err(IommuError::NotMapped(Iova::translated(here))) });
    submit(&[Command::InvalidateDomain(id.raw())]);
    result
}

/// Moves `stream` onto domain `id`, its translation half replaced in one
/// write (§2.2.2.2): translating through it, and only it, once this returns.
pub fn attach(stream: StreamId, id: DomainId) {
    let held = DOMAINS.lock();
    let amd = held.as_ref().expect("amdvi: a domain exists only once a unit translates");
    let requester = stream.requester();
    assert!(
        amd.translated.iter().any(|&(first, last)| (first..=last).contains(&requester)) && !keeps_named(amd, requester),
        "amdvi: {stream} was moved to a domain of its own, which `untranslated` refuses it"
    );
    let space = amd.domains.space(id);
    let slot = super::entry(requester);
    let old = [slot.read_u64(0), slot.read_u64(8)];
    let new = Dte::translated(space.root, id.raw(), amd.settings[usize::from(requester)]).words();
    super::super::iommu_unit::exchange16(slot, old, new);
    submit(&[Command::InvalidateDevice(requester), Command::InvalidateDomain(Dte::domain_of(old))]);
    crate::iommu::fault::attached(stream, id.raw());
    log!("iommu: {stream} moves to domain{}", id.raw());
}

fn keeps_named(amd: &Amd, id: u16) -> bool {
    amd.kept.iter().any(|named| match *named {
        Requesters::All => true,
        Requesters::One(one) => one == id,
        Requesters::Range { first, last } => (first..=last).contains(&id),
    })
}

/// The 8-byte entry `index` of the table at `table`.
fn slot(table: Address<12>, index: usize) -> crate::mm::Mmio {
    memory(table.get() + index as u64 * 8, 8)
}

/// The 2 MiB leaf at `at` under `root` reads and writes `phys`, its tables
/// grown on the way.
pub(super) fn put(tables: &mut Tables, root: Address<12>, at: u64, phys: u64) {
    let mut table = root;
    for level in (LEAF_LEVEL + 1..=ROOT_LEVEL).rev() {
        let here = slot(table, index(at, level));
        table = match entry(here.read_u64(0)) {
            Entry::Directory(below) => below,
            Entry::Absent => {
                let below = tables.alloc();
                here.write_u64(0, directory(below, level));
                below
            }
            Entry::Leaf => panic!("amdvi: {at:#x}'s level {level} entry is a page, where a table goes"),
        };
    }
    let here = slot(table, index(at, LEAF_LEVEL));
    // A live leaf here is memory a holder still reaches: the caller takes a
    // mapping back before it puts another at its address.
    assert!(entry(here.read_u64(0)) == Entry::Absent, "amdvi: {at:#x} was still mapped when a new leaf was written there");
    here.write_u64(0, leaf(Address::new(phys).expect("amdvi: a 2 MiB-aligned page below 2^48")));
}

/// Clears the leaf at `at`; `false` where none is mapped.
fn take(root: Address<12>, at: u64) -> bool {
    let mut table = root;
    for level in (LEAF_LEVEL + 1..=ROOT_LEVEL).rev() {
        match entry(slot(table, index(at, level)).read_u64(0)) {
            Entry::Directory(below) => table = below,
            Entry::Absent | Entry::Leaf => return false,
        }
    }
    let here = slot(table, index(at, LEAF_LEVEL));
    if entry(here.read_u64(0)) != Entry::Leaf {
        return false;
    }
    here.write_u64(0, 0);
    true
}
