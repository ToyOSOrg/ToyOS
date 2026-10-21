//! The controller: its register file, the one DMA grant every ring and
//! context lives in, and every operation it owes an answer for.
//!
//! What is here is xHCI 1.2's register file and memory layout, written once:
//! the §4.22.1 handoff, the §4.2 bring-up, the command ring (§4.6), the event
//! ring (§4.9.4), the transfer rings and the doorbells. Nothing here decides
//! anything about a port or a device.
//!
//! **An operation is matched by the TRB its event names** (§6.4.2.1, §6.4.2.2):
//! the only name an event carries that belongs to one operation and no other.
//! [`Op`] is one the controller holds. **A deadline is never a cancellation**:
//! dropping an `Op` the controller has not answered is a panic by name, and
//! what takes one back is the command the specification gives for it
//! ([`Hc::command`]'s Command Abort, §4.6.1.2; a transfer's Stop Endpoint and
//! Set TR Dequeue Pointer, §4.6.9 and §4.6.10, in the caller).
//!
//! **The barriers are the grant's** (`toyos-device-memory`): a TRB's body is
//! stored, then `publish`, then its control word with the cycle bit, then
//! `publish` again before the doorbell; an event's cycle bit is loaded, then
//! `observe`, then the rest of it.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::num::NonZeroU16;

use toyos_device_memory::{DmaBuffers, Registers};

use crate::exec::{self, Dead, Host, Nanos, Wake};
use crate::portsc::{Portsc, Write};
use crate::protocol::{Protocols, SupportedProtocol};
use crate::xecp::{self, Handoff};

const CAP_CAPLENGTH: usize = 0x00;
const CAP_HCSPARAMS1: usize = 0x04;
const CAP_HCSPARAMS2: usize = 0x08;
const CAP_HCCPARAMS1: usize = 0x10;
const CAP_DBOFF: usize = 0x14;
const CAP_RTSOFF: usize = 0x18;
/// HCCPARAMS1's 64-bit Addressing Capability and Context Size (§5.3.6).
const HCCPARAMS1_AC64: u32 = 1 << 0;
const HCCPARAMS1_CSZ: u32 = 1 << 2;

const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_PAGESIZE: usize = 0x08;
const OP_CRCR: usize = 0x18;
const OP_DCBAAP: usize = 0x30;
const OP_CONFIG: usize = 0x38;
const OP_PORT_BASE: usize = 0x400;
const PORT_REG_SIZE: usize = 0x10;
/// PORTSC's Port Power (§5.4.8).
const PORTSC_PP: u32 = 1 << 9;

/// USBCMD's Run/Stop, Host Controller Reset and Interrupter Enable; USBSTS's
/// HCHalted and Controller Not Ready (§5.4.1, §5.4.2).
const USBCMD_RS: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBCMD_INTE: u32 = 1 << 2;
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_CNR: u32 = 1 << 11;
/// CRCR's Command Abort (§5.4.5).
const CRCR_CA: u32 = 1 << 2;

/// Interrupter 0's registers in the runtime space (§5.5.2).
const IR0_IMAN: usize = 0x20;
const IR0_IMOD: usize = 0x24;
const IR0_ERSTSZ: usize = 0x28;
const IR0_ERSTBA: usize = 0x30;
const IR0_ERDP: usize = 0x38;
/// IMAN's Interrupt Pending (write 1 to clear) and Interrupt Enable.
const IMAN_IP_IE: u32 = 0b11;
/// ERDP's Event Handler Busy, written 1 to clear (§5.5.2.3.3).
const ERDP_EHB: u64 = 1 << 3;

const TRB_CYCLE: u32 = 1;
const fn trb_type(t: u32) -> u32 {
    t << 10
}
pub const TRB_NORMAL: u32 = trb_type(1);
pub const TRB_SETUP: u32 = trb_type(2);
pub const TRB_DATA: u32 = trb_type(3);
pub const TRB_STATUS: u32 = trb_type(4);
const TRB_LINK: u32 = trb_type(6);
pub const TRB_ENABLE_SLOT: u32 = trb_type(9);
pub const TRB_DISABLE_SLOT: u32 = trb_type(10);
pub const TRB_ADDRESS_DEVICE: u32 = trb_type(11);
pub const TRB_CONFIGURE_ENDPOINT: u32 = trb_type(12);
pub const TRB_EVALUATE_CONTEXT: u32 = trb_type(13);
pub const TRB_RESET_ENDPOINT: u32 = trb_type(14);
pub const TRB_STOP_ENDPOINT: u32 = trb_type(15);
pub const TRB_SET_TR_DEQUEUE: u32 = trb_type(16);
pub const TRB_NO_OP: u32 = trb_type(23);
/// Link's Toggle Cycle; Interrupt-on Short Packet, Chain, Interrupt On
/// Completion and Immediate Data (§6.4.1).
const LINK_TC: u32 = 1 << 1;
pub const TRB_ISP: u32 = 1 << 2;
pub const TRB_CHAIN: u32 = 1 << 4;
pub const TRB_IOC: u32 = 1 << 5;
pub const TRB_IDT: u32 = 1 << 6;
/// A Data or Status stage's direction, device to host (§6.4.1.2).
pub const TRB_DIR_IN: u32 = 1 << 16;

const EVENT_TRANSFER: u32 = 32;
const EVENT_COMMAND_COMPLETION: u32 = 33;
const EVENT_PORT_STATUS_CHANGE: u32 = 34;

/// Completion codes (§6.4.5) this driver acts on.
pub const CC_SUCCESS: u32 = 1;
pub const CC_STALL: u32 = 6;
pub const CC_SHORT_PACKET: u32 = 13;
pub const CC_COMMAND_RING_STOPPED: u32 = 24;
pub const CC_COMMAND_ABORTED: u32 = 25;
pub const CC_STOPPED: u32 = 26;
pub const CC_STOPPED_LENGTH_INVALID: u32 = 27;

/// The granularity everything in the grant is placed at; the controller's own
/// PAGESIZE must include it.
pub const PAGE: usize = 0x1000;
/// TRBs per ring: one page, its last a Link back to the first.
const RING_TRBS: usize = PAGE / 16;
/// The most bytes one TRB moves, and the boundary none may cross (§6.4.1).
pub const TRB_MOST: u64 = 0x1_0000;

const OFF_DCBAA: usize = 0;
const OFF_CMD_RING: usize = PAGE;
const OFF_ERST: usize = 2 * PAGE;
const OFF_EVENT_RING: usize = 3 * PAGE;
const OFF_SCRATCH_ARRAY: usize = 4 * PAGE;
const OFF_SCRATCH: usize = 5 * PAGE;

/// The devices this controller is given room for, whatever more slots it has.
pub const MAX_DEVICES: usize = 32;
/// Pages each device may hold at once: its contexts, its rings and its data.
pub const DEVICE_PAGES: usize = 12;
/// The scratchpad buffers one page of array can name.
const MAX_SCRATCH: usize = PAGE / 8;

/// How long a register the controller sets in microseconds may take, read
/// without an interrupt, since the controller raises none for these bits
/// (§5.4.1, §5.4.2). Policy.
const REGISTER_NS: Nanos = 2_000_000_000;
/// How long firmware is given to let go of the controller: the OS sets HC OS
/// Owned and waits for HC BIOS Owned to clear (§4.22.1; USBLEGSUP, §7.1.1),
/// a wait §4.22.1 says "should not exceed '1' second".
const HANDOFF_NS: Nanos = 1_000_000_000;
/// How long a command may go unanswered before it is aborted, and an abort
/// before the controller is called dead: "an upper limit of 5 seconds for any
/// command to be processed" (USB 3.2 §9.2.6.1) bounds what a device makes a
/// command wait on.
pub const COMMAND_NS: Nanos = 5_000_000_000;

/// One TRB, as the rings carry it (§4.11).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

/// One TD of Normal TRBs moving `pieces` on an endpoint of `max_packet`:
/// every TRB but the last chains, the last interrupts, each interrupts on a
/// short packet, and each states the packets left after it (TD Size, §4.11.2.4).
pub fn normal_td(pieces: &[(u64, u32)], max_packet: NonZeroU16) -> Vec<Trb> {
    let mps = u64::from(max_packet.get());
    let packets = pieces.iter().map(|&(_, len)| u64::from(len)).sum::<u64>().div_ceil(mps);
    let mut moved = 0u64;
    pieces
        .iter()
        .enumerate()
        .map(|(i, &(at, len))| {
            moved += u64::from(len);
            let last = i + 1 == pieces.len();
            let left = if last { 0 } else { (packets - moved / mps).min(31) as u32 };
            let flags = if last { TRB_IOC } else { TRB_CHAIN };
            Trb { param: at, status: left << 17 | len, control: TRB_NORMAL | TRB_ISP | flags }
        })
        .collect()
}

/// Why the controller was not brought up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refusal {
    /// A register the controller published puts what it names outside the
    /// window the claim maps.
    Window(&'static str, usize),
    /// A register bit that did not move within its bound.
    Stuck(&'static str),
    PageSize(u32),
    Scratchpad(usize),
    /// The grant is smaller than [`Reset::grant_bytes`].
    Grant(usize),
    /// Firmware still held HC BIOS Owned after [`HANDOFF_NS`]. §4.22.1 has the
    /// OS wait for it to clear "before attempting to use the xHC": firmware
    /// may still be driving the controller, and two agents would each believe
    /// they own it. The controller is left as found.
    FirmwareKeepsIt(u32),
}

impl core::fmt::Display for Refusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Window(what, at) => write!(f, "{what} at {at:#x} is outside the register window"),
            Self::Stuck(what) => write!(f, "{what} within {} ms", REGISTER_NS / 1_000_000),
            Self::PageSize(p) => write!(f, "PAGESIZE={p:#x} does not include 4 KiB, where every ring is placed"),
            Self::Scratchpad(n) => write!(f, "it asks for {n} scratchpad buffers, past the {MAX_SCRATCH} one page names"),
            Self::Grant(bytes) => write!(f, "a grant of {bytes} bytes is short of the layout"),
            Self::FirmwareKeepsIt(legsup) => write!(
                f,
                "firmware still owns it after {} ms (USBLEGSUP {legsup:#010x}, HC BIOS Owned set), and \
                 xHCI 1.2 §4.22.1 has the OS wait for that to clear before it uses the controller",
                HANDOFF_NS / 1_000_000
            ),
        }
    }
}

/// A ring this driver produces into: the command ring or a transfer ring.
#[derive(Clone, Copy, Debug)]
pub struct Ring {
    /// The ring's page in the grant.
    pub at: usize,
    tail: usize,
    cycle: bool,
}

impl Ring {
    fn new<D: DmaBuffers>(dma: &D, at: usize) -> Self {
        for off in (0..PAGE).step_by(8) {
            dma.write64(at + off, 0);
        }
        let ring = Self { at, tail: 0, cycle: true };
        let base = dma.device_addr(at);
        put(dma, at + (RING_TRBS - 1) * 16, Trb { param: base, status: 0, control: TRB_LINK | LINK_TC });
        ring
    }

    /// Where the controller resumes, with the cycle state it must expect in
    /// bit 0: what a context or a Set TR Dequeue names.
    pub fn dequeue<D: DmaBuffers>(&self, dma: &D) -> u64 {
        dma.device_addr(self.at + self.tail * 16) | u64::from(self.cycle)
    }

    /// Put `trbs`, one TD, on the ring with this ring's cycle bit, and answer
    /// where each landed: the names their events will carry.
    ///
    /// **A TD never crosses the ring's Link**: one that would not fit before
    /// it starts on the ring's first TRB, behind a Link TD put where it would
    /// have begun (§4.11.7), so no TD Fragment spans a segment (§4.11.5.1,
    /// §4.11.7.1). **Its first TRB is written last** (§4.11.7.1), so the
    /// controller finds none of the TD before all of it.
    fn enqueue<D: DmaBuffers>(&mut self, dma: &D, trbs: &[Trb]) -> Vec<u64> {
        assert!(!trbs.is_empty() && trbs.len() <= Self::room(), "a TD of {} TRBs on a ring that holds {}", trbs.len(), Self::room());
        if self.tail + trbs.len() > RING_TRBS - 1 {
            self.link(dma);
        }
        let (first, cycle) = (self.at + self.tail * 16, self.cycle);
        let owned = |trb: &Trb, cycle: bool| Trb { control: (trb.control & !TRB_CYCLE) | u32::from(cycle), ..*trb };
        let mut at = Vec::with_capacity(trbs.len());
        for (i, trb) in trbs.iter().enumerate() {
            at.push(dma.device_addr(self.at + self.tail * 16));
            // The first is put as the controller's not yet, and given to it below.
            put(dma, self.at + self.tail * 16, owned(trb, if i == 0 { !cycle } else { cycle }));
            self.tail += 1;
        }
        if self.tail == RING_TRBS - 1 {
            self.link(dma);
        }
        dma.publish();
        dma.write32(first + 12, owned(&trbs[0], cycle).control);
        at
    }

    /// A Link TD at the tail, back to the ring's first TRB: where the
    /// controller next turns, its cycle state toggled.
    ///
    /// **Every slot of a lap carries that lap's cycle when it closes**, so
    /// none matches the cycle the controller expects on the next (§4.9.2): a
    /// Link put early also marks each slot it skips, the ring's own Link
    /// included, with the closing lap's cycle, or a slot two laps old would
    /// be the controller's once the next lap's tail rests on it.
    fn link<D: DmaBuffers>(&mut self, dma: &D) {
        for skipped in self.tail + 1..RING_TRBS {
            dma.write32(self.at + skipped * 16 + 12, u32::from(self.cycle));
        }
        let control = TRB_LINK | LINK_TC | u32::from(self.cycle);
        put(dma, self.at + self.tail * 16, Trb { param: dma.device_addr(self.at), status: 0, control });
        self.tail = 0;
        self.cycle = !self.cycle;
    }

    /// The most TRBs one TD may have. A ring holds one operation at a time
    /// here, so nothing the controller has not consumed is overrun but an
    /// early Link: put at a tail `t` because `t + n` would pass the ring's
    /// Link, it must not be overwritten by the TD that follows it, and this
    /// bound makes `t + n > RING_TRBS - 1` imply `n <= t`.
    pub const fn room() -> usize {
        (RING_TRBS - 1) / 2
    }
}

/// The body first and the control word last, behind a release: the control
/// word carries the cycle bit, and the controller owns the TRB the moment
/// that bit matches its own (§4.9.2).
fn put<D: DmaBuffers>(dma: &D, at: usize, trb: Trb) {
    dma.write64(at, trb.param);
    dma.write32(at + 8, trb.status);
    dma.publish();
    dma.write32(at + 12, trb.control);
}

/// How an operation ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Done {
    /// The controller's completion code, the bytes a transfer moved, and for
    /// a command its parameter (Enable Slot's slot).
    Event { code: u32, moved: u32, slot: u8 },
    /// Its slot was disabled under it: the device left, and nothing will
    /// answer it.
    Gone,
}

impl Done {
    pub fn code(self) -> Option<u32> {
        match self {
            Self::Event { code, .. } => Some(code),
            Self::Gone => None,
        }
    }

    pub fn succeeded(self) -> bool {
        matches!(self, Self::Event { code: CC_SUCCESS | CC_SHORT_PACKET, .. })
    }
}

/// An operation the controller holds.
struct Held {
    slot: u8,
    /// 0 for a command.
    dci: u8,
    /// Each TRB's address and the bytes it was asked to move.
    trbs: Vec<(u64, u32)>,
    /// A control transfer's data stage that came short still runs its status
    /// stage (§4.10.1.1), which is what ends it.
    control: bool,
    moved: Option<u32>,
    done: Option<Done>,
}

/// What the controller counted, which `inspect` reads.
#[derive(Clone, Copy, Default, Debug)]
pub struct Counts {
    pub events: u64,
    /// Operations whose deadline passed with the controller still holding
    /// them.
    pub silent: u64,
    /// Of them, ones whose answer was already on the event ring: an event
    /// no interrupt announced.
    pub unannounced: u64,
    /// Events naming a TRB no operation holds.
    pub stray: u64,
    pub other: u64,
    pub aborted: u64,
}

/// A controller reset and not yet given its grant: firmware has let go of it,
/// HCRST has run, and nothing of the machine is reachable to it.
pub struct Reset<R> {
    regs: R,
    /// The device addresses the controller issues: 64 bits, or 32 where
    /// HCCPARAMS1.AC64 is clear (§5.3.6).
    pub address_bits: u8,
    op: usize,
    rt: usize,
    db: usize,
    max_ports: u8,
    slots: usize,
    context_bytes: usize,
    scratch: usize,
    pub protocols: Protocols,
}

/// How long the stop before a machine reset may take, all of it: the
/// devices' reset, the controller's halt and its own reset. A policy, below
/// the supervisor's wait for every disk driver (`toyos_quiesce::DRIVERS_MS`).
pub const STOP_NS: Nanos = 2_000_000_000;

/// How often a register the stop waits on is read again: none of them raises
/// an event the stop could wait for.
const STOP_POLL_NS: Nanos = 1_000_000;

/// What [`Hc::stop`] did, each step as read back from the controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stopped {
    /// Ports with a device connected, and those whose reset finished.
    pub connected: u32,
    pub reset: u32,
    pub halted: bool,
    /// HCRST cleared and the controller ready again.
    pub cleared: bool,
    pub ports: u32,
    /// Ports reading Port Power off afterwards: a controller without Port
    /// Power Control keeps them powered (§5.4.8).
    pub unpowered: u32,
}

impl Stopped {
    /// Every connected device was reset, and the controller halted and reset.
    pub fn whole(&self) -> bool {
        self.reset == self.connected && self.halted && self.cleared
    }
}

impl core::fmt::Display for Stopped {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}/{} connected port(s) reset, halted={}, reset={}, {}/{} port(s) unpowered",
            self.reset, self.connected, self.halted, self.cleared, self.unpowered, self.ports
        )
    }
}

/// The controller, running.
pub struct Hc<R, D> {
    regs: R,
    pub dma: D,
    op: usize,
    rt: usize,
    db: usize,
    pub max_ports: u8,
    pub context_bytes: usize,
    pub protocols: Protocols,
    cmd: RefCell<Ring>,
    /// One command at a time, so an abort concerns one.
    commanding: Cell<bool>,
    /// The Command Ring Stopped event an abort is answered by.
    stopped: Cell<bool>,
    event: Cell<(usize, bool)>,
    held: RefCell<BTreeMap<u64, Held>>,
    /// Which operation each TRB the controller holds belongs to.
    named: RefCell<BTreeMap<u64, u64>>,
    next: Cell<u64>,
    /// The driver is ending with operations held: the controller is gone, or
    /// a test is over.
    pub ending: Cell<bool>,
    changed: RefCell<Vec<bool>>,
    free: RefCell<Vec<usize>>,
    pub counts: Cell<Counts>,
}

impl<R: Registers> Reset<R> {
    /// The §4.22.1 handoff and a reset: everything before the grant, so a
    /// controller refused here never masters the bus.
    pub fn new<C: Host>(regs: R, host: &C) -> Result<Self, Refusal> {
        let cap_length = usize::from(regs.read8(CAP_CAPLENGTH));
        let hcsparams1 = regs.read32(CAP_HCSPARAMS1);
        let hcsparams2 = regs.read32(CAP_HCSPARAMS2);
        let hccparams1 = regs.read32(CAP_HCCPARAMS1);
        let db = (regs.read32(CAP_DBOFF) & !0x3) as usize;
        let rt = (regs.read32(CAP_RTSOFF) & !0x1f) as usize;
        let max_slots = (hcsparams1 & 0xff) as usize;
        let max_ports = (hcsparams1 >> 24) as u8;
        let context_bytes = if hccparams1 & HCCPARAMS1_CSZ != 0 { 64 } else { 32 };
        let scratch = ((((hcsparams2 >> 21) & 0x1f) << 5) | ((hcsparams2 >> 27) & 0x1f)) as usize;
        let op = cap_length;
        for (what, end) in [
            ("the last port register", op + OP_PORT_BASE + usize::from(max_ports) * PORT_REG_SIZE),
            ("interrupter 0", rt + IR0_ERDP + 8),
            ("the last doorbell", db + 4 * (max_slots + 1)),
        ] {
            if end > regs.bytes() {
                return Err(Refusal::Window(what, end));
            }
        }
        let pagesize = regs.read32(op + OP_PAGESIZE) & 0xffff;
        if pagesize & 1 == 0 {
            return Err(Refusal::PageSize(pagesize));
        }
        if scratch > MAX_SCRATCH {
            return Err(Refusal::Scratchpad(scratch));
        }
        let read = |at: u64| -> Option<u32> {
            let at = usize::try_from(at).ok()?;
            (at % 4 == 0 && at + 4 <= regs.bytes()).then(|| regs.read32(at))
        };
        take_from_firmware(&regs, &read, hccparams1 >> 16, host)?;
        let protocols = read_protocols(&read, hccparams1 >> 16, max_ports, host);

        let usbcmd = regs.read32(op + OP_USBCMD);
        if usbcmd & USBCMD_RS != 0 {
            regs.write32(op + OP_USBCMD, usbcmd & !USBCMD_RS);
        }
        settle(host, "it never halted", || regs.read32(op + OP_USBSTS) & USBSTS_HCH != 0)?;
        regs.write32(op + OP_USBCMD, USBCMD_HCRST);
        settle(host, "it held HCRST", || regs.read32(op + OP_USBCMD) & USBCMD_HCRST == 0)?;
        settle(host, "it stayed Controller Not Ready", || regs.read32(op + OP_USBSTS) & USBSTS_CNR == 0)?;
        let slots = max_slots.min(MAX_DEVICES);
        let address_bits = if hccparams1 & HCCPARAMS1_AC64 != 0 { 64 } else { 32 };
        Ok(Self { regs, address_bits, op, rt, db, max_ports, slots, context_bytes, scratch, protocols })
    }

    /// The grant the layout needs: the fixed head, the scratchpad, and every
    /// device's pages.
    pub fn grant_bytes(&self) -> usize {
        OFF_SCRATCH + self.scratch * PAGE + self.slots * DEVICE_PAGES * PAGE
    }

    /// Give the controller its grant, its rings and interrupter 0, and run it
    /// (§4.2).
    pub fn start<D: DmaBuffers, C: Host>(self, dma: D, host: &C) -> Result<Hc<R, D>, Refusal> {
        let bytes = self.grant_bytes();
        let Self { regs, op, rt, db, max_ports, slots, context_bytes, scratch, protocols, .. } = self;
        if dma.bytes() < bytes {
            return Err(Refusal::Grant(dma.bytes()));
        }
        for off in (0..bytes).step_by(8) {
            dma.write64(off, 0);
        }
        regs.write32(op + OP_CONFIG, slots as u32);
        for i in 0..scratch {
            dma.write64(OFF_SCRATCH_ARRAY + i * 8, dma.device_addr(OFF_SCRATCH + i * PAGE));
        }
        // DCBAA[0] names the scratchpad array (§6.1).
        dma.write64(OFF_DCBAA, if scratch > 0 { dma.device_addr(OFF_SCRATCH_ARRAY) } else { 0 });
        let cmd = Ring::new(&dma, OFF_CMD_RING);
        // One segment, the whole event ring (§6.5), named before ERSTBA is.
        dma.write64(OFF_ERST, dma.device_addr(OFF_EVENT_RING));
        dma.write32(OFF_ERST + 8, RING_TRBS as u32);
        dma.publish();
        let write64 = |at: usize, v: u64| {
            regs.write32(at, v as u32);
            regs.write32(at + 4, (v >> 32) as u32);
        };
        write64(op + OP_DCBAAP, dma.device_addr(OFF_DCBAA));
        write64(op + OP_CRCR, cmd.dequeue(&dma));
        regs.write32(rt + IR0_ERSTSZ, 1);
        write64(rt + IR0_ERDP, dma.device_addr(OFF_EVENT_RING));
        write64(rt + IR0_ERSTBA, dma.device_addr(OFF_ERST));
        // No moderation: every event is an interrupt.
        regs.write32(rt + IR0_IMOD, 0);
        regs.write32(rt + IR0_IMAN, IMAN_IP_IE);
        regs.write32(op + OP_USBCMD, USBCMD_RS | USBCMD_INTE);
        settle(host, "it stayed halted after Run", || regs.read32(op + OP_USBSTS) & USBSTS_HCH == 0)?;
        let first = OFF_SCRATCH + scratch * PAGE;
        let free = (0..slots * DEVICE_PAGES).rev().map(|i| first + i * PAGE).collect();
        let hc = Hc {
            regs,
            dma,
            op,
            rt,
            db,
            max_ports,
            context_bytes,
            protocols,
            cmd: RefCell::new(cmd),
            commanding: Cell::new(false),
            stopped: Cell::new(false),
            event: Cell::new((0, true)),
            held: RefCell::new(BTreeMap::new()),
            named: RefCell::new(BTreeMap::new()),
            next: Cell::new(0),
            ending: Cell::new(false),
            changed: RefCell::new(alloc::vec![false; usize::from(max_ports)]),
            free: RefCell::new(free),
            counts: Cell::new(Counts::default()),
        };
        // Port Power Control leaves a port unpowered after HCRST, and an
        // unpowered port reports nothing; without PPC the write does nothing.
        for p in 0..max_ports {
            let portsc = hc.portsc(p);
            if portsc.raw() & PORTSC_PP == 0 {
                hc.write_portsc(p, portsc.neutral().powered());
            }
        }
        Ok(hc)
    }
}

impl<R: Registers, D: DmaBuffers> Hc<R, D> {
    /// The machine is about to reset or power off: hand every device back
    /// with no command outstanding and leave the controller as firmware
    /// would find it, within [`STOP_NS`].
    ///
    /// **A platform reset is not a way to end a transfer.** It leaves VBUS up,
    /// so a device mid-Bulk-Only command sees no power cycle either. A port
    /// reset returns the device to its Default state (USB 2.0 §7.1.7.5) and
    /// abandons whatever command it was inside, so it comes first, on every
    /// connected port at once, while the controller still runs. A USB 2 device
    /// then gets TRSTRCY before anything else happens on its bus. After that
    /// the controller is halted (§5.4.1), reset, and its ports unpowered where
    /// it has Port Power Control. Every operation still held is let go: the
    /// driver ends here.
    pub async fn stop<C: Host>(&self, wake: &Wake<C>) -> Stopped {
        self.ending.set(true);
        let deadline = wake.now().saturating_add(STOP_NS);
        let mut done = Stopped { ports: u32::from(self.max_ports), ..Stopped::default() };
        let mut usb2 = false;
        let mut reset: Vec<u8> = Vec::new();
        for port in 0..self.max_ports {
            let portsc = self.portsc(port);
            if !portsc.connected() {
                continue;
            }
            usb2 |= self.protocols.of(port) != Some(crate::protocol::Protocol::Usb3);
            // An earlier reset's flag cleared with the write, so a flag read
            // after it is this reset's (`Portsc::reset_finished`).
            self.write_portsc(port, portsc.neutral().acknowledging_reset(portsc).resetting());
            reset.push(port);
        }
        done.connected = reset.len() as u32;
        let finished = || reset.iter().filter(|&&p| self.portsc(p).reset_finished()).count() as u32;
        polled(wake, deadline, || finished() == done.connected).await;
        done.reset = finished();
        if usb2 && done.reset > 0 {
            exec::sleep_until(wake, wake.now().saturating_add(crate::usb::RESET_RECOVERY_NS)).await;
        }
        let usbcmd = self.regs.read32(self.op + OP_USBCMD);
        self.regs.write32(self.op + OP_USBCMD, usbcmd & !USBCMD_RS);
        done.halted = polled(wake, deadline, || self.regs.read32(self.op + OP_USBSTS) & USBSTS_HCH != 0).await;
        if done.halted {
            self.regs.write32(self.op + OP_USBCMD, USBCMD_HCRST);
            done.cleared = polled(wake, deadline, || {
                self.regs.read32(self.op + OP_USBCMD) & USBCMD_HCRST == 0
                    && self.regs.read32(self.op + OP_USBSTS) & USBSTS_CNR == 0
            })
            .await;
        }
        for port in 0..self.max_ports {
            self.write_portsc(port, self.portsc(port).neutral().unpowered());
            if self.portsc(port).raw() & PORTSC_PP == 0 {
                done.unpowered += 1;
            }
        }
        done
    }

    pub fn portsc(&self, port: u8) -> Portsc {
        Portsc::from_raw(self.regs.read32(self.op + OP_PORT_BASE + usize::from(port) * PORT_REG_SIZE))
    }

    pub fn write_portsc(&self, port: u8, write: Write) {
        self.regs.write32(self.op + OP_PORT_BASE + usize::from(port) * PORT_REG_SIZE, write.raw());
    }

    /// Whether a Port Status Change Event has named `port` since the last
    /// call, taking it.
    pub fn take_change(&self, port: u8) -> bool {
        core::mem::take(&mut self.changed.borrow_mut()[usize::from(port)])
    }

    /// A page of the grant for a device, zeroed, or `None` with none left.
    pub fn page(&self) -> Option<usize> {
        let at = self.free.borrow_mut().pop()?;
        for off in (0..PAGE).step_by(8) {
            self.dma.write64(at + off, 0);
        }
        Some(at)
    }

    /// Pages the controller no longer reaches, given back.
    pub fn release(&self, pages: impl IntoIterator<Item = usize>) {
        self.free.borrow_mut().extend(pages);
    }

    pub fn pages_free(&self) -> usize {
        self.free.borrow().len()
    }

    /// A fresh ring on the page at `at`.
    pub fn ring(&self, at: usize) -> Ring {
        Ring::new(&self.dma, at)
    }

    /// Name `out`, a device's output context page, as `slot`'s (§6.1); `None`
    /// takes the name away once the slot is disabled.
    pub fn name_slot(&self, slot: u8, out: Option<usize>) {
        let addr = out.map_or(0, |at| self.dma.device_addr(at));
        self.dma.write64(OFF_DCBAA + usize::from(slot) * 8, addr);
        self.dma.publish();
    }

    /// Context `index` of the context page at `page`: 0 is an input context's
    /// control context or an output context's slot context (§6.2.1, §6.2.5).
    pub fn context(&self, page: usize, index: usize) -> usize {
        page + index * self.context_bytes
    }

    /// Take the events the ring holds, at most one ring's worth, and tell the
    /// controller so: Interrupt Pending cleared before the drain, and one ERDP
    /// write after it, which clears Event Handler Busy (§4.17.2, §5.5.2.3.3). Answers
    /// whether there was any; [`Self::event_waiting`] says whether more came.
    pub fn take_events(&self) -> bool {
        // Interrupt Pending first: an event the controller writes after this
        // sets it again and interrupts, where one cleared after the drain
        // would be an event no interrupt announces (§4.17.2).
        self.regs.write32(self.rt + IR0_IMAN, IMAN_IP_IE);
        let mut took = false;
        let (mut head, mut cycle) = self.event.get();
        for _ in 0..RING_TRBS {
            let at = OFF_EVENT_RING + head * 16;
            let control = self.dma.read32(at + 12);
            if (control & TRB_CYCLE != 0) != cycle {
                break;
            }
            self.dma.observe();
            let param = self.dma.read64(at);
            let status = self.dma.read32(at + 8);
            head += 1;
            if head == RING_TRBS {
                head = 0;
                cycle = !cycle;
            }
            took = true;
            self.dispatch(control, param, status);
        }
        self.event.set((head, cycle));
        // Every drain ends Event Handler Busy, one that found nothing too: an
        // interrupt raised for an event an earlier drain already took leaves
        // it set, and while it is set no event raises another (§4.17.2).
        let erdp = self.dma.device_addr(OFF_EVENT_RING + head * 16) | ERDP_EHB;
        self.regs.write32(self.rt + IR0_ERDP, erdp as u32);
        self.regs.write32(self.rt + IR0_ERDP + 4, (erdp >> 32) as u32);
        took
    }

    /// Whether an event is on the ring that nothing has taken.
    pub fn event_waiting(&self) -> bool {
        let (head, cycle) = self.event.get();
        let control = self.dma.read32(OFF_EVENT_RING + head * 16 + 12);
        (control & TRB_CYCLE != 0) == cycle
    }

    fn dispatch(&self, control: u32, param: u64, status: u32) {
        let mut counts = self.counts.get();
        counts.events += 1;
        let code = status >> 24;
        match (control >> 10) & 0x3f {
            EVENT_PORT_STATUS_CHANGE => {
                let port = usize::from((param >> 24) as u8);
                match port.checked_sub(1).and_then(|p| self.changed.borrow_mut().get_mut(p).map(|c| *c = true)) {
                    Some(()) => {}
                    None => counts.stray += 1,
                }
            }
            EVENT_COMMAND_COMPLETION if code == CC_COMMAND_RING_STOPPED => self.stopped.set(true),
            EVENT_COMMAND_COMPLETION => {
                let slot = (control >> 24) as u8;
                if !self.answer(param & !0xf, code, 0, slot) {
                    counts.stray += 1;
                }
            }
            EVENT_TRANSFER => {
                if !self.answer(param & !0xf, code, status & 0x00ff_ffff, 0) {
                    counts.stray += 1;
                }
            }
            _ => counts.other += 1,
        }
        self.counts.set(counts);
    }

    /// The event naming TRB `trb`: its operation's answer, or part of it.
    fn answer(&self, trb: u64, code: u32, residue: u32, slot: u8) -> bool {
        let Some(&id) = self.named.borrow().get(&trb) else { return false };
        let mut held = self.held.borrow_mut();
        let op = held.get_mut(&id).expect("a named TRB's operation is held");
        let index = op.trbs.iter().position(|(at, _)| *at == trb).expect("named by its own TRBs");
        let before: u32 = op.trbs[..index].iter().map(|(_, len)| *len).sum();
        let asked = op.trbs[index].1;
        // A residue past what was asked is the controller contradicting
        // itself: nothing of that TRB moved.
        let moved = before + asked.saturating_sub(residue.min(asked));
        let last = index + 1 == op.trbs.len();
        let ends = last || code != CC_SHORT_PACKET || !op.control;
        if !ends {
            op.moved = Some(moved);
            return true;
        }
        let moved = match (code, op.moved) {
            (CC_SUCCESS, Some(short)) if last => short,
            (CC_SUCCESS, None) if last => op.trbs.iter().map(|(_, len)| *len).sum(),
            _ => moved,
        };
        op.done = Some(Done::Event { code, moved, slot });
        true
    }

    fn hold(&self, slot: u8, dci: u8, trbs: Vec<(u64, u32)>, control: bool) -> u64 {
        let id = self.next.get();
        self.next.set(id + 1);
        let mut named = self.named.borrow_mut();
        for (at, _) in &trbs {
            named.insert(*at, id);
        }
        self.held.borrow_mut().insert(id, Held { slot, dci, trbs, control, moved: None, done: None });
        id
    }

    fn done(&self, id: u64) -> Option<Done> {
        self.held.borrow().get(&id).and_then(|op| op.done)
    }

    /// The operation is over and the controller holds none of its TRBs.
    fn forget(&self, id: u64) -> Done {
        let op = self.held.borrow_mut().remove(&id).expect("an operation is forgotten once");
        let mut named = self.named.borrow_mut();
        for (at, _) in &op.trbs {
            named.remove(at);
        }
        op.done.expect("forgotten once answered")
    }

    /// Every operation on `slot` is answered [`Done::Gone`]: the slot was
    /// disabled, or the controller would not say it was, and nothing on it
    /// will be answered now.
    pub fn slot_gone(&self, slot: u8) {
        for op in self.held.borrow_mut().values_mut().filter(|op| op.slot == slot && op.dci != 0) {
            op.done.get_or_insert(Done::Gone);
        }
    }

    /// Put `trbs`, one transfer descriptor, on `ring` and ring the endpoint's
    /// doorbell. The last TRB interrupts; every one before it chains.
    pub fn transfer(&self, slot: u8, dci: u8, ring: &mut Ring, trbs: &[Trb], control: bool) -> Op<'_, R, D> {
        let at = ring.enqueue(&self.dma, trbs);
        let named = at
            .into_iter()
            .zip(trbs)
            .map(|(at, trb)| {
                // A Setup stage's length is its own eight bytes, which no data
                // count includes.
                let moves = match trb.control & (0x3f << 10) {
                    TRB_SETUP | TRB_STATUS => 0,
                    _ => trb.status & 0x1_ffff,
                };
                (at, moves)
            })
            .collect();
        self.dma.publish();
        self.regs.write32(self.db + 4 * usize::from(slot), u32::from(dci));
        Op { hc: self, id: Some(self.hold(slot, dci, named, control)) }
    }

    /// Issue `trb` on the command ring and await its completion. **One
    /// command at a time**: another waits its turn. One unanswered within
    /// [`COMMAND_NS`] is aborted (§4.6.1.2) and answered
    /// [`CC_COMMAND_ABORTED`]; an abort the controller does not answer is a
    /// controller that is gone.
    pub async fn command<C: Host>(&self, wake: &Wake<C>, trb: Trb) -> Result<Done, Dead> {
        exec::until(wake, || (!self.commanding.get()).then_some(())).await;
        self.commanding.set(true);
        let at = self.cmd.borrow_mut().enqueue(&self.dma, &[trb])[0];
        self.dma.publish();
        self.regs.write32(self.db, 0);
        let mut op = Op { hc: self, id: Some(self.hold(0, 0, alloc::vec![(at, 0)], false)) };
        let deadline = wake.now() + COMMAND_NS;
        let done = match op.answer(wake, deadline).await {
            Some(done) => done,
            None => {
                let mut counts = self.counts.get();
                counts.aborted += 1;
                self.counts.set(counts);
                self.stopped.set(false);
                // The pointer fields are ignored while the ring runs, so the
                // low dword alone carries the abort.
                self.regs.write32(self.op + OP_CRCR, CRCR_CA);
                let deadline = wake.now() + COMMAND_NS;
                let stopped = exec::until_or(wake, deadline, || self.stopped.get().then_some(())).await;
                let id = op.id.take().expect("still held");
                if stopped.is_none() {
                    // What it holds is past taking back: the process ends,
                    // and its claim's release is the controller's reset.
                    self.ending.set(true);
                    return Err(Dead("a command and then its abort went unanswered"));
                }
                // §4.6.1.2: the command the abort stopped is answered Command
                // Aborted, and one that finished first keeps its own answer;
                // a stopped ring holds neither.
                let aborted = Done::Event { code: CC_COMMAND_ABORTED, moved: 0, slot: 0 };
                self.held.borrow_mut().get_mut(&id).expect("held").done.get_or_insert(aborted);
                self.forget(id)
            }
        };
        self.commanding.set(false);
        Ok(done)
    }
}

/// An operation the controller holds until its answer arrives or its slot
/// is disabled.
pub struct Op<'h, R: Registers, D: DmaBuffers> {
    hc: &'h Hc<R, D>,
    id: Option<u64>,
}

impl<R: Registers, D: DmaBuffers> Op<'_, R, D> {
    /// Its answer, or `None` once the clock reaches `deadline` with the
    /// controller still holding it.
    pub async fn answer<C: Host>(&mut self, wake: &Wake<C>, deadline: Nanos) -> Option<Done> {
        let id = self.id.expect("an operation is answered once");
        let Some(done) = exec::until_or(wake, deadline, || self.hc.done(id)).await else {
            let mut counts = self.hc.counts.get();
            counts.silent += 1;
            counts.unannounced += u64::from(self.hc.event_waiting());
            self.hc.counts.set(counts);
            return None;
        };
        self.hc.forget(id);
        self.id = None;
        Some(done)
    }

    /// Its answer, however long the device takes: an interrupt endpoint's
    /// report comes when a key moves.
    pub async fn wait<C: Host>(mut self, wake: &Wake<C>) -> Done {
        let (hc, id) = (self.hc, self.id.expect("an operation is answered once"));
        exec::until(wake, || hc.done(id)).await;
        self.id = None;
        hc.forget(id)
    }

    /// The controller gave it back unanswered: a Set TR Dequeue Pointer moved
    /// its ring past it, or its slot was disabled. An event for it that
    /// arrives after is counted stray.
    pub fn reclaimed(mut self) -> Done {
        let id = self.id.take().expect("held");
        let stopped = Done::Event { code: CC_STOPPED, moved: 0, slot: 0 };
        self.hc.held.borrow_mut().get_mut(&id).expect("held").done.get_or_insert(stopped);
        self.hc.forget(id)
    }
}

impl<R: Registers, D: DmaBuffers> Drop for Op<'_, R, D> {
    fn drop(&mut self) {
        if self.hc.ending.get() {
            return;
        }
        #[cfg(test)]
        if std::thread::panicking() {
            return;
        }
        if let Some(id) = self.id {
            panic!("usbd: operation {id}, which the controller still holds, was let go of unanswered");
        }
    }
}

/// §4.22.1: ask firmware for the controller (HC OS Owned, USBLEGSUP §7.1.1)
/// and give it [`HANDOFF_NS`] to clear HC BIOS Owned. Released or never
/// claimed, its SMIs are turned off and the controller is the driver's;
/// kept, it is refused by name and left as found, since §4.22.1 has the OS
/// wait for firmware to let go "before attempting to use the xHC".
fn take_from_firmware<R: Registers, C: Host>(
    regs: &R,
    read: &dyn Fn(u64) -> Option<u32>,
    xecp_dwords: u32,
    host: &C,
) -> Result<(), Refusal> {
    let legsup = match xecp::find(read, xecp_dwords, xecp::CAP_ID_LEGACY) {
        Ok(Some(at)) if read(at + xecp::LEGCTLSTS).is_some() => at as usize,
        Ok(Some(at)) => {
            host.say(format_args!("USB Legacy Support at {at:#x} runs past the register window — no handoff"));
            return Ok(());
        }
        Ok(None) => {
            host.say(format_args!("no USB Legacy Support capability, nothing to hand over"));
            return Ok(());
        }
        Err(why) => {
            host.say(format_args!("the extended capability list does not walk ({why:?}) — no handoff"));
            return Ok(());
        }
    };
    let before = regs.read32(legsup);
    regs.write32(legsup, before | xecp::LEGSUP_OS_OWNED);
    let asked = host.now();
    let mut now = regs.read32(legsup);
    // Firmware answers in its own SMI handler and raises nothing a process
    // can wait on, so its semaphore is read until it clears or the bound passes.
    while now & xecp::LEGSUP_BIOS_OWNED != 0 && host.now() - asked < HANDOFF_NS {
        core::hint::spin_loop();
        now = regs.read32(legsup);
    }
    let took = (host.now() - asked) / 1000;
    match xecp::handoff(before, now) {
        Handoff::NeverClaimed => host.say(format_args!("firmware did not claim the controller (USBLEGSUP {before:#010x})")),
        Handoff::Released => host.say(format_args!(
            "firmware released the controller in {took} us (USBLEGSUP {before:#010x} -> {now:#010x})"
        )),
        Handoff::Kept => return Err(Refusal::FirmwareKeepsIt(now)),
    }
    let ctl_at = legsup + xecp::LEGCTLSTS as usize;
    let ctl = regs.read32(ctl_at);
    regs.write32(ctl_at, xecp::smis_off(ctl));
    let after = regs.read32(ctl_at);
    host.say(format_args!("USBLEGCTLSTS {ctl:#010x} -> {after:#010x} (SMI generation off)"));
    Ok(())
}

/// What each port register speaks, from every Supported Protocol capability.
fn read_protocols<C: Host>(read: &dyn Fn(u64) -> Option<u32>, xecp_dwords: u32, max_ports: u8, host: &C) -> Protocols {
    let mut protocols = Protocols::UNKNOWN;
    let walked = xecp::for_each(read, xecp_dwords, xecp::CAP_ID_PROTOCOL, &mut |at| {
        let (Some(dw0), Some(dw1), Some(dw2)) = (read(at), read(at + 4), read(at + 8)) else {
            return host.say(format_args!("a Supported Protocol capability at {at:#x} runs past the register window"));
        };
        match SupportedProtocol::decode(dw0, dw1, dw2, max_ports) {
            Ok(found) => {
                host.say(format_args!(
                    "USB {}.{:x} on ports {}..={}",
                    found.major,
                    found.minor >> 4,
                    found.first_port + 1,
                    found.first_port + found.port_count
                ));
                protocols.record(&found);
            }
            Err(why) => host.say(format_args!("a Supported Protocol capability at {at:#x} is unusable: {why:?}")),
        }
    });
    if let Err(why) = walked {
        host.say(format_args!("the extended capability list does not walk: {why:?}"));
    }
    protocols
}

/// Read until `done` or [`REGISTER_NS`]: the controller sets these bits in
/// microseconds and raises no interrupt for any of them (§5.4.1, §5.4.2).
/// Whether `done` came true by `deadline`, read every [`STOP_POLL_NS`].
async fn polled<C: Host>(wake: &Wake<C>, deadline: Nanos, done: impl Fn() -> bool) -> bool {
    loop {
        if done() {
            return true;
        }
        let now = wake.now();
        if now >= deadline {
            return false;
        }
        exec::sleep_until(wake, now.saturating_add(STOP_POLL_NS).min(deadline)).await;
    }
}

fn settle<C: Host>(host: &C, what: &'static str, done: impl Fn() -> bool) -> Result<(), Refusal> {
    let began = host.now();
    while !done() {
        if host.now() - began >= REGISTER_NS {
            return Err(Refusal::Stuck(what));
        }
        core::hint::spin_loop();
    }
    Ok(())
}

#[cfg(test)]
mod ring_tests {
    use super::*;

    /// One ring page, its device address its offset.
    struct Page(RefCell<Vec<u8>>);

    impl DmaBuffers for Page {
        fn bytes(&self) -> usize {
            self.0.borrow().len()
        }
        fn device_addr(&self, at: usize) -> u64 {
            at as u64
        }
        fn read16(&self, at: usize) -> u16 {
            u16::from_le_bytes(self.0.borrow()[at..at + 2].try_into().unwrap())
        }
        fn read32(&self, at: usize) -> u32 {
            u32::from_le_bytes(self.0.borrow()[at..at + 4].try_into().unwrap())
        }
        fn read64(&self, at: usize) -> u64 {
            u64::from_le_bytes(self.0.borrow()[at..at + 8].try_into().unwrap())
        }
        fn write16(&self, at: usize, v: u16) {
            self.0.borrow_mut()[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }
        fn write32(&self, at: usize, v: u32) {
            self.0.borrow_mut()[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn write64(&self, at: usize, v: u64) {
            self.0.borrow_mut()[at..at + 8].copy_from_slice(&v.to_le_bytes());
        }
        fn publish(&self) {}
        fn observe(&self) {}
    }

    /// The controller's walk (§4.9.2): from its dequeue, take each TRB whose
    /// cycle is its Consumer Cycle State, following Links and toggling at
    /// one with TC, and stop at the first that is not its own.
    fn walk(page: &Page, at: &mut u64, ccs: &mut bool) -> Vec<u64> {
        let mut took = Vec::new();
        for _ in 0..4 * RING_TRBS {
            let control = page.read32(*at as usize + 12);
            if (control & TRB_CYCLE != 0) != *ccs {
                return took;
            }
            if control & (0x3f << 10) == TRB_LINK {
                *at = page.read64(*at as usize);
                *ccs ^= control & LINK_TC != 0;
            } else {
                took.push(*at);
                *at += 16;
            }
        }
        panic!("the walk never stopped");
    }

    /// TDs of 1 to 3 TRBs over many laps, early Links leaving gaps at every
    /// offset: after each, the controller takes exactly that TD and stops at
    /// the tail, from where a Set TR Dequeue would also resume it.
    #[test]
    fn the_controller_takes_each_td_and_stops_at_the_tail() {
        let page = Page(RefCell::new(alloc::vec![0; PAGE]));
        let mut ring = Ring::new(&page, 0);
        let (mut at, mut ccs) = (0u64, true);
        let mut seed = 0x2545_f491_u32;
        let mut early = 0;
        // The reviewer's lap first: one TRB a TD to the end, then to 253
        // and a 3-TRB TD, then one TRB a TD to rest on the gap.
        let lengths = core::iter::repeat_n(1, 255 + 253)
            .chain([3])
            .chain(core::iter::repeat_n(1, 254))
            .chain(core::iter::from_fn(|| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                Some([1, 1, 2, 3][seed as usize % 4])
            }))
            .take(20_000);
        for n in lengths {
            if ring.tail + n > RING_TRBS - 1 && ring.tail < RING_TRBS - 1 {
                early += 1;
            }
            let trbs: Vec<Trb> = (0..n).map(|i| Trb { param: i as u64, status: 0, control: TRB_NORMAL }).collect();
            let named = ring.enqueue(&page, &trbs);
            assert_eq!(walk(&page, &mut at, &mut ccs), named);
            assert_eq!((at, ccs), (ring.dequeue(&page) & !1, ring.dequeue(&page) & 1 == 1));
        }
        assert!(early > 20, "{early} early Links");
    }

    /// TDs as long as [`Ring::room`] allows, put behind an early Link at
    /// every tail that forces one: none overwrites the Link before it.
    #[test]
    fn the_longest_td_leaves_the_early_link_before_it_whole() {
        let page = Page(RefCell::new(alloc::vec![0; PAGE]));
        let mut ring = Ring::new(&page, 0);
        let (mut at, mut ccs) = (0u64, true);
        for lead in 0..RING_TRBS {
            for n in [lead % 7 + 1, Ring::room()] {
                let trbs: Vec<Trb> = (0..n).map(|i| Trb { param: i as u64, status: 0, control: TRB_NORMAL }).collect();
                let named = ring.enqueue(&page, &trbs);
                assert_eq!(walk(&page, &mut at, &mut ccs), named, "a TD of {n}");
            }
        }
    }
}
