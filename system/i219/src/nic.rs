//! The I219, as this program drives it: the clock and the claim the driver's
//! logic is written against beside `toyos-pci-claim`'s register window and
//! grant, and nothing else.
//!
//! What the kernel keeps is the *claim*: config space, the interrupt vector it
//! programmed into this function, and the address space the function
//! translates through. An address written into a descriptor is one
//! [`PciDev::dma_alloc`] answered; one this driver invented instead is refused
//! at the unit and recorded against this process.
//!
//! **Two views of one grant, and the split is the boundary itself.** `Grant`
//! is the driver's logic's, and it reaches descriptors — sixteen bytes each,
//! read and written a word at a time. [`Nic::frames`] is this file's, and it
//! reaches payloads, which it copies to and from the link. The logic never
//! touches a payload and this file never touches a descriptor, which is what
//! lets the logic live in a crate that forbids `unsafe` at all.

use std::cell::Cell;
use std::rc::Rc;

use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::shm::ReadBytes;
use toyos::{say, DmaRegion, PciDev};
use toyos_abi::syscall::{RegWidth, SyscallError};
use toyos_device_memory::Registers;
use toyos_i219::{Clock, Delivery, Interrupts, NoDelivery, Part};
use toyos_link::state::{Counter, Counters, Group, Link};
use toyos_pci_claim::{Bar, Grant, KernelRefused};

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

    fn taken_within(&self, nanos: u64) -> Result<u32, SyscallError> {
        let poller = Poller::new(1);
        poller.watch(&*self.0, READABLE, 0);
        poller.wait(1, nanos, |_| {});
        self.taken()
    }
}

type Driver = toyos_i219::I219<Bar, Monotonic, Grant, Claim>;

/// Why the function was not brought up.
#[derive(Debug)]
pub enum Opening {
    Kernel(KernelRefused),
    /// The claim published no memory BAR this driver may map.
    NoWindow,
    /// The claim's configuration space named no message the kernel armed.
    Delivery(NoDelivery<SyscallError>),
    /// The part itself refused, and the word is the driver's.
    Driver(toyos_i219::Refusal<SyscallError>),
}

impl std::fmt::Display for Opening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(why) => write!(f, "{why}"),
            Self::NoWindow => write!(f, "the claim published no BAR this driver may map"),
            Self::Delivery(why) => write!(f, "{why}"),
            Self::Driver(why) => write!(f, "{why}"),
        }
    }
}

/// The card, brought up and driving.
pub struct Nic {
    /// Above the mappings it reaches: dropping the driver writes the part's
    /// registers, and fields drop in the order they are declared.
    driver: Driver,
    /// Held for their mappings' lives: the register window points into the
    /// first and the grant's into the second.
    _mapped: SharedMemory,
    region: DmaRegion,
    claim: Rc<PciDev>,
    /// This file's view of the same grant: the frame payloads, and nothing else.
    frames: Window,
    mac: [u8; 6],
    label: &'static str,
    reported: Latch<(toyos_i219::Counters, toyos_i219::Link)>,
}

/// The claim's register window, and the mapping it points into.
fn registers(dev: &PciDev, label: &str) -> Result<(Bar, SharedMemory), Opening> {
    let info = toyos_pci_claim::describe(dev).map_err(Opening::Kernel)?;
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
    let (window, mapped) = Bar::map(dev, bar, bytes).map_err(Opening::Kernel)?;
    say!(
        "{}: PCI {:02x}:{:02x}.{} registers in BAR {bar} ({bytes:#x} bytes)",
        label,
        info.bus,
        info.dev,
        info.func,
    );
    Ok((window, mapped))
}

/// Everything the claim is asked for before the part is reached, in the order
/// it is asked: the register window, the part stopped, then one grant — as the
/// driver reaches its descriptors — with the two mappings they point into, and
/// the function started.
fn granted(dev: &PciDev, label: &str) -> Result<(Bar, Grant, SharedMemory, DmaRegion), Opening> {
    let (bar, mapped) = registers(dev, label)?;
    // Before the start: a part a previous holder left receiving would write
    // into its descriptors.
    // Said first: whether the kernel's release reset the part is read here, in
    // the receive and transmit enables it handed over.
    say!(
        "{label}: inherited RCTL {:#010x} TCTL {:#010x}",
        bar.read32(toyos_i219::regs::RCTL),
        bar.read32(toyos_i219::regs::TCTL)
    );
    toyos_i219::quiesce(&bar);
    // Every buffer address a descriptor carries is 64 bits (82574 §7.1.3, §7.2.10.1).
    toyos_pci_claim::limit(dev, 64).map_err(Opening::Kernel)?;
    let (grant, region) = Grant::alloc(dev, toyos_i219::GRANT_BYTES).map_err(Opening::Kernel)?;
    toyos_pci_claim::start(dev).map_err(Opening::Kernel)?;
    Ok((bar, grant, mapped, region))
}

/// What a bring-up says about itself: a sentence each for what it asked the
/// I219's PHY before its reset and what that reset did, and one for the rest.
fn brought_up_words(brought_up: toyos_i219::BringUp) -> Vec<String> {
    let mut words = vec![
        brought_up.delivered.to_string(),
        format!(
            "out of the reset: TCTL {:#010x} TXDCTL {:#010x} TARC0 {:#010x} RXCSUM {:#010x} MRQC \
             {:#010x} RFCTL {:#010x}, receive addresses past the first valid {:#06x} (left to \
             firmware); TCTL {:#010x} after the bring-up",
            brought_up.out_of_reset.tctl,
            brought_up.out_of_reset.txdctl,
            brought_up.out_of_reset.tarc0,
            brought_up.out_of_reset.rxcsum,
            brought_up.out_of_reset.mrqc,
            brought_up.out_of_reset.rfctl,
            brought_up.out_of_reset.receive_addresses_valid,
            brought_up.transmit_control
        ),
    ];
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

fn say_brought_up(label: &str, brought_up: toyos_i219::BringUp) {
    for words in brought_up_words(brought_up) {
        say!("{label}: {words}");
    }
}

impl Nic {
    /// Take the claim's register window and one grant, and bring the part up.
    pub fn open(dev: PciDev, part: Part) -> Result<Self, Opening> {
        let label = match part {
            Part::I219 => "i219",
            Part::E82574 => "82574",
        };
        let dev = Rc::new(dev);
        // Bound before the driver, so one that is refused lets its part go
        // while both are still mapped.
        let (bar, grant, mapped, region) = granted(&dev, label)?;
        let delivery = Delivery::of(|at| dev.config_read(at.into(), RegWidth::U32))
            .map_err(Opening::Delivery)?;
        let frames = grant.window();
        let driver =
            toyos_i219::I219::open(part, delivery, bar, Monotonic, grant, Claim(Rc::clone(&dev)))
                .map_err(Opening::Driver)?;
        let mac = driver.mac();
        say_brought_up(label, driver.brought_up());
        Ok(Self { driver, _mapped: mapped, region, claim: dev, frames, mac, label, reported: Latch::default() })
    }

    /// The driver's name for its part: its lines' and the link's.
    pub fn label(&self) -> &'static str {
        self.label
    }

    fn link(&self) -> Link {
        match self.driver.link() {
            toyos_i219::Link::Down => Link::Down,
            toyos_i219::Link::Up { speed, full_duplex } => Link::Up { mbps: speed.mbps().into(), full_duplex },
        }
    }
}

impl toyos_link::driver::Nic for Nic {
    const MEDIUM: toyos_link::wire::Medium = toyos_link::wire::Medium::Wired;

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn claim(&self) -> &PciDev {
        &self.claim
    }

    /// **A claim that refuses the read for anything but `WouldBlock` is the
    /// kernel saying this function is no longer this process's**: a fault at
    /// the unit is the one that happens, and by the time it is answered the
    /// function's bus mastering is gone. So is a part that kept the frames a
    /// link change left in its transmit ring past the driver's deadline.
    /// Nothing drives either from here, and this dies where it can be read.
    fn begin_pass(&mut self) -> Link {
        if let Err(why) = self.driver.begin_pass() {
            panic!("{}: this NIC cannot be driven on — {why}", self.label);
        }
        self.link()
    }

    fn receive(&mut self, take: &mut dyn FnMut(&[u8])) -> bool {
        let Some(frame) = self.driver.poll_rx() else { return false };
        let window = self.frames.sub(frame.at, frame.len);
        // SAFETY: the window is inside the grant, which lives as long as
        // `self`; the device has finished with this buffer — the `DD` its
        // descriptor carries is what said so — and it is not published again
        // until `rx_done`, after this borrow ends. The I219 is the chipset
        // vendor's integrated MAC, inside the trusted base, so its descriptor
        // protocol is believed; an external or hot-plug device's driver must
        // copy instead.
        take(unsafe { std::slice::from_raw_parts(window.as_ptr() as *const u8, frame.len) });
        self.driver.rx_done(frame);
        true
    }

    fn received(&mut self) {
        self.driver.rx_flush();
    }

    fn tx_room(&mut self) -> usize {
        self.driver.tx_room()
    }

    fn wake_on_room(&mut self) -> usize {
        self.driver.wake_on_room()
    }

    fn send(&mut self, frame: ReadBytes<'_>) {
        let len = frame.len();
        let slot = self.driver.tx_reserve(len).unwrap_or_else(|| {
            panic!("{}: a {len}-byte frame was offered to a transmit ring that refuses it", self.label)
        });
        let memory = &self.region.memory;
        assert!(slot.at.checked_add(len).is_some_and(|end| end <= memory.len()), "{}: a transmit slot past the grant", self.label);
        // SAFETY: the assertion keeps `len` bytes from `slot.at` inside the
        // grant's mapping, which lives as long as `self`; the part reads none
        // of them until `tx_commit` publishes the slot; and the link's region
        // the frame is copied from is another mapping.
        unsafe { frame.read_to(memory.as_ptr().add(slot.at)) };
        self.driver.tx_commit(slot);
    }

    fn pass_due_in(&self) -> Option<u64> {
        self.driver.pass_due_in()
    }

    fn join(&mut self, group: Group) {
        self.driver.accept_multicast(group.mac());
    }

    /// What the driver and the MAC have counted so far, the transmit ring
    /// reclaimed first so a frame that has left is counted as sent.
    fn counters(&mut self) -> Counters {
        self.driver.reclaim();
        let wire = self.driver.wire();
        let ours = self.driver.counters();
        let mut counters = Counters::default();
        for (counter, value) in [
            (Counter::DescriptorsSent, ours.sent.into()),
            (Counter::DescriptorsReceived, ours.received.into()),
            (Counter::DescriptorsStranded, ours.stranded.into()),
            (Counter::TransmitFull, ours.tx_full.into()),
            (Counter::TransmitWakeArmed, ours.tx_wake_armed.into()),
            (Counter::TransmitWakeTaken, ours.tx_wake_taken.into()),
            (Counter::TransmitFullNanos, ours.tx_full_nanos),
            (Counter::TransmitUnreported, ours.tx_unreported),
            (Counter::TransmitUnreportedMax, ours.tx_unreported_max.into()),
            (Counter::WireSent, wire.sent),
            (Counter::WireReceived, wire.received),
            (Counter::WireSeen, wire.seen),
            (Counter::ErrorsMissed, wire.missed),
            (Counter::ErrorsCrc, wire.crc_errors),
        ] {
            counters.set(counter, value);
        }
        counters
    }

    /// Say what this driver has refused or been told about, and what the link
    /// is doing — when either has moved. Once a pass, never per frame: a burst
    /// of refusals is one line, where a line each is more frames for a served
    /// log to send.
    ///
    /// Keyed on the anomalies and never on the spurious count: that one moves
    /// on its own.
    fn report(&mut self) {
        let driver = &self.driver;
        let counters = driver.counters();
        let link = driver.link();
        let Some((was_counters, was_link)) = self.reported.moved((counters.anomalies(), link))
        else {
            return;
        };
        let label = self.label;
        if link != was_link {
            match link {
                toyos_i219::Link::Up { speed, full_duplex } => say!(
                    "{label}: link up at {} Mb/s {}{}",
                    speed.mbps(),
                    if full_duplex { "full duplex" } else { "half duplex" },
                    match driver.link_up_after_nanos() {
                        Some(nanos) => format!(", {} ms after the driver came up", nanos / 1_000_000),
                        None => String::new(),
                    },
                ),
                toyos_i219::Link::Down => say!("{label}: link down"),
            }
        }
        if counters.anomalies() != was_counters {
            say!(
                "{label}: refused {} over-length, {} errored, {} split and {} empty \
                 descriptor(s); {} overrun(s), {} descriptor-starvation report(s), \
                 {} frame(s) left in the ring at a link change, {} spurious interrupt(s)",
                counters.over_length,
                counters.errored,
                counters.split,
                counters.empty,
                counters.overruns,
                counters.starved,
                counters.stranded,
                counters.spurious,
            );
        }
    }
}

/// What a diagnostic last said, so it says it again only on a change.
///
/// **On change, not per element**: a device flooding a ring with descriptors a
/// driver will not act on costs one line, not one per descriptor, which is the
/// difference between a diagnostic and a way to drown the console from the
/// other side of the boundary.
struct Latch<T: Copy + PartialEq>(Cell<T>);

impl<T: Copy + PartialEq + Default> Default for Latch<T> {
    fn default() -> Self {
        Self(Cell::new(T::default()))
    }
}

impl<T: Copy + PartialEq> Latch<T> {
    /// The previous value if `now` is not it, and `None` if nothing has moved.
    fn moved(&self, now: T) -> Option<T> {
        let was = self.0.replace(now);
        (was != now).then_some(was)
    }
}
