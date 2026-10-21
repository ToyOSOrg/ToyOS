//! The devices the stub's ports hold, each written from its class's
//! specification: a HID keyboard (HID 1.11, its report descriptor Appendix
//! E.6's), and a stick, SCSI over Bulk-Only (BOT 1.0, SPC-4, SBC-3) on a disk
//! in memory.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use super::{Function, Reply};

/// HID 1.11 Appendix E.6, the boot keyboard's report descriptor.
pub const E6_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
    0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05,
    0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19,
    0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

fn device_descriptor(packet: u8, vendor: u16, product: u16, serial: u8) -> Vec<u8> {
    let [v0, v1] = vendor.to_le_bytes();
    let [p0, p1] = product.to_le_bytes();
    vec![18, 1, 0x00, 0x02, 0, 0, 0, packet, v0, v1, p0, p1, 0x00, 0x01, 0, 0, serial, 1]
}

fn string(text: &str) -> Vec<u8> {
    let mut out = vec![0, 3];
    for unit in text.encode_utf16() {
        out.extend(unit.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}

/// The requests every device answers alike (USB 2.0 §9.4): its descriptors,
/// SET_CONFIGURATION and CLEAR_FEATURE.
fn standard(setup: [u8; 8], device: &[u8], config: &[u8], serial: Option<&str>) -> Option<Reply> {
    let (kind, request, value) = (setup[0], setup[1], u16::from_le_bytes([setup[2], setup[3]]));
    let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
    let answer = |bytes: Vec<u8>| Some(Reply::Data(bytes.into_iter().take(length).collect()));
    match (kind, request, value >> 8, value & 0xFF) {
        (0x80, 6, 1, _) => answer(device.to_vec()),
        (0x80, 6, 2, _) => answer(config.to_vec()),
        (0x80, 6, 3, 0) => answer(vec![4, 3, 0x09, 0x04]),
        (0x80, 6, 3, 3) => serial.map(string).map(Reply::Data).map(|r| match r {
            Reply::Data(b) => Reply::Data(b.into_iter().take(length).collect()),
            other => other,
        }),
        (0x00, 9, ..) | (0x02, 1, ..) => Some(Reply::Data(Vec::new())),
        _ => None,
    }
}

/// A keyboard whose reports a test pushes.
pub struct Keyboard {
    pub reports: Rc<RefCell<VecDeque<Vec<u8>>>>,
    /// Every Setup packet it was sent.
    pub requests: Rc<RefCell<Vec<[u8; 8]>>>,
    /// Whether it answers SET_IDLE, which is optional outside the boot
    /// subclass (HID 1.11 §7.2).
    pub stalls_idle: bool,
}

impl Keyboard {
    pub fn new() -> (Self, Rc<RefCell<VecDeque<Vec<u8>>>>) {
        let reports = Rc::new(RefCell::new(VecDeque::new()));
        (Self { reports: reports.clone(), requests: Rc::default(), stalls_idle: false }, reports)
    }

    fn config() -> Vec<u8> {
        let [l0, l1] = (E6_KEYBOARD.len() as u16).to_le_bytes();
        let mut c = vec![9, 2, 34, 0, 1, 1, 0, 0xA0, 50];
        c.extend([9, 4, 0, 0, 1, 3, 1, 1, 0]);
        c.extend([9, 0x21, 0x11, 0x01, 0, 1, 0x22, l0, l1]);
        c.extend([7, 5, 0x81, 3, 8, 0, 10]);
        c
    }
}

impl Function for Keyboard {
    fn control(&mut self, setup: [u8; 8], _: &[u8]) -> Reply {
        self.requests.borrow_mut().push(setup);
        let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
        if let Some(reply) = standard(setup, &device_descriptor(8, 0x1209, 0x0001, 0), &Self::config(), None) {
            return reply;
        }
        match (setup[0], setup[1], setup[3]) {
            (0x81, 6, 0x22) => Reply::Data(E6_KEYBOARD.iter().copied().take(length).collect()),
            (0x21, 0x0A, _) if self.stalls_idle => Reply::Stall,
            (0x21, 0x0A | 0x0B, _) => Reply::Data(Vec::new()),
            _ => Reply::Stall,
        }
    }

    fn transfer(&mut self, address: u8, _: Option<&[u8]>, _: usize) -> Reply {
        assert_eq!(address, 0x81, "a keyboard has one endpoint");
        match self.reports.borrow_mut().pop_front() {
            Some(report) => Reply::Data(report),
            None => Reply::Wait,
        }
    }
}

/// What a stick's bulk pipes do, which a test sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Pipes {
    #[default]
    Answer,
    /// The Bulk-In endpoint never answers: every IN transfer waits forever.
    SilentIn,
    /// The Bulk-Out endpoint never answers.
    SilentOut,
}

enum Bot {
    Idle,
    DataIn { data: Vec<u8>, csw: [u8; 13] },
    DataOut { lba: u64, len: usize, csw: [u8; 13] },
    Status([u8; 13]),
}

/// A stick: one logical unit of 512-byte sectors over Bulk-Only.
pub struct Stick {
    pub disk: Rc<RefCell<Vec<u8>>>,
    pub pipes: Rc<Cell<Pipes>>,
    pub serial: String,
    /// CBWs it took, by operation code.
    pub commands: Rc<RefCell<Vec<u8>>>,
    state: Bot,
    sense: (u8, u8, u8),
}

const SECTOR: usize = 512;

impl Stick {
    pub fn new(disk: Vec<u8>, serial: &str) -> Self {
        Self {
            disk: Rc::new(RefCell::new(disk)),
            pipes: Rc::default(),
            serial: serial.into(),
            commands: Rc::default(),
            state: Bot::Idle,
            sense: (0, 0, 0),
        }
    }

    fn config() -> Vec<u8> {
        let mut c = vec![9, 2, 32, 0, 1, 1, 0, 0xC0, 50];
        c.extend([9, 4, 0, 0, 2, 0x08, 0x06, 0x50, 0]);
        c.extend([7, 5, 0x81, 2, 0x00, 0x02, 0]);
        c.extend([7, 5, 0x02, 2, 0x00, 0x02, 0]);
        c
    }

    fn csw(tag: [u8; 4], residue: u32, status: u8) -> [u8; 13] {
        let mut csw = [0u8; 13];
        csw[..4].copy_from_slice(b"USBS");
        csw[4..8].copy_from_slice(&tag);
        csw[8..12].copy_from_slice(&residue.to_le_bytes());
        csw[12] = status;
        csw
    }

    /// The command block `cbw` carries, run (SPC-4, SBC-3).
    fn command(&mut self, cbw: &[u8]) -> Bot {
        let tag = [cbw[4], cbw[5], cbw[6], cbw[7]];
        let length = u32::from_le_bytes([cbw[8], cbw[9], cbw[10], cbw[11]]) as usize;
        let cdb = &cbw[15..31];
        self.commands.borrow_mut().push(cdb[0]);
        let sectors = self.disk.borrow().len() / SECTOR;
        let be32 = |at: usize| u32::from_be_bytes([cdb[at], cdb[at + 1], cdb[at + 2], cdb[at + 3]]);
        let reply_in = |mut data: Vec<u8>, sense: &mut (u8, u8, u8)| {
            *sense = (0, 0, 0);
            data.truncate(length);
            let residue = (length - data.len()) as u32;
            Bot::DataIn { data, csw: Self::csw(tag, residue, 0) }
        };
        match cdb[0] {
            0x00 | 0x35 => {
                self.sense = (0, 0, 0);
                Bot::Status(Self::csw(tag, 0, 0))
            }
            0x03 => {
                let mut sense = vec![0u8; 18];
                sense[0] = 0x70;
                sense[2] = self.sense.0;
                sense[7] = 10;
                sense[12] = self.sense.1;
                sense[13] = self.sense.2;
                reply_in(sense, &mut self.sense)
            }
            0x12 => {
                let mut data = vec![0u8; 36];
                data[4] = 31;
                data[8..16].copy_from_slice(b"ToyStub ");
                data[16..32].copy_from_slice(b"Stick           ");
                data[32..36].copy_from_slice(b"1.00");
                reply_in(data, &mut self.sense)
            }
            0x25 => {
                let mut data = ((sectors - 1) as u32).to_be_bytes().to_vec();
                data.extend((SECTOR as u32).to_be_bytes());
                reply_in(data, &mut self.sense)
            }
            0x28 => {
                let (lba, count) = (be32(2) as usize, usize::from(u16::from_be_bytes([cdb[7], cdb[8]])));
                let data = self.disk.borrow()[lba * SECTOR..(lba + count) * SECTOR].to_vec();
                reply_in(data, &mut self.sense)
            }
            0x2A => {
                let count = usize::from(u16::from_be_bytes([cdb[7], cdb[8]]));
                Bot::DataOut { lba: u64::from(be32(2)), len: count * SECTOR, csw: Self::csw(tag, 0, 0) }
            }
            _ => {
                // ILLEGAL REQUEST, INVALID COMMAND OPERATION CODE.
                self.sense = (0x05, 0x20, 0x00);
                Bot::Status(Self::csw(tag, length as u32, 1))
            }
        }
    }
}

impl Function for Stick {
    fn reset(&mut self) {
        self.state = Bot::Idle;
    }

    fn control(&mut self, setup: [u8; 8], _: &[u8]) -> Reply {
        let device = device_descriptor(64, 0x1209, 0x0002, 3);
        if let Some(reply) = standard(setup, &device, &Self::config(), Some(&self.serial)) {
            return reply;
        }
        match (setup[0], setup[1]) {
            // Bulk-Only Mass Storage Reset (§3.1): ready for the next CBW.
            (0x21, 0xFF) => {
                self.state = Bot::Idle;
                Reply::Data(Vec::new())
            }
            _ => Reply::Stall,
        }
    }

    fn transfer(&mut self, address: u8, out: Option<&[u8]>, max: usize) -> Reply {
        match (address, self.pipes.get()) {
            (0x81, Pipes::SilentIn) | (0x02, Pipes::SilentOut) => return Reply::Wait,
            _ => {}
        }
        match (address, out, std::mem::replace(&mut self.state, Bot::Idle)) {
            (0x02, Some(cbw), Bot::Idle) if cbw.len() == 31 && &cbw[..4] == b"USBC" => {
                self.state = self.command(cbw);
                Reply::Data(Vec::new())
            }
            (0x02, Some(data), Bot::DataOut { lba, len, csw }) => {
                let take = data.len().min(len);
                let at = lba as usize * SECTOR;
                self.disk.borrow_mut()[at..at + take].copy_from_slice(&data[..take]);
                self.state = Bot::Status(csw);
                Reply::Data(Vec::new())
            }
            (0x81, None, Bot::DataIn { data, csw }) => {
                self.state = Bot::Status(csw);
                Reply::Data(data.into_iter().take(max).collect())
            }
            (0x81, None, Bot::Status(csw)) => Reply::Data(csw.to_vec()),
            (_, _, state) => {
                // Out of step: §6.6.1 has the device stall what it was not
                // expecting.
                self.state = state;
                Reply::Stall
            }
        }
    }
}

/// A disk of `sectors` 512-byte sectors carrying a GPT (UEFI 2.10 §5.3) that
/// names one partition, `unique`, from LBA `first` for `count` sectors.
pub fn gpt_disk(sectors: u64, unique: [u8; 16], first: u64, count: u64) -> Vec<u8> {
    let mut disk = vec![0u8; sectors as usize * SECTOR];
    let mut entries = vec![0u8; 128 * 128];
    entries[..16].copy_from_slice(&[0x11; 16]);
    entries[16..32].copy_from_slice(&unique);
    entries[32..40].copy_from_slice(&first.to_le_bytes());
    entries[40..48].copy_from_slice(&(first + count - 1).to_le_bytes());
    let entries_crc = toyos_gpt::crc32(&entries);
    let last = sectors - 1;
    let header = |mine: u64, other: u64, entries_at: u64| {
        let mut h = vec![0u8; 92];
        h[..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&mine.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&(last - 33).to_le_bytes());
        h[56..72].copy_from_slice(&[0x22; 16]);
        h[72..80].copy_from_slice(&entries_at.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = toyos_gpt::crc32(&h);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    // A protective MBR (§5.2.3), whose one entry covers the disk.
    disk[446 + 4] = 0xEE;
    disk[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    disk[446 + 12..446 + 16].copy_from_slice(&((sectors - 1).min(u64::from(u32::MAX)) as u32).to_le_bytes());
    disk[510] = 0x55;
    disk[511] = 0xAA;
    let at = |lba: u64| lba as usize * SECTOR;
    disk[at(1)..at(1) + 92].copy_from_slice(&header(1, last, 2));
    disk[at(2)..at(2) + entries.len()].copy_from_slice(&entries);
    disk[at(last - 32)..at(last - 32) + entries.len()].copy_from_slice(&entries);
    disk[at(last)..at(last) + 92].copy_from_slice(&header(last, 1, last - 32));
    disk
}
