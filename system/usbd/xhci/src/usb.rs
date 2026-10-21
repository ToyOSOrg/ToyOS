//! The root hub and what is on it: one task per port, each following its
//! port through `port::PortState`, enumerating what connects in xHCI 1.2
//! §4.3's order, and serving the device's functions until it leaves.
//!
//! **Boot discovery is hot-plug.** Every port's task starts at bring-up; a
//! device present then and one plugged in later run the same code. A port
//! found connected is reset before its device is asked anything
//! (`PortState::adopt`).
//!
//! **A device's functions run beside its port's watch.** Each bound
//! interface is one future, and the port's task polls them with its own
//! look at the port; a teardown disables the slot, which answers every
//! operation still held on it [`Done::Gone`], and the functions end on that.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::future::{poll_fn, Future};
use core::pin::pin;
use core::task::Poll;

use toyos_device_memory::{DmaBuffers, Registers};

use crate::configure::{self, Configure};
use crate::descriptor::{self, Configuration, Endpoint};
use crate::exec::{self, Dead, Host, Nanos, Task, Tasks, Wake};
use crate::hc::{self, Done, Hc, Op, Ring, Trb};
use crate::input::Inputs;
use crate::msc::Disks;
use crate::port::{self, GaveUp, Gone, PortState, Reset, Step};
use crate::protocol::Protocol;
use crate::recovery::{Act, Command, EndpointState, Recovery};

/// TRSTRCY: "The USB System Software guarantees a minimum of 10 ms for reset
/// recovery" (USB 2.0 §7.1.7.3, §7.1.7.5), which xHCI leaves to software
/// (xHCI 1.2 §4.6.8). A fixed delay the specification mandates and raises no
/// event for. USB 3.2 §9.2.6.2 lets a SuperSpeed device be accessed at once.
pub(crate) const RESET_RECOVERY_NS: Nanos = 10_000_000;

/// How long a control transfer may go unanswered: "an upper limit of 5
/// seconds for any command to be processed" (USB 3.2 §9.2.6.1).
pub const CONTROL_NS: Nanos = 5_000_000_000;

/// What a configuration descriptor is read with: more than any device in
/// reach answers, and a longer one is walked as the prefix that arrived.
const CONFIG_BYTES: u16 = 1024;

/// Which classes this usbd binds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Config {
    /// HID interfaces are bound; an owner of input found not to be this
    /// usbd's ends their binding (`input::Opened::NotGiven`).
    pub input: bool,
    /// The row receives `disk`: mass-storage interfaces are bound.
    pub disks: bool,
}

/// A device this controller has enumerated, for `inspect`.
#[derive(Clone, Debug)]
pub struct Named {
    pub port: u8,
    pub slot: u8,
    pub speed: u8,
    pub device: descriptor::Device,
    /// What each interface was bound as, or why not.
    pub functions: Vec<String>,
}

/// What a function asks of its device's port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ask {
    /// A port reset and enumeration again: the disk on it is held for its
    /// return (`identity::RETURN_WINDOW`).
    Reenumerate,
    /// The device is let go of, and the last thing it is sent is a reset.
    Offline,
}

/// The driver: the controller, every port's belief, and what is bound.
pub struct Usb<R: Registers, D: DmaBuffers, C: Host, T> {
    pub hc: Hc<R, D>,
    pub wake: Wake<C>,
    ports: Vec<RefCell<PortState>>,
    /// Ports connected at bring-up whose first look has not ended.
    unsettled: RefCell<Vec<bool>>,
    pub named: RefCell<BTreeMap<u8, Named>>,
    pub disks: Disks<T>,
    pub input: RefCell<Inputs>,
    pub hid: Cell<toyos_usbhid::report::Counts>,
    pub config: Config,
    pub(crate) table: fn(&mut dyn toyos_gpt::Sectors) -> T,
    /// The bring-up's No-Op: `None` while it is outstanding.
    pub noop: Cell<Option<bool>>,
    /// A mass-storage device's run of breaks, carried by its port across the
    /// port reset its ladder asked for, so a device that never answers comes
    /// to its last rung.
    pub(crate) runs: RefCell<BTreeMap<u8, crate::ladder::Run>>,
}

impl<R: Registers, D: DmaBuffers, C: Host, T> Usb<R, D, C, T> {
    /// The ports as the controller came up, every connected one given the
    /// reset a device left by whatever ran before needs.
    pub fn new(hc: Hc<R, D>, host: C, config: Config, table: fn(&mut dyn toyos_gpt::Sectors) -> T) -> Self {
        let wake = Wake::new(host);
        let now = wake.now();
        let mut unsettled = Vec::new();
        let ports = (0..hc.max_ports)
            .map(|p| {
                let mut port = PortState::EMPTY;
                port.speaks(hc.protocols.of(p));
                let portsc = hc.portsc(p);
                unsettled.push(portsc.connected());
                if portsc.connected() {
                    let (kind, writes) = port.adopt(portsc, now);
                    wake.host().say(format_args!(
                        "port {} found connected (PORTSC {:#010x}); {kind:?} resetting it",
                        p + 1,
                        portsc.raw()
                    ));
                    for write in writes {
                        hc.write_portsc(p, write);
                    }
                }
                RefCell::new(port)
            })
            .collect();
        Self {
            hc,
            wake,
            ports,
            unsettled: RefCell::new(unsettled),
            named: RefCell::new(BTreeMap::new()),
            disks: Disks::default(),
            input: RefCell::new(Inputs::default()),
            hid: Cell::new(toyos_usbhid::report::Counts::default()),
            config,
            table,
            noop: Cell::new(None),
            runs: RefCell::new(BTreeMap::new()),
        }
    }

    /// Every task the driver runs: the bring-up's No-Op, each port's, and
    /// the keeper of disks held for their device's return.
    pub fn tasks(&self) -> Tasks<'_> {
        let mut tasks = Tasks::default();
        tasks.spawn(self.no_op());
        for p in 0..self.hc.max_ports {
            tasks.spawn(self.port(p));
        }
        tasks.spawn(self.disks.keep(self));
        tasks
    }

    pub fn say(&self, line: core::fmt::Arguments<'_>) {
        self.wake.host().say(line);
    }

    /// Every port connected at bring-up was enumerated and its device bound
    /// or refused, and every disk on them read or refused.
    pub fn settled(&self) -> bool {
        !self.unsettled.borrow().iter().any(|u| *u) && self.disks.settled()
    }

    fn settle(&self, p: u8) {
        self.unsettled.borrow_mut()[usize::from(p)] = false;
    }

    /// A No-Op that says the command ring and the interrupt both work before
    /// any device depends on them.
    async fn no_op(&self) -> Result<(), Dead> {
        let done = self.hc.command(&self.wake, Trb { control: hc::TRB_NO_OP, ..Trb::default() }).await?;
        self.noop.set(Some(done.succeeded()));
        match done.succeeded() {
            true => self.say(format_args!("a No-Op command completed")),
            false => self.say(format_args!("the bring-up's No-Op command: {done:?}")),
        }
        Ok(())
    }

    /// One port, for the driver's life.
    async fn port(&self, p: u8) -> Result<(), Dead> {
        loop {
            match self.look(p) {
                Look::Idle => exec::until(&self.wake, || self.hc.take_change(p).then_some(())).await,
                Look::Wait(at) => {
                    exec::until_or(&self.wake, at, || self.hc.take_change(p).then_some(())).await;
                }
                Look::Again => {}
                Look::GaveUp(why) => {
                    self.say(format_args!("port {}: given up on: {why:?} (PORTSC {:#010x})", p + 1, self.hc.portsc(p).raw()));
                    self.settle(p);
                }
                Look::Teardown(gone) => {
                    // A device refused or let go of: it holds no slot, or one
                    // nothing serves.
                    self.say(format_args!("port {}: its device is gone ({gone:?})", p + 1));
                    let slot = self.ports[usize::from(p)].borrow_mut().take_slot();
                    if let Some(slot) = slot {
                        self.disable(slot.get(), Vec::new()).await?;
                    }
                    self.named.borrow_mut().remove(&p);
                    self.ports[usize::from(p)].borrow_mut().torn_down();
                }
                Look::Enumerate(after) => {
                    let bound = self.arrived(p, after).await?;
                    self.settle(p);
                    if let Some(device) = bound {
                        self.serve(device).await?;
                    }
                }
            }
        }
    }

    /// What the port asks for now, with its machine's borrow over.
    fn look(&self, p: u8) -> Look {
        let portsc = self.hc.portsc(p);
        let mut port = self.ports[usize::from(p)].borrow_mut();
        let now = self.wake.now();
        #[cfg(test)]
        let before = *port;
        let step = port.step(portsc, now);
        #[cfg(test)]
        if let Some(why) = crate::invariants::check(&before, &step, portsc, now) {
            panic!("port {}: {why:?} from {before:?} on {portsc:?}", p + 1);
        }
        match step {
            Step::Idle => Look::Idle,
            Step::Wait(at) => Look::Wait(at),
            Step::Write(write) => {
                self.hc.write_portsc(p, write);
                Look::Again
            }
            Step::Reset(kind, write) => {
                self.say(format_args!("port {} {kind:?} reset", p + 1));
                self.hc.write_portsc(p, write);
                Look::Again
            }
            Step::GaveUp(why) => Look::GaveUp(why),
            Step::Enumerate { after, pending } => {
                pending.running();
                Look::Enumerate(after)
            }
            Step::Teardown(gone, pending) => {
                pending.running();
                Look::Teardown(gone)
            }
        }
    }

    /// A port whose reset finished: acknowledge it, enumerate what is there
    /// and bind its functions. `None` for a device enumerated nothing was
    /// bound on, or one refused; the port keeps it until it is pulled.
    async fn arrived(&self, p: u8, after: Option<Reset>) -> Result<Option<Device<'_, R, D, C, T>>, Dead> {
        let portsc = self.hc.portsc(p);
        self.hc.write_portsc(p, port::enumeration_ack(after, portsc));
        let portsc = self.hc.portsc(p);
        let port = &self.ports[usize::from(p)];
        if !portsc.enabled() {
            self.say(format_args!("port {}: reset and not enabled (PORTSC {:#010x})", p + 1, portsc.raw()));
            port.borrow_mut().enumerated(None);
            return Ok(None);
        }
        if after == Some(Reset::Hot) && self.hc.protocols.of(p) != Some(Protocol::Usb3) {
            exec::sleep_until(&self.wake, self.wake.now() + RESET_RECOVERY_NS).await;
        }
        let speed = portsc.speed();
        let Some(packet) = initial_ep0_packet(speed) else {
            self.say(format_args!("port {}: came up at speed {speed}, which this driver has no default for", p + 1));
            port.borrow_mut().enumerated(None);
            return Ok(None);
        };
        let done = self.hc.command(&self.wake, Trb { control: hc::TRB_ENABLE_SLOT, ..Trb::default() }).await?;
        let Done::Event { code: hc::CC_SUCCESS, slot, .. } = done else {
            self.say(format_args!("port {}: Enable Slot: {done:?}; leaving it unnamed", p + 1));
            port.borrow_mut().enumerated(None);
            return Ok(None);
        };
        let Some(device) = Device::new(self, p, slot, speed, packet) else {
            self.say(format_args!("port {}: no room in the grant for slot {slot}; leaving it unnamed", p + 1));
            self.disable(slot, Vec::new()).await?;
            port.borrow_mut().enumerated(None);
            return Ok(None);
        };
        match device.enumerate().await? {
            Ok(()) => {}
            Err(why) => {
                self.say(format_args!("port {}: {why}; leaving it unnamed", p + 1));
                let pages = device.pages.take();
                self.disable(slot, pages).await?;
                port.borrow_mut().enumerated(None);
                return Ok(None);
            }
        }
        port.borrow_mut().enumerated(core::num::NonZeroU8::new(slot));
        Ok(Some(device))
    }

    /// Serve `device`'s functions until the port takes it away or every one
    /// has ended, and then give its slot back.
    async fn serve(&self, device: Device<'_, R, D, C, T>) -> Result<(), Dead> {
        let p = device.port;
        let functions = device.functions();
        let mut running = pin!(exec::join(functions));
        // The port is read against the belief its enumeration left before
        // anything else: a pull inside the enumeration raised no edge after it.
        let mut wait_at: Option<Nanos> = Some(self.wake.now());
        let mut finished = false;
        let ended = loop {
            enum Woke {
                Ended(Result<(), Dead>),
                Asked(Ask),
                Look,
            }
            let woke = poll_fn(|cx| {
                if let Poll::Ready(ended) = running.as_mut().poll(cx) {
                    return Poll::Ready(Woke::Ended(ended));
                }
                if let Some(ask) = device.ask.take() {
                    self.wake.stir();
                    return Poll::Ready(Woke::Asked(ask));
                }
                if self.hc.take_change(p) || wait_at.is_some_and(|at| self.wake.now() >= at) {
                    self.wake.stir();
                    return Poll::Ready(Woke::Look);
                }
                if let Some(at) = wait_at {
                    self.wake.wake_at(at);
                }
                Poll::Pending
            })
            .await;
            match woke {
                Woke::Ended(ended) => {
                    ended?;
                    finished = true;
                    // A function that asked its port for something ends on
                    // asking.
                    break device.ask.take().map(Leave::Asked);
                }
                Woke::Asked(ask) => break Some(Leave::Asked(ask)),
                Woke::Look => match self.look(p) {
                    Look::Idle => wait_at = None,
                    Look::Wait(at) => wait_at = Some(at),
                    Look::Again => wait_at = Some(self.wake.now()),
                    Look::Teardown(gone) => break Some(Leave::Gone(gone)),
                    look @ (Look::GaveUp(_) | Look::Enumerate(_)) => {
                        unreachable!("a port holding a device it enumerated asked {look:?}")
                    }
                },
            }
        };
        let slot = self.ports[usize::from(p)].borrow_mut().take_slot().map_or(device.slot, |s| s.get());
        match ended {
            None => self.say(format_args!("port {}: nothing on its device is served now; it keeps the port", p + 1)),
            Some(Leave::Gone(gone)) => self.say(format_args!("port {}: its device is gone ({gone:?})", p + 1)),
            Some(Leave::Asked(ask)) => self.say(format_args!("port {}: its device is reset ({ask:?})", p + 1)),
        }
        device.leaving.set(true);
        let pages = device.pages.take();
        // The functions are polled while the slot goes: one may hold the
        // command ring the Disable Slot waits its turn on. Every operation on
        // the slot is answered once it has gone, so every function ends.
        let mut disabling = pin!(self.disable(slot, pages));
        let mut disabled = false;
        poll_fn(|cx| {
            if !disabled
                && let Poll::Ready(done) = disabling.as_mut().poll(cx) {
                    disabled = true;
                    if let Err(dead) = done {
                        return Poll::Ready(Err(dead));
                    }
                }
            if !finished
                && let Poll::Ready(done) = running.as_mut().poll(cx) {
                    finished = true;
                    if let Err(dead) = done {
                        return Poll::Ready(Err(dead));
                    }
                }
            if disabled && finished { Poll::Ready(Ok(())) } else { Poll::Pending }
        })
        .await?;
        let mut port = self.ports[usize::from(p)].borrow_mut();
        match ended {
            Some(Leave::Gone(_)) => {
                self.named.borrow_mut().remove(&p);
                port.torn_down();
            }
            Some(Leave::Asked(Ask::Reenumerate)) => {
                self.named.borrow_mut().remove(&p);
                let portsc = self.hc.portsc(p);
                let (_, writes) = port.adopt(portsc, self.wake.now());
                for write in writes {
                    self.hc.write_portsc(p, write);
                }
            }
            Some(Leave::Asked(Ask::Offline)) => {
                let portsc = self.hc.portsc(p);
                let kind = port::offline_reset(self.hc.protocols.of(p));
                self.hc.write_portsc(p, port::reset_write(kind, portsc));
            }
            // Its functions ended by themselves: it stays in the port, which
            // looks for its pull.
            None => {}
        }
        Ok(())
    }

    /// Disable `slot` and give back its pages; a Disable Slot the controller
    /// would not answer keeps them, since it may still reach them.
    async fn disable(&self, slot: u8, pages: Vec<usize>) -> Result<(), Dead> {
        let trb = Trb { control: hc::TRB_DISABLE_SLOT | (u32::from(slot) << 24), ..Trb::default() };
        let done = self.hc.command(&self.wake, trb).await?;
        self.hc.slot_gone(slot);
        if done.succeeded() {
            self.hc.name_slot(slot, None);
            self.hc.release(pages);
        } else {
            self.say(format_args!(
                "Disable Slot {slot}: {done:?}; its {} page(s) stay the controller's",
                pages.len()
            ));
        }
        Ok(())
    }
}

enum Leave {
    Gone(Gone),
    Asked(Ask),
}

/// What a port asks for, with its machine's borrow over.
#[derive(Debug)]
enum Look {
    Idle,
    Wait(Nanos),
    Again,
    GaveUp(GaveUp),
    Enumerate(Option<Reset>),
    Teardown(Gone),
}

/// EP0's Max Packet Size before the device has been asked, and `None` for a
/// speed this driver has no encoding for.
///
/// Low, High and SuperSpeed each fix it — 8, 64 and 512. **Full Speed does
/// not**: `bMaxPacketSize0` is 8, 16, 32 or 64 there, unknown until the first
/// eight bytes of the device descriptor are read, and every device answers
/// those in one packet (USB 2.0 §5.5.3), so 8 is what Address Device carries
/// and what Evaluate Context replaces (xHCI 1.2 §4.3, step 7).
pub fn initial_ep0_packet(speed: u8) -> Option<u16> {
    match speed {
        1 | 2 => Some(8),
        3 => Some(64),
        4..=7 => Some(512),
        _ => None,
    }
}

/// What `bMaxPacketSize0` means at this speed, or `None` when the device named
/// a size a device of that speed does not have. SuperSpeed states the
/// exponent: the byte is 9 and the size is 512 (USB 3.2 §9.6.1).
pub fn ep0_packet_from_descriptor(speed: u8, stated: u8) -> Option<u16> {
    match (speed, stated) {
        (1, 8 | 16 | 32 | 64) => Some(u16::from(stated)),
        (2, 8) => Some(8),
        (3, 64) => Some(64),
        (4..=7, 9) => Some(512),
        _ => None,
    }
}

/// A port speed's name (xHCI 1.2 Table 7-13's default Protocol Speed IDs).
pub fn speed_name(speed: u8) -> &'static str {
    match speed {
        1 => "full",
        2 => "low",
        3 => "high",
        4 => "super",
        5 => "super-plus-gen2x1",
        6 => "super-plus-gen1x2",
        7 => "super-plus-gen2x2",
        _ => "unknown",
    }
}

/// A standard, class or vendor request (USB 2.0 §9.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    fn packet(self) -> u64 {
        u64::from(self.request_type)
            | u64::from(self.request) << 8
            | u64::from(self.value) << 16
            | u64::from(self.index) << 32
            | u64::from(self.length) << 48
    }

    fn data_in(self) -> bool {
        self.request_type & 0x80 != 0
    }

    pub const fn get_descriptor(kind: u8, index: u8, length: u16) -> Self {
        Self { request_type: 0x80, request: 6, value: (kind as u16) << 8 | index as u16, index: 0, length }
    }
}

/// How a transfer the device did not complete ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Failed {
    /// It stalled: the device refused the request.
    Stall,
    /// The controller ended it with this completion code.
    Code(u32),
    /// Nothing answered by its deadline, and the controller gave it back.
    Silent,
    /// The device left.
    Gone,
    /// The endpoint would not run again.
    Stuck,
}

/// One enumerated device: its slot, its contexts and rings, and its
/// configuration.
pub struct Device<'u, R: Registers, D: DmaBuffers, C: Host, T> {
    pub usb: &'u Usb<R, D, C, T>,
    pub port: u8,
    pub slot: u8,
    pub speed: u8,
    packet: Cell<u16>,
    out: usize,
    input: usize,
    /// EP0's data stage lands here.
    data: usize,
    ep0: RefCell<Ring>,
    ep0_busy: Cell<bool>,
    rings: RefCell<BTreeMap<u8, Ring>>,
    /// Every page the device holds, given back with its slot.
    pub pages: RefCell<Vec<usize>>,
    pub descriptor: Cell<Option<descriptor::Device>>,
    pub config: RefCell<Option<Configuration>>,
    pub ask: Cell<Option<Ask>>,
    /// Its port took it away: a function issues nothing more.
    pub leaving: Cell<bool>,
}

impl<'u, R: Registers, D: DmaBuffers, C: Host, T> Device<'u, R, D, C, T> {
    fn new(usb: &'u Usb<R, D, C, T>, port: u8, slot: u8, speed: u8, packet: u16) -> Option<Self> {
        let hc = &usb.hc;
        let mut pages = Vec::new();
        let mut take = || -> Option<usize> {
            let page = hc.page()?;
            pages.push(page);
            Some(page)
        };
        let (out, input, ring, data) = match (take(), take(), take(), take()) {
            (Some(out), Some(input), Some(ring), Some(data)) => (out, input, ring, data),
            _ => {
                hc.release(pages);
                return None;
            }
        };
        hc.name_slot(slot, Some(out));
        let ep0 = hc.ring(ring);
        Some(Self {
            usb,
            port,
            slot,
            speed,
            packet: Cell::new(packet),
            out,
            input,
            data,
            ep0: RefCell::new(ep0),
            ep0_busy: Cell::new(false),
            rings: RefCell::new(BTreeMap::new()),
            pages: RefCell::new(pages),
            descriptor: Cell::new(None),
            config: RefCell::new(None),
            ask: Cell::new(None),
            leaving: Cell::new(false),
        })
    }

    pub fn wake(&self) -> &'u Wake<C> {
        &self.usb.wake
    }

    pub fn hc(&self) -> &'u Hc<R, D> {
        &self.usb.hc
    }

    /// A zeroed page of the grant this device holds until its slot goes.
    pub fn page(&self) -> Option<usize> {
        let page = self.hc().page()?;
        self.pages.borrow_mut().push(page);
        Some(page)
    }

    fn input_word(&self, context: usize, dword: usize, value: u32) {
        self.hc().dma.write32(self.hc().context(self.input, context) + dword * 4, value);
    }

    fn clear_input(&self) {
        for off in (0..hc::PAGE).step_by(8) {
            self.hc().dma.write64(self.input + off, 0);
        }
    }

    /// A command that names this slot's input context.
    async fn input_command(&self, kind: u32) -> Result<Done, Dead> {
        self.hc().dma.publish();
        let trb = Trb { param: self.hc().dma.device_addr(self.input), status: 0, control: kind | u32::from(self.slot) << 24 };
        self.hc().command(self.wake(), trb).await
    }

    /// xHCI 1.2 §4.3's order up to the configuration: Address Device; at full
    /// speed, the descriptor's first eight bytes and Evaluate Context; the
    /// device descriptor; the configuration.
    async fn enumerate(&self) -> Result<Result<(), String>, Dead> {
        let ep0 = |packet: u16| (3u32 << 1) | (4u32 << 3) | (u32::from(packet) << 16);
        self.clear_input();
        // Add the slot and EP0 (§4.3.3): one context entry, the speed and the
        // root-hub port.
        self.input_word(0, 1, 0b11);
        self.input_word(1, 0, (u32::from(self.speed) << 20) | (1 << 27));
        self.input_word(1, 1, (u32::from(self.port) + 1) << 16);
        let dequeue = self.ep0.borrow().dequeue(&self.hc().dma);
        self.input_word(2, 1, ep0(self.packet.get()));
        self.input_word(2, 2, dequeue as u32);
        self.input_word(2, 3, (dequeue >> 32) as u32);
        self.input_word(2, 4, 8);
        let done = self.input_command(hc::TRB_ADDRESS_DEVICE).await?;
        if !done.succeeded() {
            return Ok(Err(alloc::format!("Address Device: {done:?}")));
        }
        if self.speed == 1 {
            let prefix = match self.control(Setup::get_descriptor(1, 0, 8)).await? {
                Ok(bytes) => bytes,
                Err(why) => return Ok(Err(alloc::format!("the device descriptor's first 8 bytes: {why:?}"))),
            };
            let stated = match descriptor::ep0_packet(&prefix) {
                Ok(stated) => stated,
                Err(why) => return Ok(Err(alloc::format!("the device descriptor's first 8 bytes: {why}"))),
            };
            let Some(packet) = ep0_packet_from_descriptor(self.speed, stated) else {
                return Ok(Err(alloc::format!("bMaxPacketSize0={stated} is no size a full-speed device has")));
            };
            if packet != self.packet.get() {
                // EP0's packet size, and nothing else (§4.6.7).
                self.clear_input();
                self.input_word(0, 1, 1 << 1);
                self.input_word(2, 1, ep0(packet));
                let done = self.input_command(hc::TRB_EVALUATE_CONTEXT).await?;
                if !done.succeeded() {
                    return Ok(Err(alloc::format!("Evaluate Context: {done:?}")));
                }
                self.packet.set(packet);
            }
        }
        let device = match self.control(Setup::get_descriptor(1, 0, descriptor::DEVICE_BYTES as u16)).await? {
            Ok(bytes) => match descriptor::device(&bytes) {
                Ok(device) => device,
                Err(why) => return Ok(Err(alloc::format!("the device descriptor: {why}"))),
            },
            Err(why) => return Ok(Err(alloc::format!("the device descriptor: {why:?}"))),
        };
        self.descriptor.set(Some(device));
        let config = match self.control(Setup::get_descriptor(2, 0, CONFIG_BYTES)).await? {
            Ok(bytes) => match descriptor::configuration(&bytes) {
                Ok(config) => config,
                Err(why) => return Ok(Err(alloc::format!("the configuration: {why}"))),
            },
            Err(why) => return Ok(Err(alloc::format!("the configuration: {why:?}"))),
        };
        *self.config.borrow_mut() = Some(config);
        Ok(Ok(()))
    }

    /// Bind what this usbd binds of the configuration: create the endpoints
    /// every bound interface uses with one Configure Endpoint, select the
    /// configuration (§4.3.5), and answer each function's future.
    fn functions(&self) -> Vec<Task<'_>> {
        let usb = self.usb;
        let config = self.config.borrow().clone().expect("enumerated");
        let device = self.descriptor.get().expect("enumerated");
        let mut bound: Vec<(descriptor::Interface, Function, Option<crate::msc::Binding<'_>>)> = Vec::new();
        let mut said = Vec::new();
        for interface in &config.interfaces {
            let class = interface.class;
            let function = match (class.class, class.subclass, class.protocol) {
                (3, ..) if usb.config.input => Ok(Function::Hid),
                (3, ..) => Err("HID, and this usbd receives no `input`"),
                (8, 6, 0x50) if usb.config.disks => Ok(Function::Disk),
                (8, 6, 0x50) => Err("mass storage, and this usbd receives no `disk`"),
                (8, ..) => Err("mass storage other than SCSI over Bulk-Only"),
                _ => Err("a class this driver does not bind"),
            };
            match function {
                Ok(function) => {
                    said.push(alloc::format!("interface {}: {function:?}", interface.number));
                    // Counted from here, so the bus is not called settled
                    // between its bind and its table's read.
                    let binding = (function == Function::Disk).then(|| usb.disks.binding());
                    bound.push((interface.clone(), function, binding));
                }
                Err(why) => said.push(alloc::format!(
                    "interface {} ({:02x}:{:02x}:{:02x}): not bound, {why}",
                    interface.number,
                    class.class,
                    class.subclass,
                    class.protocol
                )),
            }
        }
        usb.say(format_args!(
            "port {}: {:04x}:{:04x} at {} speed on slot {}: {}",
            self.port + 1,
            device.vendor,
            device.product,
            speed_name(self.speed),
            self.slot,
            said.join("; ")
        ));
        usb.named.borrow_mut().insert(
            self.port,
            Named { port: self.port, slot: self.slot, speed: self.speed, device, functions: said },
        );
        let mut all: Vec<Task<'_>> = Vec::new();
        if bound.is_empty() {
            return all;
        }
        all.push(Box::pin(async move {
            let endpoints: Vec<Endpoint> = bound.iter().flat_map(|(i, ..)| i.endpoints.iter().copied()).collect();
            if let Err(why) = self.configure(&endpoints, Configure::First { speed: self.speed, port: self.port + 1 }).await? {
                usb.say(format_args!("port {}: {why}; nothing on it is bound", self.port + 1));
                return Ok(());
            }
            let select = Setup { request_type: 0, request: 9, value: u16::from(config.value), index: 0, length: 0 };
            if let Err(why) = self.control(select).await? {
                usb.say(format_args!("port {}: SET_CONFIGURATION({}): {why:?}; nothing on it is bound", self.port + 1, config.value));
                return Ok(());
            }
            let functions: Vec<Task<'_>> = bound
                .into_iter()
                .map(|(interface, function, binding)| -> Task<'_> {
                    match (function, binding) {
                        (Function::Disk, Some(binding)) => Box::pin(crate::msc::serve(self, interface, binding)),
                        _ => Box::pin(crate::hid::serve(self, interface)),
                    }
                })
                .collect();
            exec::join(functions).await
        }));
        all
    }

    /// Create `endpoints`, each on a fresh ring (§4.6.6), or re-create them
    /// with their toggles zeroed.
    pub async fn configure(&self, endpoints: &[Endpoint], how: Configure) -> Result<Result<(), String>, Dead> {
        let mut contexts = Vec::new();
        for ep in endpoints {
            let kind = match (ep.kind, ep.is_in()) {
                (descriptor::BULK, true) => configure::Kind::BulkIn,
                (descriptor::BULK, false) => configure::Kind::BulkOut,
                (descriptor::INTERRUPT, true) => configure::Kind::InterruptIn,
                (descriptor::INTERRUPT, false) => configure::Kind::InterruptOut,
                (kind, _) => return Ok(Err(alloc::format!("endpoint {:#04x} is of transfer type {kind}, which nothing here drives", ep.address))),
            };
            let page = match self.rings.borrow().get(&ep.dci()) {
                Some(ring) => ring.at,
                None => match self.page() {
                    Some(page) => page,
                    None => return Ok(Err("the grant has no page left for an endpoint's ring".into())),
                },
            };
            let ring = self.hc().ring(page);
            let dequeue = ring.dequeue(&self.hc().dma);
            self.rings.borrow_mut().insert(ep.dci(), ring);
            let interval = if kind == configure::Kind::InterruptIn || kind == configure::Kind::InterruptOut {
                configure::interval(self.speed, ep.interval)
            } else {
                0
            };
            contexts.push(configure::Endpoint {
                dci: ep.dci(),
                kind,
                max_packet: ep.max_packet,
                max_burst: ep.max_burst,
                interval,
                dequeue,
            });
        }
        self.clear_input();
        for word in configure::endpoints(&contexts, how) {
            self.input_word(word.context, word.dword, word.value);
        }
        let done = self.input_command(hc::TRB_CONFIGURE_ENDPOINT).await?;
        Ok(if done.succeeded() { Ok(()) } else { Err(alloc::format!("Configure Endpoint: {done:?}")) })
    }

    /// One request on EP0, its data stage, if any, device to host into the
    /// device's data page: the bytes that arrived.
    pub async fn control(&self, setup: Setup) -> Result<Result<Vec<u8>, Failed>, Dead> {
        exec::until(self.wake(), || (!self.ep0_busy.get()).then_some(())).await;
        self.ep0_busy.set(true);
        let answered = self.control_held(setup).await;
        self.ep0_busy.set(false);
        answered
    }

    async fn control_held(&self, setup: Setup) -> Result<Result<Vec<u8>, Failed>, Dead> {
        if self.leaving.get() {
            return Ok(Err(Failed::Gone));
        }
        let length = u32::from(setup.length.min(hc::PAGE as u16));
        let at = self.hc().dma.device_addr(self.data);
        // Setup (§6.4.1.2.1): its Transfer Type says which data stage follows.
        let (trt, dir) = match (length, setup.data_in()) {
            (0, _) => (0, 0),
            (_, true) => (3 << 16, hc::TRB_DIR_IN),
            (_, false) => (2 << 16, 0),
        };
        let mut trbs = alloc::vec![Trb { param: setup.packet(), status: 8, control: hc::TRB_SETUP | hc::TRB_IDT | trt }];
        if length > 0 {
            trbs.push(Trb { param: at, status: length, control: hc::TRB_DATA | hc::TRB_ISP | dir });
        }
        // The status stage goes the other way from the data, and IN when
        // there is none (§8.5.3).
        let status_dir = if length > 0 && setup.data_in() { 0 } else { hc::TRB_DIR_IN };
        trbs.push(Trb { param: 0, status: 0, control: hc::TRB_STATUS | hc::TRB_IOC | status_dir });
        let mut op = {
            let mut ring = self.ep0.borrow_mut();
            self.hc().transfer(self.slot, 1, &mut ring, &trbs, true)
        };
        let deadline = self.wake().now() + CONTROL_NS;
        let done = match op.answer(self.wake(), deadline).await {
            Some(done) => done,
            None => {
                return Ok(match self.restart(1, Some(op)).await? {
                    Ok(()) => Err(Failed::Silent),
                    Err(()) => Err(Failed::Stuck),
                })
            }
        };
        Ok(match done {
            Done::Gone => Err(Failed::Gone),
            Done::Event { code: hc::CC_SUCCESS | hc::CC_SHORT_PACKET, moved, .. } => {
                let moved = moved.min(length) as usize;
                Ok((0..moved).map(|i| self.read8(self.data + i)).collect())
            }
            Done::Event { code, .. } => match self.restart(1, None).await? {
                Ok(()) if code == hc::CC_STALL => Err(Failed::Stall),
                Ok(()) => Err(Failed::Code(code)),
                Err(()) => Err(Failed::Stuck),
            },
        })
    }

    /// A byte of the grant.
    pub fn read8(&self, at: usize) -> u8 {
        let word = self.hc().dma.read32(at & !3);
        (word >> ((at & 3) * 8)) as u8
    }

    /// Bytes into the grant, a dword at a time from a dword-aligned `at`.
    pub fn write_bytes(&self, at: usize, bytes: &[u8]) {
        for (i, chunk) in bytes.chunks(4).enumerate() {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            self.hc().dma.write32(at + i * 4, u32::from_le_bytes(word));
        }
    }

    /// The state the controller's output context gives endpoint `dci`.
    pub fn endpoint_state(&self, dci: u8) -> EndpointState {
        EndpointState::decode(self.hc().dma.read32(self.hc().context(self.out, usize::from(dci))))
    }

    /// Put `trbs` on endpoint `dci`'s ring.
    pub fn submit(&self, dci: u8, trbs: &[Trb]) -> Op<'u, R, D> {
        let mut rings = self.rings.borrow_mut();
        let ring = rings.get_mut(&dci).expect("a configured endpoint's ring");
        self.hc().transfer(self.slot, dci, ring, trbs, false)
    }

    /// Endpoint `dci` back to Running from whatever state its controller
    /// gives it (`recovery`), taking back `held`, a transfer it was given and
    /// never answered: once Set TR Dequeue Pointer has moved the ring past it,
    /// the controller holds none of it. `Err` for an endpoint that would not
    /// run again.
    pub async fn restart(&self, dci: u8, held: Option<Op<'u, R, D>>) -> Result<Result<(), ()>, Dead> {
        let state = self.endpoint_state(dci);
        let (mut recovery, mut act) = match Recovery::begin(state) {
            Ok(begun) => begun,
            Err(why) => {
                self.usb.say(format_args!("slot {} endpoint {dci}: {why:?}", self.slot));
                if let Some(op) = held {
                    op.reclaimed();
                }
                return Ok(Err(()));
            }
        };
        loop {
            match act {
                Act::Command(command) => {
                    let id = u32::from(self.slot) << 24 | u32::from(dci) << 16;
                    let trb = match command {
                        Command::ResetEndpoint => Trb { control: hc::TRB_RESET_ENDPOINT | id, ..Trb::default() },
                        Command::StopEndpoint => Trb { control: hc::TRB_STOP_ENDPOINT | id, ..Trb::default() },
                        Command::SetDequeue => {
                            let rings = self.rings.borrow();
                            let ring = if dci == 1 { *self.ep0.borrow() } else { *rings.get(&dci).expect("configured") };
                            Trb { param: ring.dequeue(&self.hc().dma), status: 0, control: hc::TRB_SET_TR_DEQUEUE | id }
                        }
                    };
                    let done = self.hc().command(self.wake(), trb).await?;
                    if !done.succeeded() {
                        self.usb.say(format_args!(
                            "slot {} endpoint {dci}: {} from {state}: {done:?}",
                            self.slot,
                            command.name()
                        ));
                        if let Some(op) = held {
                            op.reclaimed();
                        }
                        return Ok(Err(()));
                    }
                }
                // A control endpoint's stall is cleared by the next Setup
                // (USB 2.0 §8.5.3.4); any other is cleared at the device too.
                Act::ClearHalt if dci == 1 => {}
                Act::ClearHalt => {
                    let address = (dci / 2) | if dci % 2 == 1 { 0x80 } else { 0 };
                    let clear = Setup { request_type: 0x02, request: 1, value: 0, index: u16::from(address), length: 0 };
                    // Boxed: a control transfer's own recovery comes back here.
                    if Box::pin(self.control(clear)).await?.is_err() {
                        // After Set TR Dequeue Pointer (`Recovery`'s order), so
                        // the ring is already past `held`.
                        if let Some(op) = held {
                            op.reclaimed();
                        }
                        return Ok(Err(()));
                    }
                }
                Act::Running => break,
            }
            act = recovery.completed();
        }
        if let Some(op) = held {
            op.reclaimed();
        }
        Ok(Ok(()))
    }
}

/// What an interface is bound as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Function {
    Hid,
    Disk,
}
