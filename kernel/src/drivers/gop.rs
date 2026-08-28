use alloc::boxed::Box;

use toyos_abi::syscall::SyscallError;

use crate::arch::{mtrr, pat};
use crate::mm::paging::CachePolicy;
use crate::mm::{PAGE_2M, align_2m_checked, DirectMap};
use crate::gpu::{Gpu, GpuInfo};
use crate::log;
use crate::object::shm::Region;

struct GopGpu;

impl Gpu for GopGpu {
    fn present_rect(&mut self, _x: u32, _y: u32, _w: u32, _h: u32) {
        // GOP framebuffer is memory-mapped — writes are immediately visible.
    }

    fn set_cursor(&mut self, _hot_x: u32, _hot_y: u32) {}
    fn move_cursor(&mut self, _x: u32, _y: u32) {}

    fn set_resolution(&mut self, _width: u32, _height: u32) -> Result<GpuInfo, SyscallError> {
        // GOP cannot change resolution after UEFI boot services exit.
        Err(SyscallError::NotSupported)
    }
}

/// `addr` is the physical address of the framebuffer supplied by firmware.
pub fn init(
    addr: u64,
    size: u64,
    width: u32,
    height: u32,
    stride: u32,
    pixel_format: u32,
) -> (Box<dyn Gpu>, GpuInfo) {
    // A size smaller than stride*height*4 maps less than the compositor writes.
    // Boot-time firmware data has no actionable error path, so this panics rather than returning one.
    let needed = stride as u64 * height as u64 * 4;
    assert!(
        size >= needed,
        "GOP: firmware reports a {size}-byte framebuffer for {width}x{height} stride={stride}, \
         which needs {needed}"
    );
    // A wrapped 2 MiB round-up would register a too-small region while writes continue at the full size.
    let aligned_size = align_2m_checked(size as usize)
        .unwrap_or_else(|| panic!("GOP: firmware reports a {size}-byte framebuffer")) as u64;
    // SDM Vol. 3A §11.12.4: one physical page can't hold two memory types, so this
    // must match the cache policy used for the client mapping below.
    crate::mm::paging::map_mmio(addr, aligned_size, CachePolicy::WriteCombining);

    let fb = DirectMap::from_phys(addr);
    // GOP has no second buffer; front and back scanout regions alias the same memory.
    let scanout = core::array::from_fn(|_| Region {
        phys: fb,
        size: aligned_size,
        cache: CachePolicy::WriteCombining,
        pages: None,
    });
    log!("GOP: {}x{} stride={} format={} at {:#x}",
        width, height, stride, pixel_format, addr);

    // Reads the cache policy actually installed, not the one requested.
    let mtrr = mtrr::range_type(addr, aligned_size);
    let installed = crate::mm::paging::kernel()
        .lock()
        .direct_map_policy(addr)
        .expect("GOP: the scanout is mapped");
    assert!(
        installed == CachePolicy::WriteCombining,
        "GOP: the scanout is mapped {installed:?}"
    );
    log!("GOP: scanout memory type {} (MTRR {}, PAT entry {})",
        mtrr::effective_under_wc(&mtrr).map_or("unknown", |t| t.name()),
        mtrr.name(),
        pat::WC_ENTRY);

    let cursor_pages = crate::mm::pmm::alloc_contiguous(1, crate::mm::pmm::Category::Framebuffer).expect("GOP: cursor alloc failed");
    let cursor_phys = cursor_pages[0].direct_map().phys();
    // Cursor buffer is plain system RAM, not scanout, so it keeps the default write-back type.
    let cursor = Region {
        phys: DirectMap::from_phys(cursor_phys),
        size: PAGE_2M,
        cache: CachePolicy::DeferToMtrr,
        pages: None,
    };
    core::mem::forget(cursor_pages); // lives forever (GPU is never torn down)

    let info = GpuInfo {
        scanout,
        cursor,
        width,
        height,
        stride,
        pixel_format,
        flags: 0,
    };

    (Box::new(GopGpu), info)
}
