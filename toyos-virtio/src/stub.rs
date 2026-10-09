//! A virtio PCI device, as the specification describes it — not as the driver
//! beside it expects it.
//!
//! Written from *VIRTIO Version 1.2* and cited to it clause by clause. **Where
//! the specification binds the driver, this file is what holds it to that**: an
//! access of the wrong width, a write to a read-only field, a feature accepted
//! that was not offered, a status bit cleared, a queue enabled before it was
//! configured, a notification before `DRIVER_OK` — each is an assertion here,
//! by section, and a test that reaches one is red. Where the specification
//! binds the *device*, [`Permits`] is how a test has it break the rule, and
//! the raw ring writes below are how a test has it say anything at all.
//!
//! The grant is the same object: the registers and the memory are one
//! machine's, and one [`Event`] trace orders what the driver did to both.

use std::cell::RefCell;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use toyos_device_memory::{DmaBuffers, Registers};

use crate::pci::{status, Source, VendorCap, VIRTIO_F_ACCESS_PLATFORM, VIRTIO_F_VERSION_1};
use crate::queue::{
    Buffer, Parts, AVAIL_ENTRY_BYTES, DESC_BYTES, RING_ENTRIES, USED_ELEM_BYTES,
};

/// Where the device reaches the grant. Not zero: a driver that told the
/// device a grant offset instead of a device address is then not inside it.
pub const DEVICE_BASE: u64 = 0x0000_0001_0000_0000;
pub const GRANT_BYTES: usize = 0x8000;

/// The BAR, laid out the way a common emulated device lays its own out: a
/// page for each structure.
pub const BAR_BYTES: usize = 0x4000;
pub const COMMON_AT: u32 = 0x1000;
pub const ISR_AT: u32 = 0x0000;
pub const DEVICE_AT: u32 = 0x2000;
pub const NOTIFY_AT: u32 = 0x3000;
pub const NOTIFY_BYTES: u32 = 0x1000;
pub const NOTIFY_OFF_MULTIPLIER: u32 = 4;
pub const QUEUES: usize = 3;
pub const QUEUE_MAX: u16 = 256;

/// §4.1.5.1.2: what a vector field reads when the device mapped none.
const NO_VECTOR: u16 = 0xFFFF;

/// A device-type feature bit, and one the device does not offer.
pub const FEATURE_OFFERED: u64 = 1 << 5;
pub const FEATURE_WITHHELD: u64 = 1 << 7;

/// What the driver did, in the order it did it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A write of `device_status`.
    Status(u8),
    /// A write of any other common-configuration field, by its offset.
    Common { field: usize, value: u32 },
    /// A read of the device-specific structure.
    DeviceConfig { at: usize },
    /// A store into the grant.
    Store { at: usize, bytes: usize, value: u64 },
    /// A load from the grant.
    Load { at: usize, bytes: usize },
    /// [`DmaBuffers::publish`].
    Publish,
    /// [`DmaBuffers::observe`].
    Observe,
    /// An available-buffer notification (§4.1.5.2).
    Notify { at: usize, value: u16 },
}

/// What a device does that the specification forbids it, or leaves open.
#[derive(Clone, Copy, Default)]
pub struct Permits {
    /// §4.1.4.3.1 has `device_status` read 0 once the reset is done. This one
    /// is never done.
    pub reset_never_completes: bool,
    /// §2.2.2: the device "MUST fail to set the FEATURES_OK device status bit"
    /// for a subset it does not take.
    pub refuses_features: bool,
    /// §4.1.5.1.2: mapping a vector "could fail", and then reads `NO_VECTOR`.
    pub no_vector_for: Option<Source>,
}

#[derive(Clone, Copy, Default)]
pub struct QueueRegs {
    /// What `queue_size` reads: the maximum on reset, then what was written.
    pub size: u16,
    pub vector: u16,
    pub enabled: bool,
    pub notify_off: u16,
    pub desc: u64,
    pub driver: u64,
    pub device: u64,
}

pub struct State {
    pub permits: Permits,
    pub offered: u64,
    pub accepted: u64,
    pub status: u8,
    pub config_vector: u16,
    pub queues: [QueueRegs; QUEUES],
    pub device_config: Vec<u8>,
    /// §4.1.4.4: what the notification capability carries.
    pub notify_off_multiplier: u32,
    pub trace: Vec<Event>,
    pub grant: Vec<u8>,
    device_feature_select: u32,
    driver_feature_select: u32,
    queue_select: u16,
}

/// The machine. A clone is the same machine: the driver holds one as its
/// [`Registers`], one in each queue as its [`DmaBuffers`], and the test keeps
/// one to be the device with.
#[derive(Clone)]
pub struct Machine(Rc<RefCell<State>>);

impl Machine {
    /// A device after power-on, offering [`FEATURE_OFFERED`] and the two
    /// transport bits, with a six-byte device-specific structure.
    pub fn new() -> Self {
        let mut state = State {
            permits: Permits::default(),
            offered: VIRTIO_F_VERSION_1 | VIRTIO_F_ACCESS_PLATFORM | FEATURE_OFFERED,
            accepted: 0,
            status: 0,
            config_vector: NO_VECTOR,
            queues: [QueueRegs::default(); QUEUES],
            device_config: vec![0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            notify_off_multiplier: NOTIFY_OFF_MULTIPLIER,
            trace: Vec::new(),
            grant: vec![0xA5; GRANT_BYTES],
            device_feature_select: 0,
            driver_feature_select: 0,
            queue_select: 0,
        };
        state.reset();
        Self(Rc::new(RefCell::new(state)))
    }

    /// The capabilities a walk of this device's configuration space yields,
    /// in the list's order: its four structures, and the configuration-access
    /// one (§4.1.4.9) no driver here uses.
    pub fn caps(&self) -> Vec<VendorCap> {
        let cap = |cfg_type, offset, length, notify_off_multiplier| VendorCap {
            cfg_type,
            bar: 4,
            offset,
            length,
            notify_off_multiplier,
        };
        vec![
            cap(1, COMMON_AT, 0x1000, 0),
            cap(3, ISR_AT, 0x1000, 0),
            cap(4, DEVICE_AT, self.state(|s| s.device_config.len() as u32), 0),
            cap(2, NOTIFY_AT, NOTIFY_BYTES, self.state(|s| s.notify_off_multiplier)),
            cap(5, 0, 0, 0),
        ]
    }

    pub fn state<T>(&self, read: impl FnOnce(&mut State) -> T) -> T {
        read(&mut self.0.borrow_mut())
    }

    pub fn trace(&self) -> Vec<Event> {
        self.state(|s| s.trace.clone())
    }

    pub fn forget_trace(&self) {
        self.state(|s| s.trace.clear());
    }

    /// Every `device_status` the driver wrote, in order.
    pub fn statuses(&self) -> Vec<u8> {
        let trace = self.trace();
        trace.iter().filter_map(|event| if let Event::Status(s) = event { Some(*s) } else { None }).collect()
    }

    /// The device's view of a queue's rings.
    pub fn ring(&self, parts: Parts, size: u16) -> Ring {
        Ring { machine: self.clone(), parts, size }
    }
}

impl State {
    /// §2.4.1 and §4.1.5.1.2.1: status 0, every event unmapped; §4.1.4.3.1:
    /// `queue_enable` 0 and `queue_size` the maximum.
    fn reset(&mut self) {
        self.status = 0;
        self.accepted = 0;
        self.config_vector = NO_VECTOR;
        for (index, queue) in self.queues.iter_mut().enumerate() {
            *queue = QueueRegs {
                size: QUEUE_MAX,
                vector: NO_VECTOR,
                notify_off: index as u16,
                ..QueueRegs::default()
            };
        }
    }

    fn selected(&mut self) -> Option<&mut QueueRegs> {
        self.queues.get_mut(self.queue_select as usize)
    }

    /// §4.1.3.1: each field at its own width, a sixty-four-bit one as two
    /// halves.
    fn common_read(&mut self, field: usize, bytes: usize) -> u32 {
        let queue = self.selected().map(|q| *q);
        let half = |value: u64, high: bool| if high { (value >> 32) as u32 } else { value as u32 };
        let (width, value) = match field {
            0x00 => (4, self.device_feature_select),
            // §4.1.4.3.1: 0 for any select but 0 and 1.
            0x04 => (4, match self.device_feature_select {
                0 => self.offered as u32,
                1 => (self.offered >> 32) as u32,
                _ => 0,
            }),
            0x08 => (4, self.driver_feature_select),
            0x0C => (4, half(self.accepted, self.driver_feature_select == 1)),
            0x10 => (2, self.config_vector as u32),
            0x12 => (2, QUEUES as u32),
            0x14 => (1, self.status as u32),
            0x15 => (1, 0),
            0x16 => (2, self.queue_select as u32),
            // §4.1.4.3.1: 0 for a queue the device does not have.
            0x18 => (2, queue.map_or(0, |q| q.size as u32)),
            0x1A => (2, queue.map_or(NO_VECTOR as u32, |q| q.vector as u32)),
            0x1C => (2, queue.map_or(0, |q| q.enabled as u32)),
            0x1E => (2, queue.map_or(0, |q| q.notify_off as u32)),
            0x20 | 0x24 => (4, half(queue.map_or(0, |q| q.desc), field == 0x24)),
            0x28 | 0x2C => (4, half(queue.map_or(0, |q| q.driver), field == 0x2C)),
            0x30 | 0x34 => (4, half(queue.map_or(0, |q| q.device), field == 0x34)),
            _ => panic!("stub: a read at {field:#x} of the common structure is of no field"),
        };
        assert_eq!(bytes, width, "stub: §4.1.3.1: a {bytes}-byte read of the {width}-byte field at {field:#x}");
        value
    }

    fn common_write(&mut self, field: usize, bytes: usize, value: u32) {
        let width = match field {
            0x00 | 0x08 | 0x0C | 0x20..=0x37 if field.is_multiple_of(4) => 4,
            0x10 | 0x16 | 0x18 | 0x1A | 0x1C => 2,
            0x14 => 1,
            0x04 | 0x12 | 0x15 | 0x1E => {
                panic!("stub: §4.1.4.3.2: a write to the read-only field at {field:#x}")
            }
            _ => panic!("stub: a write at {field:#x} of the common structure is of no field"),
        };
        assert_eq!(bytes, width, "stub: §4.1.3.1: a {bytes}-byte write of the {width}-byte field at {field:#x}");
        if field == 0x14 {
            return self.write_status(value as u8);
        }
        self.trace.push(Event::Common { field, value });
        let (live, settled) = (self.status & status::DRIVER_OK != 0, self.status & status::FEATURES_OK != 0);
        let set_half = |whole: &mut u64, high: bool| {
            *whole = if high {
                (*whole & 0xFFFF_FFFF) | (value as u64) << 32
            } else {
                (*whole & !0xFFFF_FFFF) | value as u64
            }
        };
        match field {
            0x00 => self.device_feature_select = value,
            0x08 => self.driver_feature_select = value,
            0x0C => {
                assert!(
                    self.status & status::DRIVER != 0 && !settled,
                    "stub: §3.1.1: features written with DEVICE_STATUS={:#x}",
                    self.status
                );
                let high = self.driver_feature_select == 1;
                let offered = if high { (self.offered >> 32) as u32 } else { self.offered as u32 };
                assert_eq!(value & !offered, 0, "stub: §2.2.1: accepted a feature never offered");
                set_half(&mut self.accepted, high);
            }
            0x10 => {
                self.config_vector =
                    if self.permits.no_vector_for == Some(Source::Config) { NO_VECTOR } else { value as u16 };
            }
            0x16 => self.queue_select = value as u16,
            _ => {
                let refuses = self.permits.no_vector_for == Some(Source::Queue(self.queue_select));
                let select = self.queue_select;
                let queue = self
                    .selected()
                    .unwrap_or_else(|| panic!("stub: a write to queue {select}, which this device has not"));
                assert!(!queue.enabled, "stub: §4.1.4.3.2: queue {select} configured after its enable");
                assert!(settled && !live, "stub: §3.1.1: queue {select} configured outside step 7");
                match field {
                    0x18 => {
                        assert!(
                            (value as u16).is_power_of_two() && value as u16 <= QUEUE_MAX,
                            "stub: §4.1.4.3.2: queue size {value}"
                        );
                        queue.size = value as u16;
                    }
                    0x1A => queue.vector = if refuses { NO_VECTOR } else { value as u16 },
                    0x1C => {
                        assert_eq!(value, 1, "stub: §4.1.4.3.2: {value} written to queue_enable");
                        assert!(
                            queue.desc != 0 && queue.driver != 0 && queue.device != 0,
                            "stub: §4.1.4.3.2: queue {select} enabled before its addresses"
                        );
                        queue.enabled = true;
                    }
                    0x20 | 0x24 => set_half(&mut queue.desc, field == 0x24),
                    0x28 | 0x2C => set_half(&mut queue.driver, field == 0x2C),
                    0x30 | 0x34 => set_half(&mut queue.device, field == 0x34),
                    _ => unreachable!("the width match above named every field"),
                }
            }
        }
    }

    fn write_status(&mut self, value: u8) {
        self.trace.push(Event::Status(value));
        if value == 0 {
            if !self.permits.reset_never_completes {
                self.reset();
            }
            return;
        }
        assert_eq!(self.status & !value, 0, "stub: §2.1.1: status {value:#x} clears a bit of {:#x}", self.status);
        self.status = value;
        if value & status::FEATURES_OK != 0 && self.permits.refuses_features {
            self.status &= !status::FEATURES_OK;
        }
    }

    /// §4.1.5.2 and §4.1.4.4: the queue's index, sixteen bits, at its own
    /// notify address — and §3.1.1: none before `DRIVER_OK`.
    fn notify(&mut self, at: usize, bytes: usize, value: u32) {
        assert_eq!(bytes, 2, "stub: §4.1.5.2: a {bytes}-byte notification");
        assert!(self.status & status::DRIVER_OK != 0, "stub: §3.1.1: a notification before DRIVER_OK");
        let queue = self
            .queues
            .get(value as usize)
            .unwrap_or_else(|| panic!("stub: a notification for queue {value}, which this device has not"));
        assert!(queue.enabled, "stub: a notification for queue {value}, which is not enabled");
        assert_eq!(
            at,
            queue.notify_off as usize * self.notify_off_multiplier as usize,
            "stub: §4.1.4.4: queue {value} notified at another queue's address"
        );
        self.trace.push(Event::Notify { at, value: value as u16 });
    }

    fn register(&mut self, at: usize, bytes: usize, write: Option<u32>) -> u32 {
        assert!(at + bytes <= BAR_BYTES, "stub: a {bytes}-byte access at {at:#x} is past the BAR");
        assert_eq!(at % bytes, 0, "stub: a {bytes}-byte access at {at:#x} is not aligned");
        let device = DEVICE_AT as usize..DEVICE_AT as usize + self.device_config.len();
        let notify = NOTIFY_AT as usize..(NOTIFY_AT + NOTIFY_BYTES) as usize;
        let common = COMMON_AT as usize..COMMON_AT as usize + 0x1000;
        if common.contains(&at) {
            match write {
                Some(value) => self.common_write(at - COMMON_AT as usize, bytes, value),
                None => return self.common_read(at - COMMON_AT as usize, bytes),
            }
        } else if notify.contains(&at) {
            let value = write.expect("stub: a read of the notification structure");
            self.notify(at - NOTIFY_AT as usize, bytes, value);
        } else if device.contains(&at) {
            assert!(write.is_none() && bytes == 1, "stub: the device structure is bytes, read");
            self.trace.push(Event::DeviceConfig { at: at - DEVICE_AT as usize });
            return self.device_config[at - DEVICE_AT as usize] as u32;
        } else {
            panic!("stub: an access at {at:#x}, which is in none of the device's structures");
        }
        0
    }

    fn load(&mut self, at: usize, bytes: usize) -> u64 {
        assert!(at + bytes <= self.grant.len(), "stub: a {bytes}-byte load at {at:#x} is past the grant");
        assert_eq!(at % bytes, 0, "stub: a {bytes}-byte load at {at:#x} is not aligned");
        self.trace.push(Event::Load { at, bytes });
        self.peek(at, bytes)
    }

    fn store(&mut self, at: usize, bytes: usize, value: u64) {
        assert!(at + bytes <= self.grant.len(), "stub: a {bytes}-byte store at {at:#x} is past the grant");
        assert_eq!(at % bytes, 0, "stub: a {bytes}-byte store at {at:#x} is not aligned");
        self.trace.push(Event::Store { at, bytes, value });
        self.poke(at, bytes, value);
    }

    /// The device's own access to the grant, which is no [`Event`].
    pub fn peek(&self, at: usize, bytes: usize) -> u64 {
        let mut word = [0u8; 8];
        word[..bytes].copy_from_slice(&self.grant[at..at + bytes]);
        u64::from_le_bytes(word)
    }

    pub fn poke(&mut self, at: usize, bytes: usize, value: u64) {
        self.grant[at..at + bytes].copy_from_slice(&value.to_le_bytes()[..bytes]);
    }
}

impl Registers for Machine {
    fn bytes(&self) -> usize {
        BAR_BYTES
    }

    fn read8(&self, at: usize) -> u8 {
        self.state(|s| s.register(at, 1, None)) as u8
    }

    fn read16(&self, at: usize) -> u16 {
        self.state(|s| s.register(at, 2, None)) as u16
    }

    fn read32(&self, at: usize) -> u32 {
        self.state(|s| s.register(at, 4, None))
    }

    fn write8(&self, at: usize, value: u8) {
        self.state(|s| s.register(at, 1, Some(value as u32)));
    }

    fn write16(&self, at: usize, value: u16) {
        self.state(|s| s.register(at, 2, Some(value as u32)));
    }

    fn write32(&self, at: usize, value: u32) {
        self.state(|s| s.register(at, 4, Some(value)));
    }
}

impl DmaBuffers for Machine {
    fn bytes(&self) -> usize {
        GRANT_BYTES
    }

    fn device_addr(&self, at: usize) -> u64 {
        DEVICE_BASE + at as u64
    }

    fn read16(&self, at: usize) -> u16 {
        self.state(|s| s.load(at, 2)) as u16
    }

    fn read32(&self, at: usize) -> u32 {
        self.state(|s| s.load(at, 4)) as u32
    }

    fn read64(&self, at: usize) -> u64 {
        self.state(|s| s.load(at, 8))
    }

    fn write16(&self, at: usize, value: u16) {
        self.state(|s| s.store(at, 2, value as u64));
    }

    fn write32(&self, at: usize, value: u32) {
        self.state(|s| s.store(at, 4, value as u64));
    }

    fn write64(&self, at: usize, value: u64) {
        self.state(|s| s.store(at, 8, value));
    }

    fn publish(&self) {
        self.state(|s| s.trace.push(Event::Publish));
    }

    fn observe(&self) {
        self.state(|s| s.trace.push(Event::Observe));
    }
}

/// One queue's rings, as the device reaches them.
pub struct Ring {
    machine: Machine,
    pub parts: Parts,
    pub size: u16,
}

impl Ring {
    pub fn avail_idx(&self) -> u16 {
        self.machine.state(|s| s.peek(self.parts.avail + 2, 2)) as u16
    }

    pub fn used_idx(&self) -> u16 {
        self.machine.state(|s| s.peek(self.parts.used + 2, 2)) as u16
    }

    /// The head in available-ring entry `nth`, counted as the index counts.
    pub fn avail_entry(&self, nth: u16) -> u16 {
        let at = self.parts.avail + RING_ENTRIES + (nth % self.size) as usize * AVAIL_ENTRY_BYTES;
        self.machine.state(|s| s.peek(at, 2)) as u16
    }

    /// The chain at `head`, read the way §2.7.5 has a device read it: each
    /// descriptor, and the next while `VIRTQ_DESC_F_NEXT` says there is one.
    pub fn chain(&self, head: u16) -> Vec<Buffer> {
        let mut chain = Vec::new();
        let mut index = head;
        loop {
            assert!(index < self.size, "stub: descriptor {index} is past a table of {}", self.size);
            assert!(chain.len() < self.size as usize, "stub: §2.7.5.2: the chain at {head} loops");
            let at = self.parts.desc + index as usize * DESC_BYTES;
            let (addr, len, flags, next) = self.machine.state(|s| {
                (s.peek(at, 8), s.peek(at + 8, 4) as u32, s.peek(at + 12, 2) as u16, s.peek(at + 14, 2) as u16)
            });
            assert_eq!(flags & !3, 0, "stub: descriptor {index} carries flags {flags:#x}");
            chain.push(Buffer { addr, len, writable: flags & 2 != 0 });
            if flags & 1 == 0 {
                return chain;
            }
            index = next;
        }
    }

    /// Write used element `nth`, counted as the index counts, and nothing
    /// else: the index stays where it was.
    pub fn write_used(&self, nth: u16, id: u32, len: u32) {
        let at = self.parts.used + RING_ENTRIES + (nth % self.size) as usize * USED_ELEM_BYTES;
        self.machine.state(|s| {
            s.poke(at, 4, id as u64);
            s.poke(at + 4, 4, len as u64);
        });
    }

    pub fn set_used_idx(&self, idx: u16) {
        self.machine.state(|s| s.poke(self.parts.used + 2, 2, idx as u64));
    }

    /// §2.7.8: return the chain at `id`, `len` bytes written — the element,
    /// and then the index that counts it (§2.7.8.2).
    pub fn use_chain(&self, id: u32, len: u32) {
        let idx = self.used_idx();
        self.write_used(idx, id, len);
        self.set_used_idx(idx.wrapping_add(1));
    }

    /// Overwrite one descriptor, which §2.7.5.1 forbids a device and nothing
    /// stops it doing.
    pub fn scribble_desc(&self, index: u16, flags: u16, next: u16) {
        let at = self.parts.desc + index as usize * DESC_BYTES;
        self.machine.state(|s| {
            s.poke(at + 12, 2, flags as u64);
            s.poke(at + 14, 2, next as u64);
        });
    }
}
