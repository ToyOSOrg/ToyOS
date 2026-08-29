use core::sync::atomic::{fence, Ordering};

use toyos_untrusted::{Refused, Untrusted};

use crate::mm::Mmio;
use super::pci::PciDevice;
use crate::mm::paging::CachePolicy;
use crate::log;

const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

const PCI_CAP_ID_VENDOR: u8 = 0x09;

/// Ceiling on how much of a BAR the kernel maps: the window's page tables are
/// real memory, so a device advertising more gets this much mapped and a named
/// refusal for any capability past it.
const BAR_WINDOW_CEILING: u64 = 16 << 20;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;

pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

pub const COMMON_DEVICE_FEATURE_SELECT: u64 = 0x00;
pub const COMMON_DEVICE_FEATURE: u64 = 0x04;
pub const COMMON_DRIVER_FEATURE_SELECT: u64 = 0x08;
pub const COMMON_DRIVER_FEATURE: u64 = 0x0C;
const COMMON_MSIX_CONFIG: u64 = 0x10;
pub const COMMON_DEVICE_STATUS: u64 = 0x14;
const COMMON_QUEUE_SELECT: u64 = 0x16;
pub const COMMON_QUEUE_SIZE: u64 = 0x18;
const COMMON_QUEUE_MSIX: u64 = 0x1A;
pub const COMMON_QUEUE_ENABLE: u64 = 0x1C;
pub const COMMON_QUEUE_NOTIFY_OFF: u64 = 0x1E;
pub const COMMON_QUEUE_DESC: u64 = 0x20;
pub const COMMON_QUEUE_DRIVER: u64 = 0x28;
pub const COMMON_QUEUE_DEVICE: u64 = 0x30;

/// Sentinel a virtio device reads back for a vector it could not allocate (virtio 1.2 §4.1.5.1.2).
const NO_VECTOR: u16 = 0xFFFF;

const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// One descriptor, for a caller sizing a table it allocates itself.
pub const DESC_BYTES: usize = core::mem::size_of::<VirtqDesc>();

/// Byte size of a split virtqueue's available ring for `queue_size` entries (virtio 1.2 §2.7.6).
/// Excludes `used_event`, which belongs to `VIRTIO_F_EVENT_IDX`, never negotiated by this kernel.
pub const fn avail_bytes(queue_size: u16) -> usize {
    AVAIL_RING_OFF + queue_size as usize * 2
}

/// Byte size of a split virtqueue's used ring for `queue_size` entries (virtio 1.2 §2.7.8).
pub const fn used_bytes(queue_size: u16) -> usize {
    USED_RING_OFF + queue_size as usize * USED_ELEM_SIZE
}

/// Byte size [`VirtqueueRegions::from_contiguous`] lays a queue of `queue_size` out in: the three rings plus padding.
pub const fn contiguous_bytes(queue_size: u16) -> usize {
    let avail_off = (queue_size as usize * DESC_BYTES + 1) & !1;
    let used_off = (avail_off + avail_bytes(queue_size) + 3) & !3;
    used_off + used_bytes(queue_size)
}

// Avail ring layout: flags(u16) + idx(u16) + ring[size](u16 each)
const AVAIL_IDX_OFF: usize = 2;
const AVAIL_RING_OFF: usize = 4;

// Used ring layout: flags(u16) + idx(u16) + ring[size](id:u32 + len:u32 each)
const USED_IDX_OFF: usize = 2;
const USED_RING_OFF: usize = 4;
const USED_ELEM_SIZE: usize = 8; // id(u32) + len(u32)

/// Parsed VirtIO PCI capability locations.
struct VirtioPciConfig {
    common: Mmio,
    notify: Mmio,
    notify_off_multiplier: u32,
    #[allow(dead_code)] // parsed from spec, used for interrupt-based operation
    isr: Mmio,
    device: Mmio,
}

/// Why a virtio PCI capability names no config window, or the window it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CapWindow {
    Refused(Refused),
    Unmapped,
}

/// The required capability a device's chain never produced a window for; its
/// chain crossed a trust boundary, so the device is refused, never asserted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingCap {
    Common,
    Notify,
    Isr,
    Device,
}

impl core::fmt::Display for MissingCap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self {
            Self::Common => "COMMON_CFG",
            Self::Notify => "NOTIFY_CFG",
            Self::Isr => "ISR_CFG",
            Self::Device => "DEVICE_CFG",
        };
        write!(f, "published no usable {name} capability")
    }
}

/// The config sub-window a virtio PCI capability names, or why it names none:
/// the device's index, offset and length checked before a subregion is taken.
fn cap_subwindow(
    mapped_bars: &[Option<Mmio>; 6],
    bar_byte: u8,
    offset: u32,
    length: u32,
) -> Result<Mmio, CapWindow> {
    let bar_idx = Untrusted::new(bar_byte).index(mapped_bars.len()).map_err(CapWindow::Refused)?;
    let bar = mapped_bars[bar_idx].as_ref().ok_or(CapWindow::Unmapped)?;
    // The mapped window is the BAR's advertised size or the ceiling, never under 16 bytes.
    let window = bar.size();
    let size = (length as u64).max(4);
    // length ≤ window keeps `window - size` from underflowing and offset + size inside the window.
    Untrusted::new(length).at_most(window).map_err(CapWindow::Refused)?;
    let offset = Untrusted::new(offset).at_most(window - size).map_err(CapWindow::Refused)?;
    Ok(bar.subregion(offset, size))
}

impl VirtioPciConfig {
    fn parse(pci_dev: &PciDevice) -> Result<Self, MissingCap> {
        let mut common = None;
        let mut notify = None;
        let mut notify_off_multiplier = 0u32;
        let mut isr = None;
        let mut device = None;

        let mut mapped_bars: [Option<crate::mm::Mmio>; 6] = [None, None, None, None, None, None];
        for cap in pci_dev.capabilities() {
            if cap.id() != PCI_CAP_ID_VENDOR { continue; }
            // The BAR index is the device's; one past the six the array holds skips the capability rather than indexing it.
            let Ok(bar_idx) = Untrusted::new(cap.read_u8(4)).index(mapped_bars.len()) else { continue };
            if mapped_bars[bar_idx].is_none() {
                // A capability naming a non-memory or unsizable BAR maps to no window.
                match pci_dev.memory_bar(bar_idx as u8) {
                    Ok(memory) => match pci_dev.bar_size(bar_idx as u8) {
                        Ok(advertised) => {
                            mapped_bars[bar_idx] = Some(crate::mm::paging::map_mmio(
                                memory.address(),
                                advertised.min(BAR_WINDOW_CEILING),
                                CachePolicy::DeferToMtrr,
                            ));
                        }
                        Err(why) => log!(
                            "VirtIO: PCI {:02x}:{:02x}.{} BAR {bar_idx}: {why} — skipping \
                             every capability in it",
                            pci_dev.bus, pci_dev.dev, pci_dev.func),
                    },
                    Err(why) => log!(
                        "VirtIO: PCI {:02x}:{:02x}.{} names BAR {bar_idx} and {why} — skipping \
                         every capability in it",
                        pci_dev.bus, pci_dev.dev, pci_dev.func),
                }
            }
        }

        for cap in pci_dev.capabilities() {
            if cap.id() != PCI_CAP_ID_VENDOR {
                continue;
            }
            let cfg_type = cap.read_u8(3);
            // Index, offset and length are the device's; a bad one skips this
            // capability rather than indexing the array or asserting past the window.
            let mmio = match cap_subwindow(&mapped_bars, cap.read_u8(4), cap.read_u32(8), cap.read_u32(12)) {
                Ok(mmio) => mmio,
                Err(CapWindow::Unmapped) => continue,
                Err(CapWindow::Refused(why)) => {
                    log!("VirtIO: PCI {:02x}:{:02x}.{} capability {why} — skipped",
                        pci_dev.bus, pci_dev.dev, pci_dev.func);
                    continue;
                }
            };

            match cfg_type {
                VIRTIO_PCI_CAP_COMMON_CFG if common.is_none() => common = Some(mmio),
                VIRTIO_PCI_CAP_NOTIFY_CFG if notify.is_none() => {
                    notify = Some(mmio);
                    notify_off_multiplier = cap.read_u32(16);
                }
                VIRTIO_PCI_CAP_ISR_CFG if isr.is_none() => isr = Some(mmio),
                VIRTIO_PCI_CAP_DEVICE_CFG if device.is_none() => device = Some(mmio),
                _ => {}
            }
        }

        Ok(Self {
            common: common.ok_or(MissingCap::Common)?,
            notify: notify.ok_or(MissingCap::Notify)?,
            notify_off_multiplier,
            isr: isr.ok_or(MissingCap::Isr)?,
            device: device.ok_or(MissingCap::Device)?,
        })
    }
}

use crate::mm::Dma;

/// Split virtqueue rings, reached only through [`Dma`]'s volatile, bounds-checked reads and writes.
pub struct VirtqueueRegions<'pool> {
    pub desc: Dma<'pool>,
    pub avail: Dma<'pool>,
    pub used: Dma<'pool>,
}

impl<'pool> VirtqueueRegions<'pool> {
    /// Compute regions from a single contiguous DMA buffer.
    pub fn from_contiguous(buf: Dma<'pool>, queue_size: u16) -> Self {
        let desc_size = queue_size as usize * DESC_BYTES;
        let avail_size = avail_bytes(queue_size);
        let used_size = used_bytes(queue_size);
        let avail_off = (desc_size + 1) & !1;
        let used_off = (avail_off + avail_size + 3) & !3;
        Self {
            desc: buf.subview(0, desc_size),
            avail: buf.subview(avail_off, avail_size),
            used: buf.subview(used_off, used_size),
        }
    }

    /// Compute regions from three separate DMA pages.
    pub fn from_separate(
        desc: Dma<'pool>,
        avail: Dma<'pool>,
        used: Dma<'pool>,
        queue_size: u16,
    ) -> Self {
        Self {
            desc: desc.subview(0, queue_size as usize * DESC_BYTES),
            avail: avail.subview(0, avail_bytes(queue_size)),
            used: used.subview(0, used_bytes(queue_size)),
        }
    }
}

/// Proof a descriptor slot is available for submission; `id()` is always below the queue's size.
/// Non-Copy, non-Clone: `submit()` consumes it, so a spent slot cannot be reused for another chain.
pub struct DescSlot(u16);

impl DescSlot {
    pub fn id(&self) -> u16 { self.0 }
}

/// Why a used-ring element this driver read is not one it will act on.
/// Refused rather than clamped: there is nothing here to recover from a forged completion.
/// Userland maps virtio-sound's control and event queues writable, so neither device-written field is trustworthy unchecked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsedRefusal {
    /// The head descriptor id is not an index into this queue's table.
    Head(Refused),
    /// The head names a descriptor this queue has published no chain at.
    NoChain { id: u16 },
    /// The device claims more bytes written than the chain this head was given.
    Written { id: u16, refused: Refused },
}

impl core::fmt::Display for UsedRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Head(refused) => write!(f, "its head descriptor {refused}"),
            Self::NoChain { id } => {
                write!(f, "a completion for descriptor {id}, where this queue published no chain")
            }
            Self::Written { id, refused } => {
                write!(f, "chain {id} was written {refused}")
            }
        }
    }
}

/// Interrupt-context, lock-free consumer of a virtqueue's used ring; an ISR can drain while another CPU submits under a lock.
/// Lock-free because it reads only device-written memory and its own `last_used_idx`, never shared driver state.
pub struct UsedRingConsumer<'pool> {
    used: Dma<'pool>,
    size: u16,
    last_used_idx: u16,
    refused: u32,
}

impl UsedRingConsumer<'_> {
    /// Non-blocking poll: the head descriptor id of a completed chain, or `None` if nothing new.
    /// Never logs: the only caller is an ISR and the log backend's lock is one it cannot wait on.
    pub fn poll(&mut self) -> Option<u16> {
        loop {
            let used_idx: u16 = self.used.read(USED_IDX_OFF);
            if used_idx == self.last_used_idx {
                return None;
            }
            // Acquire: pairs with the device's release when it bumps the used idx after writing the element.
            fence(Ordering::Acquire);
            let slot = self.last_used_idx % self.size;
            let id: Untrusted<u32> =
                Untrusted::new(self.used.read(USED_RING_OFF + slot as usize * USED_ELEM_SIZE));
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            // A refused head is skipped, not returned: `None` here means the ring is empty.
            let Ok(head) = id.index(self.size as usize) else {
                self.refused = self.refused.saturating_add(1);
                continue;
            };
            // Exact: `index` proved `head < self.size`, a `u16`.
            return Some(head as u16);
        }
    }

    /// How many used-ring elements this consumer has refused, for the life of the boot.
    pub fn refused(&self) -> u32 {
        self.refused
    }
}

/// A VirtIO split virtqueue.
pub struct Virtqueue<'pool> {
    desc: Dma<'pool>,
    avail: Dma<'pool>,
    used: Dma<'pool>,
    size: u16,
    last_used_idx: u16,
    notify_offset: u16,
    used_split: bool,
    /// Bytes each chain's descriptor was given; the one bound a device-reported `len` is compared against. 0 means no chain.
    chain_bytes: alloc::vec::Vec<u32>,
    refused: u32,
}

/// Direction of a buffer in a descriptor chain.
pub enum BufDir {
    Readable,
    Writable,
}

/// How many bytes a chain gives the device to write into.
/// Saturating, not wrapping: a wrap would shrink the bound below what was actually posted.
fn chain_bytes(bufs: &[(u64, u32, BufDir)]) -> u32 {
    bufs.iter().fold(0u32, |sum, (_, len, _)| sum.saturating_add(*len))
}

impl<'pool> Virtqueue<'pool> {
    /// Create a new virtqueue from a contiguous DMA region.
    /// Zeroes the whole buffer, including the padding, so the caller gets a page it can reason about.
    pub fn new(buf: Dma<'pool>, queue_size: u16) -> Self {
        buf.zero();
        Self::from_regions(&VirtqueueRegions::from_contiguous(buf, queue_size), queue_size)
    }

    /// Create a new virtqueue from explicit DMA regions.
    pub fn from_regions(regions: &VirtqueueRegions<'pool>, queue_size: u16) -> Self {
        let (desc, avail, used) = (regions.desc, regions.avail, regions.used);
        desc.zero();
        avail.zero();
        used.zero();
        Self {
            desc,
            avail,
            used,
            size: queue_size,
            last_used_idx: 0,
            notify_offset: 0,
            used_split: false,
            chain_bytes: alloc::vec![0u32; queue_size as usize],
            refused: 0,
        }
    }

    /// Hand the used ring to a dedicated consumer; afterwards `poll_used`/`has_used` panic here.
    pub fn split_used_consumer(&mut self) -> UsedRingConsumer<'pool> {
        assert!(!self.used_split, "virtqueue: used ring already split");
        self.used_split = true;
        UsedRingConsumer {
            used: self.used,
            size: self.size,
            last_used_idx: self.last_used_idx,
            refused: 0,
        }
    }

    /// Physical addresses for device register programming.
    pub fn descs_phys(&self) -> u64 { self.desc.phys() }
    pub fn avail_phys(&self) -> u64 { self.avail.phys() }
    pub fn used_phys(&self) -> u64 { self.used.phys() }

    /// Where in the notification region this queue's doorbell sits; meaningless before `setup_queue` runs.
    pub fn notify_bytes(&self, multiplier: u32) -> u64 {
        self.notify_offset as u64 * multiplier as u64
    }

    /// Write one descriptor chain without publishing it.
    /// Addressed by index, not [`DescSlot`]: the in-flight proof a slot carries belongs to the publisher, not here.
    pub fn write_chain(&mut self, first_desc: u16, bufs: &[(u64, u32, BufDir)]) {
        assert!(
            (first_desc as usize + bufs.len()) <= self.size as usize,
            "virtqueue: chain at {first_desc} of {} descriptors runs past a queue of {}",
            bufs.len(),
            self.size
        );
        self.chain_bytes[first_desc as usize] = chain_bytes(bufs);
        for (i, (addr, len, dir)) in bufs.iter().enumerate() {
            let desc_idx = first_desc + i as u16;
            let mut flags: u16 = match dir {
                BufDir::Readable => 0,
                BufDir::Writable => VIRTQ_DESC_F_WRITE,
            };
            if i != bufs.len() - 1 {
                flags |= VIRTQ_DESC_F_NEXT;
            }
            let desc = VirtqDesc { addr: *addr, len: *len, flags, next: desc_idx + 1 };
            self.desc.write(desc_idx as usize * core::mem::size_of::<VirtqDesc>(), desc);
        }
    }

    /// Where the used element at ring position `i` sits.
    fn used_elem_at(&self, i: u16) -> usize {
        USED_RING_OFF + i as usize * USED_ELEM_SIZE
    }

    /// The two fields of a used element, returned as [`Untrusted`] so no caller can use them without naming a bound.
    /// The only reads of a used element's fields in this queue; nothing else here touches them unwrapped.
    fn used_ring_id(&self, i: u16) -> Untrusted<u32> {
        Untrusted::new(self.used.read(self.used_elem_at(i)))
    }
    fn used_ring_len(&self, i: u16) -> Untrusted<u32> {
        Untrusted::new(self.used.read(self.used_elem_at(i) + 4))
    }

    /// The initial pool of descriptor slots; call once after construction.
    pub fn initial_slots(&self) -> alloc::vec::Vec<DescSlot> {
        (0..self.size).map(DescSlot).collect()
    }

    /// Submit a descriptor chain and notify the device (non-blocking); consumes the proving `DescSlot`.
    pub fn submit(
        &mut self,
        slot: DescSlot,
        bufs: &[(u64, u32, BufDir)],
        notify_mmio: Mmio,
        notify_multiplier: u32,
        queue_index: u16,
    ) -> u16 {
        let size = self.size;
        let first_desc = slot.0;
        self.chain_bytes[first_desc as usize] = chain_bytes(bufs);
        for (i, (addr, len, dir)) in bufs.iter().enumerate() {
            let desc_idx = (first_desc + i as u16) % size;
            let is_last = i == bufs.len() - 1;
            let next_idx = (desc_idx + 1) % size;

            let mut flags: u16 = match dir {
                BufDir::Readable => 0,
                BufDir::Writable => VIRTQ_DESC_F_WRITE,
            };
            if !is_last {
                flags |= VIRTQ_DESC_F_NEXT;
            }

            let desc = VirtqDesc { addr: *addr, len: *len, flags, next: next_idx };
            self.desc.write(desc_idx as usize * core::mem::size_of::<VirtqDesc>(), desc);
        }

        let avail_idx: u16 = self.avail.read(AVAIL_IDX_OFF);
        self.avail.write(AVAIL_RING_OFF + (avail_idx % size) as usize * 2, first_desc);
        fence(Ordering::Release);
        self.avail.write(AVAIL_IDX_OFF, avail_idx.wrapping_add(1));

        fence(Ordering::Release);
        let notify_off = self.notify_offset as u64 * notify_multiplier as u64;
        notify_mmio.write_u16(notify_off, queue_index);

        first_desc
    }

    /// Check if the device has completed any request.
    pub fn has_used(&self) -> bool {
        assert!(!self.used_split, "virtqueue: used ring split off");
        let used_idx: u16 = self.used.read(USED_IDX_OFF);
        used_idx != self.last_used_idx
    }

    /// Non-blocking poll of the used ring: `(DescSlot, written_len)` on completion, `None` if nothing new.
    /// A refused element is counted and skipped, never returned, so one forged element cannot hide the ones behind it.
    /// Never logs: the caller may hold `serial::BackendGuard`, the lock the log backend itself takes.
    pub fn poll_used(&mut self) -> Option<(DescSlot, u32)> {
        assert!(!self.used_split, "virtqueue: used ring split off");
        loop {
            let used_idx: u16 = self.used.read(USED_IDX_OFF);
            if used_idx == self.last_used_idx {
                return None;
            }
            fence(Ordering::Acquire);
            let slot = self.last_used_idx % self.size;
            let id = self.used_ring_id(slot);
            let len = self.used_ring_len(slot);
            self.last_used_idx = self.last_used_idx.wrapping_add(1);
            match self.parse_used(id, len) {
                Ok(elem) => return Some(elem),
                Err(_) => {
                    // Forfeit rather than recovered: losing a token costs throughput, believing a bad one costs memory.
                    self.refused = self.refused.saturating_add(1);
                    continue;
                }
            }
        }
    }

    /// What a used-ring element must satisfy, separated from the volatile reads so the self-test can exercise it.
    fn parse_used(
        &self,
        id: Untrusted<u32>,
        len: Untrusted<u32>,
    ) -> Result<(DescSlot, u32), UsedRefusal> {
        // `chain_bytes` is exactly `size` long: the descriptor table's own bound, not a constant beside it.
        let head = id.index(self.chain_bytes.len()).map_err(UsedRefusal::Head)?;
        // Exact: `index` proved `head < size`, a `u16`.
        let id = head as u16;
        let chain = self.chain_bytes[head];
        if chain == 0 {
            return Err(UsedRefusal::NoChain { id });
        }
        let written = len
            .at_most(chain as u64)
            .map_err(|refused| UsedRefusal::Written { id, refused })?;
        // Exact: `at_most` proved it is no more than `chain`, a `u32`.
        Ok((DescSlot(id), written as u32))
    }

    /// How many used-ring elements this queue has refused, for the life of the boot.
    pub fn refused(&self) -> u32 {
        self.refused
    }

    /// Write one used-ring element as a device would; the only writer of a used ring in this kernel, for [`used_selftest`] alone.
    /// Compiled only into the actuator kernel, so the shipping kernel never gains a way to write its own used ring.
    #[cfg(feature = "boot-actuators")]
    fn write_used_as_a_device_would(&self, at: u16, id: u32, len: u32) {
        let slot = at % self.size;
        self.used.write(self.used_elem_at(slot), id);
        self.used.write(self.used_elem_at(slot) + 4, len);
        fence(Ordering::Release);
        self.used.write::<u16>(USED_IDX_OFF, at.wrapping_add(1));
    }

    /// Submit a descriptor chain and block until the device completes it, returning the recovered `DescSlot`.
    pub fn submit_and_wait(
        &mut self,
        slot: DescSlot,
        bufs: &[(u64, u32, BufDir)],
        notify_mmio: Mmio,
        notify_multiplier: u32,
        queue_index: u16,
    ) -> DescSlot {
        self.submit(slot, bufs, notify_mmio, notify_multiplier, queue_index);
        loop {
            if let Some((slot, _)) = self.poll_used() {
                return slot;
            }
            core::hint::spin_loop();
        }
    }
}

/// Run [`Virtqueue::poll_used`] over eleven crafted used-ring elements no real device would ever send.
/// Exercises the shipped `poll_used` over a real [`Virtqueue`] and DMA page; only the writer of the ring is not a device.
#[cfg(feature = "boot-actuators")]
pub fn used_selftest() {
    use super::DmaPool;

    const SIZE: u16 = 16;
    /// The chain the self-test publishes at descriptor 3, in bytes.
    const CHAIN: u32 = 256;
    /// A descriptor inside the queue that no chain was ever built at.
    const UNBUILT: u32 = 5;
    const CASES: usize = 11;

    // Not leaked: the pool's pages go back when this returns, and `Dma<'_>`'s borrow keeps the queue from outliving them.
    let pool = DmaPool::alloc(0x1000);
    let dma = pool.view();
    let mut q = Virtqueue::new(dma.subview(0, 0x1000), SIZE);
    q.write_chain(3, &[(dma.phys(), CHAIN, BufDir::Writable)]);

    // `at` is what the queue's own `last_used_idx` will be when this element is read.
    let publish = Virtqueue::write_used_as_a_device_would;

    /// One table row: name, head id, completion length, and what `poll_used` must answer (`None` = must refuse).
    type Case = (&'static str, u32, u32, Option<(u16, u32)>);

    /// One element, and what `poll_used` must answer for it.
    const TABLE: [Case; 9] = [
        ("a chain the device filled", 3, CHAIN, Some((3, CHAIN))),
        ("a chain the device part-filled", 3, 1, Some((3, 1))),
        // A readable-only chain: the device wrote nothing into it and says so.
        ("a chain the device wrote nothing into", 3, 0, Some((3, 0))),
        ("a head past the queue", SIZE as u32, 0, None),
        // 0x1_0003 narrows to 3 under `as u16`; a driver that truncated before comparing would accept this.
        ("a head whose low 16 bits are in range", 0x1_0003, CHAIN, None),
        ("a head of every bit", u32::MAX, 0, None),
        ("one byte more than the chain", 3, CHAIN + 1, None),
        ("a length of every bit", 3, u32::MAX, None),
        ("a completion for a chain never published", UNBUILT, 0, None),
    ];

    let mut passed = 0usize;
    let mut at = 0u16;
    for (name, id, len, want) in TABLE {
        publish(&q, at, id, len);
        at = at.wrapping_add(1);
        let got = q.poll_used().map(|(slot, len)| (slot.id(), len));
        if got == want {
            passed += 1;
        } else {
            log!("virtio: used-ring selftest FAILED on {name}: got {got:?}, want {want:?}");
        }
    }

    // A completion behind a refused element is still delivered: forging one element must not hide the rest.
    publish(&q, at, u32::MAX, u32::MAX);
    publish(&q, at.wrapping_add(1), 3, CHAIN);
    let got = q.poll_used().map(|(slot, len)| (slot.id(), len));
    if got == Some((3, CHAIN)) {
        passed += 1;
    } else {
        log!("virtio: used-ring selftest FAILED on a chain behind a refused element: got {got:?}");
    }

    // The count is checked too: every case above would pass against a `poll_used` that refused right but counted nothing.
    let refused = q.refused();
    if refused != 7 {
        log!("virtio: used-ring selftest FAILED on the count: refused {refused}, want 7");
    } else {
        passed += 1;
    }
    log!("virtio: used-ring selftest {passed}/{CASES}");
}

/// Drive the real walk, window check and parse over config space no device
/// produces: a cyclic or spec-forbidden link, a BAR, offset or length past the
/// window, and a chain missing a required capability.
#[cfg(feature = "boot-actuators")]
pub fn cap_selftest() {
    use super::pci::{PciDevice, CAPABILITIES_PTR};
    use super::DmaPool;
    use crate::mm::{DirectMap, Mmio};
    use alloc::vec::Vec;

    const CASES: usize = 13;
    // Twice the 0x4000 the window bound used to guess, so a capability past the
    // guess but inside the BAR's real size has a case to be accepted by.
    const WINDOW: u64 = 0x8000;
    // One buffer for both the config space and the BAR window, so a permitted subregion stays inside it.
    let pool = DmaPool::alloc(WINDOW as usize);
    let phys = pool.view().phys();
    // SAFETY: the pool owns this page until it drops at end of function, and no Mmio built over it escapes.
    let cfg = unsafe { Mmio::over_phys(DirectMap::from_phys(phys), WINDOW) };
    let device = PciDevice::over_config(cfg);

    let mut passed = 0usize;
    let mut walk_case = |name: &str, head: u8, links: &[(u64, u8, u8)], want: &[u64]| {
        for off in 0..0x100u64 {
            cfg.write_u8(off, 0);
        }
        cfg.write_u8(CAPABILITIES_PTR, head);
        for &(at, id, next) in links {
            cfg.write_u8(at, id);
            cfg.write_u8(at + 1, next);
        }
        // Bounded, so a walk that fails to terminate is a wrong length here, not a hang.
        let got: Vec<u64> = device.capabilities().take(64).map(|c| c.offset()).collect();
        if got.as_slice() == want {
            passed += 1;
        } else {
            log!("virtio: pci cap selftest FAILED on {name}: yielded {got:?}, want {want:?}");
        }
    };

    const ID: u8 = PCI_CAP_ID_VENDOR;
    walk_case("a well-formed chain", 0x40, &[(0x40, ID, 0x50), (0x50, ID, 0)], &[0x40, 0x50]);
    walk_case("a list that cycles on itself", 0x40, &[(0x40, ID, 0x40)], &[0x40]);
    walk_case("a link that is not dword-aligned", 0x40, &[(0x40, ID, 0x43)], &[0x40]);
    walk_case("a link below the standard header", 0x40, &[(0x40, ID, 0x10)], &[0x40]);
    walk_case("a head the spec forbids", 0x41, &[], &[]);

    let mut bars: [Option<Mmio>; 6] = [None, None, None, None, None, None];
    bars[0] = Some(cfg);
    let mut win_case = |name: &str, bar: u8, offset: u32, length: u32, want_ok: bool| {
        let got = cap_subwindow(&bars, bar, offset, length);
        if got.is_ok() == want_ok {
            passed += 1;
        } else {
            log!("virtio: pci cap selftest FAILED on {name}: got ok={}, wanted ok={want_ok}", got.is_ok());
        }
    };
    win_case("a window inside the BAR", 0, 0x100, 0x100, true);
    win_case("a window ending exactly at the BAR's edge", 0, WINDOW as u32 - 4, 4, true);
    // Reds on the guessed 0x4000 constant this bound used to be.
    win_case("a window past the old guess, inside the BAR's real size", 0, 0x4000, 0x100, true);
    win_case("a BAR index past the six a header has", 7, 0, 4, false);
    win_case("a BAR the device did not map", 1, 0x100, 0x100, false);
    win_case("an offset that carries the window past the BAR", 0, WINDOW as u32 - 3, 0x100, false);
    win_case("a length past the whole BAR window", 0, 0, WINDOW as u32 + 1, false);

    // A chain lacking a required window is refused by name — this was four `expect`s and a panic.
    for off in 0..0x100u64 {
        cfg.write_u8(off, 0);
    }
    match VirtioPciConfig::parse(&device) {
        Err(MissingCap::Common) => passed += 1,
        Err(why) => log!("virtio: pci cap selftest FAILED on a chain missing its required caps: {why}"),
        Ok(_) => log!("virtio: pci cap selftest FAILED: an empty chain parsed"),
    }

    log!("virtio: pci cap selftest {passed}/{CASES}");
}

/// Which of a device's two interrupt sources it declined to bind — not a driver or kernel bug.
#[derive(Debug, Clone, Copy)]
pub enum NoVector {
    Config,
    Queue(u16),
}

impl core::fmt::Display for NoVector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Config => write!(f, "its configuration-change interrupt"),
            Self::Queue(queue) => write!(f, "queue {queue}'s interrupt"),
        }
    }
}

/// A fully initialized VirtIO device.
pub struct VirtioDevice {
    config: VirtioPciConfig,
}

impl VirtioDevice {
    /// Initialize a VirtIO PCI device: reset, negotiate features, prepare for queue setup.
    /// `Err` names the required capability the device never published a usable window for.
    pub fn init(pci_dev: &PciDevice, accepted_features: u64) -> Result<Self, MissingCap> {
        pci_dev.enable_bus_master();

        let config = VirtioPciConfig::parse(pci_dev)?;
        let common = config.common;

        // Order fixed by virtio 1.2 §3.1.1: reset, ACKNOWLEDGE, DRIVER, negotiate features, FEATURES_OK, verify.
        common.write_u32(COMMON_DEVICE_STATUS, 0);
        while common.read_u32(COMMON_DEVICE_STATUS) != 0 {
            core::hint::spin_loop();
        }

        common.write_u32(COMMON_DEVICE_STATUS, STATUS_ACKNOWLEDGE as u32);

        common.write_u32(COMMON_DEVICE_STATUS,
            STATUS_ACKNOWLEDGE as u32 | STATUS_DRIVER as u32);

        common.write_u32(COMMON_DEVICE_FEATURE_SELECT, 0);
        let device_features_lo = common.read_u32(COMMON_DEVICE_FEATURE);
        common.write_u32(COMMON_DEVICE_FEATURE_SELECT, 1);
        let device_features_hi = common.read_u32(COMMON_DEVICE_FEATURE);
        let device_features = (device_features_hi as u64) << 32 | device_features_lo as u64;

        let features = device_features & accepted_features;
        log!("VirtIO: device features={:#x} negotiated={:#x}", device_features, features);

        common.write_u32(COMMON_DRIVER_FEATURE_SELECT, 0);
        common.write_u32(COMMON_DRIVER_FEATURE, features as u32);
        common.write_u32(COMMON_DRIVER_FEATURE_SELECT, 1);
        common.write_u32(COMMON_DRIVER_FEATURE, (features >> 32) as u32);

        let status = STATUS_ACKNOWLEDGE as u32 | STATUS_DRIVER as u32 | STATUS_FEATURES_OK as u32;
        common.write_u32(COMMON_DEVICE_STATUS, status);

        assert!(
            common.read_u32(COMMON_DEVICE_STATUS) & STATUS_FEATURES_OK as u32 != 0,
            "VirtIO: device rejected features"
        );

        Ok(Self { config })
    }

    /// Configure a virtqueue's addresses and size; does not enable it — call `enable_queue()` after MSI-X vectors are set.
    pub fn setup_queue(&self, index: u16, queue: &mut Virtqueue<'_>) {
        let common = self.config.common;

        common.write_u16(COMMON_QUEUE_SELECT, index);

        let max_size = common.read_u16(COMMON_QUEUE_SIZE);
        assert!(max_size >= queue.size, "VirtIO: queue {} too small (max={}, need={})", index, max_size, queue.size);
        common.write_u16(COMMON_QUEUE_SIZE, queue.size);

        common.write_u64(COMMON_QUEUE_DESC, queue.descs_phys());
        common.write_u64(COMMON_QUEUE_DRIVER, queue.avail_phys());
        common.write_u64(COMMON_QUEUE_DEVICE, queue.used_phys());

        queue.notify_offset = common.read_u16(COMMON_QUEUE_NOTIFY_OFF);
    }

    /// Enable a previously configured virtqueue.
    pub fn enable_queue(&self, index: u16) {
        let common = self.config.common;
        common.write_u16(COMMON_QUEUE_SELECT, index);
        common.write_u16(COMMON_QUEUE_ENABLE, 1);
    }

    /// Point the device's config-change and `queue`'s used-ring interrupt at `pci::MSIX_ENTRY`.
    /// Kept separate from `PciDevice::enable_msix`: both calls are needed, neither implies the other.
    pub fn bind_msix(&self, queue: u16) -> Result<(), NoVector> {
        let common = self.config.common;
        common.write_u16(COMMON_MSIX_CONFIG, super::pci::MSIX_ENTRY);
        if common.read_u16(COMMON_MSIX_CONFIG) == NO_VECTOR {
            return Err(NoVector::Config);
        }
        common.write_u16(COMMON_QUEUE_SELECT, queue);
        common.write_u16(COMMON_QUEUE_MSIX, super::pci::MSIX_ENTRY);
        if common.read_u16(COMMON_QUEUE_MSIX) == NO_VECTOR {
            return Err(NoVector::Queue(queue));
        }
        Ok(())
    }

    /// Set DRIVER_OK — device is now live.
    pub fn activate(&self) {
        let status = STATUS_ACKNOWLEDGE as u32
            | STATUS_DRIVER as u32
            | STATUS_FEATURES_OK as u32
            | STATUS_DRIVER_OK as u32;
        self.config.common.write_u32(COMMON_DEVICE_STATUS, status);
    }

    pub fn notify_mmio(&self) -> Mmio {
        self.config.notify
    }

    pub fn notify_off_multiplier(&self) -> u32 {
        self.config.notify_off_multiplier
    }

    pub fn device_config(&self) -> Mmio {
        self.config.device
    }
}
