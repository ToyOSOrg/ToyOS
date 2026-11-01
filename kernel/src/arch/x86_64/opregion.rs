//! The IGD OpRegion's window on this architecture, whose direct map reaches
//! firmware's reserved memory: decided by `toyos_userbound::window`, its
//! header read through the direct map, and every 2 MiB leaf of it held to
//! the direct map's own cacheable type, so the aperture aliases no other.

use toyos_abi::pci::WindowRefused;
use toyos_userbound::window::{self, Window, OPREGION_HEADER};

use crate::mm::policy::CachePolicy;
use crate::mm::PAGE_2M;

/// The OpRegion at `asls`, or why it was refused.
pub fn window(asls: u32) -> Result<Window, WindowRefused> {
    let map = crate::mm::firmware_map();
    let mapped_end = crate::mm::direct_map_end().get();
    let decided = crate::pcidev::with_bar_memory(|bars| {
        super::paging::with_driven_windows(|driven| {
            window::opregion(map, mapped_end, driven.iter().copied().chain(bars), asls)
        })
    })?;
    // `map_mmio` retypes whole 2 MiB leaves, and `DRIVEN` holds exact
    // extents: a leaf a driven window shares would be another type here.
    let mut leaf = decided.start & !(PAGE_2M - 1);
    while leaf < decided.start + decided.bytes {
        if super::paging::direct_map_policy(leaf) != Some(CachePolicy::Normal) {
            return Err(WindowRefused::Device);
        }
        leaf += PAGE_2M;
    }
    let mut header = [0u8; OPREGION_HEADER];
    let at = crate::mm::DirectMap::from_phys(decided.start + decided.offset);
    for (n, byte) in header.iter_mut().enumerate() {
        // SAFETY: the policy passed every page of the window as firmware's
        // own, inside the direct map and in no memory this kernel hands out:
        // mapped for the machine's life, and no Rust object.
        *byte = unsafe { at.as_ptr::<u8>().add(n).read_volatile() };
    }
    window::opregion_header(&header)?;
    Ok(decided)
}
