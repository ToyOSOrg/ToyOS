//! soundserver as the driver of a virtio-sound function it holds as a PCI
//! claim.
//!
//! What the kernel keeps is the claim: config space, the vector it programmed
//! into the function's MSI-X table, and the address space the function
//! translates through. **The transport and the queues are `toyos-virtio`'s,
//! and the sound device's messages and queues `toyos-virtio-sound`'s**, where
//! every word the device writes back is bounded and every refusal is
//! host-tested; the register window, the grant and the bring-up up to
//! `FEATURES_OK` are `toyos-pci-claim`'s. This file is what is left: the
//! sound device's vectors, the times its periods came back at, and what
//! becomes of a refusal.
//!
//! **Only the transmit queue raises an interrupt.** The control queue is polled
//! for the one answer it owes, and the event queue is read when a period comes
//! back, so the claim is readable exactly when a period has played — and the
//! kernel's record of that message says when it landed, which is the time the
//! DLL is clocked by.
//!
//! **A refusal after bring-up ends soundserver** by its own name, as netstack's
//! does: no conforming device writes one.

use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_pci_claim::virtio::{negotiate, Negotiated};
use toyos_pci_claim::{Bar, Grant, KernelRefused};
use toyos_virtio::pci::{Live, NO_VECTOR};
use toyos_virtio_sound::{Queues, Sound, PERIOD_BYTES, STREAM_ID};

use crate::backend::Completion;

/// virtio's vendor id and the sound device's (§4.1.2.1: `0x1040` + 25), as
/// the manifest row spells the claim.
pub const PCI_ID: toyos_abi::syscall::PciId = toyos_abi::syscall::PciId { vendor: 0x1af4, device: 0x1059 };

/// The one MSI-X table entry the kernel programs.
const MSIX_ENTRY: u16 = 0;

/// §5.14.4: `streams`, the second `le32` of the configuration.
const CONFIG_STREAMS: usize = 4;

/// Why this machine's virtio-sound function cannot carry audio. Each is a line
/// soundserver prints before it falls back to the null sink.
pub enum Refusal {
    Claim(toyos_pci_claim::virtio::Refusal),
    Sound(toyos_virtio_sound::Refusal),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Claim(why) => write!(f, "{why}"),
            Self::Sound(why) => write!(f, "{why}"),
        }
    }
}

impl From<toyos_pci_claim::virtio::Refusal> for Refusal {
    fn from(why: toyos_pci_claim::virtio::Refusal) -> Self {
        Self::Claim(why)
    }
}

impl From<toyos_virtio::pci::Refusal> for Refusal {
    fn from(why: toyos_virtio::pci::Refusal) -> Self {
        Self::Claim(why.into())
    }
}

impl From<KernelRefused> for Refusal {
    fn from(why: KernelRefused) -> Self {
        Self::Claim(why.into())
    }
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
        // §5.14.3: the sound device defines no feature bit.
        let Negotiated { setup, mapping, features } = negotiate(&dev, 0)?;
        say!("soundserver: {features}");

        let (grant, region) = Grant::alloc(&dev, toyos_virtio_sound::GRANT_BYTES as u64)?;
        grant.window().zero();

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
        let virtio = Self { dev, sound, grant, _bar: mapping, _region: region };
        Ok((virtio, stream.rate, stream.channels))
    }

    pub fn dev(&self) -> &PciDev {
        &self.dev
    }

    pub fn buffer(&self, idx: usize) -> *mut u8 {
        self.grant.window().sub(Sound::<Grant, Live<Bar>>::period(idx), PERIOD_BYTES).as_ptr()
    }

    /// The periods played since the last call, and when the device said so.
    ///
    /// The claim's record is taken before the ring is read, and the ring is
    /// read only with one in hand ([`Sound::played`]).
    pub fn completion(&mut self) -> Option<Completion> {
        let record = match self.dev.irq() {
            Ok(record) => Some(record),
            Err(SyscallError::WouldBlock) => None,
            Err(why) => panic!("soundserver: the virtio-sound claim's interrupt record: {why:?}"),
        };
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
        let (mask, record) = self
            .sound
            .played(record)
            .unwrap_or_else(|why| panic!("soundserver: virtio-sound cannot be driven on — {why}"))?;
        Some(Completion { mask, first_nanos: record.first_nanos, last_nanos: record.last_nanos })
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
