use alloc::boxed::Box;

use super::pci::PciDevice;
use super::virtio::{BufDir, DescSlot, Virtqueue, VirtioDevice, VIRTIO_F_VERSION_1};
use super::DmaPool;
use toyos_abi::syscall::SyscallError;
use crate::iommu::DeviceSpace;
use crate::mm::{Dma, Unaligned, PAGE_2M};
use crate::gpu::{FLAG_HARDWARE_CURSOR, Gpu, GpuInfo};
use crate::log;
use crate::mm::paging::CachePolicy;
use crate::object::shm::{Pages, Region};

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_GPU_DEVICE: u16 = 0x1050; // 0x1040 + device_id 16

const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_RESOURCE_UNREF: u32 = 0x0102;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const CMD_GET_EDID: u32 = 0x010a;
const CMD_UPDATE_CURSOR: u32 = 0x0300;
const CMD_MOVE_CURSOR: u32 = 0x0301;

const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_EDID: u32 = 0x1104;

const VIRTIO_GPU_F_EDID: u64 = 1 << 1;

const FORMAT_B8G8R8A8_UNORM: u32 = 1;
const FORMAT_B8G8R8X8_UNORM: u32 = 2;

const OFF_CONTROLQ: usize      = 0x0000;
const OFF_CONTROLQ_BUFS: usize = 0x1000;
const OFF_CURSORQ: usize       = 0x2000;
const OFF_CURSORQ_BUFS: usize  = 0x3000;
const DMA_SIZE: usize           = 0x4000;

const CURSOR_SIZE: u32 = 64;
const CURSOR_RESOURCE_ID: u32 = 1;
/// Scanouts count up from here, so no mode change ever reuses the cursor's id.
const FIRST_SCANOUT_RESOURCE_ID: u32 = 2;
#[cfg(feature = "boot-actuators")]
const FOREIGN_RESOURCE_ID: u32 = u32::MAX;
#[cfg(feature = "boot-actuators")]
const FOREIGN_COLUMNS: u32 = 32;

const REQ_OFFSET: usize = 0x000;
const RESP_OFFSET: usize = 0x800;

#[repr(C)]
#[derive(Clone, Copy)]
struct CtrlHeader {
    cmd_type: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
    ring_idx: u8,
    padding: [u8; 3],
}

impl CtrlHeader {
    fn new(cmd_type: u32) -> Self {
        Self { cmd_type, flags: 0, fence_id: 0, ctx_id: 0, ring_idx: 0, padding: [0; 3] }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceCreate2d {
    hdr: CtrlHeader,
    resource_id: u32,
    format: u32,
    width: u32,
    height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceUnref {
    hdr: CtrlHeader,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SetScanout {
    hdr: CtrlHeader,
    r: Rect,
    scanout_id: u32,
    resource_id: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceFlush {
    hdr: CtrlHeader,
    r: Rect,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TransferToHost2d {
    hdr: CtrlHeader,
    r: Rect,
    offset: u64,
    resource_id: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ResourceAttachBacking {
    hdr: CtrlHeader,
    resource_id: u32,
    nr_entries: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemEntry {
    addr: u64,
    length: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CursorPos {
    scanout_id: u32,
    x: u32,
    y: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UpdateCursor {
    hdr: CtrlHeader,
    pos: CursorPos,
    resource_id: u32,
    hot_x: u32,
    hot_y: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GetEdid {
    hdr: CtrlHeader,
    scanout: u32,
    padding: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RespEdid {
    hdr: CtrlHeader,
    size: u32,
    padding: u32,
    edid: [u8; 1024],
}

/// Pages the device can reach, for as long as it may reach them: the device
/// address is the attachment's and not the pages', which a holder's
/// `Arc<Pages>` keeps alive past every scanout they backed.
struct AttachedBacking {
    space: DeviceSpace,
    at: u64,
    bytes: u64,
}

impl AttachedBacking {
    /// A domain this kernel cannot map into is a kernel bug: the address is the
    /// allocator's own and the pages are whole 2 MiB frames.
    fn take(space: DeviceSpace, phys: u64, bytes: u64) -> Self {
        let at = space.map(phys, bytes).unwrap_or_else(|why| panic!("VirtIO GPU: {why}"));
        Self { space, at, bytes }
    }
}

impl Drop for AttachedBacking {
    fn drop(&mut self) {
        self.space
            .unmap(self.at, self.bytes)
            .unwrap_or_else(|why| panic!("VirtIO GPU: {why}"));
    }
}

/// Ref-counted scanout pages: a resolution change drops this without forcing
/// existing holders (e.g. a compositor) to unmap.
struct FbAlloc {
    regions: [Region; 2],
    /// `regions[0]`'s; `regions[1]` is the compositor's back buffer and is never
    /// given to the device. `None` only for the placeholder built before the display.
    backing: Option<AttachedBacking>,
}

impl FbAlloc {
    fn device_addr(&self) -> u64 {
        self.backing.as_ref().expect("VirtIO GPU: a framebuffer the device was never given").at
    }
}

struct GpuController {
    device: VirtioDevice,
    space: DeviceSpace,
    controlq: Virtqueue<'static>,
    cursorq: Virtqueue<'static>,
    control_slot: Option<DescSlot>,
    cursor_slot: Option<DescSlot>,
    /// req buffers are `Unaligned` (CPU-written); resp buffers are volatile
    /// (device-written).
    req: Dma<'static, Unaligned>,
    resp: Dma<'static>,
    cursor_req: Dma<'static, Unaligned>,
    #[allow(dead_code)] // the cursor queue's answers are not read
    cursor_resp: Dma<'static>,
    width: u32,
    height: u32,
    resource: u32,
    fb: FbAlloc,
    cursor: Region,
    cursor_backing: Option<AttachedBacking>,
}

impl GpuController {
    /// Reads what the device wrote into the response buffer.
    fn answer<T: Copy>(&self) -> T {
        // Safe only because submit_and_wait's fence(Acquire) already ordered
        // the device's write before this read.
        self.resp.read(0)
    }

    /// Writes `req` into the request buffer, submits it, and returns what the
    /// device answered.
    fn command_of<Req: Copy, Resp: Copy>(&mut self, req: &Req) -> Resp {
        self.req.write(0, *req);

        let slot = self.control_slot.take().expect("GPU: no control slot");
        let returned = self.controlq.submit_and_wait(
            slot,
            &[
                (self.req.device_addr(), core::mem::size_of::<Req>() as u32, BufDir::Readable),
                (self.resp.device_addr(), core::mem::size_of::<Resp>() as u32, BufDir::Writable),
            ],
            self.device.notify_mmio(),
            self.device.notify_off_multiplier(),
            0, // controlq index
        );
        self.control_slot = Some(returned);

        self.answer()
    }

    /// Every command but GET_EDID: answers with just the response header's
    /// type field.
    fn command<T: Copy>(&mut self, req: &T) -> u32 {
        // CtrlHeader sized, not u32: the device must be told how much room it
        // has to write.
        let hdr: CtrlHeader = self.command_of(req);
        hdr.cmd_type
    }

    fn get_edid(&mut self, scanout: u32) -> RespEdid {
        let cmd = GetEdid {
            hdr: CtrlHeader::new(CMD_GET_EDID),
            scanout,
            padding: 0,
        };
        self.command_of(&cmd)
    }

    fn create_resource(&mut self, id: u32, format: u32, width: u32, height: u32) {
        let cmd = ResourceCreate2d {
            hdr: CtrlHeader::new(CMD_RESOURCE_CREATE_2D),
            resource_id: id,
            format,
            width,
            height,
        };
        let resp = self.command(&cmd);
        assert!(resp == RESP_OK_NODATA, "VirtIO GPU: RESOURCE_CREATE_2D failed: {:#x}", resp);
    }

    fn destroy_resource(&mut self, id: u32) {
        let cmd = ResourceUnref {
            hdr: CtrlHeader::new(CMD_RESOURCE_UNREF),
            resource_id: id,
            padding: 0,
        };
        let resp = self.command(&cmd);
        assert!(resp == RESP_OK_NODATA, "VirtIO GPU: RESOURCE_UNREF failed: {:#x}", resp);
    }

    fn attach_backing(&mut self, id: u32, addr: u64, len: u32) {
        let resp = self.attach_backing_answering(id, addr, len);
        assert!(resp == RESP_OK_NODATA, "VirtIO GPU: RESOURCE_ATTACH_BACKING failed: {:#x}", resp);
    }

    fn attach_backing_answering(&mut self, id: u32, addr: u64, len: u32) -> u32 {
        // Variable-length payload: header followed by mem_entry array, written
        // consecutively.
        let cmd = ResourceAttachBacking {
            hdr: CtrlHeader::new(CMD_RESOURCE_ATTACH_BACKING),
            resource_id: id,
            nr_entries: 1,
        };
        let entry = MemEntry { addr, length: len, padding: 0 };

        let cmd_size = core::mem::size_of::<ResourceAttachBacking>();
        let entry_size = core::mem::size_of::<MemEntry>();
        self.req.write(0, cmd);
        self.req.write(cmd_size, entry);

        let slot = self.control_slot.take().expect("GPU: no control slot");
        self.control_slot = Some(self.controlq.submit_and_wait(
            slot,
            &[
                (self.req.device_addr(), (cmd_size + entry_size) as u32, BufDir::Readable),
                (self.resp.device_addr(), core::mem::size_of::<CtrlHeader>() as u32, BufDir::Writable),
            ],
            self.device.notify_mmio(),
            self.device.notify_off_multiplier(),
            0,
        ));

        self.answer::<CtrlHeader>().cmd_type
    }

    fn set_scanout(&mut self, scanout: u32, resource: u32, rect: Rect) {
        let cmd = SetScanout {
            hdr: CtrlHeader::new(CMD_SET_SCANOUT),
            r: rect,
            scanout_id: scanout,
            resource_id: resource,
        };
        let resp = self.command(&cmd);
        assert!(resp == RESP_OK_NODATA, "VirtIO GPU: SET_SCANOUT failed: {:#x}", resp);
    }

    fn transfer_to_host(&mut self, resource: u32, rect: Rect, offset: u64) {
        let cmd = TransferToHost2d {
            hdr: CtrlHeader::new(CMD_TRANSFER_TO_HOST_2D),
            r: rect,
            offset,
            resource_id: resource,
            padding: 0,
        };
        let resp = self.command(&cmd);
        assert!(resp == RESP_OK_NODATA, "VirtIO GPU: TRANSFER_TO_HOST_2D failed: {:#x}", resp);
    }

    fn flush(&mut self, resource: u32, rect: Rect) {
        let cmd = ResourceFlush {
            hdr: CtrlHeader::new(CMD_RESOURCE_FLUSH),
            r: rect,
            resource_id: resource,
            padding: 0,
        };
        let resp = self.command(&cmd);
        assert!(resp == RESP_OK_NODATA, "VirtIO GPU: RESOURCE_FLUSH failed: {:#x}", resp);
    }

    fn cursor_command<T: Copy>(&mut self, req: &T) {
        self.cursor_req.write(0, *req);
        let slot = self.cursor_slot.take().expect("GPU: no cursor slot");
        self.cursor_slot = Some(self.cursorq.submit_and_wait(
            slot,
            &[
                (self.cursor_req.device_addr(), core::mem::size_of::<T>() as u32, BufDir::Readable),
                (self.cursor_resp.device_addr(), core::mem::size_of::<CtrlHeader>() as u32, BufDir::Writable),
            ],
            self.device.notify_mmio(),
            self.device.notify_off_multiplier(),
            1, // cursor queue index
        ));
    }

    fn update_cursor(&mut self, x: u32, y: u32, hot_x: u32, hot_y: u32) {
        let cmd = UpdateCursor {
            hdr: CtrlHeader::new(CMD_UPDATE_CURSOR),
            pos: CursorPos { scanout_id: 0, x, y, padding: 0 },
            resource_id: CURSOR_RESOURCE_ID,
            hot_x,
            hot_y,
            padding: 0,
        };
        self.cursor_command(&cmd);
    }

    fn move_cursor(&mut self, x: u32, y: u32) {
        let cmd = UpdateCursor {
            hdr: CtrlHeader::new(CMD_MOVE_CURSOR),
            pos: CursorPos { scanout_id: 0, x, y, padding: 0 },
            resource_id: CURSOR_RESOURCE_ID,
            hot_x: 0,
            hot_y: 0,
            padding: 0,
        };
        self.cursor_command(&cmd);
    }

    /// `None` when the physical memory isn't there; the caller decides whether
    /// that is fatal.
    fn alloc_framebuffer(&mut self, fb_size: u32) -> Option<FbAlloc> {
        let fb_size = fb_size as usize;
        let fb_pages = fb_size.div_ceil(PAGE_2M as usize);
        let fb_aligned = (fb_pages * PAGE_2M as usize) as u64;
        let all_pages =
            [crate::mm::pmm::alloc_contiguous(fb_pages, crate::mm::pmm::Category::Framebuffer)?,
             crate::mm::pmm::alloc_contiguous(fb_pages, crate::mm::pmm::Category::Framebuffer)?];
        let regions = all_pages.map(|pages| {
            let phys = pages[0].direct_map().phys();
            Region {
                phys: crate::DirectMap::from_phys(phys),
                size: fb_aligned,
                cache: CachePolicy::DeferToMtrr,
                pages: Some(alloc::sync::Arc::new(Pages::new(pages))),
            }
        });
        let backing = AttachedBacking::take(self.space, regions[0].phys.phys(), fb_aligned);
        log!(
            "VirtIO GPU: scanout buffer at {:?} phys={:#x} device={:#x} ({} bytes)",
            regions[0].phys.as_mut_ptr::<u8>(),
            regions[0].phys.phys(),
            backing.at,
            fb_size
        );
        Some(FbAlloc { regions, backing: Some(backing) })
    }

    /// Hand the device a backing in another driver's pool, by its *physical*
    /// address, which this display's own domain does not map. The answer is
    /// logged, not asserted: the unit's fault halts every CPU from the handler,
    /// so which of the halt and the response arrives first is a race.
    #[cfg(feature = "boot-actuators")]
    fn attach_a_foreign_backing(&mut self) {
        let foreign =
            super::nvme::FOREIGN_PROBE.load(core::sync::atomic::Ordering::Relaxed);
        assert!(foreign != 0, "VirtIO GPU: this machine staged no foreign pool to aim at");
        // Sized to the probe exactly, so one transfer of the whole resource reads every byte of it.
        const ROWS: u32 = super::nvme::PROBE_LEN as u32 / (FOREIGN_COLUMNS * 4);
        self.create_resource(FOREIGN_RESOURCE_ID, FORMAT_B8G8R8X8_UNORM, FOREIGN_COLUMNS, ROWS);
        let resp = self.attach_backing_answering(
            FOREIGN_RESOURCE_ID,
            foreign,
            super::nvme::PROBE_LEN as u32,
        );
        log!(
            "VirtIO GPU: a backing at {foreign:#x}, inside another driver's pool, was answered \
             {resp:#x} (actuator)"
        );
        if resp == RESP_OK_NODATA {
            let rect = Rect { x: 0, y: 0, width: FOREIGN_COLUMNS, height: ROWS };
            self.transfer_to_host(FOREIGN_RESOURCE_ID, rect, 0);
            log!(
                "VirtIO GPU: the device read all {} bytes of it (actuator)",
                super::nvme::PROBE_LEN
            );
        }
    }

    fn build_gpu_info(&self) -> GpuInfo {
        GpuInfo {
            scanout: self.fb.regions.clone(),
            cursor: self.cursor.clone(),
            width: self.width,
            height: self.height,
            stride: self.width,
            pixel_format: 1, // BGR (B8G8R8X8_UNORM)
            flags: FLAG_HARDWARE_CURSOR,
        }
    }
}

impl Gpu for GpuController {
    fn present_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let rect = if w == 0 || h == 0 {
            Rect { x: 0, y: 0, width: self.width, height: self.height }
        } else {
            let cx = x.min(self.width);
            let cy = y.min(self.height);
            let cw = w.min(self.width - cx);
            let ch = h.min(self.height - cy);
            if cw == 0 || ch == 0 { return; }
            Rect { x: cx, y: cy, width: cw, height: ch }
        };
        let offset = (rect.y as u64 * self.width as u64 + rect.x as u64) * 4;
        self.transfer_to_host(self.resource, rect, offset);
        self.flush(self.resource, rect);
    }

    fn set_cursor(&mut self, hot_x: u32, hot_y: u32) {
        let rect = Rect { x: 0, y: 0, width: CURSOR_SIZE, height: CURSOR_SIZE };
        self.transfer_to_host(CURSOR_RESOURCE_ID, rect, 0);
        self.update_cursor(0, 0, hot_x, hot_y);
    }

    fn move_cursor(&mut self, x: u32, y: u32) {
        GpuController::move_cursor(self, x, y);
    }

    fn set_resolution(&mut self, width: u32, height: u32) -> Result<GpuInfo, SyscallError> {
        if width == self.width && height == self.height {
            return Ok(self.build_gpu_info());
        }

        let fb_size = fb_size_bytes(width, height).ok_or(SyscallError::InvalidArgument)?;

        log!("VirtIO GPU: changing resolution {}x{} -> {}x{}", self.width, self.height, width, height);

        // Old framebuffer stays live until the swap below: a failure here
        // leaves the display unchanged.
        let new_fb = self.alloc_framebuffer(fb_size).ok_or(SyscallError::ResourceExhausted)?;

        let old_resource = self.resource;
        self.resource += 1;
        self.create_resource(self.resource, FORMAT_B8G8R8X8_UNORM, width, height);
        self.attach_backing(self.resource, new_fb.device_addr(), fb_size);

        let rect = Rect { x: 0, y: 0, width, height };
        self.set_scanout(0, self.resource, rect);

        // Acknowledged on the control queue's used ring, so the device is off the
        // old backing before its address stops translating below.
        self.destroy_resource(old_resource);
        // Old pages free only when the last holder drops them; the attachment
        // ends here whatever holds them.
        drop(core::mem::replace(&mut self.fb, new_fb));

        self.width = width;
        self.height = height;

        log!("VirtIO GPU: resolution set to {}x{}", width, height);

        Ok(self.build_gpu_info())
    }
}

/// Byte size for a resolution, or `None` if zero or too large to fit `u32`.
fn fb_size_bytes(width: u32, height: u32) -> Option<u32> {
    // u64: width * height * 4 can overflow u32.
    let bytes = (width as u64).checked_mul(height as u64)?.checked_mul(4)?;
    if bytes == 0 || bytes > u32::MAX as u64 {
        return None;
    }
    Some(bytes as u32)
}

/// Probes for the VirtIO GPU; `None` if absent.
pub fn init(devices: &[PciDevice]) -> Option<(Box<dyn Gpu>, GpuInfo)> {
    let pci_dev = *devices.iter().find(|d| d.is_id(VIRTIO_VENDOR, VIRTIO_GPU_DEVICE))?;
    log!("VirtIO GPU: found at PCI {:02x}:{:02x}.{}", pci_dev.bus, pci_dev.dev, pci_dev.func);
    let space = DeviceSpace::create();
    // Leaked rather than held in a `static`: the display is never unbound.
    let dma = DmaPool::alloc_in(DMA_SIZE, space).leak();
    // Before the device is told any address, and after the command pool's mapping.
    space.attach(pci_dev.bus, pci_dev.dev, pci_dev.func);

    let device = match VirtioDevice::init(&pci_dev, VIRTIO_F_VERSION_1 | VIRTIO_GPU_F_EDID) {
        Ok(device) => device,
        Err(why) => {
            log!("VirtIO GPU: PCI {:02x}:{:02x}.{} {why} — device refused",
                pci_dev.bus, pci_dev.dev, pci_dev.func);
            return None;
        }
    };

    let mut controlq = Virtqueue::new(dma.subview(OFF_CONTROLQ, 0x1000), 16);
    let mut cursorq = Virtqueue::new(dma.subview(OFF_CURSORQ, 0x1000), 16);

    device.setup_queue(0, &mut controlq);
    device.setup_queue(1, &mut cursorq);
    device.enable_queue(0);
    device.enable_queue(1);
    device.activate();

    let mut control_slots = controlq.initial_slots();
    let mut cursor_slots = cursorq.initial_slots();
    let control_slot = control_slots.pop().expect("GPU: no control slots");
    let cursor_slot = cursor_slots.pop().expect("GPU: no cursor slots");
    drop(control_slots);
    drop(cursor_slots);

    let ctrl_bufs = dma.subview(OFF_CONTROLQ_BUFS, 0x1000);
    let cursor_bufs = dma.subview(OFF_CURSORQ_BUFS, 0x1000);
    // Each half of a buffer page: request at 0, response at `RESP_OFFSET`.
    const HALF: usize = RESP_OFFSET - REQ_OFFSET;

    let mut gpu = GpuController {
        device,
        space,
        controlq,
        cursorq,
        control_slot: Some(control_slot),
        cursor_slot: Some(cursor_slot),
        req: ctrl_bufs.subview(REQ_OFFSET, HALF).unaligned(),
        resp: ctrl_bufs.subview(RESP_OFFSET, HALF),
        cursor_req: cursor_bufs.subview(REQ_OFFSET, HALF).unaligned(),
        cursor_resp: cursor_bufs.subview(RESP_OFFSET, HALF),
        width: 0,
        height: 0,
        resource: FIRST_SCANOUT_RESOURCE_ID,
        fb: FbAlloc { regions: core::array::from_fn(|_| Region::empty()), backing: None },
        cursor: Region::empty(),
        cursor_backing: None,
    };

    // EDID's reported resolution is often a stale firmware default; the
    // preferred mode is the first Detailed Timing Descriptor.
    let edid = gpu.get_edid(0);
    let (width, height) = if edid.hdr.cmd_type == RESP_OK_EDID {
        let dtd = &edid.edid[54..72];
        let w = dtd[2] as u32 | ((dtd[4] as u32 >> 4) << 8);
        let h = dtd[5] as u32 | ((dtd[7] as u32 >> 4) << 8);
        if w >= 1280 && h >= 720 {
            (w, h)
        } else {
            (1280, 720) // EDID reports stale firmware resolution, use default
        }
    } else {
        (1280, 720)
    };
    log!("VirtIO GPU: display {}x{}", width, height);

    // Boot-time: failure here means the machine can't run, so it dies loudly.
    let fb_size = fb_size_bytes(width, height).expect("VirtIO GPU: nonsense display dimensions");
    gpu.fb = gpu.alloc_framebuffer(fb_size).expect("VirtIO GPU: framebuffer alloc failed");

    gpu.create_resource(gpu.resource, FORMAT_B8G8R8X8_UNORM, width, height);
    gpu.attach_backing(gpu.resource, gpu.fb.device_addr(), fb_size);

    let rect = Rect { x: 0, y: 0, width, height };
    gpu.set_scanout(0, gpu.resource, rect);

    let cursor_bytes = (CURSOR_SIZE * CURSOR_SIZE * 4) as usize;
    let cursor_pages = crate::mm::pmm::alloc_contiguous(1, crate::mm::pmm::Category::Framebuffer).expect("VirtIO GPU: cursor alloc failed");
    let cursor_ptr = cursor_pages[0].direct_map().as_mut_ptr::<u8>();
    let cursor_phys = cursor_pages[0].direct_map().phys();
    gpu.cursor = Region {
        phys: crate::DirectMap::from_phys(cursor_phys),
        size: PAGE_2M,
        cache: CachePolicy::DeferToMtrr,
        pages: Some(alloc::sync::Arc::new(Pages::new(cursor_pages))),
    };
    let cursor_backing = AttachedBacking::take(space, cursor_phys, PAGE_2M);
    gpu.create_resource(CURSOR_RESOURCE_ID, FORMAT_B8G8R8A8_UNORM, CURSOR_SIZE, CURSOR_SIZE);
    gpu.attach_backing(CURSOR_RESOURCE_ID, cursor_backing.at, cursor_bytes as u32);
    log!(
        "VirtIO GPU: cursor resource at {:?} phys={:#x} device={:#x}",
        cursor_ptr,
        cursor_phys,
        cursor_backing.at
    );
    gpu.cursor_backing = Some(cursor_backing);

    gpu.width = width;
    gpu.height = height;

    #[cfg(feature = "boot-actuators")]
    if crate::actuator::iommu_gpu_foreign_backing() {
        gpu.attach_a_foreign_backing();
    }

    let info = gpu.build_gpu_info();

    Some((Box::new(gpu), info))
}
