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
        self.window.bytes()
    }

    fn read(&self, reg: usize) -> u32 {
        self.window.read::<u32>(reg)
    }

    fn write(&self, reg: usize, value: u32) {
        self.window.write::<u32>(reg, value);
    }
}

/// The machine's monotonic clock: one syscall, which the kernel serves from an
/// anchor plus the timestamp counter — and the thread's own sleep, which is how
/// this process gives the processor away between two accesses to a register
/// another agent is also reading.
pub struct Monotonic;

impl Clock for Monotonic {
    fn nanos(&self) -> u64 {
        toyos_abi::clock::nanos_since_boot()
    }

    fn pause(&self, nanos: u64) {
        std::thread::sleep(std::time::Duration::from_nanos(nanos));
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
        self.window.bytes()
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
    part: toyos_i219::Part,
    reported: Latch<(toyos_i219::Counters, toyos_i219::Link)>,
}

/// The claim's register window, mapped.
fn registers(dev: &PciDev) -> Result<Bar, Opening> {
    let info = dev
        .describe()
        .map_err(KernelRefused::on("the claim's description"))
        .map_err(Opening::Kernel)?;
    // The lowest BAR wide enough, rather than BAR 0 by name: the kernel
    // reports 0 bytes for a BAR it keeps back, and the MSI-X table's is one
    // it keeps.
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
    // SAFETY: `map_bar` answered `bytes` bytes of live mapping, and
    // `mapped` is moved into the `Bar` that carries the window.
    let window = unsafe { Window::new(mapped.as_ptr(), bytes as usize) };
    Ok(Bar { window, _mapped: mapped })
}

/// Everything the claim is asked for before the part is reached, in the order
/// it is asked: the register window, the part stopped, then one grant — as the
/// driver reaches its descriptors, and as netd reaches its frames.
fn granted(dev: &PciDev) -> Result<(Bar, Grant, Window), Opening> {
    let bar = registers(dev)?;
    // Before the grant: the grant is what starts the function mastering, and a
    // part a previous holder left receiving would write into its descriptors.
    // Said first: whether the kernel's release reset the part is read here, in
    // the receive and transmit enables it handed over.
    crate::say!(
        "netd: I219: inherited RCTL {:#010x} TCTL {:#010x}",
        toyos_i219::Registers::read(&bar, toyos_i219::regs::RCTL),
        toyos_i219::Registers::read(&bar, toyos_i219::regs::TCTL)
    );
    toyos_i219::quiesce(&bar);
    let region = dev
        .dma_alloc(toyos_i219::GRANT_BYTES)
        .map_err(KernelRefused::on("a DMA grant"))
        .map_err(Opening::Kernel)?;
    let device_base = region.device_addr;
    // SAFETY: `dma_alloc` answered `GRANT_BYTES` bytes of live mapping, and
    // `region` is moved into the `Grant`, which its caller keeps for as long
    // as it keeps either window.
    let frames =
        unsafe { Window::new(region.memory.as_ptr(), toyos_i219::GRANT_BYTES as usize) };
    Ok((bar, Grant { window: frames, device_base, _region: region }, frames))
}

/// What a bring-up says about itself: a sentence each for what it asked the
/// I219's PHY before its reset and what that reset did, and one for the rest.
pub fn brought_up_words(brought_up: toyos_i219::BringUp) -> Vec<String> {
    let mut words = Vec::new();
    match brought_up.woke {
        Some(Ok(woke)) => words.push(format!("before the reset the PHY was asked: {woke}")),
        Some(Err(why)) => words.push(format!("the PHY was not asked before the reset: {why}")),
        None => {}
    }
    if let Some(reset) = brought_up.reset {
        words.push(reset.to_string());
    }
    words.push(format!(
        "{}, and the PHY {}",
        if brought_up.master_quiet {
            "the function stopped mastering before it was reset"
        } else {
            "the function was still mastering when it was reset"
        },
        match brought_up.phy {
            Ok(phy) => phy.to_string(),
            Err(why) => format!("was not brought up: {why}"),
        },
    ));
    words
}

fn say_brought_up(brought_up: toyos_i219::BringUp) {
    for words in brought_up_words(brought_up) {
        crate::say!("netd: I219: {words}");
    }
}

impl Nic {
    /// Take the claim's register window and one grant, and bring the part up.
    pub fn open(dev: PciDev, part: toyos_i219::Part) -> Result<Self, Opening> {
        let dev = Rc::new(dev);
        let (bar, grant, frames) = granted(&dev)?;
        let driver =
            toyos_i219::I219::open(part, bar, Monotonic, grant, Claim(Rc::clone(&dev)))
                .map_err(Opening::Driver)?;
        let mac = driver.mac();
        say_brought_up(driver.brought_up());
        Ok(Self {
            driver: RefCell::new(driver),
            claim: dev,
            frames,
            dropped: RefCell::new(vec![0; toyos_i219::TX_BUF_BYTES]),
            mac,
            part,
            reported: Latch::default(),
        })
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn part(&self) -> toyos_i219::Part {
        self.part
    }

    /// The claim, for the poller: readable means an interrupt has landed.
    pub fn claim(&self) -> &PciDev {
        &self.claim
    }

    /// `crate::PROVOKE_MESSAGE`: raise one enabled cause on purpose.
    pub fn provoke_message(&self) {
        self.driver.borrow().provoke_message();
    }

    /// Pass frames sent to the multicast address `group`.
    pub fn accept_multicast(&self, group: [u8; 6]) {
        self.driver.borrow().accept_multicast(group);
    }

    /// What the bring-up found.
    pub fn brought_up(&self) -> toyos_i219::BringUp {
        self.driver.borrow().brought_up()
    }

    pub fn link(&self) -> toyos_i219::Link {
        self.driver.borrow().link()
    }

    /// What the driver and the MAC have counted so far, the transmit ring
    /// reclaimed first so a frame that has left is counted as sent.
    pub fn counts(&self) -> toyos_i219::lease::Counts {
        let mut driver = self.driver.borrow_mut();
        driver.reclaim();
        let wire = driver.wire();
        toyos_i219::lease::Counts::of(driver.counters(), wire)
    }

    /// Take the interrupt, acknowledge its causes and refresh the link — and
    /// answer the link where this pass found it changed.
    pub fn begin_pass(&self) -> Result<Option<toyos_i219::Link>, SyscallError> {
        let pass = self.driver.borrow_mut().begin_pass()?;
        Ok(pass.link_changed.then(|| self.link()))
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
    /// the link is doing — when either has moved. Once a pass, never per frame:
    /// a burst of drops is one line, where a line per drop is more frames for a
    /// served log to send through the ring that is already full.
    ///
    /// Keyed on the anomalies and never on the spurious count: that one moves
    /// on its own.
    pub fn report(&self) {
        let driver = self.driver.borrow();
        let counters = driver.counters();
        let link = driver.link();
        let Some((was_counters, was_link)) = self.reported.moved((counters.anomalies(), link))
        else {
            return;
        };
        if link != was_link {
            match link {
                toyos_i219::Link::Up { speed, full_duplex } => crate::say!(
                    "netd: I219: link up at {} Mb/s {}{}",
                    speed.mbps(),
                    if full_duplex { "full duplex" } else { "half duplex" },
                    match driver.link_up_after_nanos() {
                        Some(nanos) => format!(", {} ms after the driver came up", nanos / 1_000_000),
                        None => String::new(),
                    },
                ),
                toyos_i219::Link::Down => crate::say!("netd: I219: link down"),
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
