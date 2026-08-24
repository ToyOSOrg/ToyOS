//! The virtio-net driver: bring-up, the two virtqueues, and the one page a
//! claimant maps.
//!
//! **Two DMA pools, and the line between them is who writes an address.** A
//! split virtqueue puts every address this driver ever programs in one place —
//! the descriptor table — so the tables, the available rings and the used rings
//! live in [`KERNEL_DMA_BYTES`] of memory no process maps, and what a
//! `DeviceType::Nic` claim grants is the frame buffers and nothing else.
//!
//! Until 2026-08-23 it was one pool and the whole 2 MiB page was the grant, so
//! `netd` mapped 7,088 bytes of live virtqueue writable: 256 RX descriptors
//! carrying the physical address the NIC would DMA the next frame into, and a
//! TX descriptor the device reads. Rewriting one aimed the device at any
//! physical address in the machine — kernel text, a page table, another process
//! — in either direction, and the IOMMU refuses nothing
//! (`kernel/src/iommu/mod.rs`). `virtio_sound` had already split its pools for
//! this reason and `virtio_gpu` publishes only its framebuffer; virtio-net was
//! the one device that handed its virtqueue out, and it predates both.
//!
//! [`assert_queues_are_private`] is that rule stated against the addresses the
//! device was really programmed with, so re-merging the pools cannot be done
//! quietly. The kernel page costs one 2 MiB frame it does not fill —
//! `DmaPool` allocates whole pages and a claim maps a whole page, so the two
//! cannot share one.

use alloc::boxed::Box;

use super::pci::{PciDevice, MSIX_ENTRY};
use super::virtio::{BufDir, DescSlot, Virtqueue, VirtqueueRegions, VirtioDevice, VIRTIO_F_VERSION_1};
use super::DmaPool;
use crate::mm::paging::CachePolicy;
use crate::log;
use crate::mm::Dma;
use crate::net::NicInfo;
use crate::object::shm::Region;
use toyos_abi::syscall::SyscallError;

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_NET_DEVICE: u16 = 0x1041; // 0x1040 + device_id 1

const VIRTIO_NET_F_MAC: u64 = 1 << 5;

// VirtIO 1.0 net header: always 12 bytes (includes num_buffers) with VERSION_1
const NET_HDR_SIZE: usize = 12;

const RX_QUEUE: u16 = 0;
const TX_QUEUE: u16 = 1;
const RX_QUEUE_SIZE: u16 = 256;
const TX_QUEUE_SIZE: u16 = 16;
const RX_BUF_COUNT: usize = 256;
const RX_BUF_SIZE: u32 = 4096;

// The kernel-only pool (byte offsets, 4 KiB-aligned): both virtqueues in full.
// No process maps a byte of this.
const OFF_RXQ_DESC: usize  = 0x0000;
const OFF_RXQ_AVAIL: usize = 0x1000;
const OFF_RXQ_USED: usize  = 0x2000;
const OFF_TXQ: usize       = 0x3000;
const KERNEL_DMA_BYTES: usize = 0x4000;

// The shared pool: the frame buffers, and nothing else. `NicInfo`'s offsets are
// relative to this pool's first byte, which is the first byte of the page a
// claim maps.
const OFF_RX_BUFS: usize = 0x0000;
const OFF_TX_BUF: usize  = OFF_RX_BUFS + RX_BUF_COUNT * RX_BUF_SIZE as usize;
const TX_BUF_LEN: usize  = 0x1000;
const SHARED_DMA_BYTES: usize = OFF_TX_BUF + TX_BUF_LEN;

const _: () = {
    use super::virtio::{avail_bytes, contiguous_bytes, used_bytes, DESC_BYTES};
    // The RX queue's three rings, a 4 KiB page each (`from_separate`).
    assert!(RX_QUEUE_SIZE as usize * DESC_BYTES <= OFF_RXQ_AVAIL - OFF_RXQ_DESC);
    assert!(avail_bytes(RX_QUEUE_SIZE) <= OFF_RXQ_USED - OFF_RXQ_AVAIL);
    assert!(used_bytes(RX_QUEUE_SIZE) <= OFF_TXQ - OFF_RXQ_USED);
    // The TX queue, all three rings in one page (`Virtqueue::new`).
    assert!(contiguous_bytes(TX_QUEUE_SIZE) <= KERNEL_DMA_BYTES - OFF_TXQ);
    // A claim maps one 2 MiB page, so everything it names has to be in one.
    assert!(SHARED_DMA_BYTES <= crate::mm::PAGE_2M as usize);
};

const VIRTIO_NET_VECTOR: u8 = 0x22;

/// **The RX buffers are [`Dma`] views and not raw pointers, and that is what
/// deleted this type's `unsafe impl Send`.** A view knows its own physical
/// address and its own length, so `rx_phys` went with the pointers.
///
/// `'static` because both pools are leaked at `init`: a NIC this kernel has
/// bound is bound for the boot, its RX buffers are mapped into `netd`, and the
/// `static Lock<Option<DmaPool>>` that used to hold the pages alive said all
/// that only by existing.
///
/// The two queues here are views of the kernel-only pool and the buffers are
/// views of the shared one — see the module header for why that is not an
/// arrangement anyone may tidy away.
struct VirtioNic {
    device: VirtioDevice,
    rxq: Virtqueue<'static>,
    txq: Virtqueue<'static>,
    tx_phys: u64,
    rx_bufs: [Dma<'static>; RX_BUF_COUNT],
    // Maps virtqueue descriptor index -> rx_bufs index
    desc_to_buf: [u16; RX_QUEUE_SIZE as usize],
    /// The RX queue's refusal count as of the last line this driver wrote
    /// about it.
    ///
    /// `poll_used` cannot log — `virtio_console` calls it under the serial
    /// backend's own lock — so the count is carried and named here, which is
    /// the one virtio queue whose used ring a userland process could reach.
    reported_refusals: u32,
    /// Stash area: slot returned by poll_used, indexed by buf_idx, consumed by refill_rx_buf.
    pending_rx_slots: [Option<DescSlot>; RX_BUF_COUNT],
    tx_slot: Option<DescSlot>,
}

impl VirtioNic {
    fn refill_rx(&mut self, buf_idx: usize, slot: DescSlot) {
        // Bounded by construction: the subview is exactly `NET_HDR_SIZE` bytes
        // at the front of a `RX_BUF_SIZE` buffer. This runs before the
        // descriptor is handed back to the device, so nothing is writing the
        // buffer, and `buf_idx` came from `desc_to_buf`, whose every entry
        // `poll_used` bounded to this queue.
        self.rx_bufs[buf_idx].subview(0, NET_HDR_SIZE).zero();
        let desc_id = self.rxq.submit(
            slot,
            &[(self.rx_bufs[buf_idx].phys(), RX_BUF_SIZE, BufDir::Writable)],
            self.device.notify_mmio(),
            self.device.notify_off_multiplier(),
            RX_QUEUE,
        );
        self.desc_to_buf[desc_id as usize] = buf_idx as u16;
    }
}

impl crate::net::Nic for VirtioNic {
    fn has_packet(&self) -> bool {
        self.rxq.has_used()
    }

    fn poll_rx(&mut self) -> Option<(usize, usize)> {
        let polled = self.rxq.poll_used();
        // Named here and not in `poll_used`: this runs on the `irq_ring` drain,
        // where a log line is ordinary, and it is the used ring an untrusted
        // process is closest to. Reported on change rather than per element, so
        // a device or a peer writing garbage costs one line and not a flood.
        let refused = self.rxq.refused();
        if refused != self.reported_refusals {
            log!(
                "VirtIO net: refused {} RX used-ring element(s) — the device named a descriptor \
                 this queue never published or claimed more bytes than it was given",
                refused - self.reported_refusals
            );
            self.reported_refusals = refused;
        }
        let (slot, written_len) = polled?;
        // In range by construction: `poll_used` refuses a head past the queue,
        // and `desc_to_buf` is exactly `RX_QUEUE_SIZE` long.
        let buf_idx = self.desc_to_buf[slot.id() as usize] as usize;
        let total = written_len as usize;
        if total <= NET_HDR_SIZE {
            self.refill_rx(buf_idx, slot);
            return None;
        }
        // Stash the slot for refill_rx_buf to consume later
        self.pending_rx_slots[buf_idx] = Some(slot);
        Some((buf_idx, total - NET_HDR_SIZE))
    }

    fn refill_rx_buf(&mut self, buf_index: usize) -> Result<(), SyscallError> {
        // RX_BUF_COUNT is not a chosen number: it is the length of
        // `pending_rx_slots`/`rx_bufs` and the buffer count baked
        // into the DMA pool layout, so this check is exactly the array bound.
        // An index past it used to be silently ignored *and* reported as
        // success; an unpolled index used to panic the kernel.
        if buf_index >= RX_BUF_COUNT { return Err(SyscallError::InvalidArgument); }
        let Some(slot) = self.pending_rx_slots[buf_index].take() else {
            return Err(SyscallError::InvalidArgument);
        };
        self.refill_rx(buf_index, slot);
        Ok(())
    }

    fn tx_buf_len(&self) -> usize { TX_BUF_LEN }

    fn submit_tx(&mut self, total_len: usize) {
        let slot = self.tx_slot.take().expect("virtio-net: no tx slot");
        self.tx_slot = Some(self.txq.submit_and_wait(
            slot,
            &[(self.tx_phys, total_len as u32, BufDir::Readable)],
            self.device.notify_mmio(),
            self.device.notify_off_multiplier(),
            TX_QUEUE,
        ));
    }
}

/// Arm this NIC's RX interrupt, or say why the machine has no NIC.
///
/// A refusal rather than a panic, and that is the whole of what this returns.
/// Nothing in this driver polls: `poll_rx` runs behind an `irq_ring` record
/// only vector 0x22's ISR publishes, so a NIC whose messages cannot reach a CPU
/// delivers nothing for the life of the boot. A machine that cannot have
/// networking still boots, still has a console, and still says why — which is
/// what the xHCI driver settled for the same shape of question.
fn arm_interrupt(pci_dev: &PciDevice, device: &VirtioDevice) -> bool {
    if !pci_dev.enable_msix(VIRTIO_NET_VECTOR) {
        log!("VirtIO net: NOT INITIALISED at PCI {:02x}:{:02x}.{} — its MSI-X could not be \
             armed and this driver has no other way to be told a packet arrived",
            pci_dev.bus, pci_dev.dev, pci_dev.func);
        return false;
    }
    if let Err(refused) = device.bind_msix(RX_QUEUE) {
        log!("VirtIO net: NOT INITIALISED at PCI {:02x}:{:02x}.{} — the device refused a \
             vector for {}", pci_dev.bus, pci_dev.dev, pci_dev.func, refused);
        return false;
    }
    log!("VirtIO net: MSI-X vector {:#x} on table entry {}", VIRTIO_NET_VECTOR, MSIX_ENTRY);
    true
}

/// **The rule the two pools exist to keep**, stated against the six addresses
/// this device was really programmed with rather than against the layout
/// constants above: no ring of either virtqueue is inside the page a
/// `DeviceType::Nic` claim maps.
///
/// A panic, and the only one on this path. Every other refusal here is a
/// machine that boots without networking; this one is a kernel that has just
/// handed a userland process the ability to name any physical address in the
/// machine, and it would be silent for the life of the boot. Six comparisons,
/// once per bind.
fn assert_queues_are_private(rxq: &Virtqueue<'_>, txq: &Virtqueue<'_>, shared_phys: u64) {
    let page = shared_phys..shared_phys + crate::mm::PAGE_2M;
    for (what, phys) in [
        ("the RX descriptor table", rxq.descs_phys()),
        ("the RX available ring", rxq.avail_phys()),
        ("the RX used ring", rxq.used_phys()),
        ("the TX descriptor table", txq.descs_phys()),
        ("the TX available ring", txq.avail_phys()),
        ("the TX used ring", txq.used_phys()),
    ] {
        assert!(
            !page.contains(&phys),
            "VirtIO net: {what} is at {phys:#x}, inside the {:#x}-byte page at {shared_phys:#x} \
             that a NIC claim maps writable — its holder could aim this device at any physical \
             address in the machine",
            crate::mm::PAGE_2M,
        );
    }
}

pub fn init(devices: &[PciDevice]) {
    let pci_dev = match devices.iter().find(|d| d.is_id(VIRTIO_VENDOR, VIRTIO_NET_DEVICE)) {
        Some(dev) => *dev,
        None => {
            log!("VirtIO net: no device found");
            return;
        }
    };
    log!("VirtIO net: found at PCI {:02x}:{:02x}.{}", pci_dev.bus, pci_dev.dev, pci_dev.func);
    // Leaked rather than held in a `static`: this NIC is never unbound and its
    // buffer window is handed to whoever claims the class, so the pages outlive
    // every scope by design.
    let kernel_mem = DmaPool::alloc(KERNEL_DMA_BYTES).leak();
    let shared = DmaPool::alloc(SHARED_DMA_BYTES).leak();
    // Exclusive: the pools were allocated on the two lines above and nothing
    // else holds a view of either. The shared one is zeroed because a claimant
    // maps the whole 2 MiB page — every byte past the buffers included — and
    // those are pages this machine has already used for something else.
    shared.zero();
    pci_dev.enable_bus_master();

    let device = VirtioDevice::init(&pci_dev, VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC);

    // RX queue: 256 entries, separate pages for desc/avail/used
    let rxq_regions = VirtqueueRegions::from_separate(
        kernel_mem.subview(OFF_RXQ_DESC, OFF_RXQ_AVAIL - OFF_RXQ_DESC),
        kernel_mem.subview(OFF_RXQ_AVAIL, OFF_RXQ_USED - OFF_RXQ_AVAIL),
        kernel_mem.subview(OFF_RXQ_USED, OFF_TXQ - OFF_RXQ_USED),
        RX_QUEUE_SIZE,
    );
    let mut rxq = Virtqueue::from_regions(&rxq_regions, RX_QUEUE_SIZE);

    // TX queue: 16 entries, all three rings inside one page
    let mut txq = Virtqueue::new(
        kernel_mem.subview(OFF_TXQ, KERNEL_DMA_BYTES - OFF_TXQ),
        TX_QUEUE_SIZE,
    );

    device.setup_queue(RX_QUEUE, &mut rxq);
    device.setup_queue(TX_QUEUE, &mut txq);
    if !arm_interrupt(&pci_dev, &device) {
        return;
    }
    device.enable_queue(RX_QUEUE);
    device.enable_queue(TX_QUEUE);
    device.activate();

    let cfg = device.device_config();
    let mac = [
        cfg.read_u8(0), cfg.read_u8(1), cfg.read_u8(2),
        cfg.read_u8(3), cfg.read_u8(4), cfg.read_u8(5),
    ];
    log!("VirtIO net: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

    let rx_bufs: [Dma<'static>; RX_BUF_COUNT] = core::array::from_fn(|i| {
        shared.subview(OFF_RX_BUFS + i * RX_BUF_SIZE as usize, RX_BUF_SIZE as usize)
    });
    let tx_phys = shared.phys() + OFF_TX_BUF as u64;

    // `DmaPool` allocations are whole 2 MiB pages, so the pool's first byte is
    // the first byte of the page a claim maps and `NicInfo`'s offsets are
    // relative to it.
    let dma_region = Region {
        phys: crate::DirectMap::from_phys(shared.phys()),
        size: crate::mm::PAGE_2M,
        cache: CachePolicy::DeferToMtrr,
        pages: None,
    };
    assert_queues_are_private(&rxq, &txq, shared.phys());

    crate::net::set_nic_info(NicInfo {
        dma: toyos_abi::HANDLE_INVALID,
        rx_buf_offset: OFF_RX_BUFS as u32,
        tx_buf_offset: OFF_TX_BUF as u32,
        mac,
        rx_buf_count: RX_BUF_COUNT as u16,
        rx_buf_size: RX_BUF_SIZE as u16,
        net_hdr_size: NET_HDR_SIZE as u16,
    }, dma_region);

    let mut rx_slots = rxq.initial_slots();
    let mut tx_slots = txq.initial_slots();
    let tx_slot = tx_slots.pop().expect("virtio-net: no tx slots");
    drop(tx_slots);

    const NONE_SLOT: Option<DescSlot> = None;
    let mut nic = VirtioNic {
        device, rxq, txq, tx_phys, rx_bufs,
        desc_to_buf: [0; RX_QUEUE_SIZE as usize],
        reported_refusals: 0,
        pending_rx_slots: [NONE_SLOT; RX_BUF_COUNT],
        tx_slot: Some(tx_slot),
    };

    for i in 0..RX_BUF_COUNT {
        let slot = rx_slots.pop().expect("virtio-net: not enough rx slots");
        nic.refill_rx(i, slot);
    }

    crate::net::register(Box::new(nic));
    log!("VirtIO net: {} RX buffers, queue size {}", RX_BUF_COUNT, RX_QUEUE_SIZE);
}
