//! Address spaces a device is put in, and the invalidation that publishes a
//! change to one.
//!
//! Every layout and rule here is quoted from Intel VT-d Rev. 4.0, order number
//! D51397-015:
//!
//! - **Second-level entry**, 9.8 and Table 27: `R` bit 0, `W` bit 1, page-size
//!   bit 7 at a page-directory level, address 51:12; the walk ANDs `R`/`W` down
//!   the levels (3.7.1), so a leaf grants no more than what is above it.
//! - **Context entry**, 9.3 Figure 9-3: `P` bit 0, `T` 3:2 with `00b` naming
//!   the second-level table, `SLPTPTR` 51:12, `AW` 66:64 as levels minus two,
//!   `DID` 87:72.
//! - **Invalidation descriptors**, 6.5.2.1 Figure 6-8 and 6.5.2.2 Figure 6-9:
//!   context cache type `1h` with `G` 5:4 = `11b` device-selective, `DID`
//!   31:16, `SID` 47:32; IOTLB type `2h` with `G` = `10b` domain-selective,
//!   `DR` bit 7, `DW` bit 6. A context-entry change takes the first and then
//!   the second, in that order.
//! - **`CAP.CM`**, 6.1: with caching mode reported, software invalidates after
//!   *every* change, a mapping becoming present included.
//!
//! Lock order here is `DOMAINS`, `REMAP`, `UNITS`, `TABLES`, never the reverse.

use alloc::vec::Vec;

use crate::iommu::{AddressWidth, DomainId, IommuError, Iova, StreamId};
use crate::sync::Lock;

use super::table::{self, Domain};
use super::{TABLES, UNITS};

/// The first id a driver's domain can take: `table::KERNEL_DOMAIN` is the
/// identity domain every driver that has not moved is still on.
const FIRST: u16 = table::KERNEL_DOMAIN + 1;

/// What every enabled unit agreed on, which is what a domain can be built to.
enum Agreement {
    None,
    /// The width every unit reported, and the smallest `CAP.ND` among them.
    One(AddressWidth, u32),
    Split,
}

struct Domains {
    agreement: Agreement,
    /// Indexed by id minus [`FIRST`]; never shrinks, since a released id would
    /// name a domain some unit may still have cached.
    live: Vec<Domain>,
}

static DOMAINS: Lock<Domains> =
    Lock::new(Domains { agreement: Agreement::None, live: Vec::new() });

pub fn unit_agrees(width: AddressWidth, ceiling: u32) {
    let mut domains = DOMAINS.lock();
    domains.agreement = match domains.agreement {
        Agreement::None => Agreement::One(width, ceiling),
        Agreement::One(seen, cap) if seen == width => {
            Agreement::One(width, cap.min(ceiling))
        }
        _ => Agreement::Split,
    };
}

pub fn create() -> Result<DomainId, IommuError> {
    let mut domains = DOMAINS.lock();
    let (width, ceiling) = match domains.agreement {
        Agreement::None => return Err(IommuError::NoUnit),
        Agreement::Split => return Err(IommuError::WidthsDisagree),
        Agreement::One(width, ceiling) => (width, ceiling),
    };
    let id = FIRST + domains.live.len() as u16;
    if u32::from(id) >= ceiling {
        return Err(IommuError::DomainsExhausted(ceiling));
    }
    let domain = Domain::new(&mut TABLES.lock(), id, width);
    log!(
        "iommu: domain{id} root={:#x} aw={} addresses from {:#x}",
        domain.root().phys(),
        width.bits(),
        domain.floor()
    );
    domains.live.push(domain);
    Ok(DomainId::new(id))
}

pub fn map(id: DomainId, phys: u64, bytes: u64) -> Result<Iova, IommuError> {
    if !phys.is_multiple_of(crate::mm::PAGE_2M) {
        return Err(IommuError::Unaligned(phys));
    }
    let mut domains = DOMAINS.lock();
    let domain = domains.at(id);
    let at = domain
        .reserve(bytes)
        .ok_or(IommuError::AddressesExhausted(domain.width().bits()))?;
    let (did, domain) = (domain.id(), *domain);
    // `CAP.CM` is set on every unit here, so a mapping is not live until the
    // absence the unit may have cached is invalidated.
    let mut units = UNITS.lock();
    table::map(&mut TABLES.lock(), &domain, at, phys, bytes);
    for unit in units.iter_mut() {
        unit.invalidate_domain(did);
    }
    log!(
        "iommu: domain{did} maps {:#x}..{:#x} at {:#x}",
        phys,
        phys + bytes.next_multiple_of(crate::mm::PAGE_2M),
        at.raw(),
    );
    Ok(at)
}

pub fn unmap(id: DomainId, at: Iova, bytes: u64) -> Result<(), IommuError> {
    let mut domains = DOMAINS.lock();
    let domain = *domains.at(id);
    let mut units = UNITS.lock();
    table::unmap(&domain, at, bytes)?;
    for unit in units.iter_mut() {
        unit.invalidate_domain(domain.id());
    }
    Ok(())
}

/// Rewrite `stream`'s context entry in every unit, then its caches in Section
/// 6.5.2.1's order: context cache, then IOTLB.
pub fn attach(stream: StreamId, id: DomainId) {
    let mut domains = DOMAINS.lock();
    let domain = *domains.at(id);
    let mut units = UNITS.lock();
    for unit in units.iter_mut() {
        table::bind(&mut TABLES.lock(), unit.root(), stream, &domain);
        unit.invalidate_context(domain.id(), stream.requester());
    }
    super::fault::attached(stream, domain.id());
    log!("iommu: {stream} moves to domain{}", domain.id());
}

impl Domains {
    /// A `DomainId` only [`create`] mints, so the index is always in range.
    fn at(&mut self, id: DomainId) -> &mut Domain {
        &mut self.live[usize::from(id.raw() - FIRST)]
    }
}
