//! The part, as the specification describes it — not as the driver beside it
//! expects it.
//!
//! Written from the *Intel 82574 GbE Controller Family Datasheet* (317694-018,
//! rev 2.7) and cited to it clause by clause. **The rule this file is under is
//! that it implements what the document permits, not what `lib.rs` happens to
//! do**: every latitude the datasheet gives the hardware is a [`Permits`] entry
//! here, on by default, and a driver that only works when one of them is off is
//! a driver with a bug in it. A stub that answered what the code under test
//! expected would be a finding, not a test.
//!
//! Every run takes a seed; every assertion this file makes prints it; and the
//! seed reproduces the run exactly.
//!
//! What is deliberately *not* modelled: the PHY and its MDIO registers, the
//! NVM's own access protocol, checksum offload, VLAN insertion, RSS and the
//! second queue, flow control, and every statistic counter. The driver reaches
//! none of them.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;
use std::{format, vec};

use crate::regs::{self, cause, ctrl, rah, rctl, rx_desc, status, tctl, tx_desc};
use crate::{Clock, DmaBuffers, Interrupts, Registers};

/// Where the device reaches the grant. Not zero and not a small number: a
/// driver that wrote a grant *offset* into a descriptor instead of a device
/// address would still be inside a zero-based region, and this makes that
/// mistake a refusal instead of a pass.
pub const DEVICE_BASE: u64 = 0x0000_0001_0000_0000;

/// The station address the modelled NVM holds (§10.2.5.23: entry 0 is loaded
/// from the `IA` field after a reset).
pub const NVM_MAC: [u8; 6] = [0x54, 0xbf, 0x64, 0x11, 0x22, 0x33];

/// Nanoseconds the modelled clock moves on each read. A driver's deadline has
/// to be reachable, and a clock that stood still would hang the test rather
/// than fail it.
const CLOCK_STEP_NANOS: u64 = 1_000;

/// How many reads of `CTRL` the reset stays asserted for (§10.2.2.1: the bit is
/// self-clearing, and the datasheet gives no time).
const RESET_READS: u32 = 3;

/// A latitude the datasheet gives the hardware. Each is on unless a test turns
/// it off, and each names the clause that permits it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Permits {
    /// §7.1.7.1: descriptors "accumulate and are opportunistically written out
    /// in cache line-oriented chunks", so a completion may not be visible the
    /// moment the frame arrived, and the order inside a chunk is the
    /// hardware's.
    pub batched_writeback: bool,
    /// §7.1.8: the head register "includes those descriptors completed but not
    /// yet stored in memory", so `RDH` may be ahead of what a driver can read.
    pub shadow_head: bool,
    /// §7.1.7.2: a descriptor with a null data address is written back "with
    /// the `DD` bit set in the status byte and all other bits unchanged".
    pub null_padding: bool,
    /// §7.4.5: an interrupt whose cause was already cleared. "This results in
    /// a spurious interrupt."
    pub spurious_interrupts: bool,
    /// §10.2.4.1 case 3: "Interrupt was not asserted (ICR.INT_ASSERT=0): Read
    /// has no side affect." The document's own §7.4.5 says instead that "all
    /// bits in the ICR register are cleared on a read to ICR"; both readings
    /// are of the same specification, so the model takes one or the other and
    /// a driver has to be right under either.
    pub icr_read_has_no_side_effect: bool,
}

impl Default for Permits {
    fn default() -> Self {
        Self {
            batched_writeback: true,
            shadow_head: true,
            null_padding: true,
            spurious_interrupts: true,
            icr_read_has_no_side_effect: true,
        }
    }
}

/// The register file plus everything behind it.
struct Model {
    seed: u64,
    rng: u64,
    permits: Permits,
    file: Vec<u32>,
    memory: Vec<u8>,
    nanos: u64,
    /// Reads of `CTRL` left before `RST` clears itself.
    reset_reads: u32,
    /// Whether the modelled NVM answers, and therefore whether `RAH0.AV` is
    /// set after a reset (§10.2.5.23).
    has_nvm: bool,
    link_up: bool,
    /// `STATUS.SPEED`'s encoding: `10b` is 1000 Mb/s.
    speed_code: u32,
    /// Frames the wire has delivered and the device has not yet placed.
    inbound: VecDeque<Vec<u8>>,
    /// Frames the device has put on the wire.
    pub sent: Vec<Vec<u8>>,
    /// The device's own receive head: descriptors it has taken and filled.
    rx_head: usize,
    /// Receive descriptors the device has taken and not written back: the
    /// index, the status word it will report, and the bytes it will leave in
    /// the buffer. **The frame travels with the write-back**, because that is
    /// what `DD` promises — §7.1.3.3: "DD indicates whether hardware is done
    /// with the descriptor. When the DD bit is set along with EOP, the received
    /// packet is completely in main memory." Before it, what is in the buffer
    /// is the device's business, and this model puts a pattern there to say so.
    rx_holding: Vec<(usize, u64, Vec<u8>)>,
    tx_head: usize,
    tx_holding: Vec<usize>,
    /// Messages the function has sent that the claim has not read.
    messages: u32,
    /// Whether the device does its work when a tail register is written.
    ///
    /// **Nothing in the datasheet says *when* the hardware acts** — a fetch
    /// happens "as soon as any descriptors are made available" (§7.2.4.1) and
    /// a write-back "opportunistically" (§7.1.7.1) — so a test may hold it and
    /// watch a driver meet a device that has not caught up.
    held: bool,
}

/// A tiny seeded generator. Not cryptography and not a distribution: a way to
/// make "the hardware chose" reproducible from one printed number.
fn next(rng: &mut u64) -> u64 {
    let mut x = *rng;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *rng = x;
    x
}

impl Model {
    fn new(seed: u64, permits: Permits) -> Self {
        let mut model = Self {
            seed,
            rng: seed | 1,
            permits,
            file: vec![0; regs::REGISTER_BYTES / 4],
            memory: vec![0; 16 * 1024 * 1024],
            nanos: 0,
            reset_reads: 0,
            has_nvm: true,
            link_up: false,
            speed_code: 0b10,
            inbound: VecDeque::new(),
            sent: Vec::new(),
            rx_head: 0,
            rx_holding: Vec::new(),
            tx_head: 0,
            tx_holding: Vec::new(),
            messages: 0,
            held: false,
        };
        model.power_on();
        model
    }

    /// What the register file holds out of reset. §10.2.2.1's `CTRL` has two
    /// reserved bits documented as "Set to 1b" (bit 3) and "must be set to 1b"
    /// (bit 20, `ADVD3WUC`), and a driver that composes a whole `CTRL` value
    /// instead of keeping what it read clears them — which is what makes them
    /// worth modelling.
    fn power_on(&mut self) {
        for word in self.file.iter_mut() {
            *word = 0;
        }
        self.set(regs::CTRL, (1 << 3) | (1 << 20));
        self.rx_head = 0;
        self.tx_head = 0;
        self.rx_holding.clear();
        self.tx_holding.clear();
        self.load_station_address();
        self.refresh_status();
    }

    fn load_station_address(&mut self) {
        if !self.has_nvm {
            self.set(regs::RAL0, 0);
            self.set(regs::RAH0, 0);
            return;
        }
        let m = NVM_MAC;
        let low = u32::from_le_bytes([m[0], m[1], m[2], m[3]]);
        let high = rah::AV | u16::from_le_bytes([m[4], m[5]]) as u32;
        self.set(regs::RAL0, low);
        self.set(regs::RAH0, high);
    }

    fn refresh_status(&mut self) {
        let mut value = 1 << 19; // GIO Master Enable Status, §10.2.2.2.
        if self.link_up && self.get(regs::CTRL) & ctrl::SLU != 0 {
            value |= status::LU | status::FD;
            value |= (self.speed_code & status::SPEED_MASK) << status::SPEED_SHIFT;
        }
        self.set(regs::STATUS, value);
    }

    fn get(&self, reg: usize) -> u32 {
        self.file[reg / 4]
    }

    fn set(&mut self, reg: usize, value: u32) {
        self.file[reg / 4] = value;
    }

    /// One 32-bit read of the register file, with the side effects the
    /// datasheet gives each register.
    fn read(&mut self, reg: usize) -> u32 {
        assert!(
            reg % 4 == 0 && reg + 4 <= regs::REGISTER_BYTES,
            "seed {}: a {reg:#x} register read is outside the file",
            self.seed
        );
        match reg {
            regs::CTRL => {
                if self.reset_reads > 0 {
                    self.reset_reads -= 1;
                    if self.reset_reads == 0 {
                        // §10.2.2.1: "This bit is self-clearing." The reset
                        // itself has already happened; this is the bit going
                        // away.
                        let held = self.get(regs::CTRL) & !ctrl::RST;
                        self.set(regs::CTRL, held);
                    }
                }
                self.get(regs::CTRL)
            }
            regs::STATUS => {
                self.refresh_status();
                self.get(regs::STATUS)
            }
            regs::ICR => {
                let value = self.get(regs::ICR);
                let masked_all = self.get(regs::IMS) == 0;
                let asserted = value & cause::INT_ASSERTED != 0;
                // §10.2.4.1's three cases. Case 1 clears unconditionally; case
                // 3 does nothing at all — and whether case 3 or §7.4.5's
                // blanket read-to-clear applies is what `Permits` chooses.
                if masked_all || asserted || !self.permits.icr_read_has_no_side_effect {
                    self.set(regs::ICR, 0);
                }
                value
            }
            // §10.2.4.6 and §10.2.4.4: write-only, and a read of one answers
            // nothing.
            regs::IMC | regs::ICS => 0,
            _ => self.get(reg),
        }
    }

    fn write(&mut self, reg: usize, value: u32) {
        assert!(
            reg % 4 == 0 && reg + 4 <= regs::REGISTER_BYTES,
            "seed {}: a {reg:#x} register write is outside the file",
            self.seed
        );
        match reg {
            regs::CTRL => {
                self.set(regs::CTRL, value);
                if value & ctrl::RST != 0 {
                    // §10.2.2.1: "a reset of the MAC function of the device".
                    // Everything but the reserved defaults and the NVM's
                    // station address goes.
                    self.power_on();
                    self.set(regs::CTRL, self.get(regs::CTRL) | ctrl::RST);
                    self.reset_reads = RESET_READS;
                }
                self.refresh_status();
            }
            // §10.2.4.5: set, not assign.
            regs::IMS => {
                let held = self.get(regs::IMS);
                self.set(regs::IMS, held | value);
            }
            // §10.2.4.6: clear.
            regs::IMC => {
                let held = self.get(regs::IMS);
                self.set(regs::IMS, held & !value);
            }
            // §10.2.4.4: set a cause, as if the event had happened.
            regs::ICS => self.raise(value),
            // §10.2.4.1: "Writing a 1b to any bit in the register also clears
            // that bit. Writing a 0b to any bit has no effect on that bit."
            // `INT_ASSERTED` is not writable and clears with its causes.
            regs::ICR => {
                let held = self.get(regs::ICR);
                let mut left = held & !(value & !cause::INT_ASSERTED);
                if left & !cause::INT_ASSERTED == 0 {
                    left = 0;
                }
                self.set(regs::ICR, left);
            }
            // §10.2.2.2: read-only.
            regs::STATUS => {}
            regs::RDT => {
                self.set(regs::RDT, value);
                if !self.held {
                    self.run();
                }
            }
            regs::TDT => {
                self.set(regs::TDT, value);
                if !self.held {
                    self.run();
                }
            }
            _ => self.set(reg, value),
        }
    }

    /// Record a cause and, if it is unmasked, send a message.
    fn raise(&mut self, causes: u32) {
        let held = self.get(regs::ICR);
        let now = held | causes;
        let unmasked = now & self.get(regs::IMS) & !cause::INT_ASSERTED;
        self.set(regs::ICR, if unmasked != 0 { now | cause::INT_ASSERTED } else { now });
        if unmasked != 0 {
            self.messages = self.messages.saturating_add(1);
        }
    }

    /// Turn a device address in a descriptor into a grant offset, the way the
    /// unit does: an address this function was never granted reaches nothing.
    fn resolve(&self, address: u64) -> usize {
        assert!(
            address >= DEVICE_BASE && address < DEVICE_BASE + self.memory.len() as u64,
            "seed {}: the driver put {address:#x} in a descriptor, which is not an address \
             this function was granted — on a real machine the unit refuses it and the \
             kernel records a DMA fault against the process",
            self.seed
        );
        (address - DEVICE_BASE) as usize
    }

    /// Where the receive descriptor ring is, as the registers say.
    fn rx_ring(&self) -> usize {
        self.resolve(((self.get(regs::RDBAH) as u64) << 32) | self.get(regs::RDBAL) as u64)
    }

    /// Where the transmit descriptor ring is, as the registers say.
    fn tx_ring(&self) -> usize {
        self.resolve(((self.get(regs::TDBAH) as u64) << 32) | self.get(regs::TDBAL) as u64)
    }

    fn desc_read(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.memory[at..at + 8].try_into().unwrap())
    }

    fn desc_write(&mut self, at: usize, word: u64) {
        self.memory[at..at + 8].copy_from_slice(&word.to_le_bytes());
    }

    /// One tick of the device: place what has arrived, send what has been
    /// published, and write back what it is holding.
    fn run(&mut self) {
        self.receive();
        self.transmit();
        self.flush_rx();
        self.flush_tx();
        if self.permits.spurious_interrupts && next(&mut self.rng) % 32 == 0 {
            // §7.4.5: a message whose cause is already gone. Nothing is set in
            // `ICR`, which is exactly what makes it spurious.
            self.messages = self.messages.saturating_add(1);
        }
    }

    /// Place frames into descriptors, up to what `RDT` made available.
    fn receive(&mut self) {
        if self.get(regs::RCTL) & rctl::EN == 0 {
            return;
        }
        let ring = self.rx_ring();
        let count = self.get(regs::RDLEN) as usize / rx_desc::BYTES;
        if count == 0 {
            return;
        }
        let tail = self.get(regs::RDT) as usize % count;
        let strip_crc = self.get(regs::RCTL) & rctl::SECRC != 0;
        while self.rx_head != tail {
            let Some(frame) = self.inbound.pop_front() else { return };
            let at = ring + self.rx_head * rx_desc::BYTES;
            let buffer = self.desc_read(at);
            // §7.1.7.2: a null data address is stored into and written back
            // with `DD` alone.
            if buffer == 0 {
                if !self.permits.null_padding {
                    return;
                }
                let word = (rx_desc::status::DD as u64) << rx_desc::STATUS_SHIFT;
                self.rx_holding.push((self.rx_head, word, Vec::new()));
                self.rx_head = (self.rx_head + 1) % count;
                self.inbound.push_front(frame);
                continue;
            }
            let into = self.resolve(buffer);
            // §10.2.5.1: `LPE` is clear, so a frame over 1522 bytes is
            // discarded by the hardware and never spans two buffers.
            if frame.len() > 1522 {
                continue;
            }
            let stored = if strip_crc { frame.len() } else { frame.len() + 4 };
            assert!(
                stored <= crate::RX_BUF_BYTES,
                "seed {}: the model was asked to store {stored} bytes in a \
                 {}-byte buffer",
                self.seed,
                crate::RX_BUF_BYTES
            );
            // The buffer is the device's until its descriptor says otherwise,
            // so a driver that reads it before `DD` reads this and not a frame.
            for byte in self.memory[into..into + crate::RX_BUF_BYTES].iter_mut() {
                *byte = 0xDE;
            }
            let word = (stored as u64 & rx_desc::LENGTH_MASK)
                | ((rx_desc::status::DD | rx_desc::status::EOP) as u64)
                    << rx_desc::STATUS_SHIFT;
            self.rx_holding.push((self.rx_head, word, frame));
            self.rx_head = (self.rx_head + 1) % count;
        }
    }

    /// Write back the descriptors the device is holding.
    ///
    /// §7.1.7.1 lets it accumulate them and write them "in cache line-oriented
    /// chunks"; §7.1.8 says the head advances "just prior to" — but the head
    /// register is a shadow that may already count descriptors "not yet stored
    /// in memory", so the register moves first when [`Permits::shadow_head`]
    /// is on.
    fn flush_rx(&mut self) {
        if self.rx_holding.is_empty() {
            return;
        }
        let ring = self.rx_ring();
        let batch = if self.permits.batched_writeback {
            let chunk = 1 + (next(&mut self.rng) % 4) as usize;
            chunk.min(self.rx_holding.len())
        } else {
            self.rx_holding.len()
        };
        let mut holding: Vec<(usize, u64, Vec<u8>)> = self.rx_holding.drain(..batch).collect();
        if self.permits.shadow_head {
            // The register counts what the device has finished with, whether or
            // not memory says so yet. A driver reading `RDH` to find
            // completions reads descriptors that are not there.
            self.set(regs::RDH, self.rx_head as u32);
        }
        if self.permits.batched_writeback {
            // The order inside a chunk is the hardware's, so a driver may not
            // infer an earlier descriptor's state from a later one's.
            let rounds = holding.len();
            for i in 0..rounds {
                let j = i + (next(&mut self.rng) as usize % (rounds - i));
                holding.swap(i, j);
            }
        }
        for (index, word, frame) in holding {
            if !frame.is_empty() {
                let buffer = self.desc_read(ring + index * rx_desc::BYTES);
                let into = self.resolve(buffer);
                self.memory[into..into + frame.len()].copy_from_slice(&frame);
            }
            self.desc_write(ring + index * rx_desc::BYTES + 8, word);
        }
        if !self.permits.shadow_head {
            self.set(regs::RDH, self.rx_head as u32);
        }
        self.raise(cause::RXT0);
    }

    /// Take every published transmit descriptor and put its frame on the wire.
    fn transmit(&mut self) {
        if self.get(regs::TCTL) & tctl::EN == 0 {
            return;
        }
        let ring = self.tx_ring();
        let count = self.get(regs::TDLEN) as usize / tx_desc::BYTES;
        if count == 0 {
            return;
        }
        let tail = self.get(regs::TDT) as usize % count;
        // §7.2.4.1: "The 82574 NEVER fetches descriptors beyond the descriptor
        // tail pointer."
        while self.tx_head != tail {
            let at = ring + self.tx_head * tx_desc::BYTES;
            let buffer = self.desc_read(at);
            let word = self.desc_read(at + 8);
            let command = ((word >> tx_desc::CMD_SHIFT) & 0xFF) as u8;
            assert!(
                command & tx_desc::cmd::DEXT == 0,
                "seed {}: the driver published a descriptor with DEXT set, and §7.2.10 \
                 says an extended descriptor's fields mean something else entirely",
                self.seed
            );
            assert!(
                command & tx_desc::cmd::EOP != 0,
                "seed {}: the driver published a transmit descriptor with no EOP, so this \
                 frame would never be put on the wire",
                self.seed
            );
            let len = (word & tx_desc::LENGTH_MASK) as usize;
            let from = self.resolve(buffer);
            self.sent.push(self.memory[from..from + len].to_vec());
            if command & tx_desc::cmd::RS != 0 {
                self.tx_holding.push(self.tx_head);
            }
            self.tx_head = (self.tx_head + 1) % count;
        }
    }

    /// §7.2.4.2: with `RS` set and no `IDE`, "The device writes back only the
    /// status byte of the descriptor (TDESCR.STA) and all other bytes of the
    /// descriptor are left unchanged" — and it may hold a batch first.
    fn flush_tx(&mut self) {
        if self.tx_holding.is_empty() {
            return;
        }
        let ring = self.tx_ring();
        let batch = if self.permits.batched_writeback {
            let chunk = 1 + (next(&mut self.rng) % 4) as usize;
            chunk.min(self.tx_holding.len())
        } else {
            self.tx_holding.len()
        };
        let holding: Vec<usize> = self.tx_holding.drain(..batch).collect();
        self.set(regs::TDH, self.tx_head as u32);
        for index in holding {
            let at = ring + index * tx_desc::BYTES + 8;
            let word = self.desc_read(at);
            self.desc_write(
                at,
                word | ((tx_desc::STATUS_DD as u64) << tx_desc::STATUS_SHIFT),
            );
        }
        self.raise(cause::TXDW);
    }
}

/// The part, and the four views a driver is built on.
///
/// One model behind all four, because on a machine they are one device.
#[derive(Clone)]
pub struct Nic(Rc<RefCell<Model>>);

impl Nic {
    pub fn new(seed: u64) -> Self {
        Self::with(seed, Permits::default())
    }

    pub fn with(seed: u64, permits: Permits) -> Self {
        Self(Rc::new(RefCell::new(Model::new(seed, permits))))
    }

    pub fn seed(&self) -> u64 {
        self.0.borrow().seed
    }

    /// The four the driver takes.
    pub fn parts(&self) -> (Bar, Ticker, Grant, Line) {
        (
            Bar(Rc::clone(&self.0)),
            Ticker(Rc::clone(&self.0)),
            Grant(Rc::clone(&self.0)),
            Line(Rc::clone(&self.0)),
        )
    }

    /// This part has no NVM, so §10.2.5.23's "if no NVM is present" arm is
    /// what a driver reading `RAH0` finds.
    pub fn without_nvm(&self) {
        let mut model = self.0.borrow_mut();
        model.has_nvm = false;
        model.load_station_address();
    }

    /// The link came up or went away. §10.2.4.1: `LSC` "is set whenever the
    /// link status changes (either from up to down, or from down to up)".
    pub fn set_link(&self, up: bool) {
        let mut model = self.0.borrow_mut();
        if model.link_up == up {
            return;
        }
        model.link_up = up;
        model.refresh_status();
        model.raise(cause::LSC);
    }

    /// A frame arrives on the wire. It is placed when the device next runs.
    pub fn deliver(&self, frame: &[u8]) {
        self.0.borrow_mut().inbound.push_back(frame.to_vec());
    }

    /// Let the device work: place what has arrived, send what is published,
    /// write back what it holds.
    pub fn run(&self) {
        self.0.borrow_mut().run();
    }

    /// Frames the device has put on the wire, taken.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        core::mem::take(&mut self.0.borrow_mut().sent)
    }

    /// Stop the device acting on a tail register write, so it acts only when
    /// [`Self::run`] says so.
    pub fn hold(&self) {
        self.0.borrow_mut().held = true;
    }

    /// One register, read without any of its side effects — for an assertion
    /// about what the driver programmed.
    pub fn peek(&self, reg: usize) -> u32 {
        self.0.borrow().get(reg)
    }

    /// Write one receive descriptor's status word behind the driver's back, so
    /// a test can hand it a number no working part would write.
    pub fn poke_rx_status(&self, index: usize, word: u64) {
        let mut model = self.0.borrow_mut();
        let at = model.rx_ring() + index * rx_desc::BYTES + 8;
        model.desc_write(at, word);
    }

    /// Null the data address of one receive descriptor, which is the state
    /// §7.1.7.2's null descriptor padding is defined over.
    pub fn null_rx_buffer(&self, index: usize) {
        let mut model = self.0.borrow_mut();
        let at = model.rx_ring() + index * rx_desc::BYTES;
        model.desc_write(at, 0);
    }

    /// §7.4.5's spurious interrupt: a message whose cause has already been
    /// cleared, so `ICR` says nothing at all when the driver reads it.
    pub fn spurious(&self) {
        let mut model = self.0.borrow_mut();
        model.messages = model.messages.saturating_add(1);
    }

    /// Bytes of the grant, for an assertion about a frame's content.
    pub fn bytes(&self, at: usize, len: usize) -> Vec<u8> {
        self.0.borrow().memory[at..at + len].to_vec()
    }

    /// Put bytes in the grant, the way the driver's caller fills a transmit
    /// buffer.
    pub fn put_bytes(&self, at: usize, bytes: &[u8]) {
        self.0.borrow_mut().memory[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// Say what happened, with the seed that reproduces it.
    pub fn because(&self, what: &str) -> std::string::String {
        format!("seed {}: {what}", self.seed())
    }
}

pub struct Bar(Rc<RefCell<Model>>);

impl Registers for Bar {
    fn len(&self) -> usize {
        regs::REGISTER_BYTES
    }

    fn read(&self, reg: usize) -> u32 {
        self.0.borrow_mut().read(reg)
    }

    fn write(&self, reg: usize, value: u32) {
        self.0.borrow_mut().write(reg, value);
    }
}

pub struct Ticker(Rc<RefCell<Model>>);

impl Clock for Ticker {
    fn nanos(&self) -> u64 {
        let mut model = self.0.borrow_mut();
        model.nanos += CLOCK_STEP_NANOS;
        model.nanos
    }
}

pub struct Grant(Rc<RefCell<Model>>);

impl DmaBuffers for Grant {
    fn len(&self) -> usize {
        self.0.borrow().memory.len()
    }

    fn device_addr(&self, at: usize) -> u64 {
        DEVICE_BASE + at as u64
    }

    fn read(&self, at: usize) -> u64 {
        self.0.borrow().desc_read(at)
    }

    fn write(&self, at: usize, word: u64) {
        self.0.borrow_mut().desc_write(at, word);
    }

    // The host has one memory and one observer of it, so the two barriers are
    // where they are on a machine — at the boundary — and cost nothing here.
    fn publish(&self) {}
    fn observe(&self) {}
}

pub struct Line(Rc<RefCell<Model>>);

impl Interrupts for Line {
    fn taken(&self) -> u32 {
        core::mem::take(&mut self.0.borrow_mut().messages)
    }
}
