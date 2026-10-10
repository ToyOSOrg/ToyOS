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

use crate::iommu::{AddressWidth, DomainId, IommuError, Iova, StreamId};
use crate::sync::Lock;

use super::table::{self, Domain};
use super::{TABLES, UNITS};

const FIRST: u16 = table::KERNEL_DOMAIN + 1;

/// What every enabled unit agreed on, which is what a domain can be built to.
enum Agreement {
    None,
    /// The width every unit reported, the smallest `CAP.ND` among them, and the
    /// smallest `CAP.MGAW` — the last two are minima because a domain has to
    /// hold on the narrowest unit that will ever translate for it.
    One(AddressWidth, u32, u8),
    Split,
}

struct Domains {
    agreement: Agreement,
    /// By id minus [`FIRST`]; never shrinks, since a released id would name a
    /// domain some unit may still have cached.
    live: Vec<Domain>,
    /// What no domain's addresses may reach: the root bridges' windows and
    /// the regions firmware reserved, as `(start, end)`.
    reserved: Vec<(u64, u64)>,
}

static DOMAINS: Lock<Domains> =
    Lock::new(Domains { agreement: Agreement::None, live: Vec::new(), reserved: Vec::new() });

/// Before any domain is made: every one is built clear of these.
pub fn avoid(reserved: Vec<(u64, u64)>) {
    DOMAINS.lock().reserved = reserved;
}

pub fn unit_agrees(width: AddressWidth, ceiling: u32, mgaw: u8) {
    let mut domains = DOMAINS.lock();
    domains.agreement = match domains.agreement {
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
    let mut domains = DOMAINS.lock();
    let (width, ceiling, mgaw) = match domains.agreement {
        Agreement::None => return Err(IommuError::NoUnit),
        Agreement::Split => return Err(IommuError::WidthsDisagree),
        Agreement::One(width, ceiling, mgaw) => (width, ceiling, mgaw),
    };
    let id = FIRST + domains.live.len() as u16;
    if u32::from(id) >= ceiling {
        return Err(IommuError::DomainsExhausted(ceiling));
    }
    let (domain, first) = Domain::new(&mut TABLES.lock(), id, width, mgaw, &domains.reserved, room)?;
    log!(
        "iommu: domain{id} root={:#x} aw={} mgaw={} addresses from {:#x} to {:#x}",
        domain.root().phys(),
        width.bits(),
        mgaw,
        domain.window().floor(),
        domain.window().ceiling()
    );
    domains.live.push(domain);
    Ok((DomainId::new(id), first))
}

pub fn map(id: DomainId, phys: u64, bytes: u64) -> Result<Iova, IommuError> {
    if !phys.is_multiple_of(crate::mm::PAGE_2M) {
        return Err(IommuError::Unaligned(phys));
    }
    let mut domains = DOMAINS.lock();
    let domain = domains.at(id);
    let at = domain.window_mut().reserve(bytes).ok_or(IommuError::AddressesExhausted(domain.window().ceiling()))?;
    let (did, domain) = (domain.id(), *domain);
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

/// Put `bytes` at `phys` at `at`, room this domain handed out before and whose
/// mapping was taken back: a device still aimed there reaches these pages.
/// Room it never handed out is a kernel bug, since [`map`] may yet hand it out.
pub fn map_at(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<(), IommuError> {
    let did = place(id, at, phys, bytes)?;
    log!(
        "iommu: domain{did} maps {:#x}..{:#x} at {:#x} again",
        phys,
        phys + bytes.next_multiple_of(crate::mm::PAGE_2M),
        at.raw(),
    );
    Ok(())
}

/// [`map_at`] without its record line, for a mapping a holder makes and
/// takes back as often as it likes: a line each would let one process flood
/// the record ring.
pub fn place(id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<u16, IommuError> {
    if !phys.is_multiple_of(crate::mm::PAGE_2M) {
        return Err(IommuError::Unaligned(phys));
    }
    let mut domains = DOMAINS.lock();
    let domain = *domains.at(id);
    assert!(
        domain.window().handed_out(at, bytes),
        "iommu: domain{} never handed out {:#x}+{bytes:#x}",
        domain.id(),
        at.raw()
    );
    let mut units = UNITS.lock();
    table::map(&mut TABLES.lock(), &domain, at, phys, bytes);
    for unit in units.iter_mut() {
        unit.invalidate_domain(domain.id());
    }
    Ok(domain.id())
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

pub fn attach(stream: StreamId, id: DomainId) {
    let mut domains = DOMAINS.lock();
    let domain = *domains.at(id);
    let mut units = UNITS.lock();
    for unit in units.iter_mut() {
        unit.attach(stream, &domain);
    }
    crate::iommu::fault::attached(stream, domain.id());
    log!("iommu: {stream} moves to domain{}", domain.id());
}

impl Domains {
    /// A `DomainId` only [`create`] mints, so the index is always in range.
    fn at(&mut self, id: DomainId) -> &mut Domain {
        &mut self.live[usize::from(id.raw() - FIRST)]
    }
}
