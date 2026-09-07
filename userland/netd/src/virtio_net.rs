//! The virtio-net driver. It runs here, in netd, not in the kernel.
//!
//! What the kernel keeps is the *claim*: config space, the interrupt vector it
//! programmed into this function's MSI-X table, and the address space the
//! function translates through. Everything below that is this file, and none of
//! it is authority: an address written into a descriptor is one
//! `PciDev::dma_alloc` answered, and one this driver invents instead is refused
//! at the unit and recorded against this process.
//!
//! **The device is not trusted, and neither is a used-ring element it wrote.**
//! [`Rings::parse_used`] bounds the head against the descriptor table's own
//! length and the written length against what that chain was given; an element
//! failing either is counted and dropped, because losing a token costs
//! throughput and believing a bad one costs memory. The device is on the far
//! side of the same boundary wherever the driver sits.
//!
//! Virtio 1.2 throughout: §4.1.4 for the PCI capability layout, §2.7 for the
//! split virtqueue, §5.1 for the network device.

use std::cell::{Cell, RefCell};

use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::{RegWidth, SyscallError};

/// PCI's own vendor-specific capability id; virtio's four config windows are
/// all published under it (§4.1.4).
const CAP_ID_VENDOR: u8 = 0x09;
const CAP_COMMON_CFG: u8 = 1;
const CAP_NOTIFY_CFG: u8 = 2;
const CAP_ISR_CFG: u8 = 3;
const CAP_DEVICE_CFG: u8 = 4;

/// Where the capability list starts, and how far a walk may follow it. The
/// pointer is the *device's*, so a malformed or cyclic chain ends the walk
/// rather than running for ever.
const CAPABILITIES_PTR: u32 = 0x34;
const MAX_CAPABILITIES: usize = 48;

// Common configuration structure, §4.1.4.3.
const COMMON_DEVICE_FEATURE_SELECT: usize = 0x00;
const COMMON_DEVICE_FEATURE: usize = 0x04;
const COMMON_DRIVER_FEATURE_SELECT: usize = 0x08;
const COMMON_DRIVER_FEATURE: usize = 0x0C;
const COMMON_MSIX_CONFIG: usize = 0x10;
const COMMON_DEVICE_STATUS: usize = 0x14;
const COMMON_QUEUE_SELECT: usize = 0x16;
const COMMON_QUEUE_SIZE: usize = 0x18;
const COMMON_QUEUE_MSIX: usize = 0x1A;
const COMMON_QUEUE_ENABLE: usize = 0x1C;
const COMMON_QUEUE_NOTIFY_OFF: usize = 0x1E;
const COMMON_QUEUE_DESC: usize = 0x20;
const COMMON_QUEUE_DRIVER: usize = 0x28;
const COMMON_QUEUE_DEVICE: usize = 0x30;

const STATUS_ACKNOWLEDGE: u32 = 1;
const STATUS_DRIVER: u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;
const STATUS_FEATURES_OK: u32 = 8;

const VIRTIO_F_VERSION_1: u64 = 1 << 32;
/// §6: the device's buffer addresses are the platform's, so what translates any
/// other function's translates this one's. A device never offered it is one no
/// unit sees; the driver accepts it wherever it is offered, and the negotiated
/// set is logged for a gate to read back.
const VIRTIO_F_ACCESS_PLATFORM: u64 = 1 << 33;
const VIRTIO_NET_F_MAC: u64 = 1 << 5;

/// The one MSI-X table entry the kernel programs. A device answering
/// [`NO_VECTOR`] for it has refused the binding (§4.1.5.1.2).
const MSIX_ENTRY: u16 = 0;
const NO_VECTOR: u16 = 0xFFFF;

const VIRTQ_DESC_F_WRITE: u16 = 2;
const DESC_BYTES: usize = 16;
const AVAIL_IDX_OFF: usize = 2;
const AVAIL_RING_OFF: usize = 4;
const USED_IDX_OFF: usize = 2;
const USED_RING_OFF: usize = 4;
const USED_ELEM_BYTES: usize = 8;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
/// One descriptor per receive buffer, and the two counts are one number: buffer
/// `i` is posted at head `i` and nowhere else, so a completion's head *is* the
/// buffer it filled and nothing the device wrote is used as an index.
const RX_QUEUE_SIZE: u16 = 256;
pub const RX_BUF_COUNT: usize = RX_QUEUE_SIZE as usize;
pub const RX_BUF_SIZE: usize = 4096;
const TX_QUEUE_SIZE: u16 = 16;
pub const TX_BUF_SIZE: usize = 4096;
/// The header virtio 1.0 puts in front of every frame, both directions: always
/// twelve bytes with `VERSION_1`, `num_buffers` included (§5.1.6).
pub const NET_HDR_SIZE: usize = 12;

/// The grant's layout: the receive queue's three rings on a page each, the
/// transmit queue's three in one page, then the frames. One grant, because
/// every byte of it is this process's own — what a wrong offset here corrupts
/// is this driver's memory and nobody else's.
const OFF_RX_DESC: usize = 0x0000;
const OFF_RX_AVAIL: usize = 0x1000;
const OFF_RX_USED: usize = 0x2000;
const OFF_TX_RINGS: usize = 0x3000;
const OFF_RX_BUFS: usize = 0x4000;
const OFF_TX_BUF: usize = OFF_RX_BUFS + RX_BUF_COUNT * RX_BUF_SIZE;
const GRANT_BYTES: u64 = (OFF_TX_BUF + TX_BUF_SIZE) as u64;

const fn tx_avail_off() -> usize {
    (TX_QUEUE_SIZE as usize * DESC_BYTES + 1) & !1
}

const fn tx_used_off() -> usize {
    (tx_avail_off() + AVAIL_RING_OFF + TX_QUEUE_SIZE as usize * 2 + 3) & !3
}

const _: () = {
    assert!(RX_QUEUE_SIZE as usize * DESC_BYTES <= OFF_RX_AVAIL - OFF_RX_DESC);
    assert!(AVAIL_RING_OFF + RX_QUEUE_SIZE as usize * 2 <= OFF_RX_USED - OFF_RX_AVAIL);
    assert!(
        USED_RING_OFF + RX_QUEUE_SIZE as usize * USED_ELEM_BYTES <= OFF_TX_RINGS - OFF_RX_USED
    );
    assert!(tx_used_off() + USED_RING_OFF + TX_QUEUE_SIZE as usize * USED_ELEM_BYTES <= 0x1000);
    assert!(NET_HDR_SIZE < RX_BUF_SIZE && NET_HDR_SIZE < TX_BUF_SIZE);
};

/// A bounds-checked volatile window over memory something other than this
/// thread also reads or writes — a register aperture, or a ring the device
/// walks.
///
/// Volatile because both race this process: a plain read of a used-ring index
/// can be hoisted out of the loop that waits on it, and a plain write to a
/// doorbell can be elided entirely.
#[derive(Clone, Copy)]
struct Window {
    base: *mut u8,
    len: usize,
}

impl Window {
    /// # Safety
    /// `base` must name at least `len` bytes of a live mapping, for as long as
    /// this window or any window derived from it is used.
    unsafe fn new(base: *mut u8, len: usize) -> Self {
        Self { base, len }
    }

    fn sub(self, offset: usize, len: usize) -> Self {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "netd: a {len}-byte window at {offset:#x} runs past {:#x}",
            self.len
        );
        // SAFETY: the assertion above kept `offset + len` inside `self`, which
        // its own constructor's contract says is mapped.
        Self { base: unsafe { self.base.add(offset) }, len }
    }

    fn at<T>(self, offset: usize) -> *mut T {
        assert!(
            offset.checked_add(size_of::<T>()).is_some_and(|end| end <= self.len),
            "netd: a {}-byte access at {offset:#x} runs past {:#x}",
            size_of::<T>(),
            self.len
        );
        assert!(
            (self.base as usize + offset) % align_of::<T>() == 0,
            "netd: a {}-byte access at {offset:#x} is not aligned for it",
            size_of::<T>(),
        );
        // SAFETY: bounded and aligned by the two assertions above.
        unsafe { self.base.add(offset) as *mut T }
    }

    fn read<T: Copy>(self, offset: usize) -> T {
        // SAFETY: `at` bounded and aligned the pointer; volatile because the
        // device may write the same bytes concurrently.
        unsafe { self.at::<T>(offset).read_volatile() }
    }

    fn write<T: Copy>(self, offset: usize, value: T) {
        // SAFETY: `at` bounded and aligned the pointer; volatile because the
        // device may read the same bytes concurrently.
        unsafe { self.at::<T>(offset).write_volatile(value) }
    }

    fn zero(self) {
        // SAFETY: `self.len` bytes from `self.base`, which is the whole of what
        // this window covers.
        unsafe { std::ptr::write_bytes(self.base, 0, self.len) }
    }
}

/// One split-virtqueue descriptor (§2.7.5).
#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Why a used-ring element was refused. The device wrote it, so it is a claim
/// about this driver's own memory and the only safe answer is to drop it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UsedRefusal {
    /// A head index past the descriptor table.
    Head(u32),
    /// A head this driver has no chain published at — a completion for a
    /// buffer it has already taken back, or one it never posted.
    NoChain(u16),
    /// More bytes written than the chain was given.
    Written { id: u16, len: u32, chain: u32 },
}

/// One queue's rings and the bookkeeping that says what is in flight.
struct Rings {
    desc: Window,
    avail: Window,
    used: Window,
    size: u16,
    last_used: u16,
    /// Bytes the chain at each head was given, indexed by head: the one bound a
    /// device-reported length is checked against, and 0 means no chain is
    /// published there.
    chain_bytes: Vec<u32>,
    refused: u32,
    /// Where this queue's doorbell sits, from `queue_notify_off`.
    notify_off: u16,
}

impl Rings {
    fn new(desc: Window, avail: Window, used: Window, size: u16) -> Self {
        desc.zero();
        avail.zero();
        used.zero();
        Self {
            desc,
            avail,
            used,
            size,
            last_used: 0,
            chain_bytes: vec![0; size as usize],
            refused: 0,
            notify_off: 0,
        }
    }

    /// Publish a one-descriptor chain at `head` and ring the doorbell.
    fn submit(&mut self, head: u16, addr: u64, len: u32, writable: bool, doorbell: Doorbell) {
        self.chain_bytes[head as usize] = len;
        let flags = if writable { VIRTQ_DESC_F_WRITE } else { 0 };
        // One descriptor per chain, so nothing here sets `VIRTQ_DESC_F_NEXT`
        // and `next` is never followed.
        self.desc.write(head as usize * DESC_BYTES, Desc { addr, len, flags, next: 0 });

        let avail_idx: u16 = self.avail.read(AVAIL_IDX_OFF);
        self.avail.write(AVAIL_RING_OFF + (avail_idx % self.size) as usize * 2, head);
        // The device must see the ring entry before the index that publishes
        // it, and the index before the doorbell (§2.7.13).
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        self.avail.write(AVAIL_IDX_OFF, avail_idx.wrapping_add(1));
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        doorbell.ring();
    }

    /// The next completion, or `None`. A refused element is counted and
    /// skipped, never returned, so one bad element cannot hide the ones behind
    /// it.
    ///
    /// The chain is retired here, so a device that reports the same head twice
    /// is refused by the second report rather than handing a caller a buffer it
    /// has already been given.
    fn poll_used(&mut self) -> Option<(u16, u32)> {
        loop {
            let used_idx: u16 = self.used.read(USED_IDX_OFF);
            if used_idx == self.last_used {
                return None;
            }
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            let at = USED_RING_OFF + (self.last_used % self.size) as usize * USED_ELEM_BYTES;
            let id: u32 = self.used.read(at);
            let len: u32 = self.used.read(at + 4);
            self.last_used = self.last_used.wrapping_add(1);
            match self.parse_used(id, len) {
                Ok((head, written)) => {
                    self.chain_bytes[head as usize] = 0;
                    return Some((head, written));
                }
                Err(_) => {
                    self.refused = self.refused.saturating_add(1);
                    continue;
                }
            }
        }
    }

    /// What a used element must satisfy. Separate from the volatile reads, so
    /// the tests below can drive it with elements no device would send.
    fn parse_used(&self, id: u32, len: u32) -> Result<(u16, u32), UsedRefusal> {
        if id as usize >= self.chain_bytes.len() {
            return Err(UsedRefusal::Head(id));
        }
        let id = id as u16;
        let chain = self.chain_bytes[id as usize];
        if chain == 0 {
            return Err(UsedRefusal::NoChain(id));
        }
        if len > chain {
            return Err(UsedRefusal::Written { id, len, chain });
        }
        Ok((id, len))
    }
}

/// Where one queue's doorbell is, taken out of the notify window once.
#[derive(Clone, Copy)]
struct Doorbell {
    window: Window,
    queue: u16,
}

impl Doorbell {
    fn ring(self) {
        self.window.write(0, self.queue);
    }
}

/// Why the device was not brought up. Each keeps its own word: a machine with
/// no such device and one that refused a feature set ask different things of a
/// caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    MissingCap(&'static str),
    ResetUnanswered,
    FeaturesRefused { offered: u64, status: u32 },
    NoVector(&'static str),
    /// The kernel refused a call this driver cannot work without, and the word
    /// is the kernel's own.
    Kernel(&'static str, SyscallError),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingCap(what) => write!(f, "it published no usable {what}"),
            Self::ResetUnanswered => write!(f, "it never zeroed DEVICE_STATUS for its reset"),
            Self::FeaturesRefused { offered, status } => write!(
                f,
                "it refused the feature set {offered:#x} this driver accepted, leaving \
                 DEVICE_STATUS={status:#x} without FEATURES_OK"
            ),
            Self::NoVector(what) => write!(f, "it refused a vector for {what}"),
            Self::Kernel(call, why) => write!(f, "the kernel refused {call}: {why:?}"),
        }
    }
}

/// The virtio-net function, brought up and driving.
pub struct VirtioNet {
    dev: PciDev,
    rx_doorbell: Doorbell,
    tx_doorbell: Doorbell,
    /// Held for their mappings' lives: every window above and below points into
    /// one of these two.
    _bar: SharedMemory,
    _grant: DmaRegion,
    dma: Window,
    /// Where the device reaches the grant's first byte. Not a physical address:
    /// what the unit translates for this function and for nothing else.
    dma_device_addr: u64,
    rx: RefCell<Rings>,
    tx: RefCell<Rings>,
    /// Transmit heads nothing is in flight on.
    tx_free: RefCell<Vec<u16>>,
    /// Frames dropped for want of one. A count and not a wait: a server never
    /// blocks, and a dropped frame's recovery is the peer's retransmit.
    tx_dropped: Cell<u32>,
    /// What `report_refusals` last said, so it says it again only on a change.
    reported: Cell<(u32, u32)>,
    mac: [u8; 6],
}

impl VirtioNet {
    /// Bring the function up: the capability chain, the reset handshake, the
    /// feature negotiation, both queues and the vector binding — in the order
    /// virtio 1.2 §3.1.1 fixes.
    pub fn open(dev: PciDev) -> Result<Self, Refusal> {
        let info = dev.describe().map_err(|e| Refusal::Kernel("the claim's description", e))?;

        let caps = Capabilities::walk(&dev);
        let common_cap = caps.find(CAP_COMMON_CFG).ok_or(Refusal::MissingCap("COMMON_CFG"))?;
        let notify_cap = caps.find(CAP_NOTIFY_CFG).ok_or(Refusal::MissingCap("NOTIFY_CFG"))?;
        // Found and not used: the vector is bound, so nothing here polls the
        // ISR status byte. Its absence still refuses the device — a chain
        // missing it is a chain this driver did not understand.
        let isr_cap = caps.find(CAP_ISR_CFG).ok_or(Refusal::MissingCap("ISR_CFG"))?;
        let device_cap = caps.find(CAP_DEVICE_CFG).ok_or(Refusal::MissingCap("DEVICE_CFG"))?;

        // One BAR carries all four on every virtio device in reach, and the
        // kernel hands out a BAR at a time; a device that spread them would be
        // refused here rather than driven half-mapped.
        let bar = common_cap.bar;
        if [notify_cap, isr_cap, device_cap].iter().any(|cap| cap.bar != bar) {
            return Err(Refusal::MissingCap("its four config windows in one BAR"));
        }
        let bar_bytes = *info
            .bar_bytes
            .get(bar as usize)
            .filter(|bytes| **bytes > 0)
            .ok_or(Refusal::MissingCap("a BAR this claim may map"))?;
        let mapped = dev
            .map_bar(bar as u32, bar_bytes)
            .map_err(|e| Refusal::Kernel("the register window", e))?;
        // SAFETY: the mapping is `bar_bytes` long and lives as long as
        // `mapped`, which this struct holds for its own life.
        let window = unsafe { Window::new(mapped.as_ptr(), bar_bytes as usize) };

        let common = common_cap.window(window)?;
        let notify = notify_cap.window(window)?;
        let device_cfg = device_cap.window(window)?;

        common.write::<u32>(COMMON_DEVICE_STATUS, 0);
        // The reset is acknowledged by the register reading back zero (§4.1.4.3.2).
        if common.read::<u32>(COMMON_DEVICE_STATUS) != 0 {
            return Err(Refusal::ResetUnanswered);
        }
        common.write::<u32>(COMMON_DEVICE_STATUS, STATUS_ACKNOWLEDGE);
        common.write::<u32>(COMMON_DEVICE_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);

        common.write::<u32>(COMMON_DEVICE_FEATURE_SELECT, 0);
        let lo: u32 = common.read(COMMON_DEVICE_FEATURE);
        common.write::<u32>(COMMON_DEVICE_FEATURE_SELECT, 1);
        let hi: u32 = common.read(COMMON_DEVICE_FEATURE);
        let offered = ((hi as u64) << 32) | lo as u64;
        let features =
            offered & (VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | VIRTIO_NET_F_MAC);
        // The line the kernel's virtio drivers print, in the same shape:
        // `iommu_virtio_platform` reads it back for every virtio function the
        // machine creates, and this one is no longer the kernel's to print.
        crate::say!(
            "netd: VirtIO: PCI {:02x}:{:02x}.{} features device={offered:#x} \
             negotiated={features:#x} access_platform={}",
            info.bus,
            info.dev,
            info.func,
            if features & VIRTIO_F_ACCESS_PLATFORM != 0 { 'y' } else { 'n' },
        );

        common.write::<u32>(COMMON_DRIVER_FEATURE_SELECT, 0);
        common.write::<u32>(COMMON_DRIVER_FEATURE, features as u32);
        common.write::<u32>(COMMON_DRIVER_FEATURE_SELECT, 1);
        common.write::<u32>(COMMON_DRIVER_FEATURE, (features >> 32) as u32);
        common.write::<u32>(
            COMMON_DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK,
        );
        let answered: u32 = common.read(COMMON_DEVICE_STATUS);
        if answered & STATUS_FEATURES_OK == 0 {
            return Err(Refusal::FeaturesRefused { offered: features, status: answered });
        }

        let grant = dev.dma_alloc(GRANT_BYTES).map_err(|e| Refusal::Kernel("a DMA grant", e))?;
        // SAFETY: the grant covers at least `GRANT_BYTES` — the kernel rounds
        // the request up to whole pages, never down — and lives as long as
        // `grant`, which this struct holds.
        let dma = unsafe { Window::new(grant.memory.as_ptr(), GRANT_BYTES as usize) };
        dma.zero();
        let dma_device_addr = grant.device_addr;

        let mut rx = Rings::new(
            dma.sub(OFF_RX_DESC, RX_QUEUE_SIZE as usize * DESC_BYTES),
            dma.sub(OFF_RX_AVAIL, AVAIL_RING_OFF + RX_QUEUE_SIZE as usize * 2),
            dma.sub(OFF_RX_USED, USED_RING_OFF + RX_QUEUE_SIZE as usize * USED_ELEM_BYTES),
            RX_QUEUE_SIZE,
        );
        let mut tx = Rings::new(
            dma.sub(OFF_TX_RINGS, TX_QUEUE_SIZE as usize * DESC_BYTES),
            dma.sub(OFF_TX_RINGS + tx_avail_off(), AVAIL_RING_OFF + TX_QUEUE_SIZE as usize * 2),
            dma.sub(
                OFF_TX_RINGS + tx_used_off(),
                USED_RING_OFF + TX_QUEUE_SIZE as usize * USED_ELEM_BYTES,
            ),
            TX_QUEUE_SIZE,
        );

        setup_queue(common, RX_QUEUE, &mut rx, dma_device_addr, OFF_RX_DESC, OFF_RX_AVAIL, OFF_RX_USED)?;
        setup_queue(
            common,
            TX_QUEUE,
            &mut tx,
            dma_device_addr,
            OFF_TX_RINGS,
            OFF_TX_RINGS + tx_avail_off(),
            OFF_TX_RINGS + tx_used_off(),
        )?;

        // The vector the kernel already put in the table, named to the device.
        // After the queues are configured and before they are enabled, so no
        // queue is ever live with no vector bound to it.
        common.write::<u16>(COMMON_MSIX_CONFIG, MSIX_ENTRY);
        if common.read::<u16>(COMMON_MSIX_CONFIG) == NO_VECTOR {
            return Err(Refusal::NoVector("its configuration-change interrupt"));
        }
        for (queue, what) in [(RX_QUEUE, "the receive queue"), (TX_QUEUE, "the transmit queue")] {
            common.write::<u16>(COMMON_QUEUE_SELECT, queue);
            common.write::<u16>(COMMON_QUEUE_MSIX, MSIX_ENTRY);
            if common.read::<u16>(COMMON_QUEUE_MSIX) == NO_VECTOR {
                return Err(Refusal::NoVector(what));
            }
        }
        for queue in [RX_QUEUE, TX_QUEUE] {
            common.write::<u16>(COMMON_QUEUE_SELECT, queue);
            common.write::<u16>(COMMON_QUEUE_ENABLE, 1);
        }

        let mut mac = [0u8; 6];
        for (i, byte) in mac.iter_mut().enumerate() {
            *byte = device_cfg.read::<u8>(i);
        }

        let rx_doorbell = doorbell(notify, notify_cap.notify_multiplier, rx.notify_off, RX_QUEUE);
        let tx_doorbell = doorbell(notify, notify_cap.notify_multiplier, tx.notify_off, TX_QUEUE);

        common.write::<u32>(
            COMMON_DEVICE_STATUS,
            STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK,
        );

        let nic = Self {
            dev,
            rx_doorbell,
            tx_doorbell,
            _bar: mapped,
            _grant: grant,
            dma,
            dma_device_addr,
            rx: RefCell::new(rx),
            tx: RefCell::new(tx),
            tx_free: RefCell::new((0..TX_QUEUE_SIZE).rev().collect()),
            tx_dropped: Cell::new(0),
            reported: Cell::new((0, 0)),
            mac,
        };

        // Every receive buffer posted before a frame can arrive.
        for index in 0..RX_BUF_COUNT {
            nic.post_rx(index);
        }
        Ok(nic)
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// Say what this driver has refused or dropped, when either count has
    /// moved.
    ///
    /// **On change, not per element**: a device flooding the used ring with
    /// elements this driver will not act on costs one line, not one per
    /// element — which is the difference between a diagnostic and a way to
    /// drown the console from the other side of the boundary.
    fn report_refusals(&self) {
        let refused = self.rx.borrow().refused + self.tx.borrow().refused;
        let dropped = self.tx_dropped.get();
        if (refused, dropped) == self.reported.get() {
            return;
        }
        self.reported.set((refused, dropped));
        crate::say!(
            "netd: this NIC has refused {refused} used-ring element(s) — the device named a \
             descriptor this driver never published or claimed more bytes than it was given — \
             and dropped {dropped} frame(s) with no transmit descriptor free"
        );
    }

    /// Drain the interrupt record, so the claim stops reading ready.
    ///
    /// The count is not acted on — what a message meant is in the rings — but
    /// it has to be consumed, or the poller reports the same interrupt for ever.
    pub fn take_interrupt(&self) -> u32 {
        match self.dev.irq() {
            Ok(record) => record.count,
            // `WouldBlock` is the ordinary "nothing since the last read".
            Err(SyscallError::WouldBlock) => 0,
            // Anything else is the kernel saying this device is no longer
            // this driver's: a fault at the unit is the one that happens, and
            // by the time it is answered the function's bus mastering is gone.
            // There is nothing to degrade to — every frame from here on is one
            // that silently never arrives — so this dies where it can be read.
            Err(why) => panic!("netd: this NIC's claim refused an interrupt read: {why:?}"),
        }
    }

    /// Post receive buffer `index` on its own head.
    fn post_rx(&self, index: usize) {
        let head = index as u16;
        let at = OFF_RX_BUFS + index * RX_BUF_SIZE;
        // The header is zeroed before the buffer is published: what the device
        // writes there is its own, and what it leaves behind is this driver's.
        self.dma.sub(at, NET_HDR_SIZE).zero();
        self.rx.borrow_mut().submit(
            head,
            self.dma_device_addr + at as u64,
            RX_BUF_SIZE as u32,
            true,
            self.rx_doorbell,
        );
    }

    /// The next received frame as `(buffer index, frame bytes)`, the virtio
    /// header excluded, or `None`.
    pub fn poll_rx(&self) -> Option<(usize, usize)> {
        loop {
            let polled = self.rx.borrow_mut().poll_used();
            // After the poll and before the answer, so a pass that refused
            // every element it saw still says so.
            self.report_refusals();
            let (head, written) = polled?;
            // `parse_used` bounded the head by the descriptor table's length,
            // which is this queue's size, which is the buffer count.
            let index = head as usize;
            let total = written as usize;
            if total <= NET_HDR_SIZE {
                // Shorter than its own header is nothing to hand up, and the
                // buffer goes straight back.
                self.post_rx(index);
                continue;
            }
            return Some((index, total - NET_HDR_SIZE));
        }
    }

    /// Give buffer `index` back to the device, after its frame has been read.
    ///
    /// A buffer never given back is a receive slot lost for the boot: 256 of
    /// them and this NIC stops receiving.
    pub fn rx_done(&self, index: usize) {
        assert!(index < RX_BUF_COUNT, "netd: buffer {index} is not one this NIC has");
        self.post_rx(index);
    }

    /// Where a received frame's bytes are, past the virtio header.
    pub fn rx_frame(&self, index: usize, len: usize) -> &[u8] {
        let window = self.dma.sub(OFF_RX_BUFS + index * RX_BUF_SIZE + NET_HDR_SIZE, len);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device has finished with this buffer — its used-ring
        // element is what said so — and it is not posted again until
        // `rx_done`, which the caller makes after this borrow ends.
        unsafe { std::slice::from_raw_parts(window.base as *const u8, len) }
    }

    /// The transmit buffer, past the virtio header, for a frame of `len` bytes.
    pub fn tx_frame(&self, len: usize) -> &mut [u8] {
        assert!(
            NET_HDR_SIZE + len <= TX_BUF_SIZE,
            "netd: a {len}-byte frame does not fit this NIC's transmit buffer"
        );
        let window = self.dma.sub(OFF_TX_BUF + NET_HDR_SIZE, len);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device is not reading it, because `tx` publishes the
        // descriptor only after this borrow ends.
        unsafe { std::slice::from_raw_parts_mut(window.base, len) }
    }

    /// Hand the transmit buffer's first `len` frame bytes to the device.
    ///
    /// Non-blocking, and `false` is a frame dropped for want of a head. **A
    /// server never blocks**, and a dropped frame's recovery is the peer's
    /// retransmit; spinning on the used ring here would park netd on a device.
    pub fn tx(&self, len: usize) -> bool {
        self.reclaim_tx();
        let Some(head) = self.tx_free.borrow_mut().pop() else {
            self.tx_dropped.set(self.tx_dropped.get().saturating_add(1));
            self.report_refusals();
            return false;
        };
        self.dma.sub(OFF_TX_BUF, NET_HDR_SIZE).zero();
        self.tx.borrow_mut().submit(
            head,
            self.dma_device_addr + OFF_TX_BUF as u64,
            (NET_HDR_SIZE + len) as u32,
            false,
            self.tx_doorbell,
        );
        true
    }

    /// Take back every transmit head the device has finished with.
    fn reclaim_tx(&self) {
        loop {
            let done = self.tx.borrow_mut().poll_used();
            let Some((head, _)) = done else { return };
            self.tx_free.borrow_mut().push(head);
        }
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    pub fn claim(&self) -> &PciDev {
        &self.dev
    }
}

/// Where a queue's doorbell is, from `queue_notify_off` and the multiplier the
/// notify capability published (§4.1.4.4).
fn doorbell(notify: Window, multiplier: u32, notify_off: u16, queue: u16) -> Doorbell {
    let at = notify_off as usize * multiplier as usize;
    Doorbell { window: notify.sub(at, 2), queue }
}

/// Program one queue's size and ring addresses, and read back where its
/// doorbell sits. Does not enable it: a queue must have its vector first.
fn setup_queue(
    common: Window,
    index: u16,
    rings: &mut Rings,
    dma_device_addr: u64,
    desc_off: usize,
    avail_off: usize,
    used_off: usize,
) -> Result<(), Refusal> {
    common.write::<u16>(COMMON_QUEUE_SELECT, index);
    let max: u16 = common.read(COMMON_QUEUE_SIZE);
    if max < rings.size {
        // The device's own bound against rings sized at compile time: a device
        // offering fewer is one this layout does not fit, and shrinking to it
        // would silently change how many buffers are posted.
        return Err(Refusal::MissingCap("a queue as deep as this driver's rings"));
    }
    common.write::<u16>(COMMON_QUEUE_SIZE, rings.size);
    common.write::<u64>(COMMON_QUEUE_DESC, dma_device_addr + desc_off as u64);
    common.write::<u64>(COMMON_QUEUE_DRIVER, dma_device_addr + avail_off as u64);
    common.write::<u64>(COMMON_QUEUE_DEVICE, dma_device_addr + used_off as u64);
    rings.notify_off = common.read(COMMON_QUEUE_NOTIFY_OFF);
    Ok(())
}

/// One virtio PCI capability, as read out of config space.
#[derive(Clone, Copy)]
struct Cap {
    cfg_type: u8,
    bar: u8,
    offset: u32,
    length: u32,
    notify_multiplier: u32,
}

impl Cap {
    /// The sub-window this capability names inside its BAR's mapping.
    ///
    /// Offset and length are the *device's*, so both are checked against the
    /// window before a sub-window is taken: a capability naming bytes past the
    /// BAR refuses the device rather than being clamped to one that fits.
    fn window(&self, bar: Window) -> Result<Window, Refusal> {
        let length = (self.length as usize).max(4);
        match (self.offset as usize).checked_add(length) {
            Some(end) if end <= bar.len => Ok(bar.sub(self.offset as usize, length)),
            _ => Err(Refusal::MissingCap("a capability inside its own BAR")),
        }
    }
}

/// The vendor capabilities a function published, walked once.
struct Capabilities(Vec<Cap>);

impl Capabilities {
    fn walk(dev: &PciDev) -> Self {
        let mut found = Vec::new();
        let mut seen = 0usize;
        let Ok(first) = dev.config_read(CAPABILITIES_PTR, RegWidth::U8) else {
            return Self(found);
        };
        let mut next = first;
        // The pointer is the device's: a chain that does not terminate, or one
        // pointing outside the header, ends the walk rather than running off
        // the window or for ever.
        while next >= 0x40 && next < 0x100 && seen < MAX_CAPABILITIES {
            seen += 1;
            let Ok(id) = dev.config_read(next, RegWidth::U8) else { break };
            if id as u8 == CAP_ID_VENDOR {
                let read = |at: u32, width| dev.config_read(next + at, width).unwrap_or(0);
                found.push(Cap {
                    cfg_type: read(3, RegWidth::U8) as u8,
                    bar: read(4, RegWidth::U8) as u8,
                    offset: read(8, RegWidth::U32),
                    length: read(12, RegWidth::U32),
                    notify_multiplier: read(16, RegWidth::U32),
                });
            }
            let Ok(link) = dev.config_read(next + 1, RegWidth::U8) else { break };
            next = link;
        }
        Self(found)
    }

    /// The first capability of `cfg_type` — the one §4.1.4.1 says to use where
    /// a device publishes several.
    fn find(&self, cfg_type: u8) -> Option<Cap> {
        self.0.iter().find(|cap| cap.cfg_type == cfg_type).copied()
    }
}

/// What a used-ring element must satisfy, driven with elements no device would
/// send.
///
/// **The device is on the far side of a trust boundary and moving the driver
/// out of the kernel did not change that.** Each of these was a way to make the
/// old kernel-side driver act on a number the device chose; the arms are the
/// same and the code they guard is this file's now.
#[cfg(test)]
mod tests {
    use super::*;

    /// Rings over a plain allocation. `parse_used` reads none of them — only
    /// `chain_bytes` and `size` — so what they point at does not matter.
    fn rings(size: u16) -> Rings {
        let backing = vec![0u8; 0x1000].leak();
        // SAFETY: `leak` gives the allocation the `'static` lifetime the window
        // needs, and nothing here reads or writes through it.
        let window = unsafe { Window::new(backing.as_mut_ptr(), backing.len()) };
        Rings::new(window.sub(0, 0x400), window.sub(0x400, 0x400), window.sub(0x800, 0x400), size)
    }

    #[test]
    fn a_head_past_the_table_is_refused() {
        let queue = rings(16);
        assert_eq!(queue.parse_used(16, 4), Err(UsedRefusal::Head(16)));
        assert_eq!(queue.parse_used(u32::MAX, 4), Err(UsedRefusal::Head(u32::MAX)));
    }

    /// A completion for a head with nothing published at it — a second report
    /// of one buffer, which is how a device would hand the same frame up twice.
    #[test]
    fn a_head_with_no_chain_is_refused() {
        let queue = rings(16);
        assert_eq!(queue.parse_used(3, 4), Err(UsedRefusal::NoChain(3)));
    }

    /// The one that matters most: `written` becomes the length of a slice this
    /// process hands to smoltcp, so a device claiming more than it was given
    /// would be a read past the buffer.
    #[test]
    fn more_bytes_than_the_chain_was_given_is_refused() {
        let mut queue = rings(16);
        queue.chain_bytes[3] = 100;
        assert_eq!(queue.parse_used(3, 100), Ok((3, 100)));
        assert_eq!(queue.parse_used(3, 99), Ok((3, 99)));
        assert_eq!(
            queue.parse_used(3, 101),
            Err(UsedRefusal::Written { id: 3, len: 101, chain: 100 })
        );
        assert_eq!(
            queue.parse_used(3, u32::MAX),
            Err(UsedRefusal::Written { id: 3, len: u32::MAX, chain: 100 })
        );
    }

    /// The layout the const assertions above check, restated as arithmetic a
    /// reader can follow: nothing overlaps, and everything is inside the grant.
    #[test]
    fn the_grant_holds_every_ring_and_buffer_without_overlap() {
        assert!(OFF_RX_DESC + RX_QUEUE_SIZE as usize * DESC_BYTES <= OFF_RX_AVAIL);
        assert!(OFF_RX_AVAIL + AVAIL_RING_OFF + RX_QUEUE_SIZE as usize * 2 <= OFF_RX_USED);
        assert!(
            OFF_RX_USED + USED_RING_OFF + RX_QUEUE_SIZE as usize * USED_ELEM_BYTES
                <= OFF_TX_RINGS
        );
        assert!(
            OFF_TX_RINGS
                + tx_used_off()
                + USED_RING_OFF
                + TX_QUEUE_SIZE as usize * USED_ELEM_BYTES
                <= OFF_RX_BUFS
        );
        assert_eq!(OFF_TX_BUF, OFF_RX_BUFS + RX_BUF_COUNT * RX_BUF_SIZE);
        assert!(GRANT_BYTES as usize >= OFF_TX_BUF + TX_BUF_SIZE);
    }
}
