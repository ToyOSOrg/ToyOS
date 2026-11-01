//! A Tiger Lake display engine as far as this driver reaches it, from
//! IHD-OS-TGL-Vol 2c-12.21 and Vol 12-12.21: a register file, a frame counter
//! advancing at the mode's rate, pcode's GT driver mailbox, power wells whose
//! state follows the ORed requests, and the DMC's program storage.
//!
//! The mailbox may stay busy for some reads after a command, as polling
//! "for Run/Busy indication=0b" permits; every write is traced so a test can
//! judge the order of the driver's writes.

extern crate std;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::vec;
use std::vec::Vec;

use toyos_device_memory::Registers;

use crate::regs;
use crate::Clock;

/// pcode's answers.
#[derive(Clone, Copy)]
pub struct Pcode {
    pub latency_first: u32,
    pub latency_second: u32,
    pub sagv_block_us: u32,
    /// Reads of the interface that still show Run/Busy after a command.
    pub busy_reads: u32,
    /// The error code a command completes with; 0 is success.
    pub error: u32,
}

pub struct Engine {
    pub regs: RefCell<BTreeMap<usize, u32>>,
    pub writes: RefCell<Vec<(usize, u32)>>,
    now: Cell<u64>,
    /// None for a stopped pipe.
    frame_nanos: Option<u64>,
    pcode: Pcode,
    busy: Cell<u32>,
    /// Wells a requester the driver cannot see holds on, as state bits.
    pub held_wells: Cell<u32>,
    /// The program storage drops every write, as a window that misses it.
    pub storage_drops: Cell<bool>,
    /// The driver is preempted once after its first read of a well's state
    /// following a withdrawal: that read still shows the well on, and the
    /// clock reading after it is 30 µs later. The well is off by then.
    pub preempted: Cell<bool>,
    stale: Cell<Option<u32>>,
    jump: Cell<u64>,
    /// DISPLAY_INT_CTL categories pending beyond pipe A's.
    pub other_pending: Cell<u32>,
    pub panel: Panel,
}

/// The eDP panel on AUX A: its DPCD, and how it answers.
#[derive(Default)]
pub struct Panel {
    pub dpcd: RefCell<BTreeMap<u32, u8>>,
    /// Every native write, in order.
    pub written: RefCell<Vec<(u32, u8)>>,
    /// Requests answered DEFER before the next is served.
    pub defers: Cell<u32>,
    /// No reply at all: every transaction times out.
    pub silent: Cell<bool>,
}

impl Panel {
    /// A PSR1 panel: DPCD 1.4, eDP 1.4, SET_POWER capable, a 330 µs setup time, no
    /// errors. The T14's own values are read on the bench.
    fn psr1() -> Self {
        let panel = Self::default();
        panel.dpcd.borrow_mut().extend([(0x000, 0x14), (0x070, 0x01), (0x071, 0x00), (0x700, 0x03), (0x701, 0x80)]);
        panel
    }

    /// The reply to `request`.
    fn answer(&self, request: &[u8]) -> Vec<u8> {
        if self.defers.get() > 0 {
            self.defers.set(self.defers.get() - 1);
            return vec![0x20];
        }
        let address = u32::from(request[0] & 0xF) << 16 | u32::from(request[1]) << 8 | u32::from(request[2]);
        let len = usize::from(request[3]) + 1;
        match request[0] >> 4 {
            0x9 => {
                let dpcd = self.dpcd.borrow();
                core::iter::once(0).chain((0..len as u32).map(|n| dpcd.get(&(address + n)).copied().unwrap_or(0))).collect()
            }
            0x8 => {
                for (n, &byte) in request[4..4 + len].iter().enumerate() {
                    self.dpcd.borrow_mut().insert(address + n as u32, byte);
                    self.written.borrow_mut().push((address + n as u32, byte));
                }
                vec![0x00]
            }
            other => panic!("the model's panel has no command {other:#x}"),
        }
    }
}

impl Engine {
    /// The T14's GOP as the read-only scout read it (2026-10-31).
    pub fn t14() -> Self {
        let mut r = BTreeMap::new();
        r.insert(regs::trans_conf(0), 0xc000_0024);
        r.insert(regs::TRANS_HTOTAL_A, 0x081f_077f);
        r.insert(regs::TRANS_VTOTAL_A, 0x0457_0437);
        // Vertical blank starting at vertical active is assumed; the scout
        // did not read TRANS_VBLANK.
        r.insert(regs::TRANS_VBLANK_A, 0x0457_0437);
        r.insert(regs::WM_LINETIME_A, 0x78);
        r.insert(regs::plane_ctl(0, 1), 0x8400_0008);
        r.insert(regs::plane_size(0, 1), 0x0437_077f);
        r.insert(regs::plane_wm(0, 1, 0), 0x8000_4011);
        r.insert(regs::plane_wm(0, 1, 1), 0x0000_4011);
        for level in 2..regs::LEVELS {
            r.insert(regs::plane_wm(0, 1, level), 0x0000_4007);
        }
        r.insert(regs::plane_buf_cfg(0, 1), 0x02a9_0000);
        r.insert(regs::PWR_WELL_CTL1, 0x55);
        r.insert(regs::PWR_WELL_CTL2, 0xff);
        // VGA_CONTROL's default; the scout read neither it nor the DDI. DDI A
        // in DP SST is assumed, and read on the bench.
        r.insert(regs::VGA_CONTROL, regs::VGA_DISPLAY_DISABLE);
        r.insert(regs::TRANS_DDI_FUNC_CTL_A, 1 << 31 | 1 << regs::DDI_SELECT_SHIFT | 0b010 << 24);
        // Enabled, which the driver clears after PG3; the scout did not read it.
        r.insert(regs::AUDIO_PIN_BUF_CTL, regs::AUDIO_BUFFER_ENABLE);
        r.insert(crate::dmc::WINDOW.start, 0x0c00_4040);
        r.insert(crate::dmc::WINDOW.start + 4, 0x0001_0002);
        let pcode = Pcode { latency_first: 0x3333_3300, latency_second: 0x706b_4633, sagv_block_us: 11, busy_reads: 3, error: 0 };
        // 2080 × 1112 pixels at 138.78 MHz (LINK M/N × HBR's 270 MHz).
        Self::new(r, Some(16_666_667), pcode)
    }

    pub fn new(regs: BTreeMap<usize, u32>, frame_nanos: Option<u64>, pcode: Pcode) -> Self {
        Self {
            regs: RefCell::new(regs),
            writes: RefCell::new(Vec::new()),
            now: Cell::new(1_000_000),
            frame_nanos,
            pcode,
            busy: Cell::new(0),
            held_wells: Cell::new(0),
            storage_drops: Cell::new(false),
            preempted: Cell::new(false),
            stale: Cell::new(None),
            jump: Cell::new(0),
            other_pending: Cell::new(0),
            panel: Panel::psr1(),
        }
    }

    pub fn set(&self, at: usize, value: u32) {
        self.regs.borrow_mut().insert(at, value);
    }

    pub fn get(&self, at: usize) -> u32 {
        self.regs.borrow().get(&at).copied().unwrap_or(0)
    }

    pub fn written(&self) -> Vec<(usize, u32)> {
        self.writes.borrow().iter().copied().filter(|(at, _)| !(regs::MAILBOX_INTERFACE..=regs::MAILBOX_DATA1).contains(at)).collect()
    }

    fn command(&self, interface: u32) {
        let data0 = self.get(regs::MAILBOX_DATA0);
        let answer = match interface & 0xFF {
            0x06 if data0 == 0 => self.pcode.latency_first,
            0x06 => self.pcode.latency_second,
            0x23 => self.pcode.sagv_block_us,
            other => panic!("the model's pcode has no command {other:#x}"),
        };
        self.set(regs::MAILBOX_DATA0, answer);
        self.set(regs::MAILBOX_INTERFACE, regs::RUN_BUSY | self.pcode.error);
        self.busy.set(self.pcode.busy_reads);
    }
}

impl Engine {
    /// One AUX transaction: the request out of the data registers, the
    /// panel's reply into them, and the control register done.
    fn aux(&self, control: u32) {
        let size = ((control >> regs::AUX_SIZE_SHIFT) & regs::AUX_SIZE) as usize;
        let request: Vec<u8> = (0..5).flat_map(|n| self.get(regs::DDI_AUX_DATA_A + 4 * n).to_be_bytes()).take(size).collect();
        let idle = control & !(regs::AUX_SEND_BUSY | regs::AUX_DONE | regs::AUX_TIMEOUT | regs::AUX_RECEIVE_ERROR) & !(regs::AUX_SIZE << regs::AUX_SIZE_SHIFT);
        if self.panel.silent.get() {
            self.set(regs::DDI_AUX_CTL_A, idle | regs::AUX_DONE | regs::AUX_TIMEOUT);
            return;
        }
        let reply = self.panel.answer(&request);
        for (n, chunk) in reply.chunks(4).enumerate() {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            self.set(regs::DDI_AUX_DATA_A + 4 * n, u32::from_be_bytes(word));
        }
        self.set(regs::DDI_AUX_CTL_A, idle | regs::AUX_DONE | (reply.len() as u32) << regs::AUX_SIZE_SHIFT);
    }

    /// A vertical blank begins: pipe A's identity bit is set where its mask
    /// lets it through.
    pub fn vblank(&self) {
        if self.get(regs::PIPE_A_IMR) & regs::VBLANK == 0 {
            self.set(regs::PIPE_A_IIR, self.get(regs::PIPE_A_IIR) | regs::VBLANK);
        }
    }

    fn display_pending(&self) -> u32 {
        let pipe_a = self.get(regs::PIPE_A_IIR) & self.get(regs::PIPE_A_IER) != 0;
        self.other_pending.get() | if pipe_a { regs::PIPE_A_PENDING } else { 0 }
    }
}

impl Engine {
    /// A request written: every instance shows the state of the ORed requests,
    /// and a well a hidden requester holds stays on.
    fn request(&self, at: usize, value: u32) {
        const REQUESTS: u32 = 0x2AA;
        if self.preempted.replace(false) {
            self.stale.set(Some(self.get(regs::PWR_WELL_CTL2)));
        }
        self.set(at, value & REQUESTS);
        let requested = self.get(regs::PWR_WELL_CTL1) | self.get(regs::PWR_WELL_CTL2);
        let state = (requested & REQUESTS) >> 1 | self.held_wells.get();
        for ctl in [regs::PWR_WELL_CTL1, regs::PWR_WELL_CTL2] {
            self.set(ctl, self.get(ctl) & REQUESTS | state);
        }
    }
}

impl Registers for Engine {
    fn bytes(&self) -> usize {
        regs::SPAN
    }
    fn read8(&self, _: usize) -> u8 {
        unreachable!("the driver reads only 32-bit registers")
    }
    fn read16(&self, _: usize) -> u16 {
        unreachable!("the driver reads only 32-bit registers")
    }
    fn read32(&self, at: usize) -> u32 {
        assert!(at.is_multiple_of(4) && at < regs::SPAN, "an access at {at:#x}");
        if at == regs::PWR_WELL_CTL2 {
            if let Some(stale) = self.stale.take() {
                self.jump.set(30_000);
                return stale;
            }
        }
        if at == regs::DISPLAY_INT_CTL {
            return self.get(at) & regs::DISPLAY_INTERRUPT_ENABLE | self.display_pending();
        }
        if at == regs::GFX_MSTR_INTR {
            let display = self.get(regs::DISPLAY_INT_CTL) & regs::DISPLAY_INTERRUPT_ENABLE != 0 && self.display_pending() != 0;
            return self.get(at) & regs::PRIMARY_INTERRUPT | if display { regs::DISPLAY_PENDING } else { 0 };
        }
        if at == regs::PIPE_FRMCNT_A {
            return self.frame_nanos.map_or(0x276, |period| (self.now.get() / period) as u32);
        }
        if at == regs::MAILBOX_INTERFACE && self.get(at) & regs::RUN_BUSY != 0 {
            if self.busy.get() == 0 {
                let done = self.get(at) & !regs::RUN_BUSY;
                self.set(at, done);
                return done;
            }
            self.busy.set(self.busy.get() - 1);
        }
        self.get(at)
    }
    fn write8(&self, _: usize, _: u8) {
        unreachable!("the driver writes only 32-bit registers")
    }
    fn write16(&self, _: usize, _: u16) {
        unreachable!("the driver writes only 32-bit registers")
    }
    fn write32(&self, at: usize, value: u32) {
        assert!(at.is_multiple_of(4) && at < regs::SPAN, "an access at {at:#x}");
        self.writes.borrow_mut().push((at, value));
        if at == regs::MAILBOX_INTERFACE {
            assert!(self.get(at) & regs::RUN_BUSY == 0, "a command written while the mailbox was busy");
            assert!(value & regs::RUN_BUSY != 0, "a command written without Run/Busy");
            self.command(value);
            return;
        }
        if at == regs::PWR_WELL_CTL1 || at == regs::PWR_WELL_CTL2 {
            self.request(at, value);
            return;
        }
        if at == regs::PIPE_A_IIR {
            self.set(at, self.get(at) & !value);
            return;
        }
        if at == regs::PWR_WELL_CTL_AUX2 {
            self.set(at, value & regs::AUX_A_REQUEST | (value & regs::AUX_A_REQUEST) >> 1);
            return;
        }
        if at == regs::DDI_AUX_CTL_A && value & regs::AUX_SEND_BUSY != 0 {
            self.aux(value);
            return;
        }
        if self.storage_drops.get() && crate::dmc::WINDOW.contains(&at) {
            return;
        }
        self.set(at, value);
    }
}

/// Every reading of the clock is a microsecond later, so a busy wait ends.
impl Clock for Engine {
    fn nanos(&self) -> u64 {
        self.now.set(self.now.get() + 1_000 + self.jump.take());
        self.now.get()
    }
    fn pause(&self, nanos: u64) {
        self.now.set(self.now.get() + nanos);
    }
}
