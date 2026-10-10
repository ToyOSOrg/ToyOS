//! The controller: its register window, the one DMA grant every ring and
//! context lives in, and the two rings every operation goes through.
//!
//! **Nothing here decides anything about a port or a device**: that is
//! `toyos-xhci`'s, driven from `bus`. What is here is xHCI 1.2's register file
//! and memory layout, written once: the §4.22.1 handoff, the §4.2 bring-up,
//! the command ring (§4.6), the event ring (§4.9.4) and the doorbells.
//!
//! **Every address the controller is told is one the claim's grant answered**
//! (`PciDev::dma_alloc`), so a slip here is a `DMA FAULT` record against this
//! process and never another's memory.

use toyos::volatile::Window;
use toyos::{DmaRegion, PciDev};
use toyos::shm::SharedMemory;
use toyos_abi::syscall::SyscallError;
use toyos_xhci::protocol::SupportedProtocol;
use toyos_xhci::xecp::{self, Handoff};
use toyos_xhci::{Portsc, Protocols};

const CAP_CAPLENGTH: usize = 0x00;
const CAP_HCSPARAMS1: usize = 0x04;
const CAP_HCSPARAMS2: usize = 0x08;
const CAP_HCCPARAMS1: usize = 0x10;
const CAP_DBOFF: usize = 0x14;
const CAP_RTSOFF: usize = 0x18;
/// HCCPARAMS1 bit 0, 64-bit Addressing Capability (§5.3.6).
const HCCPARAMS1_AC64: u32 = 1 << 0;

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
pub const TRB_SETUP_STAGE: u32 = trb_type(2);
pub const TRB_DATA_STAGE: u32 = trb_type(3);
pub const TRB_STATUS_STAGE: u32 = trb_type(4);
const TRB_LINK: u32 = trb_type(6);
pub const TRB_ENABLE_SLOT: u32 = trb_type(9);
pub const TRB_DISABLE_SLOT: u32 = trb_type(10);
pub const TRB_ADDRESS_DEVICE: u32 = trb_type(11);
pub const TRB_EVALUATE_CONTEXT: u32 = trb_type(13);
pub const TRB_NO_OP_COMMAND: u32 = trb_type(23);
/// Link's Toggle Cycle; a Data or Status stage's Interrupt On Completion and
/// a Data stage's Interrupt on Short Packet; a Setup stage's Immediate Data.
const LINK_TC: u32 = 1 << 1;
pub const TRB_IOC: u32 = 1 << 5;
pub const TRB_ISP: u32 = 1 << 2;
pub const TRB_IDT: u32 = 1 << 6;

const EVENT_TRANSFER: u32 = 32;
const EVENT_COMMAND_COMPLETION: u32 = 33;
const EVENT_PORT_STATUS_CHANGE: u32 = 34;

/// The granularity everything in the grant is placed at; the controller's own
/// PAGESIZE must include it.
pub const PAGE: usize = 0x1000;
/// TRBs per ring: one page.
const RING_TRBS: usize = PAGE / 16;

/// The grant's fixed head, one of each, since enumeration is serial.
const OFF_DCBAA: usize = 0;
const OFF_CMD_RING: usize = PAGE;
const OFF_ERST: usize = 2 * PAGE;
const OFF_EVENT_RING: usize = 3 * PAGE;
pub const OFF_INPUT_CTX: usize = 4 * PAGE;
pub const OFF_DATA: usize = 5 * PAGE;
const OFF_SCRATCH_ARRAY: usize = 6 * PAGE;
const OFF_SCRATCH: usize = 7 * PAGE;
/// Per device: its EP0 ring, then its output context.
const DEV_EP0_RING: usize = 0;
const DEV_OUT_CTX: usize = PAGE;
const DEV_STRIDE: usize = 2 * PAGE;

/// The devices this controller is given room for, whatever more slots it has.
const MAX_DEVICES: usize = 32;
/// The scratchpad buffers one page of array can name.
const MAX_SCRATCH: usize = PAGE / 8;
/// A grant is placed in whole 2 MiB pages; asking for that is asking for
/// what the kernel will map anyway.
const GRANT_UNIT: usize = 2 * 1024 * 1024;

/// How long a register the controller sets in microseconds may take.
/// Policy: the kernel driver's bound for the same waits.
const REGISTER_DEADLINE: std::time::Duration = std::time::Duration::from_millis(2_000);
/// How long firmware is given to let go of the controller (§4.22.1). Policy.
const HANDOFF_DEADLINE: std::time::Duration = std::time::Duration::from_secs(1);

/// One TRB, as the rings carry it (§4.11).
#[derive(Clone, Copy, Debug, Default)]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

/// What an event said, as far as the outstanding operation and the ports are
/// concerned.
#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// A Command Completion: the command TRB it names, its code and slot.
    Command { trb: u64, code: u32, slot: u8 },
    /// A Transfer event: the TRB, the endpoint, its code and residue.
    Transfer { trb: u64, slot: u8, dci: u8, code: u32, residue: u32 },
    /// A Port Status Change: a reason to read the ports, nothing more.
    Port,
    /// Anything else the controller may put on the ring.
    Other,
}

/// Why the controller was not brought up.
#[derive(Debug)]
pub enum Refusal {
    Kernel(&'static str, SyscallError),
    /// A register the controller published puts what it names outside the
    /// window the claim maps.
    Window(&'static str, usize),
    /// A register bit that did not move within [`REGISTER_DEADLINE`].
    Stuck(&'static str),
    PageSize(u32),
    Scratchpad(usize),
    /// HCCPARAMS1.AC64 clear: the controller would truncate the device
    /// addresses of a grant, which the kernel does not place below 4 GiB.
    Addresses32,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(what, why) => write!(f, "the kernel refused {what}: {why:?}"),
            Self::Window(what, at) => write!(f, "{what} at {at:#x} is outside the register window"),
            Self::Stuck(what) => write!(f, "{what} within {} ms", REGISTER_DEADLINE.as_millis()),
            Self::PageSize(p) => write!(f, "PAGESIZE={p:#x} does not include 4 KiB, where every ring is placed"),
            Self::Scratchpad(n) => write!(f, "it asks for {n} scratchpad buffers, past the {MAX_SCRATCH} one page names"),
            Self::Addresses32 => write!(f, "it addresses only 32 bits (HCCPARAMS1.AC64 clear), and a grant is not placed below 4 GiB"),
        }
    }
}

/// A ring this driver produces into: the command ring or an EP0 ring.
pub struct Ring {
    window: Window,
    base: u64,
    tail: usize,
    cycle: bool,
}

impl Ring {
    fn new(window: Window, base: u64) -> Self {
        window.zero();
        let ring = Self { window, base, tail: 0, cycle: true };
        ring.put(RING_TRBS - 1, Trb { param: base, status: 0, control: TRB_LINK | LINK_TC });
        ring
    }

    /// The TRB body first and the control word last, behind a release fence:
    /// the control word carries the cycle bit, and the controller owns the TRB
    /// the moment that bit matches its own (§4.9.2).
    fn put(&self, at: usize, trb: Trb) {
        let off = at * 16;
        self.window.write(off, trb.param);
        self.window.write(off + 8, trb.status);
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        self.window.write(off + 12, trb.control);
    }

    /// Where the controller resumes, with the cycle state it must expect in bit 0.
    pub fn dequeue(&self) -> u64 {
        (self.base + (self.tail as u64) * 16) | u64::from(self.cycle)
    }

    /// Put `trb` on the ring, and answer where it landed: the only name a
    /// completion carries (§6.4.2.1–2).
    pub fn enqueue(&mut self, mut trb: Trb) -> u64 {
        trb.control = (trb.control & !TRB_CYCLE) | u32::from(self.cycle);
        let at = self.base + (self.tail as u64) * 16;
        self.put(self.tail, trb);
        self.tail += 1;
        if self.tail == RING_TRBS - 1 {
            let link = TRB_LINK | LINK_TC | u32::from(self.cycle);
            self.put(self.tail, Trb { param: self.base, status: 0, control: link });
            self.tail = 0;
            self.cycle = !self.cycle;
        }
        at
    }
}

/// The controller, reset and running.
pub struct Controller {
    pub dev: PciDev,
    pub at: (u8, u8, u8),
    /// Held for their mappings' lives.
    _bar: SharedMemory,
    _grant: DmaRegion,
    regs: Window,
    op: usize,
    rt: usize,
    db: usize,
    grant: Window,
    grant_base: u64,
    pub max_ports: u8,
    pub context_size: usize,
    pub protocols: Protocols,
    scratch: usize,
    dev_blocks: usize,
    cmd: Ring,
    event_head: usize,
    event_cycle: bool,
}

impl Controller {
    /// Claim to running: the §4.22.1 handoff, a reset, the grant, both rings
    /// and interrupter 0, as §4.2 orders them.
    pub fn open(dev: PciDev) -> Result<Self, Refusal> {
        let info = dev.describe().map_err(|e| Refusal::Kernel("the claim's description", e))?;
        let at = (info.bus, info.dev, info.func);
        // xHCI 1.2 §5.2.1: the capability registers are in BAR 0.
        let bytes = info.bar_bytes[0];
        let bar = dev.map_bar(0, bytes).map_err(|e| Refusal::Kernel("BAR 0", e))?;
        // SAFETY: the mapping is `bytes` long and lives as long as `bar`, which
        // the controller holds for its own life.
        let regs = unsafe { Window::new(bar.as_ptr(), bytes as usize) };
        let read = |at: usize| -> Option<u32> { (at + 4 <= regs.bytes()).then(|| regs.read::<u32>(at)) };

        let cap_length = usize::from(regs.read::<u8>(CAP_CAPLENGTH));
        let hcsparams1 = regs.read::<u32>(CAP_HCSPARAMS1);
        let hcsparams2 = regs.read::<u32>(CAP_HCSPARAMS2);
        let hccparams1 = regs.read::<u32>(CAP_HCCPARAMS1);
        let db = (regs.read::<u32>(CAP_DBOFF) & !0x3) as usize;
        let rt = (regs.read::<u32>(CAP_RTSOFF) & !0x1f) as usize;
        let max_slots = (hcsparams1 & 0xff) as usize;
        let max_ports = (hcsparams1 >> 24) as u8;
        let context_size = if hccparams1 & (1 << 2) != 0 { 64 } else { 32 };
        let scratch = (((hcsparams2 >> 21) & 0x1f) << 5 | ((hcsparams2 >> 27) & 0x1f)) as usize;
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
        let pagesize = regs.read::<u32>(op + OP_PAGESIZE) & 0xffff;
        if pagesize & 1 == 0 {
            return Err(Refusal::PageSize(pagesize));
        }
        if scratch > MAX_SCRATCH {
            return Err(Refusal::Scratchpad(scratch));
        }
        if hccparams1 & HCCPARAMS1_AC64 == 0 {
            return Err(Refusal::Addresses32);
        }

        take_from_firmware(&regs, &read, hccparams1 >> 16);
        let protocols = read_protocols(&read, hccparams1 >> 16, max_ports);

        let usbcmd = regs.read::<u32>(op + OP_USBCMD);
        if usbcmd & USBCMD_RS != 0 {
            regs.write::<u32>(op + OP_USBCMD, usbcmd & !USBCMD_RS);
        }
        settle("it never halted", || regs.read::<u32>(op + OP_USBSTS) & USBSTS_HCH != 0)?;
        regs.write::<u32>(op + OP_USBCMD, USBCMD_HCRST);
        settle("it held HCRST", || regs.read::<u32>(op + OP_USBCMD) & USBCMD_HCRST == 0)?;
        settle("it stayed Controller Not Ready", || regs.read::<u32>(op + OP_USBSTS) & USBSTS_CNR == 0)?;

        // After the reset, so a controller refused above never had a grant:
        // the grant is what starts the function mastering the bus.
        let dev_blocks = max_slots.min(MAX_DEVICES);
        let bytes = (OFF_SCRATCH + scratch * PAGE + dev_blocks * DEV_STRIDE).next_multiple_of(GRANT_UNIT);
        let region = dev.dma_alloc(bytes as u64).map_err(|e| Refusal::Kernel("a DMA grant", e))?;
        // SAFETY: the kernel rounds a grant up and never down, so it covers
        // `bytes`, and lives as long as `region`, which the controller holds.
        let grant = unsafe { Window::new(region.memory.as_ptr(), bytes) };
        grant.zero();
        let base = region.device_addr;
        println!(
            "usbd: PCI {:02x}:{:02x}.{} reset: {max_ports} ports, {max_slots} slots ({dev_blocks} given room), \
             {context_size}-byte contexts, {scratch} scratchpad buffers, a {} KiB grant",
            at.0,
            at.1,
            at.2,
            bytes / 1024
        );

        regs.write::<u32>(op + OP_CONFIG, dev_blocks as u32);
        for i in 0..scratch {
            grant.write::<u64>(OFF_SCRATCH_ARRAY + i * 8, base + (OFF_SCRATCH + i * PAGE) as u64);
        }
        // DCBAA[0] names the scratchpad array (§6.1), and only once it is filled.
        grant.write::<u64>(OFF_DCBAA, if scratch > 0 { base + OFF_SCRATCH_ARRAY as u64 } else { 0 });
        regs.write::<u64>(op + OP_DCBAAP, base + OFF_DCBAA as u64);
        let cmd = Ring::new(grant.sub(OFF_CMD_RING, PAGE), base + OFF_CMD_RING as u64);
        regs.write::<u64>(op + OP_CRCR, cmd.dequeue());
        // One segment, the whole event ring (§6.5), named before ERSTBA is.
        grant.write::<u64>(OFF_ERST, base + OFF_EVENT_RING as u64);
        grant.write::<u32>(OFF_ERST + 8, RING_TRBS as u32);
        regs.write::<u32>(rt + IR0_ERSTSZ, 1);
        regs.write::<u64>(rt + IR0_ERDP, base + OFF_EVENT_RING as u64);
        regs.write::<u64>(rt + IR0_ERSTBA, base + OFF_ERST as u64);
        // No moderation: an event is an interrupt, which is what this driver
        // counts against the records it reads.
        regs.write::<u32>(rt + IR0_IMOD, 0);
        regs.write::<u32>(rt + IR0_IMAN, IMAN_IP_IE);
        regs.write::<u32>(op + OP_USBCMD, USBCMD_RS | USBCMD_INTE);
        settle("it stayed halted after Run", || regs.read::<u32>(op + OP_USBSTS) & USBSTS_HCH == 0)?;

        let ctrl = Self {
            dev,
            at,
            _bar: bar,
            _grant: region,
            regs,
            op,
            rt,
            db,
            grant,
            grant_base: base,
            max_ports,
            context_size,
            protocols,
            scratch,
            dev_blocks,
            cmd,
            event_head: 0,
            event_cycle: true,
        };
        // Port Power Control leaves a port unpowered after HCRST, and an
        // unpowered port reports nothing; without PPC the write does nothing.
        for p in 0..max_ports {
            let portsc = ctrl.portsc(p);
            if portsc.raw() & PORTSC_PP == 0 {
                ctrl.write_portsc(p, portsc.neutral().powered());
            }
        }
        Ok(ctrl)
    }

    pub fn portsc(&self, port: u8) -> Portsc {
        Portsc::from_raw(self.regs.read::<u32>(self.op + OP_PORT_BASE + usize::from(port) * PORT_REG_SIZE))
    }

    pub fn write_portsc(&self, port: u8, write: toyos_xhci::portsc::Write) {
        self.regs.write::<u32>(self.op + OP_PORT_BASE + usize::from(port) * PORT_REG_SIZE, write.raw());
    }

    /// Put a command on the ring and ring the host controller's doorbell;
    /// answers the TRB its completion will name.
    pub fn command(&mut self, trb: Trb) -> u64 {
        let at = self.cmd.enqueue(trb);
        self.regs.write::<u32>(self.db, 0);
        at
    }

    /// Ring `slot`'s doorbell for endpoint `dci`.
    pub fn doorbell(&self, slot: u8, dci: u8) {
        self.regs.write::<u32>(self.db + 4 * usize::from(slot), u32::from(dci));
    }

    /// The next event, or `None` once the ring holds nothing new. A drain
    /// ends with [`Self::consumed`].
    pub fn next_event(&mut self) -> Option<Event> {
        let at = OFF_EVENT_RING + self.event_head * 16;
        let control = self.grant.read::<u32>(at + 12);
        if (control & TRB_CYCLE != 0) != self.event_cycle {
            return None;
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        let param = self.grant.read::<u64>(at);
        let status = self.grant.read::<u32>(at + 8);
        self.event_head += 1;
        if self.event_head == RING_TRBS {
            self.event_head = 0;
            self.event_cycle = !self.event_cycle;
        }
        let code = status >> 24;
        let slot = (control >> 24) as u8;
        Some(match (control >> 10) & 0x3f {
            EVENT_COMMAND_COMPLETION => Event::Command { trb: param & !0xf, code, slot },
            EVENT_TRANSFER => Event::Transfer {
                trb: param & !0xf,
                slot,
                dci: ((control >> 16) & 0x1f) as u8,
                code,
                residue: status & 0x00ff_ffff,
            },
            EVENT_PORT_STATUS_CHANGE => Event::Port,
            _ => Event::Other,
        })
    }

    /// Whether an event is waiting that nothing has taken off the ring.
    pub fn event_waiting(&self) -> bool {
        let control = self.grant.read::<u32>(OFF_EVENT_RING + self.event_head * 16 + 12);
        (control & TRB_CYCLE != 0) == self.event_cycle
    }

    /// Tell the controller every event up to here is taken, and clear Event
    /// Handler Busy and Interrupt Pending, so the next event interrupts again
    /// (§4.17.2): one write per drain, not per event.
    pub fn consumed(&self) {
        let erdp = self.grant_base + (OFF_EVENT_RING + self.event_head * 16) as u64;
        self.regs.write::<u64>(self.rt + IR0_ERDP, erdp | ERDP_EHB);
        self.regs.write::<u32>(self.rt + IR0_IMAN, IMAN_IP_IE);
    }

    /// The grant from `off`, `len` bytes long, and where the device reaches it.
    pub fn grant(&self, off: usize, len: usize) -> (Window, u64) {
        (self.grant.sub(off, len), self.grant_base + off as u64)
    }

    /// A slot's EP0 ring and output context, or `None` for a slot past the
    /// room the grant gives devices. `slot` is the controller's answer.
    pub fn device_block(&self, slot: u8) -> Option<usize> {
        let index = usize::from(slot).checked_sub(1).filter(|i| *i < self.dev_blocks)?;
        Some(OFF_SCRATCH + self.scratch * PAGE + index * DEV_STRIDE)
    }

    /// A fresh EP0 ring in `slot`'s block, and its output context named in
    /// the DCBAA.
    pub fn bind_slot(&self, block: usize, slot: u8) -> Ring {
        let (out, out_addr) = self.grant(block + DEV_OUT_CTX, PAGE);
        out.zero();
        self.grant.write::<u64>(OFF_DCBAA + usize::from(slot) * 8, out_addr);
        let (ring, ring_addr) = self.grant(block + DEV_EP0_RING, PAGE);
        Ring::new(ring, ring_addr)
    }

    /// A slot the controller has disabled names no context any more.
    pub fn unbind_slot(&self, slot: u8) {
        self.grant.write::<u64>(OFF_DCBAA + usize::from(slot) * 8, 0);
    }

    /// One dword of context `index` of the input context, for a command about
    /// to name it.
    pub fn input_ctx(&self, index: usize, dword: usize, value: u32) {
        self.grant.write::<u32>(OFF_INPUT_CTX + index * self.context_size + dword * 4, value);
    }

    pub fn clear_input_ctx(&self) {
        self.grant.sub(OFF_INPUT_CTX, PAGE).zero();
    }
}

/// §4.22.1: ask firmware for the controller, give it [`HANDOFF_DEADLINE`] to
/// let go, and turn every SMI off whatever it answered — a controller is reset
/// from under firmware rather than left to it, with the line naming which.
fn take_from_firmware(regs: &Window, read: &dyn Fn(usize) -> Option<u32>, xecp_dwords: u32) {
    let walk_read = |at: u64| read(at as usize);
    let legsup = match xecp::find(&walk_read, xecp_dwords, xecp::CAP_ID_LEGACY) {
        Ok(Some(at)) if (at + xecp::LEGCTLSTS + 4) as usize <= regs.bytes() => at as usize,
        Ok(Some(at)) => return println!("usbd: USB Legacy Support at {at:#x} runs past the register window — no handoff"),
        Ok(None) => return println!("usbd: no USB Legacy Support capability, nothing to hand over"),
        Err(why) => return println!("usbd: the extended capability list does not walk ({why:?}) — no handoff"),
    };
    let before = regs.read::<u32>(legsup);
    regs.write::<u32>(legsup, before | xecp::LEGSUP_OS_OWNED);
    let asked = std::time::Instant::now();
    let mut now = regs.read::<u32>(legsup);
    // Firmware answers in its own SMI handler and raises nothing a process
    // can wait on, so its semaphore is read until it clears or the bound passes.
    while now & xecp::LEGSUP_BIOS_OWNED != 0 && asked.elapsed() < HANDOFF_DEADLINE {
        std::hint::spin_loop();
        now = regs.read::<u32>(legsup);
    }
    match xecp::handoff(before, now) {
        Handoff::NeverClaimed => println!("usbd: firmware did not claim the controller (USBLEGSUP {before:#010x})"),
        Handoff::Released => println!(
            "usbd: firmware released the controller in {} us (USBLEGSUP {before:#010x} -> {now:#010x})",
            asked.elapsed().as_micros()
        ),
        Handoff::Kept => println!(
            "usbd: firmware still owns the controller after {} ms (USBLEGSUP {before:#010x} -> {now:#010x}) — \
             resetting it anyway",
            HANDOFF_DEADLINE.as_millis()
        ),
    }
    let ctl = regs.read::<u32>(legsup + xecp::LEGCTLSTS as usize);
    regs.write::<u32>(legsup + xecp::LEGCTLSTS as usize, xecp::smis_off(ctl));
    let after = regs.read::<u32>(legsup + xecp::LEGCTLSTS as usize);
    println!("usbd: USBLEGCTLSTS {ctl:#010x} -> {after:#010x} (SMI generation off)");
}

/// What each port register speaks, from every Supported Protocol capability.
fn read_protocols(read: &dyn Fn(usize) -> Option<u32>, xecp_dwords: u32, max_ports: u8) -> Protocols {
    let mut protocols = Protocols::UNKNOWN;
    let walk_read = |at: u64| read(at as usize);
    let walked = xecp::for_each(&walk_read, xecp_dwords, xecp::CAP_ID_PROTOCOL, &mut |at| {
        let at = at as usize;
        let (Some(dw0), Some(dw1), Some(dw2)) = (read(at), read(at + 4), read(at + 8)) else {
            return println!("usbd: a Supported Protocol capability at {at:#x} runs past the register window");
        };
        match SupportedProtocol::decode(dw0, dw1, dw2, max_ports) {
            Ok(found) => {
                println!(
                    "usbd: USB {}.{:x} on ports {}..={}",
                    found.major,
                    found.minor >> 4,
                    found.first_port + 1,
                    found.first_port + found.port_count
                );
                protocols.record(&found);
            }
            Err(why) => println!("usbd: a Supported Protocol capability at {at:#x} is unusable: {why:?}"),
        }
    });
    if let Err(why) = walked {
        println!("usbd: the extended capability list does not walk: {why:?}");
    }
    protocols
}

/// Read until `done` or [`REGISTER_DEADLINE`]: the controller sets these bits
/// in microseconds and raises no interrupt for any of them (§5.4.1, §5.4.2).
fn settle(what: &'static str, done: impl Fn() -> bool) -> Result<(), Refusal> {
    let began = std::time::Instant::now();
    while !done() {
        if began.elapsed() >= REGISTER_DEADLINE {
            return Err(Refusal::Stuck(what));
        }
        std::hint::spin_loop();
    }
    Ok(())
}
