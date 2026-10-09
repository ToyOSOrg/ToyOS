//! soundserver as the driver of a virtio-sound function it holds as a PCI
//! claim.
//!
//! What the kernel keeps is the claim: config space, the vector it programmed
//! into the function's MSI-X table, and the address space the function
//! translates through. **The transport and the queues are `toyos-virtio`'s,
//! and the sound device's messages and queues `toyos-virtio-sound`'s**, where
//! every word the device writes back is bounded and every refusal is
//! host-tested; this file is the instructions under them — the register window
//! and the grant as `toyos-device-memory`'s boundary — and what becomes of a
//! refusal.
//!
//! **Only the transmit queue raises an interrupt.** The control queue is polled
//! for the one answer it owes, and the event queue is read when a period comes
//! back, so the claim is readable exactly when a period has played.
//!
//! **A refusal after bring-up ends soundserver** by its own name, as netstack's
//! does: no conforming device writes one.

use std::sync::atomic::{fence, Ordering};

use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::{DmaRegion, PciDev};
use toyos_abi::audio::AudioCompletionRecord;
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_device_memory::{DmaBuffers, Registers};
use toyos_virtio::pci::{vendor_caps, ConfigSpace, Layout, Live, Offer, WalkRefusal, NO_VECTOR,
                        VIRTIO_F_ACCESS_PLATFORM};
use toyos_virtio_sound::{Queues, Sound, PERIOD_BYTES, STREAM_ID};

/// virtio's vendor id and the sound device's (§4.1.2.1: `0x1040` + 25), as
/// the manifest row spells the claim.
pub const PCI_ID: toyos_abi::syscall::PciId = toyos_abi::syscall::PciId { vendor: 0x1af4, device: 0x1059 };

/// The one MSI-X table entry the kernel programs.
const MSIX_ENTRY: u16 = 0;

/// §5.14.4: `streams`, the second `le32` of the configuration.
const CONFIG_STREAMS: usize = 4;

/// A mapped BAR, as the transport reaches the device's registers.
#[derive(Clone, Copy)]
pub struct Bar(Window);

impl Registers for Bar {
    fn bytes(&self) -> usize {
        self.0.bytes()
    }
    fn read8(&self, at: usize) -> u8 {
        self.0.read(at)
    }
    fn read16(&self, at: usize) -> u16 {
        self.0.read(at)
    }
    fn read32(&self, at: usize) -> u32 {
        self.0.read(at)
    }
    fn write8(&self, at: usize, value: u8) {
        self.0.write(at, value);
    }
    fn write16(&self, at: usize, value: u16) {
        self.0.write(at, value);
    }
    fn write32(&self, at: usize, value: u32) {
        self.0.write(at, value);
    }
}

/// The DMA grant, as the queues reach it. `device_base` is what the unit
/// translates for this function and for nothing else.
#[derive(Clone, Copy)]
pub struct Grant {
    window: Window,
    device_base: u64,
}

impl DmaBuffers for Grant {
    fn bytes(&self) -> usize {
        self.window.bytes()
    }
    fn device_addr(&self, at: usize) -> u64 {
        self.device_base + at as u64
    }
    fn read16(&self, at: usize) -> u16 {
        self.window.read(at)
    }
    fn read32(&self, at: usize) -> u32 {
        self.window.read(at)
    }
    fn read64(&self, at: usize) -> u64 {
        self.window.read(at)
    }
    fn write16(&self, at: usize, value: u16) {
        self.window.write(at, value);
    }
    fn write32(&self, at: usize, value: u32) {
        self.window.write(at, value);
    }
    fn write64(&self, at: usize, value: u64) {
        self.window.write(at, value);
    }
    fn publish(&self) {
        fence(Ordering::Release);
    }
    fn observe(&self) {
        fence(Ordering::Acquire);
    }
}

/// The claim's configuration space, as the capability walk reads it.
struct ClaimConfig<'a>(&'a PciDev);

impl ConfigSpace for ClaimConfig<'_> {
    type Refused = SyscallError;

    fn read8(&self, at: u16) -> Result<u8, SyscallError> {
        self.0.config_read(at as u32, RegWidth::U8).map(|byte| byte as u8)
    }

    fn read32(&self, at: u16) -> Result<u32, SyscallError> {
        self.0.config_read(at as u32, RegWidth::U32)
    }
}

/// Why this machine's virtio-sound function cannot carry audio. Each is a line
/// soundserver prints before it falls back to the null sink.
pub enum Refusal {
    /// The kernel refused a call a bring-up cannot go on without.
    Kernel(&'static str, SyscallError),
    Walk(WalkRefusal<SyscallError>),
    Transport(toyos_virtio::pci::Refusal),
    Sound(toyos_virtio_sound::Refusal),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel(call, why) => write!(f, "the kernel refused {call}: {why:?}"),
            Self::Walk(why) => write!(f, "{why}"),
            Self::Transport(why) => write!(f, "{why}"),
            Self::Sound(why) => write!(f, "{why}"),
        }
    }
}

impl From<toyos_virtio::pci::Refusal> for Refusal {
    fn from(why: toyos_virtio::pci::Refusal) -> Self {
        Self::Transport(why)
    }
}

fn kernel(call: &'static str) -> impl Fn(SyscallError) -> Refusal {
    move |why| Refusal::Kernel(call, why)
}

pub struct Virtio {
    dev: PciDev,
    sound: Sound<Grant, Live<Bar>>,
    grant: Grant,
    /// Held for their mappings' lives: the register window points into the
    /// first and the grant into the second.
    _bar: SharedMemory,
    _region: DmaRegion,
}

impl Virtio {
    /// Bring the function up in the order `toyos-virtio`'s types fix, which is
    /// §3.1.1's, and stream 0 in §5.14.5's: the rate and channel count chosen.
    pub fn claim(dev: PciDev) -> Result<(Self, u32, u8), Refusal> {
        let info = dev.describe().map_err(kernel("the claim's description"))?;
        let layout = Layout::of(&vendor_caps(&ClaimConfig(&dev)).map_err(Refusal::Walk)?)?;
        // The kernel reports 0 bytes for a BAR it keeps back.
        let bar = layout.bar();
        let bar_bytes = *info
            .bar_bytes
            .get(bar as usize)
            .filter(|bytes| **bytes > 0)
            .ok_or(toyos_virtio::pci::Refusal::MissingCap("a BAR this claim may map"))?;
        let mapped = dev.map_bar(bar as u32, bar_bytes).map_err(kernel("the register window"))?;
        // SAFETY: the mapping is `bar_bytes` long and lives as long as
        // `mapped`, which `Virtio` holds for its own life.
        let window = unsafe { Window::new(mapped.as_ptr(), bar_bytes as usize) };

        let offer = Offer::acknowledge(Bar(window), &layout)?;
        let offered = offer.features();
        // §5.14.3: the sound device defines no feature bit.
        let setup = offer.accept(0)?;
        let features = setup.features();
        // The kernel's virtio drivers' feature line, in the same shape:
        // `iommu_virtio_platform` reads it back for every virtio function.
        say!(
            "soundserver: VirtIO: PCI {:02x}:{:02x}.{} features device={offered:#x} \
             negotiated={features:#x} access_platform={}",
            info.bus,
            info.dev,
            info.func,
            if features & VIRTIO_F_ACCESS_PLATFORM != 0 { 'y' } else { 'n' },
        );

        let region = dev
            .dma_alloc(toyos_virtio_sound::GRANT_BYTES as u64)
            .map_err(kernel("a DMA grant"))?;
        // SAFETY: the kernel rounds the request up to whole pages, never down,
        // and the grant lives as long as `region`, which `Virtio` holds.
        let window = unsafe { Window::new(region.memory.as_ptr(), toyos_virtio_sound::GRANT_BYTES) };
        window.zero();
        let grant = Grant { window, device_base: region.device_addr };

        let queues = Queues::new(grant);
        let (setup, streams) = setup
            .config_vector(NO_VECTOR)?
            .enable(&queues.control, NO_VECTOR)?
            .enable(&queues.events, NO_VECTOR)?
            .enable(&queues.tx, MSIX_ENTRY)?
            .device_read32(CONFIG_STREAMS)?;
        say!("virtio-sound: {streams} stream(s)");
        let (sound, stream) =
            Sound::open(queues, grant, setup.driver_ok(), streams).map_err(Refusal::Sound)?;
        let pcm = stream.info;
        say!(
            "virtio-sound: stream {STREAM_ID}: dir={} ch={}-{} fmts={:#x} rates={:#x}",
            pcm.direction, pcm.channels_min, pcm.channels_max, pcm.formats, pcm.rates
        );
        say!(
            "virtio-sound: configured stream {STREAM_ID}: {}Hz {}ch s16le",
            stream.rate, stream.channels
        );
        let virtio = Self { dev, sound, grant, _bar: mapped, _region: region };
        Ok((virtio, stream.rate, stream.channels))
    }

    pub fn dev(&self) -> &PciDev {
        &self.dev
    }

    pub fn buffer(&self, idx: usize) -> *mut u8 {
        self.grant.window.sub(Sound::<Grant, Live<Bar>>::period(idx), PERIOD_BYTES).as_ptr()
    }

    /// The periods played since the last call, as one record stamped now.
    ///
    /// The claim's record is taken before the ring is read, so a period that
    /// lands after the read leaves the claim readable for the next wake.
    pub fn completions(&mut self, out: &mut [AudioCompletionRecord]) -> usize {
        match self.dev.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => {}
            Err(why) => panic!("soundserver: the virtio-sound claim's interrupt record: {why:?}"),
        }
        let timestamp_nanos = toyos_abi::clock::nanos_since_boot();
        // The device's own view of an underrun, which soundserver's counters
        // cannot see.
        self.sound
            .events(|event| match event.name() {
                Some(name) => {
                    say!("virtio-sound: device event {:#x} ({name}) data={}", event.code, event.data)
                }
                None => say!("virtio-sound: device event {:#x} data={}", event.code, event.data),
            })
            .unwrap_or_else(|why| panic!("soundserver: virtio-sound cannot be driven on — {why}"));
        let mask = self
            .sound
            .completed()
            .unwrap_or_else(|why| panic!("soundserver: virtio-sound cannot be driven on — {why}"));
        if mask == 0 {
            return 0;
        }
        out[0] = AudioCompletionRecord { mask, _pad: 0, timestamp_nanos };
        1
    }

    /// Put period `idx` on the wire: its PCM descriptor is a whole period.
    pub fn submit(&mut self, idx: usize, bytes: usize) {
        assert_eq!(bytes, PERIOD_BYTES, "soundserver: a virtio-sound period is {PERIOD_BYTES} bytes");
        let starting = !self.sound.running();
        self.sound
            .submit(idx)
            .unwrap_or_else(|why| panic!("soundserver: virtio-sound could not start its stream: {why}"));
        if starting {
            say!("virtio-sound: stream {STREAM_ID} started");
        }
    }

    pub fn stop(&mut self) {
        if !self.sound.running() {
            return;
        }
        self.sound
            .stop()
            .unwrap_or_else(|why| panic!("soundserver: virtio-sound could not stop its stream: {why}"));
        say!("virtio-sound: stream {STREAM_ID} stopped");
    }
}
