//! The firmware's framebuffer, as the loader handed it over: what a
//! `framebuffer` claim maps.

use crate::device::Screen;
use crate::mm::policy::{CachePolicy, MmioPolicy};
use crate::mm::align_2m_checked;
use crate::log;

/// `addr` is the physical address of the framebuffer supplied by firmware.
/// `None` is a scanout no process can be given: one is handed over in 2 MiB
/// pages, and the architecture says whether this one's are its own.
pub fn init(
    addr: u64,
    size: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: u32,
    edid: ([u8; toyos_abi::boot::EDID_BLOCK], u32),
) -> Option<Screen> {
    if let Err(why) = crate::mm::paging::scanout_whole_pages(addr, size) {
        log!("GOP: the scanout at {addr:#x}+{size:#x} is refused: {why}");
        return None;
    }
    // A size smaller than stride*height*4 maps less than the compositor writes.
    // Boot-time firmware data has no actionable error path, so this panics rather than returning one.
    let needed = stride as u64 * height as u64 * 4;
    assert!(
        size >= needed,
        "GOP: firmware reports a {size}-byte framebuffer for {width}x{height} stride={stride}, \
         which needs {needed}"
    );
    // A wrapped 2 MiB round-up would register a too-small region while writes continue at the full size.
    let aligned_size = align_2m_checked(size)
        .unwrap_or_else(|| panic!("GOP: firmware reports a {size}-byte framebuffer"));
    // SDM Vol. 3A §11.12.4: one physical page can't hold two memory types, so this
    // must match the cache policy used for the client mapping below.
    crate::mm::paging::map_mmio(addr, aligned_size, MmioPolicy::WriteCombining);

    let scanout = crate::device::Scanout { phys: addr, size: aligned_size };
    log!("GOP: {}x{} stride={} format={} at {:#x}",
        width, height, stride, pixel_format, addr);

    // Reads the cache policy actually installed, not the one requested.
    let memory_type = crate::mm::paging::scanout_memory_type(addr, aligned_size);
    let installed = crate::mm::paging::direct_map_policy(addr).expect("GOP: the scanout is mapped");
    assert!(
        installed == CachePolicy::WriteCombining,
        "GOP: the scanout is mapped {installed:?}"
    );
    log!("GOP: scanout memory type {memory_type}");

    Some(Screen {
        info: toyos_abi::FramebufferInfo {
            // Minted per claim by `try_claim`: the process reading it does not
            // exist yet.
            scanout: toyos_abi::HANDLE_INVALID,
            width,
            height,
            stride,
            pixel_format,
            edid: edid.0,
            edid_len: edid.1,
        },
        scanout,
    })
}
