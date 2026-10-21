//! An xHC written from xHCI 1.2, for the driver's host tests: the register
//! file, the command ring, the event ring, transfer rings and the device
//! contexts, with each port's device a model of its class.
//!
//! **The specification is the oracle**, so what the controller does is what
//! the clause it cites says, and where the specification allows a latitude
//! the model takes the one least kind to a driver: a doorbell is acted on
//! only when the test lets time pass ([`Xhc::run`]), an event names only the
//! TRB the clause says it names, and a short packet ends a TD and raises no
//! event for its IOC TRB (§4.10.1.1).
//!
//! Memory is the grant and whatever a test lends at [`LENT_BASE`]; an access
//! outside both is a DMA fault, counted, and every test asserts none.

pub mod devices;
pub mod port;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use toyos_device_memory::{DmaBuffers, Registers};

use crate::exec::Nanos;
use crate::hc::Trb;
use crate::invariants;
use port::FakePort;

/// Where the grant starts in the controller's address space, and where a
/// test's lent regions do.
pub const GRANT_BASE: u64 = 0x8000_0000;
pub const LENT_BASE: u64 = 0x40_0000_0000;

const CAPLENGTH: usize = 0x20;
const RTS: usize = 0x1000;
const DB: usize = 0x2000;
const XECP: usize = 0x3000;
const BAR_BYTES: usize = 0x4000;
const SLOTS: usize = 32;

const USBCMD_RS: u32 = 1;
const USBCMD_HCRST: u32 = 2;
const USBSTS_HCH: u32 = 1;
const CRCR_CA: u32 = 1 << 2;

const CC_SUCCESS: u32 = 1;
const CC_STALL: u32 = 6;
const CC_SLOTS: u32 = 9;
const CC_SHORT: u32 = 13;
const CC_CONTEXT_STATE: u32 = 19;
const CC_RING_STOPPED: u32 = 24;
const CC_ABORTED: u32 = 25;
const CC_STOPPED: u32 = 26;

/// The memory the controller reaches.
pub struct Mem {
    regions: Vec<(u64, Vec<u8>)>,
    pub faults: u32,
}

impl Mem {
    fn at(&self, addr: u64, len: usize) -> Option<(usize, usize)> {
        self.regions.iter().enumerate().find_map(|(i, (base, bytes))| {
            let off = addr.checked_sub(*base)? as usize;
            (off + len <= bytes.len()).then_some((i, off))
        })
    }

    pub fn read(&mut self, addr: u64, out: &mut [u8]) -> bool {
        match self.at(addr, out.len()) {
            Some((i, off)) => {
                out.copy_from_slice(&self.regions[i].1[off..off + out.len()]);
                true
            }
            None => {
                self.faults += 1;
                false
            }
        }
    }

    pub fn write(&mut self, addr: u64, bytes: &[u8]) -> bool {
        match self.at(addr, bytes.len()) {
            Some((i, off)) => {
                self.regions[i].1[off..off + bytes.len()].copy_from_slice(bytes);
                true
            }
            None => {
                self.faults += 1;
                false
            }
        }
    }

    fn u32(&mut self, addr: u64) -> u32 {
        let mut b = [0; 4];
        self.read(addr, &mut b);
        u32::from_le_bytes(b)
    }

    fn u64(&mut self, addr: u64) -> u64 {
        let mut b = [0; 8];
        self.read(addr, &mut b);
        u64::from_le_bytes(b)
    }

    fn put32(&mut self, addr: u64, v: u32) {
        self.write(addr, &v.to_le_bytes());
    }

    fn trb(&mut self, addr: u64) -> Trb {
        Trb { param: self.u64(addr), status: self.u32(addr + 8), control: self.u32(addr + 12) }
    }

    /// A region a test lends, as a client lends one to a session.
    pub fn lend(&mut self, base: u64, bytes: usize) {
        self.regions.push((base, vec![0; bytes]));
    }
}

/// The grant, as the driver reaches it.
#[derive(Clone)]
pub struct Dma {
    mem: Rc<RefCell<Mem>>,
    bytes: usize,
    /// Every store and barrier in order: `(offset, is_control_word)` for a
    /// store, `None` for a `publish`.
    pub log: Rc<RefCell<Vec<Option<usize>>>>,
}

impl Dma {
    fn get<const N: usize>(&self, at: usize) -> [u8; N] {
        let mut b = [0; N];
        assert!(self.mem.borrow_mut().read(GRANT_BASE + at as u64, &mut b), "the driver read past its grant at {at:#x}");
        b
    }

    fn set(&self, at: usize, b: &[u8]) {
        assert!(self.mem.borrow_mut().write(GRANT_BASE + at as u64, b), "the driver wrote past its grant at {at:#x}");
        self.log.borrow_mut().push(Some(at));
    }
}

impl DmaBuffers for Dma {
    fn bytes(&self) -> usize {
        self.bytes
    }
    fn device_addr(&self, at: usize) -> u64 {
        GRANT_BASE + at as u64
    }
    fn read16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.get(at))
    }
    fn read32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.get(at))
    }
    fn read64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.get(at))
    }
    fn write16(&self, at: usize, v: u16) {
        self.set(at, &v.to_le_bytes());
    }
    fn write32(&self, at: usize, v: u32) {
        self.set(at, &v.to_le_bytes());
    }
    fn write64(&self, at: usize, v: u64) {
        self.set(at, &v.to_le_bytes());
    }
    fn publish(&self) {
        self.log.borrow_mut().push(None);
    }
    fn observe(&self) {}
}

/// What a device says to one request or transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Done: the bytes an IN moves, or none for an OUT taken.
    Data(Vec<u8>),
    Stall,
    /// A stall whose Transfer Event is lost: the endpoint reads Halted in its
    /// context, and no event says so.
    StallUnannounced,
    /// Nothing yet: the transfer stays the endpoint's until the device has
    /// something, as an interrupt IN waits for a report.
    Wait,
}

/// A device's functions, as the bus reaches them.
pub trait Function {
    /// A bus reset reached it: every state returns to Default (USB 2.0
    /// §9.1.1).
    fn reset(&mut self) {}
    /// A request on EP0: its Setup packet and an OUT data stage's bytes.
    fn control(&mut self, setup: [u8; 8], out: &[u8]) -> Reply;
    /// A transfer on endpoint `address`: an OUT's bytes, or an IN of at most
    /// `max` bytes.
    fn transfer(&mut self, address: u8, out: Option<&[u8]>, max: usize) -> Reply;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EpState {
    Running,
    Halted,
    Stopped,
}

struct Ep {
    dequeue: u64,
    cycle: bool,
    state: EpState,
    /// The doorbell rang and a TD may be waiting.
    rung: bool,
    /// A Setup packet whose Data and Status stages are still to come.
    setup: Option<[u8; 8]>,
}

struct Slot {
    port: usize,
    eps: BTreeMap<u8, Ep>,
}

/// How the controller answers commands.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Answers {
    Always,
    /// Never answers a command; an abort is answered as §4.6.1.2 says.
    Never,
    /// Never answers a command, nor its abort: a controller that is gone.
    NotEvenAnAbort,
    /// Answers every endpoint command with this completion code.
    Refuses(u32),
}

pub struct Xhc {
    pub mem: Rc<RefCell<Mem>>,
    pub ports: Vec<FakePort>,
    pub devices: Vec<Option<Box<dyn Function>>>,
    pub usb3: Vec<bool>,
    usbcmd: u32,
    crcr: u64,
    dcbaap: u64,
    erstba: u64,
    erdp: u64,
    /// The command ring's dequeue and cycle once running.
    cmd: Option<(u64, bool)>,
    cmd_rung: bool,
    /// A command consumed and not answered.
    current: Option<u64>,
    slots: BTreeMap<u8, Slot>,
    /// The event ring: segment base, size, enqueue index, cycle.
    event: Option<(u64, u32, u32, bool)>,
    pub now: Nanos,
    pub answers: Answers,
    legsup: u32,
    legctl: u32,
    /// Every PORTSC write the driver made that broke an invariant.
    pub violations: Vec<(usize, invariants::Violation)>,
    /// Commands the controller ran: when, and their TRB type.
    pub commands: Vec<(Nanos, u32)>,
    /// Interrupter 0's Interrupt Pending and Enable (§5.5.2.1), and Event
    /// Handler Busy (§5.5.2.3.3).
    pending: bool,
    enabled: bool,
    busy: bool,
    /// An interrupt was raised and no record of it has been taken.
    raised: bool,
}

/// The register window, as the driver reaches it.
#[derive(Clone)]
pub struct Regs(pub Rc<RefCell<Xhc>>);

impl Xhc {
    /// A controller of `ports` root-hub ports, the first `usb2` of them USB2
    /// and the rest USB3, each holding what `devices` says.
    pub fn build(ports: Vec<FakePort>, devices: Vec<Option<Box<dyn Function>>>, usb2: usize, grant: usize) -> (Regs, Dma, Rc<RefCell<Mem>>) {
        let mem = Rc::new(RefCell::new(Mem { regions: vec![(GRANT_BASE, vec![0; grant])], faults: 0 }));
        let usb3 = (0..ports.len()).map(|p| p >= usb2).collect();
        let xhc = Xhc {
            mem: mem.clone(),
            ports,
            devices,
            usb3,
            usbcmd: 0,
            crcr: 0,
            dcbaap: 0,
            erstba: 0,
            erdp: 0,
            cmd: None,
            cmd_rung: false,
            current: None,
            slots: BTreeMap::new(),
            event: None,
            now: 0,
            answers: Answers::Always,
            legsup: 1,
            legctl: 0,
            violations: Vec::new(),
            commands: Vec::new(),
            pending: false,
            enabled: false,
            busy: false,
            raised: false,
        };
        let dma = Dma { mem: mem.clone(), bytes: grant, log: Rc::default() };
        (Regs(Rc::new(RefCell::new(xhc))), dma, mem)
    }

    fn read(&self, at: usize) -> u32 {
        let usb2 = self.usb3.iter().filter(|u| !**u).count() as u32;
        let n = self.ports.len() as u32;
        match at {
            0x00 => 0x0120_0000 | CAPLENGTH as u32,
            0x04 => SLOTS as u32 | (1 << 8) | (n << 24),
            0x08 | 0x0C => 0,
            // AC64, 32-byte contexts, the extended capabilities at XECP.
            0x10 => 1 | ((XECP as u32 / 4) << 16),
            0x14 => DB as u32,
            0x18 => RTS as u32,
            a if a == CAPLENGTH => self.usbcmd,
            a if a == CAPLENGTH + 4 => u32::from(self.usbcmd & USBCMD_RS == 0) * USBSTS_HCH,
            a if a == CAPLENGTH + 8 => 1,
            a if a == CAPLENGTH + 0x18 => u32::from(self.cmd.is_some()) << 3,
            a if (CAPLENGTH + 0x400..CAPLENGTH + 0x400 + 0x10 * self.ports.len()).contains(&a) => {
                let p = (a - CAPLENGTH - 0x400) / 0x10;
                if (a - CAPLENGTH - 0x400).is_multiple_of(0x10) { self.ports[p].raw() } else { 0 }
            }
            // Legacy Support, then a Supported Protocol capability for each
            // protocol some port speaks (§7.2), each four dwords on.
            a if a == XECP => self.legsup & !(0xFF << 8) | (u32::from(usb2 > 0 || n > usb2) * 4) << 8,
            a if a == XECP + 4 => self.legctl,
            a if a == XECP + 0x10 && usb2 > 0 => 2 | (u32::from(n > usb2) * 4) << 8 | (2 << 24),
            a if a == XECP + 0x18 && usb2 > 0 => 1 | (usb2 << 8),
            a if a == XECP + 0x10 || a == XECP + 0x20 => 2 | (3 << 24),
            a if a == XECP + 0x18 || a == XECP + 0x28 => (usb2 + 1) | ((n - usb2) << 8),
            a if a == XECP + 0x14 || a == XECP + 0x24 => u32::from_le_bytes(*b"USB "),
            _ => 0,
        }
    }

    fn write(&mut self, at: usize, v: u32) {
        match at {
            a if a == CAPLENGTH => {
                if v & USBCMD_HCRST != 0 {
                    // §5.4.1: a reset puts every register at its default and
                    // the controller forgets every ring and slot.
                    self.usbcmd = 0;
                    self.cmd = None;
                    self.current = None;
                    self.slots.clear();
                    self.event = None;
                    return;
                }
                self.usbcmd = v;
            }
            a if a == CAPLENGTH + 0x18 => {
                if v & CRCR_CA != 0 {
                    return self.abort();
                }
                if self.cmd.is_none() {
                    self.crcr = (self.crcr & !0xFFFF_FFFF) | u64::from(v);
                }
            }
            a if a == CAPLENGTH + 0x1C => {
                if self.cmd.is_none() {
                    self.crcr = (self.crcr & 0xFFFF_FFFF) | u64::from(v) << 32;
                }
            }
            a if a == CAPLENGTH + 0x30 => self.dcbaap = (self.dcbaap & !0xFFFF_FFFF) | u64::from(v),
            a if a == CAPLENGTH + 0x34 => self.dcbaap = (self.dcbaap & 0xFFFF_FFFF) | u64::from(v) << 32,
            a if (CAPLENGTH + 0x400..CAPLENGTH + 0x400 + 0x10 * self.ports.len()).contains(&a) => {
                let p = (a - CAPLENGTH - 0x400) / 0x10;
                if (a - CAPLENGTH - 0x400).is_multiple_of(0x10) {
                    if let Some(why) = invariants::check_write(crate::portsc::Write::of(v), self.ports[p].read()) {
                        self.violations.push((p, why));
                    }
                    let now = self.now;
                    self.ports[p].write(v, now);
                    // PR or WPR: the device sees a bus reset.
                    if v & (1 << 4 | 1 << 31) != 0
                        && let Some(device) = self.devices.get_mut(p).and_then(|d| d.as_mut())
                    {
                        device.reset();
                    }
                }
            }
            // IMAN: Interrupt Pending is write-1-to-clear, Interrupt Enable
            // read-write.
            a if a == RTS + 0x20 => {
                if v & 1 != 0 {
                    self.pending = false;
                }
                self.enabled = v & 2 != 0;
                self.assert();
            }
            a if a == RTS + 0x30 => {
                self.erstba = (self.erstba & !0xFFFF_FFFF) | u64::from(v);
            }
            a if a == RTS + 0x34 => {
                self.erstba = (self.erstba & 0xFFFF_FFFF) | u64::from(v) << 32;
                let mut mem = self.mem.borrow_mut();
                let (base, size) = (mem.u64(self.erstba), mem.u32(self.erstba + 8));
                self.event = Some((base, size, 0, true));
            }
            a if a == RTS + 0x38 => {
                self.erdp = (self.erdp & !0xFFFF_FFFF) | u64::from(v & !0xF);
                // EHB is write-1-to-clear; cleared with an interrupt still
                // pending, that interrupt is raised now.
                if v & (1 << 3) != 0 {
                    self.busy = false;
                    self.assert();
                }
            }
            a if a == RTS + 0x3C => self.erdp = (self.erdp & 0xFFFF_FFFF) | u64::from(v) << 32,
            a if a == DB => {
                if self.cmd.is_none() {
                    self.cmd = Some((self.crcr & !0x3F, self.crcr & 1 == 1));
                }
                self.cmd_rung = true;
            }
            a if (DB + 4..DB + 4 * (SLOTS + 1)).contains(&a) => {
                let slot = ((a - DB) / 4) as u8;
                if let Some(ep) = self.slots.get_mut(&slot).and_then(|s| s.eps.get_mut(&(v as u8))) {
                    ep.rung = true;
                    if ep.state == EpState::Stopped {
                        ep.state = EpState::Running;
                    }
                }
            }
            a if a == XECP => self.legsup = v & !(1 << 16),
            a if a == XECP + 4 => self.legctl = v,
            _ => {}
        }
    }

    /// §4.6.1.2: the command running ends Command Aborted, the ring stops,
    /// and Command Ring Stopped says where.
    fn abort(&mut self) {
        if self.answers == Answers::NotEvenAnAbort {
            return;
        }
        if let Some(at) = self.current.take() {
            self.command_event(at, CC_ABORTED, 0);
        }
        let at = self.cmd.map_or(0, |(at, _)| at);
        self.command_event(at, CC_RING_STOPPED, 0);
        self.cmd_rung = false;
    }

    fn post(&mut self, trb: Trb) {
        let Some((base, size, enq, cycle)) = self.event else { return };
        let at = base + u64::from(enq) * 16;
        let mut mem = self.mem.borrow_mut();
        mem.write(at, &trb.param.to_le_bytes());
        mem.put32(at + 8, trb.status);
        mem.put32(at + 12, (trb.control & !1) | u32::from(cycle));
        drop(mem);
        let next = enq + 1;
        self.event = Some(if next == size { (base, size, 0, !cycle) } else { (base, size, next, cycle) });
        self.pending = true;
        self.assert();
    }

    /// §4.17.2, with no moderation: an interrupt is raised while one is
    /// pending, the interrupter is enabled and no handler is busy, and
    /// raising it makes the handler busy.
    fn assert(&mut self) {
        if self.pending && self.enabled && !self.busy {
            self.busy = true;
            self.raised = true;
        }
    }

    /// An interrupt raised for an event the handler has already taken: what a
    /// controller's assertion that lands inside a drain leaves behind.
    pub fn raise_for_nothing(&mut self) {
        self.pending = true;
        self.busy = true;
        self.raised = true;
    }

    /// Whether an interrupt was raised since the last call, taking it: the
    /// claim's interrupt record, as usbd reads it.
    pub fn take_interrupt(&mut self) -> bool {
        core::mem::take(&mut self.raised)
    }

    fn command_event(&mut self, at: u64, code: u32, slot: u8) {
        self.post(Trb { param: at, status: code << 24, control: (33 << 10) | u32::from(slot) << 24 });
    }

    fn transfer_event(&mut self, at: u64, code: u32, residue: u32, slot: u8, dci: u8) {
        self.post(Trb { param: at, status: code << 24 | residue, control: (32 << 10) | u32::from(dci) << 16 | u32::from(slot) << 24 });
    }

    /// The TRB at `at` of a ring whose consumer cycle is `cycle`, following
    /// Links (§4.11.5.1): `None` where the producer has not put one.
    fn next_trb(&self, at: &mut u64, cycle: &mut bool) -> Option<(u64, Trb)> {
        for _ in 0..2 {
            let trb = self.mem.borrow_mut().trb(*at);
            if (trb.control & 1 == 1) != *cycle {
                return None;
            }
            if (trb.control >> 10) & 0x3F == 6 {
                *at = trb.param & !0xF;
                if trb.control & 2 != 0 {
                    *cycle = !*cycle;
                }
                continue;
            }
            let here = *at;
            *at += 16;
            return Some((here, trb));
        }
        None
    }

    fn output_context(&self, slot: u8) -> u64 {
        let mut mem = self.mem.borrow_mut();
        mem.u64(self.dcbaap + u64::from(slot) * 8)
    }

    fn set_state(&mut self, slot: u8, dci: u8, state: EpState) {
        if let Some(ep) = self.slots.get_mut(&slot).and_then(|s| s.eps.get_mut(&dci)) {
            ep.state = state;
        }
        let out = self.output_context(slot);
        let code = match state {
            EpState::Running => 1,
            EpState::Halted => 2,
            EpState::Stopped => 3,
        };
        if out != 0 {
            self.mem.borrow_mut().put32(out + u64::from(dci) * 32, code);
        }
    }

    /// Let time pass to `now`: ports move, then the command ring and every
    /// rung endpoint run what they hold. Answers whether anything happened.
    pub fn run(&mut self, now: Nanos) -> bool {
        self.now = now;
        let mut moved = false;
        for p in 0..self.ports.len() {
            self.ports[p].tick(now);
            if self.ports[p].take_event() {
                self.post(Trb { param: ((p + 1) as u64) << 24, status: CC_SUCCESS << 24, control: 34 << 10 });
                moved = true;
            }
        }
        if self.usbcmd & USBCMD_RS == 0 {
            return moved;
        }
        while self.cmd_rung && self.current.is_none() {
            let Some((mut at, mut cycle)) = self.cmd else { break };
            let Some((here, trb)) = self.next_trb(&mut at, &mut cycle) else {
                self.cmd_rung = false;
                break;
            };
            self.cmd = Some((at, cycle));
            moved = true;
            self.commands.push((now, (trb.control >> 10) & 0x3F));
            match self.answers {
                Answers::Never | Answers::NotEvenAnAbort => self.current = Some(here),
                _ => self.command(here, trb),
            }
        }
        let rung: Vec<(u8, u8)> = self
            .slots
            .iter()
            .flat_map(|(s, slot)| slot.eps.iter().filter(|(_, e)| e.rung && e.state == EpState::Running).map(|(d, _)| (*s, *d)))
            .collect();
        for (slot, dci) in rung {
            while self.td(slot, dci) {
                moved = true;
            }
        }
        moved
    }

    fn command(&mut self, at: u64, trb: Trb) {
        let slot = (trb.control >> 24) as u8;
        let dci = ((trb.control >> 16) & 0x1F) as u8;
        let kind = (trb.control >> 10) & 0x3F;
        if let (Answers::Refuses(code), 14..=16) = (self.answers, kind) {
            return self.command_event(at, code, slot);
        }
        let code = match kind {
            23 => CC_SUCCESS,
            9 => match (1..=SLOTS as u8).find(|s| !self.slots.contains_key(s)) {
                Some(s) => {
                    self.slots.insert(s, Slot { port: usize::MAX, eps: BTreeMap::new() });
                    return self.command_event(at, CC_SUCCESS, s);
                }
                None => CC_SLOTS,
            },
            10 => {
                self.slots.remove(&slot);
                CC_SUCCESS
            }
            11 => {
                let input = trb.param & !0xF;
                let mut mem = self.mem.borrow_mut();
                let port = ((mem.u32(input + 32 + 4) >> 16) & 0xFF) as usize;
                let dequeue = mem.u64(input + 64 + 8);
                drop(mem);
                match self.slots.get_mut(&slot) {
                    Some(s) if port >= 1 && port <= self.ports.len() => {
                        s.port = port - 1;
                        s.eps.insert(1, Ep { dequeue: dequeue & !0xF, cycle: dequeue & 1 == 1, state: EpState::Running, rung: false, setup: None });
                        self.set_state(slot, 1, EpState::Running);
                        CC_SUCCESS
                    }
                    _ => 5,
                }
            }
            12 => {
                let input = trb.param & !0xF;
                let mut mem = self.mem.borrow_mut();
                let (drop_flags, add) = (mem.u32(input), mem.u32(input + 4));
                let mut added = Vec::new();
                for d in 2..32u8 {
                    if add & (1 << d) != 0 {
                        let dequeue = mem.u64(input + (u64::from(d) + 1) * 32 + 8);
                        added.push((d, dequeue));
                    }
                }
                drop(mem);
                let Some(s) = self.slots.get_mut(&slot) else { return self.command_event(at, 11, slot) };
                for d in 2..32u8 {
                    if drop_flags & (1 << d) != 0 {
                        s.eps.remove(&d);
                    }
                }
                for &(d, dequeue) in &added {
                    s.eps.insert(d, Ep { dequeue: dequeue & !0xF, cycle: dequeue & 1 == 1, state: EpState::Running, rung: false, setup: None });
                }
                for (d, _) in added {
                    self.set_state(slot, d, EpState::Running);
                }
                CC_SUCCESS
            }
            13 => CC_SUCCESS,
            14 => match self.state(slot, dci) {
                Some(EpState::Halted) => {
                    self.set_state(slot, dci, EpState::Stopped);
                    CC_SUCCESS
                }
                _ => CC_CONTEXT_STATE,
            },
            15 => match self.state(slot, dci) {
                Some(EpState::Running) => {
                    // §4.6.9: a TD the endpoint was on is answered Stopped.
                    let waiting = self.slots.get(&slot).and_then(|s| s.eps.get(&dci)).filter(|e| e.rung).map(|e| (e.dequeue, e.cycle));
                    if let Some((mut deq, mut cycle)) = waiting
                        && let Some((here, _)) = self.next_trb(&mut deq, &mut cycle) {
                            self.transfer_event(here, CC_STOPPED, 0, slot, dci);
                        }
                    self.set_state(slot, dci, EpState::Stopped);
                    if let Some(ep) = self.slots.get_mut(&slot).and_then(|s| s.eps.get_mut(&dci)) {
                        ep.rung = false;
                        ep.setup = None;
                    }
                    CC_SUCCESS
                }
                _ => CC_CONTEXT_STATE,
            },
            16 => match self.slots.get_mut(&slot).and_then(|s| s.eps.get_mut(&dci)) {
                Some(ep) if ep.state == EpState::Stopped => {
                    ep.dequeue = trb.param & !0xF;
                    ep.cycle = trb.param & 1 == 1;
                    ep.setup = None;
                    CC_SUCCESS
                }
                _ => CC_CONTEXT_STATE,
            },
            _ => 5,
        };
        self.command_event(at, code, slot);
    }

    fn state(&self, slot: u8, dci: u8) -> Option<EpState> {
        self.slots.get(&slot)?.eps.get(&dci).map(|e| e.state)
    }

    /// Run the next TD of (`slot`, `dci`), if the producer has put one and
    /// the device answers it; `false` when it is not run now.
    fn td(&mut self, slot: u8, dci: u8) -> bool {
        let (port, mut at, mut cycle) = {
            let s = &self.slots[&slot];
            let ep = &s.eps[&dci];
            (s.port, ep.dequeue, ep.cycle)
        };
        // The TD: TRBs up to one with no Chain bit, or a control transfer's
        // stages up to its Status.
        let mut trbs = Vec::new();
        loop {
            let Some((here, trb)) = self.next_trb(&mut at, &mut cycle) else {
                if trbs.is_empty() {
                    self.slots.get_mut(&slot).unwrap().eps.get_mut(&dci).unwrap().rung = false;
                }
                return false;
            };
            let kind = (trb.control >> 10) & 0x3F;
            trbs.push((here, trb));
            let ends = match kind {
                2 | 3 => false,
                4 => true,
                _ => trb.control & (1 << 4) == 0,
            };
            if ends {
                break;
            }
        }
        let Some(device) = self.devices.get_mut(port).and_then(|d| d.as_mut()) else { return false };
        if !self.ports[port].present() {
            return false;
        }
        let control = dci == 1;
        let address = (dci / 2) | if dci % 2 == 1 { 0x80 } else { 0 };
        let data: Vec<(u64, Trb)> = trbs.iter().copied().filter(|(_, t)| matches!((t.control >> 10) & 0x3F, 1 | 3)).collect();
        let total: usize = data.iter().map(|(_, t)| (t.status & 0x1_FFFF) as usize).sum();
        let inbound = if control { data.first().is_some_and(|(_, t)| t.control & (1 << 16) != 0) } else { dci % 2 == 1 };
        let mut out = Vec::new();
        if !inbound {
            let mut mem = self.mem.borrow_mut();
            for (_, t) in &data {
                let mut b = vec![0; (t.status & 0x1_FFFF) as usize];
                mem.read(t.param, &mut b);
                out.extend(b);
            }
        }
        let reply = if control {
            let setup = trbs.first().map(|(_, t)| t.param.to_le_bytes()).expect("a Setup stage");
            device.control(setup, &out)
        } else {
            device.transfer(address, (!inbound).then_some(&out[..]), total)
        };
        let last = trbs.last().copied().expect("a TD");
        match reply {
            Reply::Wait => return false,
            Reply::Stall => {
                let on = data.first().copied().unwrap_or(last).0;
                self.transfer_event(on, CC_STALL, 0, slot, dci);
                self.set_state(slot, dci, EpState::Halted);
            }
            Reply::StallUnannounced => self.set_state(slot, dci, EpState::Halted),
            Reply::Data(bytes) => {
                let bytes = if inbound { bytes } else { Vec::new() };
                let mut moved = 0usize;
                let mut short = None;
                for &(here, t) in &data {
                    let len = (t.status & 0x1_FFFF) as usize;
                    let take = len.min(bytes.len().saturating_sub(moved));
                    if inbound {
                        self.mem.borrow_mut().write(t.param, &bytes[moved..moved + take]);
                    }
                    moved += take;
                    if inbound && take < len {
                        short = Some((here, (len - take) as u32, t.control & (1 << 2) != 0));
                        break;
                    }
                }
                match short {
                    // §4.10.1.1: the TD ends at a short packet, with an event
                    // only where ISP asks for one — but a control transfer's
                    // Status stage still runs.
                    Some((here, residue, isp)) => {
                        if isp {
                            self.transfer_event(here, CC_SHORT, residue, slot, dci);
                        }
                        if control {
                            self.transfer_event(last.0, CC_SUCCESS, 0, slot, dci);
                        }
                    }
                    None => self.transfer_event(last.0, CC_SUCCESS, 0, slot, dci),
                }
            }
        }
        let ep = self.slots.get_mut(&slot).unwrap().eps.get_mut(&dci).unwrap();
        ep.dequeue = at;
        ep.cycle = cycle;
        true
    }

    /// When a port next moves on its own clock.
    pub fn due(&self) -> Option<Nanos> {
        self.ports.iter().filter_map(FakePort::due).min()
    }
}

impl Registers for Regs {
    fn bytes(&self) -> usize {
        BAR_BYTES
    }
    fn read8(&self, at: usize) -> u8 {
        (self.0.borrow().read(at & !3) >> ((at & 3) * 8)) as u8
    }
    fn read16(&self, at: usize) -> u16 {
        (self.0.borrow().read(at & !3) >> ((at & 3) * 8)) as u16
    }
    fn read32(&self, at: usize) -> u32 {
        self.0.borrow().read(at)
    }
    fn write8(&self, _: usize, _: u8) {
        panic!("no xHCI register is written a byte at a time");
    }
    fn write16(&self, _: usize, _: u16) {
        panic!("no xHCI register is written sixteen bits at a time");
    }
    fn write32(&self, at: usize, v: u32) {
        self.0.borrow_mut().write(at, v);
    }
}
