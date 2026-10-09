//! The PCI transport (§4.1): where a device's structures are, the
//! initialisation §3.1.1 orders, and a queue's configuration.
//!
//! **The order is in the types.** [`Offer`] is a device reset and
//! acknowledged, its feature bits read; [`Offer::accept`] answers a [`Setup`],
//! on which queues are configured; [`Setup::driver_ok`] answers a [`Live`],
//! and only a `Live` sends a notification — §3.1.1 has none sent before
//! `DRIVER_OK`. A queue is enabled by the one call that gave it its addresses
//! and its vector (§4.1.4.3.2), so none is enabled without them.
//!
//! **Access widths are §4.1.3.1's**: a byte for `device_status`, sixteen bits
//! for a sixteen-bit field, and a sixty-four-bit field as its two halves.
//!
//! A refusal after the device was acknowledged sets `FAILED` (§3.1.1), over
//! the bits already set: §2.1.1 has the driver clear none.

use alloc::vec::Vec;

use crate::queue::{Published, Virtqueue};
use crate::{DmaBuffers, Registers};

/// §4.1.4: `cfg_type`.
const CFG_COMMON: u8 = 1;
const CFG_NOTIFY: u8 = 2;
const CFG_ISR: u8 = 3;
const CFG_DEVICE: u8 = 4;

/// §4.1.4: `bar` "values 0x0 to 0x5 specify a Base Address register", and
/// "any other value is reserved".
const LAST_BAR: u8 = 5;

/// §4.1.4.3: `virtio_pci_common_cfg`.
mod common {
    pub const DEVICE_FEATURE_SELECT: usize = 0x00;
    pub const DEVICE_FEATURE: usize = 0x04;
    pub const DRIVER_FEATURE_SELECT: usize = 0x08;
    pub const DRIVER_FEATURE: usize = 0x0C;
    pub const CONFIG_MSIX_VECTOR: usize = 0x10;
    pub const DEVICE_STATUS: usize = 0x14;
    pub const QUEUE_SELECT: usize = 0x16;
    pub const QUEUE_SIZE: usize = 0x18;
    pub const QUEUE_MSIX_VECTOR: usize = 0x1A;
    pub const QUEUE_ENABLE: usize = 0x1C;
    pub const QUEUE_NOTIFY_OFF: usize = 0x1E;
    pub const QUEUE_DESC: usize = 0x20;
    pub const QUEUE_DRIVER: usize = 0x28;
    pub const QUEUE_DEVICE: usize = 0x30;
    /// Through `queue_device`: every field this transport reaches.
    pub const BYTES: usize = 0x38;
}

/// §2.1: the device status bits.
pub mod status {
    pub const ACKNOWLEDGE: u8 = 1;
    pub const DRIVER: u8 = 2;
    pub const DRIVER_OK: u8 = 4;
    pub const FEATURES_OK: u8 = 8;
    pub const FAILED: u8 = 128;
}

/// §6: "This indicates compliance with this specification".
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;
/// §6: the device reaches memory through addresses the platform translates,
/// which is what [`DmaBuffers::device_addr`] answers.
pub const VIRTIO_F_ACCESS_PLATFORM: u64 = 1 << 33;

/// §4.1.5.1.2: what a vector field reads when the device mapped none.
pub const NO_VECTOR: u16 = 0xFFFF;

/// One vendor-specific capability (§4.1.4's `virtio_pci_cap`), as a driver
/// read it out of configuration space. Every field is the device's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VendorCap {
    pub cfg_type: u8,
    pub bar: u8,
    pub offset: u32,
    pub length: u32,
    /// The dword after the structure, which is `notify_off_multiplier` where
    /// `cfg_type` is the notification structure's (§4.1.4.4) and nothing
    /// anywhere else.
    pub notify_off_multiplier: u32,
}

/// Which of a device's interrupt sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Config,
    Queue(u16),
}

/// Why the device is not driven. Each is something the device published or
/// answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No capability of a structure every device presents (§4.1.4.3.1,
    /// §4.1.4.4.1, §4.1.4.5.1, §4.1.4.6) in a BAR that is one.
    MissingCap(&'static str),
    /// The four structures are not in one BAR, and one BAR is what a
    /// [`Registers`] is.
    SplitAcrossBars,
    /// A structure's offset and length run past its BAR.
    OutsideBar(&'static str),
    /// A structure is shorter than the fields this transport reaches in it.
    TooShort(&'static str),
    /// A structure is off the alignment its section requires.
    Misaligned(&'static str),
    /// `device_status` did not read 0 after 0 was written (§4.1.4.3.2).
    ResetUnanswered,
    /// The device does not offer `VIRTIO_F_VERSION_1` (§6.1).
    NotVersion1 { offered: u64 },
    /// `FEATURES_OK` did not stay set (§3.1.1 step 6).
    FeaturesRefused { accepted: u64, status: u8 },
    /// A vector field did not read back the entry written to it
    /// (§4.1.5.1.2.2).
    NoVector(Source),
    /// The queue is absent, or shallower than the driver's rings
    /// (§4.1.4.3: `queue_size`).
    QueueTooShallow { queue: u16, offered: u16, wanted: u16 },
    /// The queue's `queue_notify_off` puts its notify address outside the
    /// notification structure, or off its alignment (§4.1.4.4.1).
    Doorbell { queue: u16, notify_off: u16 },
    /// A read past the device-specific structure.
    PastDeviceConfig { at: usize, bytes: usize },
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingCap(what) => write!(f, "it published no usable {what}"),
            Self::SplitAcrossBars => {
                write!(f, "it published its four configuration structures in more than one BAR")
            }
            Self::OutsideBar(what) => write!(f, "its {what} runs past the BAR it is in"),
            Self::TooShort(what) => {
                write!(f, "its {what} is shorter than the fields a driver reaches in it")
            }
            Self::Misaligned(what) => write!(f, "its {what} is not aligned for its fields"),
            Self::ResetUnanswered => write!(f, "it never zeroed DEVICE_STATUS for its reset"),
            Self::NotVersion1 { offered } => {
                write!(f, "it offers the feature set {offered:#x}, without VERSION_1")
            }
            Self::FeaturesRefused { accepted, status } => write!(
                f,
                "it refused the feature set {accepted:#x} this driver accepted, leaving \
                 DEVICE_STATUS={status:#x} without FEATURES_OK"
            ),
            Self::NoVector(Source::Config) => {
                write!(f, "it refused a vector for its configuration-change interrupt")
            }
            Self::NoVector(Source::Queue(queue)) => {
                write!(f, "it refused a vector for queue {queue}")
            }
            Self::QueueTooShallow { queue, offered, wanted } => write!(
                f,
                "its queue {queue} holds {offered} descriptor(s), where this driver's rings \
                 are {wanted}"
            ),
            Self::Doorbell { queue, notify_off } => write!(
                f,
                "its queue {queue} names notify offset {notify_off}, which is no place in its \
                 notification structure"
            ),
            Self::PastDeviceConfig { at, bytes } => write!(
                f,
                "its device configuration is {bytes} byte(s), and the driver's field at {at:#x} \
                 is not in it"
            ),
        }
    }
}

/// Where a device's structures are: the capabilities a driver uses, and the
/// one BAR they are in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    common: VendorCap,
    notify: VendorCap,
    device: VendorCap,
}

impl Layout {
    /// Choose among `caps`, in the order the device's capability list has
    /// them.
    ///
    /// §4.1.4.1: the first of each type is the one used, and a capability with
    /// a reserved `bar` or `cfg_type` is ignored. The ISR structure is not
    /// reached — a vector is bound, and §4.1.4.5.2 has it left alone then —
    /// but a device without one is not a device §4.1.4.5.1 describes.
    pub fn of(caps: &[VendorCap]) -> Result<Self, Refusal> {
        let first = |cfg_type: u8, what: &'static str| {
            caps.iter()
                .find(|cap| cap.cfg_type == cfg_type && cap.bar <= LAST_BAR)
                .copied()
                .ok_or(Refusal::MissingCap(what))
        };
        let common = first(CFG_COMMON, "COMMON_CFG")?;
        let notify = first(CFG_NOTIFY, "NOTIFY_CFG")?;
        let isr = first(CFG_ISR, "ISR_CFG")?;
        let device = first(CFG_DEVICE, "DEVICE_CFG")?;
        if [notify, isr, device].iter().any(|cap| cap.bar != common.bar) {
            return Err(Refusal::SplitAcrossBars);
        }
        Ok(Self { common, notify, device })
    }

    /// The BAR a [`Registers`] for this device is a window over.
    pub fn bar(&self) -> u8 {
        self.common.bar
    }
}

#[derive(Clone, Copy)]
struct Region {
    at: usize,
    bytes: usize,
}

impl Region {
    /// The structure `cap` names, held to the window it is in: no shorter
    /// than `need`, and on a multiple of `align`.
    fn of(
        cap: &VendorCap,
        what: &'static str,
        window: usize,
        need: usize,
        align: u32,
    ) -> Result<Self, Refusal> {
        // Sixty-four bits, so two device-chosen dwords cannot wrap.
        if cap.offset as u64 + cap.length as u64 > window as u64 {
            return Err(Refusal::OutsideBar(what));
        }
        if (cap.length as usize) < need {
            return Err(Refusal::TooShort(what));
        }
        if !cap.offset.is_multiple_of(align) {
            return Err(Refusal::Misaligned(what));
        }
        Ok(Self { at: cap.offset as usize, bytes: cap.length as usize })
    }
}

/// The registers, and where the three structures are in them.
struct Wires<R: Registers> {
    regs: R,
    common: usize,
    notify: Region,
    notify_off_multiplier: u32,
    device: Region,
    /// What `device_status` was last written with.
    status: u8,
}

impl<R: Registers> Wires<R> {
    fn set_status(&mut self, bit: u8) {
        self.status |= bit;
        self.regs.write8(self.common + common::DEVICE_STATUS, self.status);
    }

    /// Give the device up (§3.1.1), and hand the reason back.
    fn refuse(&mut self, why: Refusal) -> Refusal {
        self.set_status(status::FAILED);
        why
    }
}

/// A device reset and acknowledged, and the features it offers.
pub struct Offer<R: Registers> {
    wires: Wires<R>,
    offered: u64,
}

impl<R: Registers> Offer<R> {
    /// §3.1.1 steps 1 to 3 and the first half of 4, over the BAR `layout`
    /// names.
    ///
    /// The reset is one write and one read: a device that has not zeroed its
    /// status by then is refused, where §4.1.4.3.2 would have it waited on,
    /// and nothing more is written to it.
    pub fn acknowledge(regs: R, layout: &Layout) -> Result<Self, Refusal> {
        let window = regs.bytes();
        // §4.1.4.3.1 and §4.1.4.4.1: the alignment of each offset.
        let common = Region::of(&layout.common, "COMMON_CFG", window, common::BYTES, 4)?;
        let notify = Region::of(&layout.notify, "NOTIFY_CFG", window, 0, 2)?;
        let device = Region::of(&layout.device, "DEVICE_CFG", window, 0, 1)?;
        let mut wires = Wires {
            regs,
            common: common.at,
            notify,
            notify_off_multiplier: layout.notify.notify_off_multiplier,
            device,
            status: 0,
        };

        let at = wires.common;
        wires.regs.write8(at + common::DEVICE_STATUS, 0);
        if wires.regs.read8(at + common::DEVICE_STATUS) != 0 {
            return Err(Refusal::ResetUnanswered);
        }
        wires.set_status(status::ACKNOWLEDGE);
        wires.set_status(status::DRIVER);

        wires.regs.write32(at + common::DEVICE_FEATURE_SELECT, 0);
        let low = wires.regs.read32(at + common::DEVICE_FEATURE);
        wires.regs.write32(at + common::DEVICE_FEATURE_SELECT, 1);
        let high = wires.regs.read32(at + common::DEVICE_FEATURE);
        Ok(Self { wires, offered: (high as u64) << 32 | low as u64 })
    }

    /// Every feature bit the device offers.
    pub fn features(&self) -> u64 {
        self.offered
    }

    /// The rest of §3.1.1 step 4, and steps 5 and 6: accept `wanted` of what
    /// the device offers.
    ///
    /// `wanted` is the device type's bits. The transport's own are not the
    /// caller's to choose (§6.1): `VIRTIO_F_VERSION_1` is accepted, and a
    /// device without it is refused; `VIRTIO_F_ACCESS_PLATFORM` is accepted
    /// wherever it is offered. Nothing the device did not offer is written
    /// (§2.2.1).
    pub fn accept(mut self, wanted: u64) -> Result<Setup<R>, Refusal> {
        if self.offered & VIRTIO_F_VERSION_1 == 0 {
            return Err(self.wires.refuse(Refusal::NotVersion1 { offered: self.offered }));
        }
        let features = self.offered & (wanted | VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM);
        let wires = &mut self.wires;
        let at = wires.common;
        wires.regs.write32(at + common::DRIVER_FEATURE_SELECT, 0);
        wires.regs.write32(at + common::DRIVER_FEATURE, features as u32);
        wires.regs.write32(at + common::DRIVER_FEATURE_SELECT, 1);
        wires.regs.write32(at + common::DRIVER_FEATURE, (features >> 32) as u32);
        wires.set_status(status::FEATURES_OK);
        let answered = wires.regs.read8(at + common::DEVICE_STATUS);
        if answered & status::FEATURES_OK == 0 {
            return Err(wires.refuse(Refusal::FeaturesRefused { accepted: features, status: answered }));
        }
        Ok(Setup { wires: self.wires, features, doorbells: Vec::new() })
    }
}

/// A device whose features are settled and which is not yet live: §3.1.1
/// step 7.
pub struct Setup<R: Registers> {
    wires: Wires<R>,
    features: u64,
    /// Each enabled queue's index, and where in the BAR its notify address is.
    doorbells: Vec<(u16, usize)>,
}

impl<R: Registers> Setup<R> {
    /// The feature bits negotiated.
    pub fn features(&self) -> u64 {
        self.features
    }

    /// Map configuration changes to MSI-X table entry `entry` (§4.1.5.1.2).
    pub fn config_vector(&mut self, entry: u16) -> Result<(), Refusal> {
        self.bind(common::CONFIG_MSIX_VECTOR, entry, Source::Config)
    }

    fn bind(&mut self, field: usize, entry: u16, source: Source) -> Result<(), Refusal> {
        let at = self.wires.common + field;
        self.wires.regs.write16(at, entry);
        // §4.1.5.1.2.2: "on success, the previously written value is returned".
        if self.wires.regs.read16(at) != entry {
            return Err(self.wires.refuse(Refusal::NoVector(source)));
        }
        Ok(())
    }

    /// Give the device `queue`: its size, its three addresses and MSI-X table
    /// entry `entry` for its used-buffer notifications, and then — and only
    /// then (§4.1.4.3.2) — its enable.
    pub fn enable<M: DmaBuffers>(
        &mut self,
        queue: &Virtqueue<M>,
        entry: u16,
    ) -> Result<(), Refusal> {
        let (index, wanted) = (queue.index(), queue.size());
        let at = self.wires.common;
        self.wires.regs.write16(at + common::QUEUE_SELECT, index);
        // The most the device takes, and 0 for a queue it does not have.
        let offered = self.wires.regs.read16(at + common::QUEUE_SIZE);
        if offered < wanted {
            return Err(self.wires.refuse(Refusal::QueueTooShallow { queue: index, offered, wanted }));
        }
        self.wires.regs.write16(at + common::QUEUE_SIZE, wanted);
        for (field, addr) in [
            (common::QUEUE_DESC, queue.desc_addr()),
            (common::QUEUE_DRIVER, queue.avail_addr()),
            (common::QUEUE_DEVICE, queue.used_addr()),
        ] {
            self.wires.regs.write32(at + field, addr as u32);
            self.wires.regs.write32(at + field + 4, (addr >> 32) as u32);
        }

        // §4.1.4.4: the notify address is `cap.offset + queue_notify_off *
        // notify_off_multiplier`, and §4.1.4.4.1 has the structure hold the
        // two bytes written there. Both factors are the device's; their
        // product fits sixty-four bits.
        let notify_off = self.wires.regs.read16(at + common::QUEUE_NOTIFY_OFF);
        let into = notify_off as u64 * self.wires.notify_off_multiplier as u64;
        if into + 2 > self.wires.notify.bytes as u64 || !into.is_multiple_of(2) {
            return Err(self.wires.refuse(Refusal::Doorbell { queue: index, notify_off }));
        }
        let doorbell = self.wires.notify.at + into as usize;

        self.bind(common::QUEUE_MSIX_VECTOR, entry, Source::Queue(index))?;
        self.wires.regs.write16(at + common::QUEUE_ENABLE, 1);
        self.doorbells.push((index, doorbell));
        Ok(())
    }

    /// One byte of the device-specific structure, `at` bytes into it.
    pub fn device_read8(&mut self, at: usize) -> Result<u8, Refusal> {
        let Region { at: base, bytes } = self.wires.device;
        if at >= bytes {
            return Err(self.wires.refuse(Refusal::PastDeviceConfig { at, bytes }));
        }
        Ok(self.wires.regs.read8(base + at))
    }

    /// §3.1.1 step 8: the device is live.
    pub fn driver_ok(mut self) -> Live<R> {
        self.wires.set_status(status::DRIVER_OK);
        Live { regs: self.wires.regs, doorbells: self.doorbells }
    }
}

/// A live device: what is left of the transport is its notify addresses.
pub struct Live<R: Registers> {
    regs: R,
    doorbells: Vec<(u16, usize)>,
}

impl<R: Registers> Live<R> {
    /// Tell the device of the chain `published` made available: the queue's
    /// index, sixteen bits, at its notify address (§4.1.5.2).
    ///
    /// # Panics
    /// If the chain's queue is not one [`Setup::enable`] enabled on this
    /// device, which is the driver's own mistake.
    pub fn notify(&self, published: Published) {
        let queue = published.queue();
        let (_, at) = self
            .doorbells
            .iter()
            .find(|(index, _)| *index == queue)
            .unwrap_or_else(|| panic!("virtio: queue {queue} was published to and never enabled"));
        self.regs.write16(*at, queue);
    }
}
