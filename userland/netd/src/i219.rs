//! The I219, as netd drives it: the four implementations the driver's logic is
//! written against, and nothing else.
//!
//! **Every function below is one instruction's worth of meaning** — a volatile
//! load, a volatile store, a barrier, a syscall, a field read. There is no
//! branch, no policy and no state beyond an address in any of them, because
//! everything that decides anything is in `toyos-i219`, where the host tests it
//! against the datasheet instead of against a boot.
//!
//! What the kernel keeps is the *claim*: config space, the interrupt vector it
//! programmed into this function, and the address space the function
//! translates through. An address written into a descriptor is one
//! [`PciDev::dma_alloc`] answered; one this driver invented instead is refused
//! at the unit and recorded against this process.
//!
//! **Two views of one grant, and the split is the boundary itself.**
//! [`Grant`] is the driver's, and it reaches descriptors — sixteen bytes each,
//! read and written a word at a time. [`Frames`] is netd's, and it reaches
//! payloads. The driver never touches a payload and netd never touches a
//! descriptor, which is what lets the driver's logic live in a crate that
//! forbids `unsafe` at all.

use std::cell::RefCell;
use std::rc::Rc;

use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_i219::{Clock, DmaBuffers, Interrupts, Registers};

/// The register window: one volatile access per call.
///
/// The offsets are `toyos-i219`'s own constants, all under
/// `toyos_i219::regs::REGISTER_BYTES`, which its `open` refuses a shorter
/// window than — so the bound is proved once, above, and neither accessor here
/// carries a branch. The `debug_assert` is the check that a *driver* bug is
/// caught in a test build; it is not what makes this safe.
pub struct Bar {
    mapped: SharedMemory,
    len: usize,
}

impl Registers for Bar {
    fn bytes(&self) -> usize {
        self.len
    }

    fn read(&self, reg: usize) -> u32 {
        debug_assert!(reg.is_multiple_of(4) && reg + 4 <= self.len);
        // SAFETY: `reg + 4` is inside the `len` bytes the mapping covers, which
        // `I219::open` refused anything shorter than; the address is 4-byte
        // aligned because every offset the driver names is. Volatile because
        // the device writes the same bytes.
        unsafe { self.mapped.as_ptr().add(reg).cast::<u32>().read_volatile() }
    }

    fn write(&self, reg: usize, value: u32) {
        debug_assert!(reg.is_multiple_of(4) && reg + 4 <= self.len);
        // SAFETY: as in `read`.
        unsafe { self.mapped.as_ptr().add(reg).cast::<u32>().write_volatile(value) }
    }
}

/// The machine's monotonic clock: one syscall, which the kernel serves from an
/// anchor plus the timestamp counter.
pub struct Monotonic;

impl Clock for Monotonic {
    fn nanos(&self) -> u64 {
        toyos_abi::syscall::clock_nanos()
    }
}

/// The DMA grant, as the driver reaches its descriptors.
pub struct Grant {
    region: DmaRegion,
    len: usize,
}

impl DmaBuffers for Grant {
    fn bytes(&self) -> usize {
        self.len
    }

    fn device_addr(&self, at: usize) -> u64 {
        self.region.device_addr + at as u64
    }

    fn read(&self, at: usize) -> u64 {
        debug_assert!(at.is_multiple_of(8) && at + 8 <= self.len);
        // SAFETY: `at + 8` is inside the grant, whose length `I219::open`
        // refused anything shorter than, and every descriptor word the driver
        // names is 8-byte aligned. Volatile because the device writes the same
        // bytes.
        unsafe { self.region.memory.as_ptr().add(at).cast::<u64>().read_volatile() }
    }

    fn write(&self, at: usize, word: u64) {
        debug_assert!(at.is_multiple_of(8) && at + 8 <= self.len);
        // SAFETY: as in `read`.
        unsafe { self.region.memory.as_ptr().add(at).cast::<u64>().write_volatile(word) }
    }

    fn publish(&self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
    }

    fn observe(&self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
    }
}

/// The claim's interrupt: one non-blocking read of the record the kernel
/// accumulates.
/// Shared with [`Nic`] rather than moved into the driver: the poller watches
/// the same handle the interrupt is read off, and one claim cannot be two
/// objects.
pub struct Claim(Rc<PciDev>);

impl Interrupts for Claim {
    fn taken(&self) -> u32 {
        match self.0.irq() {
            Ok(record) => record.count,
            // The ordinary "nothing since the last read".
            Err(SyscallError::WouldBlock) => 0,
            // Anything else is the kernel saying this device is no longer this
            // driver's: a fault at the unit is the one that happens, and by
            // the time it is answered the function's bus mastering is gone.
            // Every frame from here on is one that silently never arrives, so
            // this dies where it can be read.
            Err(why) => panic!("netd: this NIC's claim refused an interrupt read: {why:?}"),
        }
    }
}

type Driver = toyos_i219::I219<Bar, Monotonic, Grant, Claim>;

/// netd's view of the same grant: the frame payloads, and nothing else.
#[derive(Clone, Copy)]
struct Frames {
    base: *mut u8,
    len: usize,
}

impl Frames {
    fn bounded(self, at: usize, len: usize) -> *mut u8 {
        assert!(
            at.checked_add(len).is_some_and(|end| end <= self.len),
            "netd: a {len}-byte frame at {at:#x} runs past the {:#x}-byte grant",
            self.len
        );
        // SAFETY: the assertion above kept `at + len` inside the grant.
        unsafe { self.base.add(at) }
    }
}

/// Why the function was not brought up.
#[derive(Debug)]
pub enum Opening {
    /// The kernel refused a call this driver cannot work without, and the word
    /// is the kernel's own.
    Kernel(&'static str, SyscallError),
    /// The claim published no memory BAR this driver may map.
    NoWindow,
    /// The part itself refused, and the word is the driver's.
    Driver(toyos_i219::Refusal),
}

impl std::fmt::Display for Opening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(call, why) => write!(f, "the kernel refused {call}: {why:?}"),
            Self::NoWindow => write!(f, "the claim published no BAR this driver may map"),
            Self::Driver(why) => write!(f, "{why}"),
        }
    }
}

/// One received frame, as netd's event loop passes it around.
#[derive(Clone, Copy)]
pub struct RxSlot {
    index: usize,
    at: usize,
    len: usize,
}

/// The card, brought up and driving.
pub struct Nic {
    driver: RefCell<Driver>,
    claim: Rc<PciDev>,
    frames: Frames,
    /// Where a dropped frame is written. Ordinary memory outside the grant, so
    /// no device can reach it: smoltcp's token has to be given somewhere to
    /// put its bytes even when there is no descriptor to send them on.
    dropped: RefCell<Vec<u8>>,
    mac: [u8; 6],
    /// What `report` last said, so it says it again only on a change.
    reported: std::cell::Cell<(toyos_i219::Counters, toyos_i219::Link)>,
}

impl Nic {
    /// Take the claim's register window and one grant, and bring the part up.
    pub fn open(dev: PciDev) -> Result<Self, Opening> {
        let dev = Rc::new(dev);
        let info = dev.describe().map_err(|e| Opening::Kernel("the claim's description", e))?;
        // The register file is in BAR 0 on every part of this family; the
        // lowest BAR the claim will map is taken rather than assumed, because
        // the kernel reports 0 bytes for one it keeps — the MSI-X table's.
        let (bar, bytes) = info
            .bar_bytes
            .iter()
            .enumerate()
            .find(|(_, bytes)| **bytes >= toyos_i219::regs::REGISTER_BYTES as u64)
            .map(|(index, bytes)| (index as u32, *bytes))
            .ok_or(Opening::NoWindow)?;
        let mapped = dev.map_bar(bar, bytes).map_err(|e| Opening::Kernel("the BAR", e))?;
        crate::say!(
            "netd: I219: PCI {:02x}:{:02x}.{} registers in BAR {bar} ({bytes:#x} bytes)",
            info.bus,
            info.dev,
            info.func,
        );

        let region = dev
            .dma_alloc(toyos_i219::GRANT_BYTES)
            .map_err(|e| Opening::Kernel("a DMA grant", e))?;
        // Taken before the region moves into the driver: netd's own view of the
        // payloads, over the same bytes the driver reaches descriptors in.
        let frames = Frames { base: region.memory.as_ptr(), len: toyos_i219::GRANT_BYTES as usize };

        let driver = toyos_i219::I219::open(
            Bar { mapped, len: bytes as usize },
            Monotonic,
            Grant { region, len: toyos_i219::GRANT_BYTES as usize },
            Claim(Rc::clone(&dev)),
        )
        .map_err(Opening::Driver)?;
        let mac = driver.mac();
        Ok(Self {
            driver: RefCell::new(driver),
            claim: dev,
            frames,
            dropped: RefCell::new(vec![0; toyos_i219::TX_BUF_BYTES]),
            mac,
            reported: std::cell::Cell::new((
                toyos_i219::Counters::default(),
                toyos_i219::Link::default(),
            )),
        })
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    pub fn claim(&self) -> &PciDev {
        &self.claim
    }

    /// Take the interrupt, acknowledge its causes and refresh the link.
    pub fn begin_pass(&self) {
        self.driver.borrow_mut().begin_pass();
        self.report();
    }

    pub fn poll_rx(&self) -> Option<RxSlot> {
        let frame = self.driver.borrow_mut().poll_rx()?;
        Some(RxSlot { index: frame.index, at: frame.at, len: frame.len })
    }

    /// The bytes of a received frame.
    pub fn rx_frame(&self, slot: RxSlot) -> &[u8] {
        let at = self.frames.bounded(slot.at, slot.len);
        // SAFETY: `bounded` kept the range inside the grant, which lives as
        // long as `self`; the device has finished with this buffer — the `DD`
        // its descriptor carries is what said so — and it is not published
        // again until `rx_done`, which the caller makes after this borrow ends.
        unsafe { std::slice::from_raw_parts(at as *const u8, slot.len) }
    }

    /// Give the buffer back once its frame has been read.
    pub fn rx_done(&self, slot: RxSlot) {
        self.driver.borrow_mut().rx_done(slot.index);
    }

    /// Fill a transmit buffer with a `len`-byte frame and hand it to the
    /// device.
    ///
    /// Non-blocking. A frame with no descriptor free is written into the
    /// scratch buffer and dropped — **a server never blocks**, smoltcp's token
    /// cannot say no, and a dropped frame's recovery is the peer's retransmit.
    pub fn tx<R>(&self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        let slot = self.driver.borrow_mut().tx_reserve(len);
        let Some(slot) = slot else {
            self.report();
            return fill(&mut self.dropped.borrow_mut()[..len]);
        };
        let at = self.frames.bounded(slot.at, len);
        // SAFETY: `bounded` kept the range inside the grant, which lives as
        // long as `self`; the device is not reading it, because this
        // descriptor is out of the ring and is published only by `tx_commit`
        // below, after `fill` has returned.
        let result = fill(unsafe { std::slice::from_raw_parts_mut(at, len) });
        self.driver.borrow_mut().tx_commit(slot);
        result
    }

    /// Say what this driver has refused, dropped or been told about, and what
    /// the link is doing — when either has moved.
    ///
    /// **On change, not per element**: a device flooding the ring with
    /// descriptors this driver will not act on costs one line, not one per
    /// descriptor, which is the difference between a diagnostic and a way to
    /// drown the console from the other side of the boundary.
    fn report(&self) {
        let driver = self.driver.borrow();
        // Keyed on the anomalies and the link, never on the spurious count:
        // that one moves on its own, and a line per message it costs is a way
        // to drown the console from the other side of the boundary.
        let counters = driver.counters();
        let link = driver.link();
        let now = (counters.anomalies(), link);
        if now == self.reported.get() {
            return;
        }
        let (was_counters, was_link) = self.reported.get();
        self.reported.set(now);
        if link != was_link {
            if link.up {
                crate::say!(
                    "netd: I219: link up at {} Mb/s {}{}",
                    link.speed_mbps,
                    if link.full_duplex { "full duplex" } else { "half duplex" },
                    match driver.link_up_after_nanos() {
                        Some(nanos) => format!(", {} ms after the driver came up", nanos / 1_000_000),
                        None => String::new(),
                    },
                );
            } else {
                crate::say!("netd: I219: link down");
            }
        }
        if counters.anomalies() != was_counters {
            crate::say!(
                "netd: I219: refused {} over-length, {} errored, {} split and {} empty \
                 descriptor(s); {} overrun(s), {} descriptor-starvation report(s), \
                 {} frame(s) dropped with no transmit descriptor free, {} spurious \
                 interrupt(s)",
                counters.over_length,
                counters.errored,
                counters.split,
                counters.empty,
                counters.overruns,
                counters.starved,
                counters.tx_dropped,
                counters.spurious,
            );
        }
    }
}
