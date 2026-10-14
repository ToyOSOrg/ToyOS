//! Address spaces a device is put in, and the invalidation that publishes a
//! change to one.
//!
//! Every layout and rule here is quoted from Intel VT-d Rev. 4.0, D51397-015.
//! **Second-level entry**, 9.8 and Table 27: `R` bit 0, `W` bit 1, page-size bit
//! 7 at a page-directory level, address 51:12, and the walk ANDs `R`/`W` down
//! the levels (3.7.1). **Context entry**, 9.3 Figure 9-3: `P` bit 0, `T` 3:2 =
//! `00b` naming that table, `SLPTPTR` 51:12, `AW` 66:64 as levels minus two,
//! `DID` 87:72. **Invalidation**, §6.5.2.1 Figure 6-8 and §6.5.2.2 Figure 6-9:
//! context cache type `1h`, `G` 5:4 = `11b` device-selective, `DID` 31:16, `SID`
//! 47:32; IOTLB type `2h`, `G` = `10b` domain-selective, `DR` bit 7, `DW` bit 6;
//! a context-entry change takes the first and then the second. **`CAP.CM`**,
//! 6.1: with caching mode reported, software invalidates after *every* change,
//! a mapping becoming present included.
//!
//! Lock order here is `DOMAINS`, `REMAP`, `UNITS`, `TABLES`, never the reverse.

use crate::log;
use alloc::vec::Vec;

use crate::iommu::domains::{self, Domains};
use crate::iommu::{AddressWidth, DomainId, IommuError, Iova, StreamId};
use crate::sync::Lock;

use super::table::{self, Space};
use super::{TABLES, UNITS};

/// What every enabled unit agreed on, which is what a domain can be built to.
enum Agreement {
    None,
    /// The width every unit reported, the smallest `CAP.ND` among them, and the
    /// smallest `CAP.MGAW` — the last two are minima because a domain has to
    /// hold on the narrowest unit that will ever translate for it.
    One(AddressWidth, u32, u8),
    Split,
}

struct Vtd {
    agreement: Agreement,
    domains: Domains<Space>,
}

static DOMAINS: Lock<Vtd> =
    Lock::new(Vtd { agreement: Agreement::None, domains: Domains::new(table::KERNEL_DOMAIN + 1) });

/// Before any domain is made: every one is built clear of these.
pub fn avoid(reserved: Option<Vec<(u64, u64)>>) {
    DOMAINS.lock().domains.avoid(reserved);
}

pub fn unit_agrees(width: AddressWidth, ceiling: u32, mgaw: u8) {
    let mut held = DOMAINS.lock();
    held.agreement = match held.agreement {
        Agreement::None => Agreement::One(width, ceiling, mgaw),
        Agreement::One(seen, cap, seen_mgaw) if seen == width => {
            Agreement::One(width, cap.min(ceiling), seen_mgaw.min(mgaw))
        }
        _ => Agreement::Split,
    };
}

/// A new domain, with `room` bytes of it handed out first: refused, with no id
/// spent, where the domain would have less than that.
pub fn create(room: u64) -> Result<(DomainId, Iova), IommuError> {
    let mut held = DOMAINS.lock();
    let (width, ceiling, mgaw) = match held.agreement {
        Agreement::None => return Err(IommuError::NoUnit),
        Agreement::Split => return Err(IommuError::WidthsDisagree),
        Agreement::One(width, ceiling, mgaw) => (width, ceiling, mgaw),
    };
    let next = held.domains.prepare(table::translatable_bits(width, mgaw), ceiling, room)?;
    let space = Space { root: TABLES.lock().alloc(), width };
    let what = alloc::format!("root={:#x} aw={} mgaw={mgaw}", space.root.phys(), width.bits());
    Ok(held.domains.add(next, space, what))
}

pub fn map(id: DomainId, phys: u64, bytes: u64) -> Result<Iova, IommuError> {
    let mut held = DOMAINS.lock();
    let (space, at) = held.domains.map(id, phys, bytes)?;
    put(id, space, at, phys, bytes);
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
/// takes back as often as it likes: a line each would let one process flood
/// the record ring.
pub fn place(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    let held = DOMAINS.lock();
    let space = held.domains.place(id, at, phys, bytes)?;
    put(id, space, at, phys, bytes);
    Ok(())
}

/// Under `DOMAINS`: the leaves, then every unit's cache of the domain.
fn put(id: DomainId, space: Space, at: Iova, phys: u64, bytes: u64) {
    let mut units = UNITS.lock();
    let mut tables = TABLES.lock();
    for (at, phys) in domains::leaves(at, phys, bytes) {
        table::put(&mut tables, space, at, phys);
    }
    for unit in units.iter_mut() {
        unit.invalidate_domain(id.raw());
    }
}

/// Take `bytes` at `at` back; whatever was cleared before a refusal is gone
/// from every unit too.
pub fn unmap(id: DomainId, at: Iova, bytes: u64) -> Result<(), IommuError> {
    let held = DOMAINS.lock();
    let space = held.domains.space(id);
    let mut units = UNITS.lock();
    let result = domains::leaves(at, 0, bytes)
        .try_for_each(|(here, _)| if table::take(space, here) { Ok(()) } else { Err(IommuError::NotMapped(Iova::translated(here))) });
    for unit in units.iter_mut() {
        unit.invalidate_domain(id.raw());
    }
    result
}

pub fn attach(stream: StreamId, id: DomainId) {
    let held = DOMAINS.lock();
    let space = held.domains.space(id);
    let mut units = UNITS.lock();
    for unit in units.iter_mut() {
        unit.attach(stream, id.raw(), space);
    }
    crate::iommu::fault::attached(stream, id.raw());
    log!("iommu: {stream} moves to domain{}", id.raw());
}
