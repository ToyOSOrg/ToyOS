//! The panel the desktop is drawn on: a virtio-gpu function this process
//! drives through its claim, or the firmware's framebuffer the kernel hands
//! over. Virtio is taken first, as soundserver takes its virtio function
//! first: a machine with both shows the desktop on the device that can follow
//! its host's window.
//!
//! The driver's decisions are `toyos-virtio-gpu`'s and host-tested there;
//! this module is the claim under it — the register window, the grant, the
//! interrupt record and the regions lent as scanouts — and what becomes of a
//! refusal. **A refusal after bring-up ends the compositor** by the driver's
//! own word: the desktop cannot outlive its display.

use std::rc::Rc;
use std::time::Duration;

use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::{endow, AsHandle, BorrowedHandle, DmaRegion, FramebufferDev, PciDev};
use toyos_abi::syscall::{DeviceType, PciId, SyscallError};
use toyos_desktop::mode::{self, Start};
use toyos_desktop::{Mode, Rect, Scale};
use toyos_pci_claim::virtio::{negotiate, Negotiated};
use toyos_pci_claim::{Bar, Grant};
use toyos_virtio::pci::Live;
use toyos_virtio::queue::Published;
use toyos_virtio_gpu::wire::{self, CURSOR_SIDE};
use toyos_virtio_gpu::{Claim, Declined, Gpu, Next, Queues};
use window::Screen;

/// A display controller of another kind, class `03:80:00`, of virtio's vendor
/// id and the GPU device's (§4.1.2.1: `0x1040` + 16), as the manifest row
/// spells the claim.
const FUNCTION: toyos_manifest::device::PciMatch = toyos_manifest::device::PciMatch::new(
    [0x03, 0x80, 0x00],
    Some(PciId { vendor: 0x1af4, device: 0x1050 }),
);

/// The one MSI-X table entry the kernel programs: both queues and the
/// configuration's changes raise it.
const MSIX_ENTRY: u16 = 0;

/// How long the device may take to answer one wait's worth of commands
/// before the compositor ends. Policy, not physics: QEMU's 2D commands answer
/// in one round trip, and this is a hang's ceiling.
const DEVICE_ANSWER: Duration = Duration::from_secs(10);

/// What `Screen` takes for a scanout whose bytes are blue, green, red and an
/// unused fourth (`VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM`).
const BGRX: u32 = 1;

pub enum Display {
    Virtio(Virtio),
    Gop(Gop),
}

impl Display {
    /// The display this machine gives the compositor; ends it where there is
    /// none, since a desktop with no panel has nothing to draw on, and where
    /// the virtio-gpu function it was given cannot be brought up.
    pub fn claim() -> (Self, Scale) {
        if let Some(dev) = endow::claim::<PciDev>(&FUNCTION.to_string()) {
            let virtio = Virtio::open(dev)
                .unwrap_or_else(|why| panic!("compositor: the virtio-gpu function cannot show the desktop: {why}"));
            let scale = virtio.scale;
            return (Self::Virtio(virtio), scale);
        }
        let claim: FramebufferDev = endow::device(DeviceType::Framebuffer)
            .expect("compositor: this machine gives it neither a virtio-gpu function nor a framebuffer");
        let gop = Gop::open(claim);
        let scale = panel_scale(gop.mode, gop.edid());
        (Self::Gop(gop), scale)
    }

    pub fn mode(&self) -> Mode {
        match self {
            Self::Virtio(v) => v.mode,
            Self::Gop(g) => g.mode,
        }
    }

    /// Where a composed frame is blitted.
    pub fn screen(&self) -> &Screen {
        match self {
            Self::Virtio(v) => &v.screen,
            Self::Gop(g) => &g.screen,
        }
    }

    /// Whether it has a cursor plane.
    pub fn hw_cursor(&self) -> bool {
        matches!(self, Self::Virtio(_))
    }

    /// Whether it takes a mode other than the one it started in.
    pub fn sets_modes(&self) -> bool {
        matches!(self, Self::Virtio(_))
    }

    /// The claim whose readiness is a wake of the device.
    pub fn handle(&self) -> Option<BorrowedHandle<'_>> {
        match self {
            Self::Virtio(v) => Some(v.dev.as_handle()),
            Self::Gop(_) => None,
        }
    }

    /// Carry `regions`, already blitted, to the panel. The firmware's
    /// framebuffer is the panel: nothing to do.
    pub fn present(&mut self, regions: &[Rect]) {
        if let Self::Virtio(v) = self {
            let rects: Vec<wire::Rect> = regions
                .iter()
                .map(|r| wire::Rect { x: r.x0 as u32, y: r.y0 as u32, width: r.w() as u32, height: r.h() as u32 })
                .collect();
            v.gpu.present(&rects).unwrap_or_else(|why| v.fault(why));
        }
    }

    /// Show `image`, 64x64 RGBA, as the hardware cursor with its pixel `hot`
    /// at `at`.
    pub fn set_cursor(&mut self, image: &[u32; (CURSOR_SIDE * CURSOR_SIDE) as usize], hot: (u32, u32), at: (u32, u32)) {
        if let Self::Virtio(v) = self {
            v.gpu.cursor(image, hot, at).unwrap_or_else(|why| v.fault(why));
        }
    }

    pub fn move_cursor(&mut self, at: (u32, u32)) {
        if let Self::Virtio(v) = self {
            v.gpu.move_cursor(at).unwrap_or_else(|why| v.fault(why));
        }
    }

    /// The claim read ready: take its record, and do what the wake owes.
    pub fn woke(&mut self) {
        if let Self::Virtio(v) = self {
            match v.dev.irq() {
                Ok(_) | Err(SyscallError::WouldBlock) => {}
                Err(why) => panic!("compositor: the virtio-gpu claim's interrupt record: {why:?}"),
            }
            v.gpu.wake().unwrap_or_else(|why| v.fault(why));
        }
    }

    /// The mode the device's output now asks for, where a wake noted a display
    /// event: the output read again, and its size and scale said. `None` for
    /// no event, a disabled output, the mode on screen, or one refused.
    pub fn followed(&mut self) -> Option<(Mode, Scale)> {
        let Self::Virtio(v) = self else { return None };
        if !v.gpu.display_changed() {
            return None;
        }
        let output = v.gpu.output().unwrap_or_else(|why| v.fault(why));
        let Some((width, height)) = output.preferred else {
            println!("compositor: the display event leaves scanout 0 disabled; the mode stays {}", v.mode);
            return None;
        };
        let wanted = Mode::new(width, height);
        let scale = panel_scale(wanted, output.edid.as_ref().map_or(&[][..], |b| &b[..]));
        match mode::follow(wanted) {
            Ok(mode) if mode != v.mode => Some((mode, scale)),
            Ok(_) => None,
            Err(why) => {
                println!("compositor: the display asks for a mode refused: {why}");
                None
            }
        }
    }

    /// Make a scanout of `mode` for the next frame to be drawn into; the mode
    /// on screen stays until [`Self::show`].
    pub fn prepare(&mut self, mode: Mode) -> Result<Pending, String> {
        let Self::Virtio(v) = self else {
            return Err("the firmware's framebuffer has its one mode".to_string());
        };
        match v.gpu.prepare((mode.width, mode.height)).unwrap_or_else(|why| v.fault(why)) {
            Ok(next) => {
                let screen = scanout_screen(next.backing(), mode);
                Ok(Pending { next, screen, mode })
            }
            Err(why) => Err(declined(why)),
        }
    }

    /// Put `pending` on screen, its frame already drawn.
    pub fn show(&mut self, pending: Pending) -> Result<(), String> {
        let Self::Virtio(v) = self else { unreachable!("only virtio prepares a mode") };
        let Pending { next, screen, mode } = pending;
        match v.gpu.show(next).unwrap_or_else(|why| v.fault(why)) {
            Ok(()) => {
                v.screen = screen;
                v.mode = mode;
                Ok(())
            }
            Err(why) => Err(declined(why)),
        }
    }
}

/// A scanout made and not yet shown, and the screen its frame is drawn on.
pub struct Pending {
    next: Next<Lent>,
    pub screen: Screen,
    mode: Mode,
}

fn declined(why: Declined<SyscallError>) -> String {
    match why {
        Declined::Lend(why) => format!("the kernel lent no scanout: {why:?}"),
        Declined::Device(command) => format!("the device answered {} out of memory", command.name()),
    }
}

/// The scale for `mode` from the panel's size in `edid`, a base block or
/// nothing; 1 where there is no size, each said.
fn panel_scale(mode: Mode, edid: &[u8]) -> Scale {
    let said = format!("compositor: display {mode}");
    if edid.is_empty() {
        println!("{said}, no panel size, scale 1");
        return Scale::ONE;
    }
    match toyos_edid::BaseBlock::parse(edid).map(|block| block.image_size()) {
        Ok(toyos_edid::ImageSize::Millimetres { width, height }) => {
            let scale = Scale::auto((mode.width, mode.height), (width, height));
            println!("{said}, panel {width}x{height} mm, scale {scale}");
            scale
        }
        Ok(size) => {
            println!("{said}, the panel's EDID gives no size ({size:?}), scale 1");
            Scale::ONE
        }
        Err(why) => {
            eprintln!("{said}, the panel's EDID is refused: {why}; scale 1");
            Scale::ONE
        }
    }
}

/// The firmware's framebuffer.
pub struct Gop {
    screen: Screen,
    mode: Mode,
    edid: Vec<u8>,
}

impl Gop {
    fn open(claim: FramebufferDev) -> Self {
        // A handle the claim's own read installed, so a refusal is the kernel
        // contradicting itself and not a client doing anything.
        let scanout = claim.open().expect("compositor: the framebuffer claim's description and scanout");
        let info = *scanout.info();
        let edid = info.edid.get(..info.edid_len as usize).expect("the kernel hands over an EDID length of 0 or one block").to_vec();
        let mode = Mode::new(info.width, info.height);
        let screen = Screen::new(scanout, info.width as usize, info.height as usize, info.stride as usize, info.pixel_format);
        Self { screen, mode, edid }
    }

    fn edid(&self) -> &[u8] {
        &self.edid
    }
}

/// A virtio-gpu function, driven.
pub struct Virtio {
    gpu: Gpu<Grant, VirtioClaim>,
    dev: Rc<PciDev>,
    screen: Screen,
    mode: Mode,
    scale: Scale,
    /// Held for their mappings' lives: the register window points into the
    /// first and the grant into the second.
    _bar: SharedMemory,
    _region: DmaRegion,
}

impl Virtio {
    /// Bring the function up in the order `toyos-virtio`'s types fix (§3.1.1),
    /// read its output (§5.7.5) and show its first mode.
    fn open(dev: PciDev) -> Result<Self, String> {
        let dev = Rc::new(dev);
        let Negotiated { setup, mapping, features } =
            negotiate(&dev, wire::F_EDID).map_err(|why| why.to_string())?;
        println!("compositor: {features}");
        let edid = setup.features() & wire::F_EDID != 0;

        let (grant, region) =
            Grant::alloc(&dev, toyos_virtio_gpu::GRANT_BYTES as u64).map_err(|why| why.to_string())?;
        grant.window().zero();
        let queues = Queues::new(grant);
        let (setup, scanouts) = setup
            .config_vector(MSIX_ENTRY)
            .and_then(|s| s.enable(&queues.control, MSIX_ENTRY))
            .and_then(|s| s.enable(&queues.cursor, MSIX_ENTRY))
            .and_then(|s| s.device_read32(wire::NUM_SCANOUTS))
            .map_err(|why| why.to_string())?;
        println!("compositor: virtio-gpu with {scanouts} scanout(s), EDID {}", if edid { "offered" } else { "not offered" });

        let claim = VirtioClaim { dev: Rc::clone(&dev), live: setup.driver_ok(), poller: Poller::new(1) };
        let mut gpu = Gpu::open(queues, grant, claim, edid);
        let output = gpu.output().map_err(|why| why.to_string())?;
        if let Some(block) = output.edid {
            let hex: String = block.iter().map(|b| format!("{b:02x}")).collect();
            println!("compositor: the device's EDID base block {hex}");
        }
        let start = Start::of(output.preferred.map(|(w, h)| Mode::new(w, h)));
        match start {
            Start::Preferred(_) => {}
            Start::Clamped { preferred, mode } => {
                println!("compositor: the device prefers {preferred}, clamped to {mode}")
            }
            Start::Least => println!("compositor: the device prefers no size, so the least mode"),
        }
        let mode = start.mode();
        let scale = panel_scale(mode, output.edid.as_ref().map_or(&[][..], |b| &b[..]));
        let next = match gpu.prepare((mode.width, mode.height)).map_err(|why| why.to_string())? {
            Ok(next) => next,
            Err(why) => return Err(format!("its first mode, {mode}: {}", declined(why))),
        };
        let screen = scanout_screen(next.backing(), mode);
        // The first frame is the back buffer's first present; until then the
        // scanout is the zeroes the kernel lent it as.
        if let Err(why) = gpu.show(next).map_err(|why| why.to_string())? {
            return Err(format!("its first mode, {mode}: {}", declined(why)));
        }
        Ok(Self { gpu, dev, screen, mode, scale, _bar: mapping, _region: region })
    }

    fn fault(&self, why: toyos_virtio_gpu::Refusal) -> ! {
        panic!("compositor: the virtio-gpu device cannot be driven on — {why}")
    }
}

fn scanout_screen(lent: &Lent, mode: Mode) -> Screen {
    let width = mode.width as usize;
    Screen::new(Rc::clone(&lent.memory), width, mode.height as usize, width, BGRX)
}

/// A region lent to the function as a scanout's backing; a [`Screen`] over it
/// keeps its mapping after the function is done with it.
pub struct Lent {
    memory: Rc<SharedMemory>,
    device_addr: u64,
}

/// What the driver runs on: the live transport, the claim's interrupt record
/// and the kernel's lending.
struct VirtioClaim {
    dev: Rc<PciDev>,
    live: Live<Bar>,
    /// Watches the claim alone, for a wait inside one command.
    poller: Poller,
}

impl Claim for VirtioClaim {
    type Backing = Lent;
    type Refused = SyscallError;

    fn notify(&self, published: Published) {
        self.live.notify(published);
    }

    fn events_read(&mut self) -> Result<u32, toyos_virtio::pci::Refusal> {
        self.live.device_read32(wire::EVENTS_READ)
    }

    fn events_clear(&mut self, events: u32) -> Result<(), toyos_virtio::pci::Refusal> {
        self.live.device_write32(wire::EVENTS_CLEAR, events)
    }

    fn wait(&mut self) -> bool {
        self.poller.watch(&*self.dev, READABLE, 0);
        self.poller.wait(1, DEVICE_ANSWER.as_nanos() as u64, |_| {});
        match self.dev.irq() {
            Ok(_) => true,
            Err(SyscallError::WouldBlock) => false,
            Err(why) => panic!("compositor: the virtio-gpu claim's interrupt record: {why:?}"),
        }
    }

    fn lend(&mut self, bytes: u64) -> Result<(Lent, u64), SyscallError> {
        let memory = SharedMemory::create_for_device(bytes as usize)?;
        let device_addr = self.dev.dma_map(memory.as_handle())?.device_addr;
        Ok((Lent { memory: Rc::new(memory), device_addr }, device_addr))
    }

    fn unlend(&mut self, lent: Lent) {
        self.dev
            .dma_unmap(lent.device_addr)
            .unwrap_or_else(|why| panic!("compositor: the kernel would not take back a scanout it lent: {why:?}"));
    }
}
