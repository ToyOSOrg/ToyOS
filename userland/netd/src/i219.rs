//! The I219, as netd drives it: the four implementations the driver's logic is
//! written against, and nothing else.
//!
//! What the kernel keeps is the *claim*: config space, the interrupt vector it
//! programmed into this function, and the address space the function
//! translates through. An address written into a descriptor is one
//! [`PciDev::dma_alloc`] answered; one this driver invented instead is refused
//! at the unit and recorded against this process.
//!
//! **Two views of one grant, and the split is the boundary itself.** [`Grant`]
//! is the driver's, and it reaches descriptors — sixteen bytes each, read and
//! written a word at a time. [`Nic::frames`] is netd's, and it reaches
//! payloads. The driver never touches a payload and netd never touches a
//! descriptor, which is what lets the driver's logic live in a crate that
//! forbids `unsafe` at all.

use std::cell::RefCell;
use std::rc::Rc;

use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;
use toyos_i219::{Clock, DmaBuffers, Interrupts, Registers};

use crate::device::{KernelRefused, Latch, Window};

/// The register window. The offsets are `toyos-i219`'s own constants, all under
/// `toyos_i219::regs::REGISTER_BYTES`, which its `open` refuses a shorter window
/// than.
pub struct Bar {
    window: Window,
    /// The mapping the window points into, kept for as long as the window is.
    _mapped: SharedMemory,
}

impl Registers for Bar {
    fn bytes(&self) -> usize {
        self.window.len()
    }

    fn read(&self, reg: usize) -> u32 {
        self.window.read::<u32>(reg)
    }

    fn write(&self, reg: usize, value: u32) {
        self.window.write::<u32>(reg, value);
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
    window: Window,
    device_base: u64,
    /// The region the window points into, kept for as long as the window is.
    _region: DmaRegion,
}

impl DmaBuffers for Grant {
    fn bytes(&self) -> usize {
        self.window.len()
    }

    fn device_addr(&self, at: usize) -> u64 {
        self.device_base + at as u64
    }

    fn read(&self, at: usize) -> u64 {
        self.window.read::<u64>(at)
    }

    fn write(&self, at: usize, word: u64) {
        self.window.write::<u64>(at, word);
    }

    fn publish(&self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
    }

    fn observe(&self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
    }
}

/// The claim's interrupt record, handed over as the kernel answered it.
///
/// Shared with [`Nic`] rather than moved into the driver: the poller watches the
/// same handle the interrupt is read off, and one claim cannot be two objects.
pub struct Claim(Rc<PciDev>);

impl Interrupts for Claim {
    type Refused = SyscallError;
    const IDLE: SyscallError = SyscallError::WouldBlock;

    fn taken(&self) -> Result<u32, SyscallError> {
        self.0.irq().map(|record| record.count)
    }
}

type Driver = toyos_i219::I219<Bar, Monotonic, Grant, Claim>;

/// Why the function was not brought up.
#[derive(Debug)]
pub enum Opening {
    Kernel(KernelRefused),
    /// The claim published no memory BAR this driver may map.
    NoWindow,
    /// The part itself refused, and the word is the driver's.
    Driver(toyos_i219::Refusal),
}

impl std::fmt::Display for Opening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(why) => write!(f, "{why}"),
            Self::NoWindow => write!(f, "the claim published no BAR this driver may map"),
            Self::Driver(why) => write!(f, "{why}"),
        }
    }
}

/// The card, brought up and driving.
pub struct Nic {
    driver: RefCell<Driver>,
    claim: Rc<PciDev>,
    /// netd's view of the same grant: the frame payloads, and nothing else.
    frames: Window,
    /// Where a dropped frame is written. Ordinary memory outside the grant, so
    /// no device can reach it: smoltcp's token has to be given somewhere to put
    /// its bytes even when there is no descriptor to send them on.
    dropped: RefCell<Vec<u8>>,
    mac: [u8; 6],
    reported: Latch<(toyos_i219::Counters, toyos_i219::Link)>,
}

impl Nic {
    /// Take the claim's register window and one grant, and bring the part up.
    pub fn open(dev: PciDev) -> Result<Self, Opening> {
        let dev = Rc::new(dev);
        let info = dev
            .describe()
            .map_err(KernelRefused::on("the claim's description"))
            .map_err(Opening::Kernel)?;
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
        let mapped = dev
            .map_bar(bar, bytes)
            .map_err(KernelRefused::on("the BAR"))
            .map_err(Opening::Kernel)?;
        crate::say!(
            "netd: I219: PCI {:02x}:{:02x}.{} registers in BAR {bar} ({bytes:#x} bytes)",
            info.bus,
            info.dev,
            info.func,
        );

        let region = dev
            .dma_alloc(toyos_i219::GRANT_BYTES)
            .map_err(KernelRefused::on("a DMA grant"))
            .map_err(Opening::Kernel)?;
        let device_base = region.device_addr;
        // SAFETY: `dma_alloc` answered `GRANT_BYTES` bytes of live mapping, and
        // `region` is moved into the `Grant` this `Nic` owns for its own life.
        let grant = unsafe {
            Window::new(region.memory.as_ptr(), toyos_i219::GRANT_BYTES as usize)
        };

        // SAFETY: the same mapping, and `mapped` is moved into the `Bar` below.
        let registers = unsafe { Window::new(mapped.as_ptr(), bytes as usize) };
        let driver = toyos_i219::I219::open(
            Bar { window: registers, _mapped: mapped },
            Monotonic,
            Grant { window: grant, device_base, _region: region },
            Claim(Rc::clone(&dev)),
        )
        .map_err(Opening::Driver)?;
        let mac = driver.mac();
        Ok(Self {
            driver: RefCell::new(driver),
            claim: dev,
            frames: grant,
            dropped: RefCell::new(vec![0; toyos_i219::TX_BUF_BYTES]),
            mac,
            reported: Latch::default(),
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
    pub fn begin_pass(&self) -> Result<(), SyscallError> {
        self.driver.borrow_mut().begin_pass()?;
        self.report();
        Ok(())
    }

    pub fn poll_rx(&self) -> Option<toyos_i219::Frame> {
        self.driver.borrow_mut().poll_rx()
    }

    /// The bytes of a received frame.
    pub fn rx_frame(&self, frame: &toyos_i219::Frame) -> &[u8] {
        let window = self.frames.sub(frame.at, frame.len);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device has finished with this buffer — the `DD` its
        // descriptor carries is what said so — and it is not published again
        // until `rx_done`, which the caller makes after this borrow ends.
        unsafe { std::slice::from_raw_parts(window.as_ptr() as *const u8, frame.len) }
    }

    /// Give the buffer back once its frame has been read.
    pub fn rx_done(&self, frame: toyos_i219::Frame) {
        self.driver.borrow_mut().rx_done(frame);
    }

    /// Fill a transmit buffer with a `len`-byte frame and hand it to the
    /// device.
    ///
    /// Non-blocking. A frame the driver has no slot for is written into the
    /// scratch buffer and dropped — **a server never blocks**, smoltcp's token
    /// cannot say no, and a dropped frame's recovery is the peer's retransmit.
    pub fn tx<R>(&self, len: usize, fill: impl FnOnce(&mut [u8]) -> R) -> R {
        let slot = self.driver.borrow_mut().tx_reserve(len);
        let Some(slot) = slot else {
            self.report();
            let mut scratch = self.dropped.borrow_mut();
            if scratch.len() < len {
                scratch.resize(len, 0);
            }
            return fill(&mut scratch[..len]);
        };
        let window = self.frames.sub(slot.at, len);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device is not reading it, because this descriptor is out
        // of the ring and is published only by `tx_commit` below, after `fill`
        // has returned.
        let result = fill(unsafe { std::slice::from_raw_parts_mut(window.as_ptr(), len) });
        self.driver.borrow_mut().tx_commit(slot);
        result
    }

    /// Say what this driver has refused, dropped or been told about, and what
    /// the link is doing — when either has moved.
    ///
    /// Keyed on the anomalies and never on the spurious count: that one moves
    /// on its own.
    fn report(&self) {
        let driver = self.driver.borrow();
        let counters = driver.counters();
        let link = driver.link();
        let Some((was_counters, was_link)) = self.reported.moved((counters.anomalies(), link))
        else {
            return;
        };
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
                 {} frame(s) too long for a transmit buffer, {} frame(s) dropped with no \
                 transmit descriptor free, {} spurious interrupt(s)",
                counters.over_length,
                counters.errored,
                counters.split,
                counters.empty,
                counters.overruns,
                counters.starved,
                counters.too_long,
                counters.tx_dropped,
                counters.spurious,
            );
        }
    }
}
