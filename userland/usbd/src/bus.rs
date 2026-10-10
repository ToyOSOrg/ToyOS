//! The root hub and what is on it: every port stepped through
//! `toyos_xhci::port`, every device enumerated in `toyos_xhci::enumerate`'s
//! order, and the one operation the controller owes an answer for
//! (`toyos_xhci::job::Outstanding`).
//!
//! **Nothing here waits.** An act is submitted, its answer arrives with an
//! interrupt and is matched by the TRB it names, and the act after it is
//! submitted then; a port's debounce and reset deadlines and an operation's
//! deadline are instants handed back to the loop as its next wake.
//!
//! **A device is named and stops there**: enumeration runs to the
//! configuration descriptor and no further, so every device keeps its slot
//! and its address and no class is bound; which interface is served is a
//! class driver's, and there is none yet.
//!
//! **Every event is announced.** The event ring is read only on an interrupt
//! record, so an operation whose deadline passes with its answer on the ring
//! is an interrupt that never came, counted and named rather than taken.

use std::num::NonZeroU8;

use toyos_abi::inventory::UsbSpeed;
use toyos_xhci::descriptor::{self, Class, Interfaces};
use toyos_xhci::enumerate::{self, Act, Command, Enumeration, Learnt, Next, Request};
use toyos_xhci::job::{Await, Outcome, Outstanding, Stages, CC_SUCCESS};
use toyos_xhci::port::{self, Gone, Nanos, PortState, Reset, Step};

use crate::hc::{self, Controller, Event, Ring, Trb};

/// How long the controller has to answer one command or transfer. Policy:
/// the kernel driver's bound for the same operations.
const ANSWER_DEADLINE: Nanos = 2_000_000_000;

/// The steps one look at one port may take before it is called stuck: the
/// longest legitimate run is an acknowledge, a teardown and a debounce.
const STEP_BUDGET: usize = 16;

/// What a configuration descriptor is read with: more than any device in
/// reach answers, and a longer one is walked as the prefix that arrived.
const CONFIG_BYTES: u16 = 1024;

/// A device this controller has enumerated.
pub struct Named {
    pub port: u8,
    pub slot: u8,
    pub speed: UsbSpeed,
    pub device: descriptor::Device,
    pub interfaces: Interfaces,
}

impl Named {
    /// What the device is: its own class, or its first interface's where it
    /// defers to its interfaces.
    pub fn class(&self) -> Class {
        match self.device.class.class {
            0 => self.interfaces.iter().next().unwrap_or(self.device.class),
            _ => self.device.class,
        }
    }
}

/// What the loop counted, which `inspect` reads.
#[derive(Default)]
pub struct Counts {
    /// Interrupt records read off the claim, and the interrupts they counted.
    pub records: u64,
    pub interrupts: u64,
    /// Events taken off the ring, each after a record.
    pub events: u64,
    /// Of them, events of a type nothing this driver does raises.
    pub other: u64,
    /// Answers found on the ring by an operation's deadline: an event no
    /// interrupt announced.
    pub unannounced: u64,
    /// Operations nothing answered.
    pub silent: u64,
    /// The bring-up's No-Op: `None` while it is outstanding.
    pub noop: Option<bool>,
}

/// What the outstanding operation is for.
enum What {
    NoOp,
    Enumerating(Enumerating),
    /// Disable Slot; then the port is empty, or keeps a device it refused.
    SlotGone { port: u8, slot: u8, refused: bool },
}

/// One device's enumeration, between its acts.
struct Enumerating {
    port: u8,
    speed: u8,
    /// EP0's packet size as the controller was last told it.
    packet: u16,
    /// Zero until Enable Slot has answered.
    slot: u8,
    ep0: Option<Ring>,
    seq: Enumeration,
    act: Act,
    device: Option<descriptor::Device>,
    interfaces: Option<Interfaces>,
}

/// What a port's step asks for, with the port machine's borrow over.
enum Look {
    Again,
    Done(Option<Nanos>),
    Enumerate(Option<Reset>),
    Teardown(Gone),
}

pub struct Bus {
    pub ctrl: Controller,
    ports: Vec<PortState>,
    outstanding: Outstanding<What>,
    pub devices: Vec<Named>,
    /// A Port Status Change arrived, or a look was cut short by an operation.
    ports_dirty: bool,
    port_wake: Option<Nanos>,
    pub counts: Counts,
}

impl Bus {
    /// The ports as the controller came up, every connected one given the
    /// reset a device left by whatever ran before needs
    /// (`PortState::adopt`), and the No-Op that says the command ring and the
    /// interrupt both work before any device depends on them.
    pub fn new(mut ctrl: Controller, now: Nanos) -> Self {
        let mut ports: Vec<PortState> = (0..ctrl.max_ports)
            .map(|p| {
                let mut port = PortState::EMPTY;
                port.speaks(ctrl.protocols.of(p));
                port
            })
            .collect();
        for (p, port) in ports.iter_mut().enumerate() {
            let p = p as u8;
            let portsc = ctrl.portsc(p);
            if portsc.connected() {
                let (kind, writes) = port.adopt(portsc, now);
                println!("usbd: port {} found connected (PORTSC {:#010x}); {kind:?} resetting it", p + 1, portsc.raw());
                for write in writes {
                    ctrl.write_portsc(p, write);
                }
            }
        }
        let mut outstanding = Outstanding::EMPTY;
        let trb = ctrl.command(Trb { control: hc::TRB_NO_OP_COMMAND, ..Trb::default() });
        outstanding.submit(What::NoOp, Await::Command { trb }, Stages::One, now + ANSWER_DEADLINE);
        Self { ctrl, ports, outstanding, devices: Vec::new(), ports_dirty: true, port_wake: None, counts: Counts::default() }
    }

    /// An interrupt record of `count`: take every event the ring holds, up to
    /// one ring's worth — the rest is the next record's.
    pub fn interrupt(&mut self, count: u64) {
        self.counts.records += 1;
        self.counts.interrupts += count;
        for _ in 0..hc::PAGE / 16 {
            let Some(event) = self.ctrl.next_event() else { break };
            self.counts.events += 1;
            match event {
                Event::Command { trb, code, slot } => {
                    self.outstanding.answered(Await::Command { trb }, code, u32::from(slot));
                }
                Event::Transfer { trb, slot, dci, code, residue } => {
                    self.outstanding.answered(Await::Transfer { slot, dci, trb }, code, residue);
                }
                // The register says what a port is; the event is a reason to look.
                Event::Port => self.ports_dirty = true,
                Event::Other => self.counts.other += 1,
            }
        }
        self.ctrl.consumed();
    }

    /// Act on whatever is over, step the ports where there is reason to, and
    /// answer when the loop must come back with nothing else to wake it.
    pub fn pass(&mut self, now: Nanos) -> Option<Nanos> {
        if let Some((what, outcome)) = self.outstanding.finished(now) {
            if outcome == Outcome::Silent {
                self.counts.silent += 1;
                if self.ctrl.event_waiting() {
                    self.counts.unannounced += 1;
                    println!("usbd: an answer is on the event ring and no interrupt said so");
                }
            }
            self.finished(what, outcome, now);
        }
        if !self.outstanding.busy() && port::due(self.ports_dirty, &self.ports) {
            self.ports_dirty = false;
            self.port_wake = None;
            for p in 0..self.ctrl.max_ports {
                self.look(p, now);
            }
        }
        match (self.outstanding.wake_at(), self.port_wake) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (at, None) | (None, at) => at,
        }
    }

    /// Step one port until it rests, waits, or starts an effect.
    fn look(&mut self, p: u8, now: Nanos) {
        for _ in 0..STEP_BUDGET {
            // An effect another port started holds the one operation slot,
            // and this port's next step may need it.
            if self.outstanding.busy() {
                self.ports_dirty = true;
                return;
            }
            let portsc = self.ctrl.portsc(p);
            let look = match self.ports[usize::from(p)].step(portsc, now) {
                Step::Idle => Look::Done(None),
                Step::Wait(at) => Look::Done(Some(at)),
                Step::Write(write) => {
                    self.ctrl.write_portsc(p, write);
                    Look::Again
                }
                Step::Reset(kind, write) => {
                    println!("usbd: port {} {kind:?} reset", p + 1);
                    self.ctrl.write_portsc(p, write);
                    Look::Again
                }
                Step::GaveUp(why) => {
                    println!("usbd: port {} given up on: {why:?} (PORTSC {:#010x})", p + 1, portsc.raw());
                    Look::Done(None)
                }
                Step::Enumerate { after, pending } => {
                    pending.running();
                    Look::Enumerate(after)
                }
                Step::Teardown(gone, pending) => {
                    pending.running();
                    Look::Teardown(gone)
                }
            };
            match look {
                Look::Again => continue,
                Look::Done(at) => {
                    self.port_wake = match (self.port_wake, at) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (w, None) | (None, w) => w,
                    };
                    return;
                }
                Look::Enumerate(after) => return self.begin(p, after, now),
                Look::Teardown(gone) => return self.teardown(p, gone, now),
            }
        }
        println!("usbd: port {} took {STEP_BUDGET} steps and did not settle; looking again", p + 1);
        self.ports_dirty = true;
    }

    fn teardown(&mut self, p: u8, gone: Gone, now: Nanos) {
        println!("usbd: port {}: its device is gone ({gone:?}, PORTSC {:#010x})", p + 1, self.ctrl.portsc(p).raw());
        self.devices.retain(|d| d.port != p);
        match self.ports[usize::from(p)].take_slot() {
            Some(slot) => self.disable(p, slot.get(), false, now),
            None => self.ports[usize::from(p)].torn_down(),
        }
    }

    fn disable(&mut self, port: u8, slot: u8, refused: bool, now: Nanos) {
        let trb = self.ctrl.command(Trb { control: hc::TRB_DISABLE_SLOT | (u32::from(slot) << 24), ..Trb::default() });
        self.outstanding.submit(What::SlotGone { port, slot, refused }, Await::Command { trb }, Stages::One, now + ANSWER_DEADLINE);
    }

    /// A port whose reset finished: acknowledge it, and ask for a slot.
    fn begin(&mut self, p: u8, after: Option<Reset>, now: Nanos) {
        let portsc = self.ctrl.portsc(p);
        println!("usbd: port {} enumerating after {after:?} (PORTSC {:#010x})", p + 1, portsc.raw());
        self.ctrl.write_portsc(p, port::enumeration_ack(after, portsc));
        let portsc = self.ctrl.portsc(p);
        if !portsc.enabled() {
            println!("usbd: port {} reset and not enabled (PORTSC {:#010x})", p + 1, portsc.raw());
            return self.ports[usize::from(p)].enumerated(None);
        }
        let speed = portsc.speed();
        let (Some(packet), Some(_)) = (enumerate::initial_ep0_packet(speed), UsbSpeed::from_psiv(speed)) else {
            println!("usbd: port {} came up at speed {speed}, which this driver has no default for", p + 1);
            return self.ports[usize::from(p)].enumerated(None);
        };
        let (seq, act) = Enumeration::begin();
        let trb = self.ctrl.command(Trb { control: hc::TRB_ENABLE_SLOT, ..Trb::default() });
        let state = Enumerating { port: p, speed, packet, slot: 0, ep0: None, seq, act, device: None, interfaces: None };
        self.outstanding.submit(What::Enumerating(state), Await::Command { trb }, Stages::One, now + ANSWER_DEADLINE);
    }

    fn finished(&mut self, what: What, outcome: Outcome, now: Nanos) {
        match what {
            What::NoOp => {
                let done = outcome.succeeded();
                self.counts.noop = Some(done);
                if done {
                    println!("usbd: a No-Op command completed, announced by an interrupt");
                } else {
                    println!("usbd: the bring-up's No-Op command: {outcome:?}");
                }
            }
            What::SlotGone { port, slot, refused } => {
                if !outcome.succeeded() {
                    println!("usbd: Disable Slot {slot}: {outcome:?}");
                }
                self.ctrl.unbind_slot(slot);
                match refused {
                    true => self.ports[usize::from(port)].enumerated(None),
                    false => self.ports[usize::from(port)].torn_down(),
                }
            }
            What::Enumerating(state) => self.stepped(state, outcome, now),
        }
    }

    /// The outstanding act of an enumeration answered.
    fn stepped(&mut self, mut state: Enumerating, outcome: Outcome, now: Nanos) {
        let learnt = match state.act {
            Act::Command(Command::EnableSlot) => {
                let Outcome::Command { code: CC_SUCCESS, slot } = outcome else {
                    return self.refuse(state, format!("Enable Slot: {outcome:?}"), now);
                };
                state.slot = slot;
                let Some(block) = self.ctrl.device_block(slot) else {
                    return self.refuse(state, format!("slot {slot} is past the room this driver gave devices"), now);
                };
                state.ep0 = Some(self.ctrl.bind_slot(block, slot));
                Learnt::Nothing
            }
            Act::Command(_) if outcome.succeeded() => Learnt::Nothing,
            Act::Command(command) => return self.refuse(state, format!("{command:?}: {outcome:?}"), now),
            Act::Request(request) => match read_back(&self.ctrl, &mut state, request, outcome) {
                Ok(learnt) => learnt,
                Err(why) => return self.refuse(state, why, now),
            },
        };
        match state.seq.completed(learnt) {
            // Where a class driver would take over: the device is named here.
            Next::Act(_, Act::Request(Request::SetConfiguration)) | Next::Refuse | Next::Bind => self.named(state),
            Next::Act(seq, act) => {
                state.seq = seq;
                state.act = act;
                self.perform(state, now);
            }
        }
    }

    fn perform(&mut self, mut state: Enumerating, now: Nanos) {
        let deadline = now + ANSWER_DEADLINE;
        match state.act {
            Act::Command(command) => {
                self.ctrl.clear_input_ctx();
                let ep0_dw1 = (3u32 << 1) | (4u32 << 3) | (u32::from(state.packet) << 16);
                let kind = match command {
                    Command::AddressDevice => {
                        // Add the slot and EP0 (§4.3.3); the slot context
                        // names one entry, the speed and the root-hub port.
                        self.ctrl.input_ctx(0, 1, 0b11);
                        self.ctrl.input_ctx(1, 0, (u32::from(state.speed) << 20) | (1 << 27));
                        self.ctrl.input_ctx(1, 1, (u32::from(state.port) + 1) << 16);
                        let dequeue = state.ep0.as_ref().expect("EP0 is bound with the slot").dequeue();
                        self.ctrl.input_ctx(2, 1, ep0_dw1);
                        self.ctrl.input_ctx(2, 2, dequeue as u32);
                        self.ctrl.input_ctx(2, 3, (dequeue >> 32) as u32);
                        self.ctrl.input_ctx(2, 4, 8);
                        hc::TRB_ADDRESS_DEVICE
                    }
                    // EP0's packet size, and nothing else (§4.6.7).
                    Command::EvaluateEp0 => {
                        self.ctrl.input_ctx(0, 1, 1 << 1);
                        self.ctrl.input_ctx(2, 1, ep0_dw1);
                        hc::TRB_EVALUATE_CONTEXT
                    }
                    Command::EnableSlot | Command::ConfigureEndpoint | Command::ResetEp0 | Command::SetEp0Dequeue => {
                        unreachable!("an enumeration that stops at its configuration asks no {command:?}")
                    }
                };
                let (_, input) = self.ctrl.grant(hc::OFF_INPUT_CTX, hc::PAGE);
                let trb = self.ctrl.command(Trb { param: input, control: kind | (u32::from(state.slot) << 24), ..Trb::default() });
                self.outstanding.submit(What::Enumerating(state), Await::Command { trb }, Stages::One, deadline);
            }
            Act::Request(request) => {
                let (w_value, want): (u16, u16) = match request {
                    Request::DeviceDescriptor { want } => (0x0100, want),
                    Request::ConfigDescriptor => (0x0200, CONFIG_BYTES),
                    Request::SetConfiguration | Request::SetProtocol => {
                        unreachable!("an enumeration that stops at its configuration sends no {request:?}")
                    }
                };
                let (data, at) = self.ctrl.grant(hc::OFF_DATA, hc::PAGE);
                data.zero();
                let ring = state.ep0.as_mut().expect("EP0 is bound with the slot");
                // GET_DESCRIPTOR, device to host (USB 2.0 §9.4.3), in three stages.
                let setup = 0x80u64 | (6 << 8) | (u64::from(w_value) << 16) | (u64::from(want) << 48);
                ring.enqueue(Trb { param: setup, status: 8, control: hc::TRB_SETUP_STAGE | hc::TRB_IDT | (3 << 16) });
                let data_trb = ring.enqueue(Trb {
                    param: at,
                    status: u32::from(want),
                    control: hc::TRB_DATA_STAGE | (1 << 16) | hc::TRB_ISP | hc::TRB_IOC,
                });
                let status_trb = ring.enqueue(Trb { control: hc::TRB_STATUS_STAGE | hc::TRB_IOC, ..Trb::default() });
                let on = |trb| Await::Transfer { slot: state.slot, dci: 1, trb };
                let (first, then) = (on(data_trb), on(status_trb));
                self.ctrl.doorbell(state.slot, 1);
                self.outstanding.submit(What::Enumerating(state), first, Stages::DataThenStatus(then), deadline);
            }
        }
    }

    fn named(&mut self, state: Enumerating) {
        let (Some(device), Some(interfaces)) = (state.device, state.interfaces) else {
            unreachable!("a device is named only once its descriptors were read")
        };
        let speed = UsbSpeed::from_psiv(state.speed).expect("checked when the port came up");
        let named = Named { port: state.port, slot: state.slot, speed, device, interfaces };
        let class = named.class();
        println!(
            "usbd: port {}: {:04x}:{:04x} at {} speed, class {:02x}:{:02x}:{:02x}, on slot {}",
            state.port + 1,
            device.vendor,
            device.product,
            speed_name(speed),
            class.class,
            class.subclass,
            class.protocol,
            state.slot
        );
        self.ports[usize::from(state.port)].enumerated(NonZeroU8::new(state.slot));
        self.devices.push(named);
    }

    /// An enumeration that cannot go on: its slot, if it had one, goes back,
    /// and the port keeps its device without one until it is pulled.
    fn refuse(&mut self, state: Enumerating, why: String, now: Nanos) {
        println!("usbd: port {}: {why}; leaving it unnamed", state.port + 1);
        match state.slot {
            0 => self.ports[usize::from(state.port)].enumerated(None),
            slot => self.disable(state.port, slot, true, now),
        }
    }
}

/// What a completed GET_DESCRIPTOR left in the data page, as the order of
/// what is left depends on it.
fn read_back(ctrl: &Controller, state: &mut Enumerating, request: Request, outcome: Outcome) -> Result<Learnt, String> {
    let Outcome::Transfer { code: CC_SUCCESS, residue } = outcome else {
        return Err(format!("{request:?}: {outcome:?}"));
    };
    let want = match request {
        Request::DeviceDescriptor { want } => want,
        _ => CONFIG_BYTES,
    };
    // A residue past what was asked is the controller contradicting itself:
    // nothing moved.
    let moved = usize::from(want).saturating_sub(residue as usize);
    let (page, _) = ctrl.grant(hc::OFF_DATA, hc::PAGE);
    let bytes: Vec<u8> = (0..moved).map(|at| page.read::<u8>(at)).collect();
    let refused = |why: descriptor::Refused| format!("{request:?}: {why}");
    match request {
        Request::DeviceDescriptor { want: 8 } => {
            let stated = descriptor::ep0_packet(&bytes).map_err(refused)?;
            let packet = enumerate::ep0_packet_from_descriptor(state.speed, stated)
                .ok_or_else(|| format!("bMaxPacketSize0={stated} is no size a speed-{} device has", state.speed))?;
            if packet == state.packet {
                return Ok(Learnt::Nothing);
            }
            state.packet = packet;
            Ok(Learnt::Ep0PacketWrong)
        }
        Request::DeviceDescriptor { .. } => {
            state.device = Some(descriptor::device(&bytes).map_err(refused)?);
            Ok(Learnt::Nothing)
        }
        Request::ConfigDescriptor => {
            let interfaces = descriptor::interfaces(&bytes).map_err(refused)?;
            state.interfaces = Some(interfaces);
            // No function is learnt: which class a device's interfaces bind
            // is the class driver's stage to decide.
            Ok(Learnt::Nothing)
        }
        Request::SetConfiguration | Request::SetProtocol => unreachable!("never sent"),
    }
}

pub fn speed_name(speed: UsbSpeed) -> &'static str {
    match speed {
        UsbSpeed::Low => "low",
        UsbSpeed::Full => "full",
        UsbSpeed::High => "high",
        UsbSpeed::Super => "super",
        UsbSpeed::SuperPlusGen2x1 => "super-plus-gen2x1",
        UsbSpeed::SuperPlusGen1x2 => "super-plus-gen1x2",
        UsbSpeed::SuperPlusGen2x2 => "super-plus-gen2x2",
    }
}
