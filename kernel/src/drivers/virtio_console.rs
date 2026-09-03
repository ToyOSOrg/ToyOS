//! VirtIO console: single-port (no MULTIPORT), replacing the 16550 UART as
//! the kernel log channel after init. Uses queues 0 (RX) and 1 (TX).
//!
//! RX is poll-driven: no UART IRQ handler is wired (see `arch/idt/mod.rs`).

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

use super::pci::PciDevice;
use super::virtio::{BufDir, DescSlot, Virtqueue, VirtioDevice, VIRTIO_F_VERSION_1};
use super::DmaPool;
use crate::log;
use crate::mm::{Dma, Unaligned};

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_CONSOLE_DEVICE: u16 = 0x1043; // 0x1040 + device_id 3

const QUEUE_SIZE: u16 = 16;
const TX_BUF_SIZE: usize = 4096;
const RX_BUF_SIZE: u32 = 256;
const RX_BUF_COUNT: usize = 8;

const OFF_TX_BUF:  usize = 0x0000; // 1 page (4KB)
const OFF_RX_BUFS: usize = 0x1000; // 8 × 256 = 2KB, fits in 1 page
const OFF_RXVQ:    usize = 0x2000; // virtqueue desc/avail/used
const OFF_TXVQ:    usize = 0x3000;
const DMA_SIZE:    usize = 0x4000;

struct RxPending {
    buf_idx: usize,
    slot: DescSlot,
    len: u32,
    pos: u32,
}

// Every field must stay individually `Send` (no raw pointers), or this needs an explicit `unsafe impl Send`.
struct VConsole {
    device: VirtioDevice,
    rx: Virtqueue<'static>,
    tx: Virtqueue<'static>,
    tx_buf: Dma<'static>,
    tx_slot: Option<DescSlot>,
    // Read only after the used ring returns the descriptor, so no read races a fill.
    rx_bufs: [Dma<'static, Unaligned>; RX_BUF_COUNT],
    /// Maps virtqueue desc id → rx_buf index (filled at refill, read at poll).
    desc_to_rx: [u8; QUEUE_SIZE as usize],
    // Draining RX buffer: slot recovered from the used ring, refilled once fully consumed.
    rx_pending: Option<RxPending>,
}

struct ConsoleCell(UnsafeCell<MaybeUninit<VConsole>>);
// SAFETY: sound because every access goes through `with_console`, gated by
// `READY` and serialized by the caller's `serial::BackendGuard`.
unsafe impl Sync for ConsoleCell {}

// Written once in `init`; every reader/writer goes through `with_console`,
// gated by `READY` and serialized by the caller's `serial::BackendGuard`.
static CONSOLE: ConsoleCell = ConsoleCell(UnsafeCell::new(MaybeUninit::uninit()));
static READY: AtomicBool = AtomicBool::new(false);

#[inline]
pub fn is_ready() -> bool {
    READY.load(Ordering::Acquire)
}

/// Disables the fast path; `is_ready()` then returns false and logging falls back to UART.
pub fn disable() {
    READY.store(false, Ordering::Release);
}

// Check-then-deref stays one function: split, a caller could deref without checking.
#[inline]
fn with_console<R>(f: impl FnOnce(&mut VConsole) -> R) -> Option<R> {
    if !is_ready() {
        return None;
    }
    // SAFETY: `READY`'s Acquire load (checked above) pairs with `init`'s Release
    // store, proving initialization; the caller's `serial::BackendGuard` provides exclusion.
    Some(f(unsafe { (*CONSOLE.0.get()).assume_init_mut() }))
}

fn refill_rx(c: &mut VConsole, buf_idx: usize, slot: DescSlot) {
    let desc_id = c.rx.submit(
        slot,
        &[(c.rx_bufs[buf_idx].device_addr(), RX_BUF_SIZE, BufDir::Writable)],
        c.device.notify_mmio(),
        c.device.notify_off_multiplier(),
        0,
    );
    c.desc_to_rx[desc_id as usize] = buf_idx as u8;
}

/// Synchronous: blocks until the host consumes each chunk. Caller must hold `serial::BackendGuard` with IRQs disabled.
pub fn write_bytes_locked(bytes: &[u8]) {
    with_console(|c| {
        let mut off = 0;
        while off < bytes.len() {
            let n = (bytes.len() - off).min(TX_BUF_SIZE);
            // `n` is bounded to `TX_BUF_SIZE`; the `BackendGuard` excludes other
            // callers, and the previous chunk's `submit_and_wait` already
            // returned, so the device is done with `tx_buf` too.
            c.tx_buf.copy_from(0, &bytes[off..off + n]);
            let slot = c.tx_slot.take().expect("vconsole: no tx slot");
            c.tx_slot = Some(c.tx.submit_and_wait(
                slot,
                &[(c.tx_buf.device_addr(), n as u32, BufDir::Readable)],
                c.device.notify_mmio(),
                c.device.notify_off_multiplier(),
                1,
            ));
            off += n;
        }
    });
}

/// Read one byte from RX. Caller must hold `serial::BackendGuard` with IRQs disabled.
pub fn try_read_byte_locked() -> Option<u8> {
    with_console(|c| {
        if c.rx_pending.is_none() {
            // `slot`/`len` are bounded by `poll_used`: id indexes `desc_to_rx`
            // (length `QUEUE_SIZE`), len is at most `RX_BUF_SIZE` — unchecked,
            // an over-long `len` would hand kernel memory to the console as input.
            let (slot, len) = c.rx.poll_used()?;
            let buf_idx = c.desc_to_rx[slot.id() as usize] as usize;
            c.rx_pending = Some(RxPending { buf_idx, slot, len, pos: 0 });
        }
        let p = c.rx_pending.as_mut().unwrap();
        // Bounded twice: `read` refuses `pos + 1 > RX_BUF_SIZE`, and `pos < len`
        // is already bounded by `poll_used` above.
        let byte: u8 = c.rx_bufs[p.buf_idx].read(p.pos as usize);
        p.pos += 1;
        if p.pos >= p.len {
            let p = c.rx_pending.take().unwrap();
            refill_rx(c, p.buf_idx, p.slot);
        }
        Some(byte)
    })
    .flatten()
}

/// Caller must hold `serial::BackendGuard` with IRQs disabled.
pub fn has_data_locked() -> bool {
    with_console(|c| c.rx_pending.is_some() || c.rx.has_used()).unwrap_or(false)
}

pub fn init(devices: &[PciDevice]) -> bool {
    let pci_dev = match devices.iter().find(|d| d.is_id(VIRTIO_VENDOR, VIRTIO_CONSOLE_DEVICE)) {
        Some(d) => *d,
        None => {
            log!("virtio-console: no device found");
            return false;
        }
    };
    log!("virtio-console: found at PCI {:02x}:{:02x}.{}", pci_dev.bus, pci_dev.dev, pci_dev.func);

    // An address space of this device's own, holding one pool and nothing else.
    let space = crate::iommu::DeviceSpace::create();
    // Leaked, not held in a `static`: the console is never unbound.
    let dma = DmaPool::alloc_in(DMA_SIZE, space).leak();
    // Before the device is told an address, and after the only mapping it gets.
    space.attach(pci_dev.bus, pci_dev.dev, pci_dev.func);

    let device = match VirtioDevice::init(&pci_dev, VIRTIO_F_VERSION_1) {
        Ok(device) => device,
        Err(why) => {
            log!("virtio-console: PCI {:02x}:{:02x}.{} {why} — device refused",
                pci_dev.bus, pci_dev.dev, pci_dev.func);
            return false;
        }
    };

    let mut rx = Virtqueue::new(dma.subview(OFF_RXVQ, 0x1000), QUEUE_SIZE);
    let mut tx = Virtqueue::new(dma.subview(OFF_TXVQ, 0x1000), QUEUE_SIZE);

    device.setup_queue(0, &mut rx);
    device.setup_queue(1, &mut tx);
    device.enable_queue(0);
    device.enable_queue(1);
    device.activate();

    let tx_buf = dma.subview(OFF_TX_BUF, TX_BUF_SIZE);

    let rx_bufs: [Dma<'static, Unaligned>; RX_BUF_COUNT] = core::array::from_fn(|i| {
        dma.subview(OFF_RX_BUFS + i * RX_BUF_SIZE as usize, RX_BUF_SIZE as usize).unaligned()
    });

    let mut tx_slots = tx.initial_slots();
    let tx_slot = tx_slots.pop().expect("vconsole: no tx slots");
    drop(tx_slots);

    let mut rx_slots = rx.initial_slots();

    let mut console = VConsole {
        device, rx, tx,
        tx_buf,
        tx_slot: Some(tx_slot),
        rx_bufs,
        desc_to_rx: [0; QUEUE_SIZE as usize],
        rx_pending: None,
    };

    for i in 0..RX_BUF_COUNT {
        let slot = rx_slots.pop().expect("vconsole: not enough rx slots");
        refill_rx(&mut console, i, slot);
    }
    drop(rx_slots);

    // SAFETY: single write, called once before `READY` is set and before any
    // AP starts, so nothing is reading `CONSOLE` yet; `with_console` only reads
    // after this function's Release store below.
    unsafe { (*CONSOLE.0.get()).write(console); }
    READY.store(true, Ordering::Release);
    crate::drivers::serial::console_changed();

    log!("virtio-console: initialized ({} RX bufs of {} bytes, TX buf {} bytes)",
        RX_BUF_COUNT, RX_BUF_SIZE, TX_BUF_SIZE);
    true
}
