//! The domains a unit translates for, whatever the backend: their ids, each
//! one's device addresses, and which 2 MiB leaves a map or a place covers. A
//! backend owns the tables these name and the invalidation that publishes
//! them, and holds this under the lock it writes those under.

use alloc::vec::Vec;

use toyos_abi::acpi::RootBridgeWindow;

use crate::iommu::window::Window;
use crate::iommu::{DomainId, IommuError, Iova};
use crate::mm::PAGE_2M;

/// Every domain one backend made, by id from `first` up, each with the
/// backend's own name for its tables.
pub struct Domains<S> {
    first: u16,
    /// By id minus `first`; never shrinks, since a released id would name a
    /// domain some unit may still have cached.
    live: Vec<(S, Window)>,
    /// The regions firmware reserved for a device, as `(start, end)`, which
    /// no domain's addresses reach beside the declared windows
    /// ([`Window::new`]).
    reserved: Vec<(u64, u64)>,
}

/// A domain [`Domains::prepare`] admitted, not yet made.
pub struct Next {
    pub id: u16,
    pub addresses: Window,
    first: Iova,
}

impl<S: Copy> Domains<S> {
    pub const fn new(first: u16) -> Self {
        Self { first, live: Vec::new(), reserved: Vec::new() }
    }

    /// Before any domain is made: every one is built clear of these.
    pub fn avoid(&mut self, reserved: Vec<(u64, u64)>) {
        self.reserved = reserved;
    }

    /// The next domain's id and its window over `translatable` bits, with
    /// `room` bytes handed out first, clear of the root bridges' declared
    /// `windows`, `None` where no declaration is held; refused, with no id
    /// spent, where ids reach `ceiling` or the window would hold less.
    pub fn prepare(&self, translatable: u8, ceiling: u32, room: u64, windows: Option<&[RootBridgeWindow]>) -> Result<Next, IommuError> {
        let id = u16::try_from(usize::from(self.first) + self.live.len())
            .ok()
            .filter(|id| u32::from(*id) < ceiling)
            .ok_or(IommuError::DomainsExhausted(ceiling))?;
        let reserved: Option<Vec<(u64, u64)>> =
            windows.map(|windows| windows.iter().map(|w| (w.base, w.end())).chain(self.reserved.iter().copied()).collect());
        let (addresses, first) = Window::new(translatable, reserved.as_deref(), room)?;
        Ok(Next { id, addresses, first })
    }

    /// `next`, made over `space`, which `what` describes in its record.
    pub fn add(&mut self, next: Next, space: S, what: impl core::fmt::Display) -> (DomainId, Iova) {
        log!(
            "iommu: domain{} {what} addresses from {:#x} to {:#x}",
            next.id,
            next.addresses.floor(),
            next.addresses.ceiling()
        );
        self.live.push((space, next.addresses));
        (DomainId::new(next.id), next.first)
    }

    pub fn space(&self, id: DomainId) -> S {
        self.live[usize::from(id.raw() - self.first)].0
    }

    /// Room for `bytes` at `phys`, never handed out before: the domain's
    /// tables, and where the leaves go.
    pub fn map(&mut self, id: DomainId, phys: u64, bytes: u64) -> Result<(S, Iova), IommuError> {
        aligned(phys)?;
        // A `DomainId` only `add` mints, so the index is always in range.
        let (space, window) = &mut self.live[usize::from(id.raw() - self.first)];
        let at = window.reserve(bytes).ok_or(IommuError::AddressesExhausted(window.ceiling()))?;
        log!("iommu: domain{} maps {phys:#x}..{:#x} at {:#x}", id.raw(), phys + bytes.next_multiple_of(PAGE_2M), at.raw());
        Ok((*space, at))
    }

    /// The domain's tables, for `bytes` at `phys` placed at `at`, room this
    /// domain already handed out. Room it never handed out is a kernel bug,
    /// since [`Self::map`] may yet hand it out.
    pub fn place(&self, id: DomainId, at: Iova, phys: u64, bytes: u64) -> Result<S, IommuError> {
        aligned(phys)?;
        let (space, window) = &self.live[usize::from(id.raw() - self.first)];
        assert!(window.handed_out(at, bytes), "iommu: domain{} never handed out {:#x}+{bytes:#x}", id.raw(), at.raw());
        Ok(*space)
    }
}

fn aligned(phys: u64) -> Result<(), IommuError> {
    if phys.is_multiple_of(PAGE_2M) {
        Ok(())
    } else {
        Err(IommuError::Unaligned(phys))
    }
}

/// Each 2 MiB leaf `bytes` at `at` covers, as `(device address, physical
/// address)` of `phys` onwards.
pub fn leaves(at: Iova, phys: u64, bytes: u64) -> impl Iterator<Item = (u64, u64)> {
    (0..bytes).step_by(PAGE_2M as usize).map(move |offset| (at.raw() + offset, phys + offset))
}

/// The record a mapping put back where a device may still be aimed leaves.
pub fn placed_again(id: DomainId, at: Iova, phys: u64, bytes: u64) {
    log!(
        "iommu: domain{} maps {phys:#x}..{:#x} at {:#x} again",
        id.raw(),
        phys + bytes.next_multiple_of(PAGE_2M),
        at.raw()
    );
}
