//! A Tiger Lake display engine as far as this driver reaches it, from
//! IHD-OS-TGL-Vol 2c-12.21 and Vol 12-12.21: a register file, a frame counter
//! advancing at the mode's rate, and pcode's GT driver mailbox.
//!
//! The mailbox may stay busy for some reads after a command, as polling
//! "for Run/Busy indication=0b" permits; every write is traced so a test can
//! judge the order of the driver's writes.

extern crate std;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
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
}

impl Engine {
    /// The T14's GOP as the read-only scout read it (2026-10-31).
    pub fn t14() -> Self {
        let mut r = BTreeMap::new();
        r.insert(regs::trans_conf(0), 0xc000_0024);
        r.insert(regs::TRANS_HTOTAL_A, 0x081f_077f);
        r.insert(regs::TRANS_VTOTAL_A, 0x0457_0437);
        r.insert(regs::WM_LINETIME_A, 0x78);
        r.insert(regs::plane_ctl(0, 1), 0x8400_0008);
        r.insert(regs::plane_stride(0, 1), 0x78);
        r.insert(regs::plane_size(0, 1), 0x0437_077f);
        r.insert(regs::plane_wm(0, 1, 0), 0x8000_4011);
        r.insert(regs::plane_wm(0, 1, 1), 0x0000_4011);
        for level in 2..regs::LEVELS {
            r.insert(regs::plane_wm(0, 1, level), 0x0000_4007);
        }
        r.insert(regs::plane_buf_cfg(0, 1), 0x02a9_0000);
        let pcode = Pcode { latency_first: 0x3333_3300, latency_second: 0x706b_4633, sagv_block_us: 11, busy_reads: 3, error: 0 };
        // 2080 × 1112 pixels at 138.78 MHz (LINK M/N × HBR's 270 MHz).
        Self::new(r, Some(16_666_667), pcode)
    }

    pub fn new(regs: BTreeMap<usize, u32>, frame_nanos: Option<u64>, pcode: Pcode) -> Self {
        Self { regs: RefCell::new(regs), writes: RefCell::new(Vec::new()), now: Cell::new(1_000_000), frame_nanos, pcode, busy: Cell::new(0) }
    }

    pub fn set(&self, at: usize, value: u32) {
        self.regs.borrow_mut().insert(at, value);
    }

    pub fn get(&self, at: usize) -> u32 {
        self.regs.borrow().get(&at).copied().unwrap_or(0)
    }

    pub fn written(&self) -> Vec<(usize, u32)> {
        self.writes.borrow().iter().copied().filter(|(at, _)| !(regs::MAILBOX_INTERFACE..regs::SPAN).contains(at)).collect()
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
        self.set(at, value);
    }
}

/// Every reading of the clock is a microsecond later, so a busy wait ends.
impl Clock for Engine {
    fn nanos(&self) -> u64 {
        self.now.set(self.now.get() + 1_000);
        self.now.get()
    }
    fn pause(&self, nanos: u64) {
        self.now.set(self.now.get() + nanos);
    }
}
