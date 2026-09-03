//! The virtio-net driver: bring-up and the two virtqueues.
//!
//! Two DMA pools, split by who writes an address: the kernel-only pool holds
//! both virtqueues in full — every address this driver programs — and the
//! shared pool, the one a `DeviceType::Nic` claim maps, holds only the frame
//! buffers. [`assert_queues_are_private`] checks that split against where the
//! rings physically are. `DmaPool` allocates whole pages and a claim maps a
//! whole page, so the two pools cannot share one.
//!
//! Both pools sit in an address space of this device's own, so the addresses in
//! its descriptors name these two pools or nothing at all.

use alloc::boxed::Box;

use super::pci::{PciDevice, MSIX_ENTRY};
use super::virtio::{BufDir, DescSlot, Virtqueue, VirtqueueRegions, VirtioDevice, VIRTIO_F_VERSION_1};
use super::DmaPool;
use crate::iommu::DeviceSpace;
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

// The kernel-only pool: no process maps a byte of this.
const OFF_RXQ_DESC: usize  = 0x0000;
const OFF_RXQ_AVAIL: usize = 0x1000;
const OFF_RXQ_USED: usize  = 0x2000;
const OFF_TXQ: usize       = 0x3000;
const KERNEL_DMA_BYTES: usize = 0x4000;

// The shared pool: `NicInfo`'s offsets are relative to its first byte, the claim page's first byte.
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

/// `'static` because both DMA pools are leaked at `init`.
struct VirtioNic {
    device: VirtioDevice,
    rxq: Virtqueue<'static>,
    txq: Virtqueue<'static>,
    tx_addr: u64,
    rx_bufs: [Dma<'static>; RX_BUF_COUNT],
    // Descriptor index -> rx_bufs index.
    desc_to_buf: [u16; RX_QUEUE_SIZE as usize],
    // `poll_used` cannot log because `virtio_console` calls it under the serial backend's own lock.
    reported_refusals: u32,
    // Slot from poll_used, indexed by buf_idx, consumed by refill_rx_buf.
    pending_rx_slots: [Option<DescSlot>; RX_BUF_COUNT],
    tx_slot: Option<DescSlot>,
}

impl VirtioNic {
    /// Where the device puts the next frame. The actuator points the first
    /// buffer at another driver's pool by its *physical* address, which this
    /// device's own domain does not map, so a unit really translating for it
    /// blocks the write instead of letting it land.
    fn rx_target(&self, buf_idx: usize) -> u64 {
        #[cfg(feature = "boot-actuators")]
        if buf_idx == 0 && crate::actuator::iommu_nic_foreign_dma() {
            let foreign =
                super::nvme::FOREIGN_PROBE.load(core::sync::atomic::Ordering::Relaxed);
            if foreign != 0 {
                return foreign;
            }
        }
        self.rx_bufs[buf_idx].device_addr()
    }

    fn refill_rx(&mut self, buf_idx: usize, slot: DescSlot) {
        // `buf_idx` is bounded by `desc_to_buf`.
        // Nothing else writes this buffer yet.
        self.rx_bufs[buf_idx].subview(0, NET_HDR_SIZE).zero();
        let desc_id = self.rxq.submit(
            slot,
            &[(self.rx_target(buf_idx), RX_BUF_SIZE, BufDir::Writable)],
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
        // The refusal count is tracked here, not in `poll_used`, because this runs on the
        // `irq_ring` drain where a log line is safe.
        // On change, not per element — a flood of refusals costs one line, not many.
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
        // In range by construction: `poll_used` bounds the head to `RX_QUEUE_SIZE`.
        let buf_idx = self.desc_to_buf[slot.id() as usize] as usize;
        let total = written_len as usize;
        if total <= NET_HDR_SIZE {
            self.refill_rx(buf_idx, slot);
            return None;
        }
        self.pending_rx_slots[buf_idx] = Some(slot);
        Some((buf_idx, total - NET_HDR_SIZE))
    }

    fn refill_rx_buf(&mut self, buf_index: usize) -> Result<(), SyscallError> {
        // RX_BUF_COUNT is the array bound for pending_rx_slots and rx_bufs.
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
            &[(self.tx_addr, total_len as u32, BufDir::Readable)],
            self.device.notify_mmio(),
            self.device.notify_off_multiplier(),
            TX_QUEUE,
        ));
    }
}

/// Nothing in this driver polls: `poll_rx` only runs off the `irq_ring` record
/// vector 0x22's ISR publishes, so an unarmed NIC delivers nothing for the
/// life of the boot.
///
/// Returns false rather than panicking so a machine without networking still
/// boots.
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

/// No ring of either virtqueue may be inside the page a `DeviceType::Nic`
/// claim maps.
///
/// Panics rather than allowing it, since the alternative is a userland
/// process able to name every address this device's domain holds.
fn assert_queues_are_private(rxq: &Virtqueue<'_>, txq: &Virtqueue<'_>, shared_phys: u64) {
    let page = shared_phys..shared_phys + crate::mm::PAGE_2M;
    let [rx_desc, rx_avail, rx_used] = rxq.rings();
    let [tx_desc, tx_avail, tx_used] = txq.rings();
    for (what, phys) in [
        ("the RX descriptor table", rx_desc.host_phys()),
        ("the RX available ring", rx_avail.host_phys()),
        ("the RX used ring", rx_used.host_phys()),
        ("the TX descriptor table", tx_desc.host_phys()),
        ("the TX available ring", tx_avail.host_phys()),
        ("the TX used ring", tx_used.host_phys()),
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

/// One ARP request for the emulated gateway, so something answers and the
/// device has a frame to write into the buffer the actuator moved: nothing else
/// in this kernel puts a packet on the wire, and the only IP stack is a userland
/// daemon that speaks when spoken to. RFC 826 over Ethernet II, and the two
/// addresses are QEMU's user-mode network's own.
#[cfg(feature = "boot-actuators")]
fn provoke_a_reply(nic: &mut VirtioNic, shared: Dma<'static>, mac: [u8; 6]) {
    use crate::net::Nic;
    const FRAME: usize = 14 + 28;
    let mut frame = [0u8; FRAME];
    frame[0..6].copy_from_slice(&[0xFF; 6]);
    frame[6..12].copy_from_slice(&mac);
    frame[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    frame[14..16].copy_from_slice(&1u16.to_be_bytes());
    frame[16..18].copy_from_slice(&0x0800u16.to_be_bytes());
    frame[18] = 6;
    frame[19] = 4;
    frame[20..22].copy_from_slice(&1u16.to_be_bytes());
    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&[10, 0, 2, 15]);
    frame[38..42].copy_from_slice(&[10, 0, 2, 2]);
    let tx = shared.subview(OFF_TX_BUF, NET_HDR_SIZE + FRAME);
    tx.zero();
    tx.copy_from(NET_HDR_SIZE, &frame);
    nic.submit_tx(NET_HDR_SIZE + FRAME);
    log!("VirtIO net: an ARP request for 10.0.2.2 is on the wire (actuator)");
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
    // An address space holding these two pools and nothing else, so a
    // descriptor this device is handed reaches its own buffers or faults.
    let space = DeviceSpace::create();
    // Leaked, not `static`: this NIC is never unbound, so the pages must outlive every scope.
    let kernel_mem = DmaPool::alloc_in(KERNEL_DMA_BYTES, space).leak();
    let shared = DmaPool::alloc_in(SHARED_DMA_BYTES, space).leak();
    // Exclusive: allocated on the two lines above, nothing else holds a view.
    // Zeroed because these pages held other data before this allocation.
    shared.zero();
    // Before the device is told an address, and after every mapping it gets:
    // the identity domain is what it leaves behind.
    space.attach(pci_dev.bus, pci_dev.dev, pci_dev.func);

    let device = match VirtioDevice::init(&pci_dev, VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC) {
        Ok(device) => device,
        Err(why) => {
            log!("VirtIO net: PCI {:02x}:{:02x}.{} {why} — device refused",
                pci_dev.bus, pci_dev.dev, pci_dev.func);
            return;
        }
    };

    let rxq_regions = VirtqueueRegions::from_separate(
        kernel_mem.subview(OFF_RXQ_DESC, OFF_RXQ_AVAIL - OFF_RXQ_DESC),
        kernel_mem.subview(OFF_RXQ_AVAIL, OFF_RXQ_USED - OFF_RXQ_AVAIL),
        kernel_mem.subview(OFF_RXQ_USED, OFF_TXQ - OFF_RXQ_USED),
        RX_QUEUE_SIZE,
    );
    let mut rxq = Virtqueue::from_regions(&rxq_regions, RX_QUEUE_SIZE);

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
    let tx_addr = shared.device_addr() + OFF_TX_BUF as u64;

    // A claim maps physical pages into a process, so this is the one place the
    // shared pool is named by where it is rather than by what the NIC calls it.
    let dma_region = Region {
        phys: crate::DirectMap::from_phys(shared.host_phys()),
        size: crate::mm::PAGE_2M,
        cache: CachePolicy::DeferToMtrr,
        pages: None,
    };
    assert_queues_are_private(&rxq, &txq, shared.host_phys());

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
        device, rxq, txq, tx_addr, rx_bufs,
        desc_to_buf: [0; RX_QUEUE_SIZE as usize],
        reported_refusals: 0,
        pending_rx_slots: [NONE_SLOT; RX_BUF_COUNT],
        tx_slot: Some(tx_slot),
    };

    for i in 0..RX_BUF_COUNT {
        let slot = rx_slots.pop().expect("virtio-net: not enough rx slots");
        nic.refill_rx(i, slot);
    }

    #[cfg(feature = "boot-actuators")]
    if crate::actuator::iommu_nic_foreign_dma() {
        provoke_a_reply(&mut nic, shared, mac);
    }

    crate::net::register(Box::new(nic));
    log!("VirtIO net: {} RX buffers, queue size {}", RX_BUF_COUNT, RX_QUEUE_SIZE);
}
