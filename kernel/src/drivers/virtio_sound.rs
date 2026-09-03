//! virtio-sound: bring-up, the virtqueues, and the register allow-list.
//!
//! Every DMA address lives in the descriptor tables, built once at bind from
//! kernel-allocated offsets; after bind the driver's whole vocabulary is an
//! avail-ring index and a doorbell write. Stream selection, format and timing
//! are soundd's, not this driver's.
//!
//! Structure layouts and command codes follow VirtIO 1.2 §5.14.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use toyos_abi::audio::AudioCompletionRecord;
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_abi::virtio_sound as abi;

use super::pci::{PciDevice, MSIX_ENTRY};
use super::virtio::{BufDir, UsedRingConsumer, VirtioDevice, Virtqueue, VirtqueueRegions,
                    VIRTIO_F_VERSION_1};
use super::DmaPool;
use crate::log;
use crate::mm::paging::CachePolicy;
use crate::mm::{Dma, Mmio};
use crate::object::shm::Region;
use crate::sync::Lock;

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_SND_DEVICE: u16 = 0x1059; // 0x1040 + device_id 25

/// Size of the TX transfer header, which QEMU subtracts from the chain's
/// readable length to get the PCM byte count; the kernel never reads its contents.
const XFER_HEADER_BYTES: u32 = 4;
/// The per-period status the device writes back: status and latency, two `le32`.
const STATUS_BYTES: u32 = 8;
/// One event: a code and its data.
const EVENT_BYTES: u32 = 8;

/// Kernel-only DMA page: three descriptor tables plus the TX used ring the
/// handler alone consumes.
const OFF_CTRL_DESC: usize = 0x0000;
const OFF_EVENT_DESC: usize = 0x0400;
const OFF_TX_DESC: usize = 0x0800;
const OFF_TX_USED: usize = 0x0C00;
const KERNEL_DMA_BYTES: usize = 0x1000;

const _: () = {
    use super::virtio::DESC_BYTES;
    assert!(abi::CONTROL_QUEUE_SIZE as usize * DESC_BYTES <= OFF_EVENT_DESC - OFF_CTRL_DESC);
    assert!(abi::EVENT_QUEUE_SIZE as usize * DESC_BYTES <= OFF_TX_DESC - OFF_EVENT_DESC);
    assert!(abi::TX_QUEUE_SIZE as usize * DESC_BYTES <= OFF_TX_USED - OFF_TX_DESC);
    assert!(abi::used_bytes(abi::TX_QUEUE_SIZE) <= KERNEL_DMA_BYTES - OFF_TX_USED);
};

/// Cap on logged refusals, past which a misbehaving driver can't spend unbounded log.
const MAX_NAMED_REFUSALS: usize = 16;


/// Written once before the vector arms and read without a lock afterwards —
/// not lock-guarded because the handler may interrupt a CPU holding [`CONTROLLER`].
struct TxIsr {
    consumer: UnsafeCell<Option<UsedRingConsumer<'static>>>,
    /// Stray used-ring entries (head names no chain); a userland bug, so counted
    /// rather than logged from the ISR.
    stray: AtomicU32,
    named_stray: AtomicBool,
}

// SAFETY: `consumer` is write-once before the vector arms and read only by the
// handler after; every other field is atomic.
unsafe impl Sync for TxIsr {}

static TX_ISR: TxIsr = TxIsr {
    consumer: UnsafeCell::new(None),
    stray: AtomicU32::new(0),
    named_stray: AtomicBool::new(false),
};

/// Drain the TX used ring; a head naming no chain is untrusted input, counted not asserted.
fn drain_tx() -> u32 {
    // SAFETY: sole accessor after init — see `TxIsr`.
    let consumer = unsafe { &mut *TX_ISR.consumer.get() };
    // A configuration-change interrupt shares this vector and may arrive before init installs the consumer.
    let Some(consumer) = consumer.as_mut() else { return 0 };
    let mut mask = 0u32;
    let refused_before = consumer.refused();
    while let Some(head) = consumer.poll() {
        let idx = head as usize / abi::TX_CHAIN as usize;
        if idx >= abi::PERIODS || head % abi::TX_CHAIN != 0 {
            TX_ISR.stray.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        mask |= 1 << idx;
    }
    // Folds in the consumer's own refusals; a head past the queue never reaches the loop above.
    let refused = consumer.refused() - refused_before;
    if refused != 0 {
        TX_ISR.stray.fetch_add(refused, Ordering::Relaxed);
    }
    mask
}

/// Rust half of the MSI-X handler, called from the IDT entry.
pub fn isr_complete() {
    // Timestamp first — this is the hardware-completion time the DLL feeds on.
    let timestamp = crate::clock::nanos_since_boot();
    let mask = drain_tx();
    if mask == 0 {
        return;
    }
    isr_push_completion(mask, timestamp);
    crate::irq_ring::isr_publish(crate::irq_ring::IrqSource::Audio, timestamp);
    // Force a scheduler entry on IRQ return so the record becomes wakes now, not at next tick.
    crate::preempt::set_need_resched();
}


const RECORD_RING_CAP: u32 = 16;

/// SPSC: producer is the MSI-X handler (single CPU, IF=0); consumer holds [`CONTROLLER`].
/// One record per interrupt, never accumulated — a folded mask would misreport lateness to soundd's DLL.
struct RecordRing {
    slots: [UnsafeCell<AudioCompletionRecord>; RECORD_RING_CAP as usize],
    head: AtomicU32,
    tail: AtomicU32,
}

const _: () = assert!(
    RECORD_RING_CAP as usize >= abi::PERIODS,
    "record ring must hold one record per period"
);

// SAFETY: slot access is arbitrated by the head/tail protocol above.
unsafe impl Sync for RecordRing {}

static RECORDS: RecordRing = RecordRing {
    slots: [const {
        UnsafeCell::new(AudioCompletionRecord { mask: 0, _pad: 0, timestamp_nanos: 0 })
    }; RECORD_RING_CAP as usize],
    head: AtomicU32::new(0),
    tail: AtomicU32::new(0),
};

/// Overflow sink for a full ring: mask is OR'd (idempotent) and timestamp is
/// newest-wins, so a driver that stops reading costs itself only timestamp granularity.
struct Spill {
    mask: AtomicU32,
    timestamp: AtomicU64,
}

static SPILL: Spill = Spill { mask: AtomicU32::new(0), timestamp: AtomicU64::new(0) };

fn isr_push_completion(mask: u32, timestamp_nanos: u64) {
    let ring = &RECORDS;
    let head = ring.head.load(Ordering::Relaxed); // sole writer of head
    let tail = ring.tail.load(Ordering::Acquire);
    if head.wrapping_sub(tail) >= RECORD_RING_CAP {
        SPILL.timestamp.store(timestamp_nanos, Ordering::Relaxed);
        SPILL.mask.fetch_or(mask, Ordering::Release);
        return;
    }
    let slot = (head % RECORD_RING_CAP) as usize;
    // SAFETY: slot is outside [tail, head) — not visible to the consumer.
    unsafe {
        *ring.slots[slot].get() = AudioCompletionRecord { mask, _pad: 0, timestamp_nanos };
    }
    // Release publishes the record before the consumer can observe the new head.
    ring.head.store(head.wrapping_add(1), Ordering::Release);
}

/// Pop the oldest pending record; called under [`CONTROLLER`], so the tail store
/// needs no CAS. Spill returns last because it is always newer than anything still queued.
fn pop_completion() -> Option<AudioCompletionRecord> {
    let ring = &RECORDS;
    let tail = ring.tail.load(Ordering::Relaxed); // sole writer of tail
    // Acquire pairs with the producer's Release store of head.
    let head = ring.head.load(Ordering::Acquire);
    if head == tail {
        let mask = SPILL.mask.swap(0, Ordering::AcqRel);
        if mask == 0 {
            return None;
        }
        return Some(AudioCompletionRecord {
            mask,
            _pad: 0,
            timestamp_nanos: SPILL.timestamp.load(Ordering::Relaxed),
        });
    }
    // SAFETY: `tail != head` puts this slot in `[tail, head)`, unwritten by the
    // producer until wrap; the Acquire load of `head` above pairs with its Release store.
    let rec = unsafe { *ring.slots[(tail % RECORD_RING_CAP) as usize].get() };
    ring.tail.store(tail.wrapping_add(1), Ordering::Release);
    Some(rec)
}

/// True if completion records are pending; lock-free.
pub fn has_pending() -> bool {
    RECORDS.head.load(Ordering::Acquire) != RECORDS.tail.load(Ordering::Acquire)
        || SPILL.mask.load(Ordering::Acquire) != 0
}

/// Copy up to `buf.len() / 16` pending records into `buf`, oldest first, and
/// name a stray completion the first time one has been counted.
pub fn drain_completed(buf: &mut crate::user_ptr::UserBytesMut) -> usize {
    let stray = TX_ISR.stray.load(Ordering::Relaxed);
    if stray != 0 && !TX_ISR.named_stray.swap(true, Ordering::Relaxed) {
        log!(
            "virtio-sound: the device completed a chain this driver never built ({stray} so far) \
             — a used-ring head past the queue, or one that heads no chain"
        );
    }
    let max = buf.len() / AudioCompletionRecord::SIZE;
    let _guard = CONTROLLER.lock();
    let mut written = 0;
    for _ in 0..max {
        let Some(rec) = pop_completion() else { break };
        // Field-wise serialization — never expose struct padding.
        let mut record = [0u8; AudioCompletionRecord::SIZE];
        record[0..4].copy_from_slice(&rec.mask.to_le_bytes());
        record[8..16].copy_from_slice(&rec.timestamp_nanos.to_le_bytes());
        buf.write_at(written, &record);
        written += AudioCompletionRecord::SIZE;
    }
    written
}

static INBOX_WATCHERS: Lock<alloc::vec::Vec<crate::inbox::InboxId>> =
    Lock::new(alloc::vec::Vec::new());

pub fn add_inbox_watcher(id: crate::inbox::InboxId) {
    let mut watchers = INBOX_WATCHERS.lock();
    if !watchers.contains(&id) {
        watchers.push(id);
    }
}

pub fn remove_inbox_watcher(id: crate::inbox::InboxId) {
    INBOX_WATCHERS.lock().retain(|&x| x != id);
}

pub fn inbox_watchers() -> alloc::vec::Vec<crate::inbox::InboxId> {
    INBOX_WATCHERS.lock().clone()
}


/// Notify region only; virtqueues and DMA pools are leaked at bring-up because
/// `TX_ISR` holds a used-ring consumer into one of them for the life of the boot.
struct Bound {
    notify: Mmio,
}

static CONTROLLER: Lock<Option<Bound>> = Lock::new(None);
static INFO: Lock<Option<(abi::VirtioSoundInfo, Region)>> = Lock::new(None);
static REFUSALS: AtomicU32 = AtomicU32::new(0);

pub fn info() -> Option<(abi::VirtioSoundInfo, Region)> {
    INFO.lock().clone()
}


/// Allow-list for the three doorbells; a doorbell value is a queue index, not an
/// address, so it can reach nothing the already-selected offset did not.
fn write_permit(info: &abi::VirtioSoundInfo, offset: u64, width: RegWidth) -> bool {
    let Ok(offset) = u32::try_from(offset) else { return false };
    width == RegWidth::U16
        && [info.notify_control, info.notify_event, info.notify_tx].contains(&offset)
}

fn refuse(what: &str, offset: u64, width: RegWidth) -> SyscallError {
    if REFUSALS.fetch_add(1, Ordering::Relaxed) < MAX_NAMED_REFUSALS as u32 {
        log!("virtio-sound: refused a {width:?} {what} of {offset:#x} — not on the allow-list");
    }
    SyscallError::PermissionDenied
}

/// No register is readable; every read is a refusal — answers arrive via memory, not MMIO.
pub fn reg_read(offset: u64, width: RegWidth) -> Result<u32, SyscallError> {
    Err(refuse("read", offset, width))
}

pub fn reg_write(offset: u64, width: RegWidth, value: u32) -> Result<(), SyscallError> {
    let (info, _) = info().ok_or(SyscallError::NotFound)?;
    if !write_permit(&info, offset, width) {
        return Err(refuse("write", offset, width));
    }
    if value > width.max_value() {
        return Err(SyscallError::InvalidArgument);
    }
    let guard = CONTROLLER.lock();
    let controller = guard.as_ref().ok_or(SyscallError::NotFound)?;
    controller.notify.write_u16(offset, value as u16);
    Ok(())
}


/// Bring up virtio-sound, or leave it unclaimed and log why — audio is optional,
/// so a refusal beats a panic over a peripheral.
pub fn init(devices: &[PciDevice]) {
    let Some(pci) = devices.iter().find(|d| d.is_id(VIRTIO_VENDOR, VIRTIO_SND_DEVICE)) else {
        return;
    };
    log!("virtio-sound: found at PCI {:02x}:{:02x}.{}", pci.bus, pci.dev, pci.func);

    let device = match VirtioDevice::init(pci, VIRTIO_F_VERSION_1) {
        Ok(device) => device,
        Err(why) => {
            log!("virtio-sound: NOT INITIALISED — PCI {:02x}:{:02x}.{} {why}",
                pci.bus, pci.dev, pci.func);
            return;
        }
    };

    let cfg = device.device_config();
    let (jacks, streams, chmaps) = (cfg.read_u32(0), cfg.read_u32(4), cfg.read_u32(8));
    log!("virtio-sound: {jacks} jacks, {streams} streams, {chmaps} chmaps");
    if streams == 0 {
        log!("virtio-sound: NOT INITIALISED — the device offers no PCM stream to play into");
        return;
    }

    // Placed after the stream check so a streamless device allocates nothing; see [`Bound`].
    let space = crate::iommu::DeviceSpace::create();
    let kernel_mem = DmaPool::alloc_in(KERNEL_DMA_BYTES, space).leak();
    let shared = DmaPool::alloc_in(abi::SHARED_BYTES, space).leak();
    // Before the device is told any address, and after both pools' mappings.
    space.attach(pci.bus, pci.dev, pci.func);
    // Exclusive: just allocated, not yet told to the device or mapped to userland.
    shared.zero();

    let mut controlq = queue(
        kernel_mem.subview(OFF_CTRL_DESC, OFF_EVENT_DESC - OFF_CTRL_DESC),
        shared.subview(abi::OFF_CTRL_AVAIL, abi::avail_bytes(abi::CONTROL_QUEUE_SIZE)),
        shared.subview(abi::OFF_CTRL_USED, abi::used_bytes(abi::CONTROL_QUEUE_SIZE)),
        abi::CONTROL_QUEUE_SIZE,
    );
    let mut eventq = queue(
        kernel_mem.subview(OFF_EVENT_DESC, OFF_TX_DESC - OFF_EVENT_DESC),
        shared.subview(abi::OFF_EVENT_AVAIL, abi::avail_bytes(abi::EVENT_QUEUE_SIZE)),
        shared.subview(abi::OFF_EVENT_USED, abi::used_bytes(abi::EVENT_QUEUE_SIZE)),
        abi::EVENT_QUEUE_SIZE,
    );
    // TX used ring lives in kernel memory only — userland must never fabricate a completion by rewriting it.
    let mut txq = queue(
        kernel_mem.subview(OFF_TX_DESC, OFF_TX_USED - OFF_TX_DESC),
        shared.subview(abi::OFF_TX_AVAIL, abi::avail_bytes(abi::TX_QUEUE_SIZE)),
        kernel_mem.subview(OFF_TX_USED, abi::used_bytes(abi::TX_QUEUE_SIZE)),
        abi::TX_QUEUE_SIZE,
    );

    build_chains(&mut controlq, &mut eventq, &mut txq, shared.device_addr());

    // Installed before the vector can fire, so no interrupt observes a half-written Option.
    // SAFETY: MSI-X is not enabled yet.
    unsafe { *TX_ISR.consumer.get() = Some(txq.split_used_consumer()) };

    device.setup_queue(abi::CONTROL_QUEUE, &mut controlq);
    device.setup_queue(abi::EVENT_QUEUE, &mut eventq);
    device.setup_queue(abi::TX_QUEUE, &mut txq);
    if !arm_interrupt(pci, &device) {
        return;
    }
    device.enable_queue(abi::CONTROL_QUEUE);
    device.enable_queue(abi::EVENT_QUEUE);
    device.enable_queue(abi::TX_QUEUE);
    device.activate();

    #[cfg(feature = "boot-actuators")]
    if crate::actuator::iommu_sound_foreign_dma() {
        answer_into_a_foreign_page(&mut controlq, &device, shared);
    }

    // DmaPool allocations are whole 2 MiB pages; ABI offsets are relative to that page.
    let dma_region = Region {
        phys: crate::DirectMap::from_phys(shared.host_phys()),
        size: crate::mm::PAGE_2M,
        cache: CachePolicy::DeferToMtrr,
        pages: None,
    };
    let multiplier = device.notify_off_multiplier();
    let info = abi::VirtioSoundInfo {
        dma: toyos_abi::HANDLE_INVALID,
        notify_control: controlq.notify_bytes(multiplier) as u32,
        notify_event: eventq.notify_bytes(multiplier) as u32,
        notify_tx: txq.notify_bytes(multiplier) as u32,
        jacks,
        streams,
        chmaps,
    };

    *CONTROLLER.lock() = Some(Bound { notify: device.notify_mmio() });
    *INFO.lock() = Some((info, dma_region));

    log!(
        "virtio-sound: bound, {} periods of {} bytes, doorbells at {:#x}/{:#x}/{:#x}",
        abi::PERIODS,
        abi::PERIOD_BYTES,
        info.notify_control,
        info.notify_event,
        info.notify_tx
    );
}

fn queue<'pool>(
    desc: Dma<'pool>,
    avail: Dma<'pool>,
    used: Dma<'pool>,
    size: u16,
) -> Virtqueue<'pool> {
    Virtqueue::from_regions(&VirtqueueRegions::from_separate(desc, avail, used, size), size)
}

/// Submit one control command whose answer buffer is in another driver's pool,
/// by its *physical* address, which this function's own domain does not map.
/// The request is the zeroed page's four bytes, a code the device answers with
/// one status word; the descriptors sit past the chain `build_chains` wrote.
#[cfg(feature = "boot-actuators")]
fn answer_into_a_foreign_page(
    controlq: &mut Virtqueue<'static>,
    device: &VirtioDevice,
    shared: Dma<'static>,
) {
    const FOREIGN_DESC: usize = 2;
    let foreign = super::nvme::FOREIGN_PROBE.load(Ordering::Relaxed);
    assert!(foreign != 0, "virtio-sound: this machine staged no foreign pool to aim at");
    let slot = controlq.initial_slots().swap_remove(FOREIGN_DESC);
    controlq.submit(
        slot,
        &[
            (shared.device_addr() + abi::OFF_CTRL_REQ as u64, 4, BufDir::Readable),
            (foreign, 8, BufDir::Writable),
        ],
        device.notify_mmio(),
        device.notify_off_multiplier(),
        abi::CONTROL_QUEUE,
    );
    log!(
        "virtio-sound: a control answer aimed at {foreign:#x}, inside another driver's pool \
         (actuator)"
    );
}

/// Builds every chain once; after this no descriptor is ever written again — the
/// driver's whole vocabulary becomes an avail-ring index and a doorbell write.
fn build_chains(
    controlq: &mut Virtqueue<'_>,
    eventq: &mut Virtqueue<'_>,
    txq: &mut Virtqueue<'_>,
    base: u64,
) {
    let at = |offset: usize| base + offset as u64;

    // One chain serves every command: the device reads the header first and
    // takes only what that command defines.
    controlq.write_chain(
        0,
        &[
            (at(abi::OFF_CTRL_REQ), abi::CTRL_BUF_BYTES as u32, BufDir::Readable),
            (at(abi::OFF_CTRL_RESP), abi::CTRL_BUF_BYTES as u32, BufDir::Writable),
        ],
    );

    // One descriptor per buffer: buffer index equals descriptor index.
    for i in 0..abi::EVENT_BUFS {
        eventq.write_chain(
            i as u16,
            &[(at(abi::OFF_EVENT_BUFS + i * abi::EVENT_BUF_STRIDE), EVENT_BYTES, BufDir::Writable)],
        );
    }

    for i in 0..abi::PERIODS {
        txq.write_chain(
            abi::tx_chain_head(i),
            &[
                (at(abi::OFF_TX_XFER + i * abi::XFER_STRIDE), XFER_HEADER_BYTES, BufDir::Readable),
                (at(abi::OFF_PCM + i * abi::PERIOD_BYTES), abi::PERIOD_BYTES as u32, BufDir::Readable),
                (at(abi::OFF_TX_STATUS + i * abi::STATUS_STRIDE), STATUS_BYTES, BufDir::Writable),
            ],
        );
    }
}

/// Arm the TX completion interrupt, or refuse and log why; never panics. The
/// handler is the TX used ring's only consumer, so an unarmed device leaves
/// every period in flight forever.
fn arm_interrupt(pci: &PciDevice, device: &VirtioDevice) -> bool {
    let vector = crate::arch::idt::VIRTIO_SOUND_VECTOR;
    if !pci.enable_msix(vector) {
        log!(
            "virtio-sound: NOT INITIALISED at PCI {:02x}:{:02x}.{} — its MSI-X could not be \
             armed and this driver has no other way to be told a period completed",
            pci.bus,
            pci.dev,
            pci.func
        );
        return false;
    }
    if let Err(refused) = device.bind_msix(abi::TX_QUEUE) {
        log!(
            "virtio-sound: NOT INITIALISED at PCI {:02x}:{:02x}.{} — the device refused a vector \
             for {refused}",
            pci.bus,
            pci.dev,
            pci.func
        );
        return false;
    }
    log!("virtio-sound: MSI-X vector {vector:#x} on table entry {MSIX_ENTRY}");
    true
}
