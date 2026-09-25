//! What the machine is made of, as the kernel knows it: the records
//! [`SYS_DEVICE_INVENTORY`] answers.
//!
//! **Typed records of one fixed width, and no text.** Every record is
//! [`RECORD_BYTES`] bytes: a kind byte and that kind's fields at fixed offsets,
//! little-endian. [`Record::encode`] and [`Record::decode`] are the only two
//! spellings of the layout, so the kernel that writes a record and the reader
//! that renders it cannot disagree about an offset; a byte that names no kind or
//! no variant is refused by name ([`Undecodable`]) rather than read as zero.
//!
//! **Who holds a device is a record of its own** ([`Claim`]): a claim is a
//! handle, and a handle can be in one process's table or moving between two, so
//! the device's record says whether it is claimed and a claim record says by
//! whom when the kernel found the handle in a table.
//!
//! The CPU count and the memory are not here: they are `SYS_SYSINFO`'s header,
//! which needs no right.
//!
//! [`SYS_DEVICE_INVENTORY`]: crate::syscall::SYS_DEVICE_INVENTORY

use crate::syscall::DeviceType;

/// The width of every record.
pub const RECORD_BYTES: usize = 64;

/// One record as it crosses the boundary.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RawRecord(pub [u8; RECORD_BYTES]);

impl RawRecord {
    pub const EMPTY: Self = Self([0; RECORD_BYTES]);
}

/// A process's name as the roster spells it: NUL-padded bytes.
pub const NAME_BYTES: usize = 28;

/// Who holds a claim: the process whose handle table the kernel found it in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Holder {
    pub pid: u32,
    pub name: [u8; NAME_BYTES],
}

impl Holder {
    /// The name up to its first NUL, or `None` when that is not UTF-8.
    pub fn name(&self) -> Option<&str> {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_BYTES);
        core::str::from_utf8(&self.name[..end]).ok()
    }
}

/// A PCI function's address: its segment group, as the MCFG entry its
/// configuration space was found through names it, and its bus, device and
/// function there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PciAddr {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

/// Who drives a PCI function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Driven {
    /// Nobody: no kernel driver took it and no process holds a claim on it.
    Free,
    /// A driver in this kernel.
    Kernel,
    /// A process holds a claim on it; a [`Claim`] record says which, when the
    /// handle was in a table.
    Claimed,
}

/// One PCI function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pci {
    pub at: PciAddr,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub driven: Driven,
}

/// What a USB device was bound as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UsbFunction {
    Keyboard,
    /// A mouse or a tablet: the kernel dispatches the two identically.
    Pointer,
    Storage,
}

/// The speed a USB port trained at: PORTSC's Port Speed under the default
/// Protocol Speed IDs (xHCI 1.2 §7.2.2.1.1, Table 7-13), which are the only
/// ones a device this kernel binds can have.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UsbSpeed {
    Full,
    Low,
    High,
    /// SuperSpeed, Gen 1 on one lane.
    Super,
    /// SuperSpeedPlus, Gen 2 on one lane.
    SuperPlusGen2x1,
    /// SuperSpeedPlus, Gen 1 on two lanes.
    SuperPlusGen1x2,
    /// SuperSpeedPlus, Gen 2 on two lanes.
    SuperPlusGen2x2,
}

impl UsbSpeed {
    /// The Port Speed value, or `None` for one no default ID names.
    pub fn from_psiv(psiv: u8) -> Option<Self> {
        Some(match psiv {
            1 => Self::Full,
            2 => Self::Low,
            3 => Self::High,
            4 => Self::Super,
            5 => Self::SuperPlusGen2x1,
            6 => Self::SuperPlusGen1x2,
            7 => Self::SuperPlusGen2x2,
            _ => return None,
        })
    }

    pub fn psiv(self) -> u8 {
        match self {
            Self::Full => 1,
            Self::Low => 2,
            Self::High => 3,
            Self::Super => 4,
            Self::SuperPlusGen2x1 => 5,
            Self::SuperPlusGen1x2 => 6,
            Self::SuperPlusGen2x2 => 7,
        }
    }
}

/// One USB device a kernel driver bound.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Usb {
    /// The xHCI controller it hangs off.
    pub controller: PciAddr,
    /// The root-hub port, from 1.
    pub port: u8,
    pub speed: UsbSpeed,
    pub vendor: u16,
    pub product: u16,
    pub function: UsbFunction,
}

/// One block device.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
    pub device: u32,
    /// In [`crate::part::BLOCK_BYTES`] blocks.
    pub blocks: u64,
}

/// Who holds a partition's blocks, as the block layer records every hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PartState {
    /// No hold is exactly this partition's span.
    Free,
    /// This kernel holds it: a filesystem it mounted, or a probe of its own.
    Kernel,
    /// A process's partition claim holds it; a [`Claim`] record says whose,
    /// when the handle was in a table.
    Claimed,
}

/// One GPT entry, as its table stated it when the disk was probed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Partition {
    pub device: u32,
    /// Its entry's index in the table.
    pub index: u32,
    pub type_guid: [u8; 16],
    pub unique_guid: [u8; 16],
    /// In the device's own logical blocks, as GPT stores them.
    pub first_lba: u64,
    pub lbas: u64,
    pub state: PartState,
}

/// What a claim is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Claimed {
    /// A device class.
    Class(DeviceType),
    Pci(PciAddr),
    /// A partition, by the block device it is on and its unique GUID: a
    /// GUID a table carries twice is refused a claim, so the pair names one.
    Partition { device: u32, unique_guid: [u8; 16] },
}

/// A claim, and the process holding its handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Claim {
    pub on: Claimed,
    pub holder: Holder,
}

/// One record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Record {
    Pci(Pci),
    Usb(Usb),
    Block(Block),
    Partition(Partition),
    Claim(Claim),
}

/// Why a record did not decode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undecodable {
    /// The kind byte names no record.
    Kind(u8),
    /// A field's bytes name no variant of its type: the field's offset, and
    /// its value.
    Variant { at: usize, value: u64 },
}

impl core::fmt::Display for Undecodable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kind(kind) => write!(f, "record kind {kind} names no record"),
            Self::Variant { at, value } => {
                write!(f, "{value} at offset {at} names no variant of its field")
            }
        }
    }
}

const KIND_PCI: u8 = 1;
const KIND_USB: u8 = 2;
const KIND_BLOCK: u8 = 3;
const KIND_PARTITION: u8 = 4;
const KIND_CLAIM: u8 = 5;

/// Little-endian writes at fixed offsets.
struct Out([u8; RECORD_BYTES]);

impl Out {
    fn u8(&mut self, at: usize, v: u8) {
        self.0[at] = v;
    }
    fn u16(&mut self, at: usize, v: u16) {
        self.0[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, at: usize, v: u32) {
        self.0[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, at: usize, v: u64) {
        self.0[at..at + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, at: usize, v: &[u8]) {
        self.0[at..at + v.len()].copy_from_slice(v);
    }
    /// Five bytes: the segment, then bus, device and function.
    fn addr(&mut self, at: usize, v: PciAddr) {
        self.u16(at, v.segment);
        self.bytes(at + 2, &[v.bus, v.dev, v.func]);
    }
}

/// Little-endian reads at fixed offsets.
struct In<'a>(&'a [u8; RECORD_BYTES]);

impl In<'_> {
    fn u8(&self, at: usize) -> u8 {
        self.0[at]
    }
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes([self.0[at], self.0[at + 1]])
    }
    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.0[at..at + 4].try_into().expect("four bytes"))
    }
    fn u64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.0[at..at + 8].try_into().expect("eight bytes"))
    }
    fn array<const N: usize>(&self, at: usize) -> [u8; N] {
        self.0[at..at + N].try_into().expect("N bytes")
    }
    fn addr(&self, at: usize) -> PciAddr {
        PciAddr { segment: self.u16(at), bus: self.0[at + 2], dev: self.0[at + 3], func: self.0[at + 4] }
    }
    fn variant<T>(&self, at: usize, of: impl FnOnce(u8) -> Option<T>) -> Result<T, Undecodable> {
        let byte = self.0[at];
        of(byte).ok_or(Undecodable::Variant { at, value: u64::from(byte) })
    }
}

fn holder_out(out: &mut Out, at: usize, h: &Holder) {
    out.u32(at, h.pid);
    out.bytes(at + 4, &h.name);
}

fn holder_in(r: &In<'_>, at: usize) -> Holder {
    Holder { pid: r.u32(at), name: r.array(at + 4) }
}

impl Record {
    pub fn encode(&self) -> RawRecord {
        let mut o = Out([0; RECORD_BYTES]);
        match self {
            Self::Pci(p) => {
                o.u8(0, KIND_PCI);
                o.addr(1, p.at);
                o.u16(6, p.vendor);
                o.u16(8, p.device);
                o.bytes(10, &[p.class, p.subclass, p.prog_if]);
                o.u8(
                    13,
                    match p.driven {
                        Driven::Free => 0,
                        Driven::Kernel => 1,
                        Driven::Claimed => 2,
                    },
                );
            }
            Self::Usb(u) => {
                o.u8(0, KIND_USB);
                o.addr(1, u.controller);
                o.u8(6, u.port);
                o.u8(7, u.speed.psiv());
                o.u16(8, u.vendor);
                o.u16(10, u.product);
                o.u8(
                    12,
                    match u.function {
                        UsbFunction::Keyboard => 0,
                        UsbFunction::Pointer => 1,
                        UsbFunction::Storage => 2,
                    },
                );
            }
            Self::Block(b) => {
                o.u8(0, KIND_BLOCK);
                o.u32(4, b.device);
                o.u64(8, b.blocks);
            }
            Self::Partition(p) => {
                o.u8(0, KIND_PARTITION);
                o.u8(
                    1,
                    match p.state {
                        PartState::Free => 0,
                        PartState::Kernel => 1,
                        PartState::Claimed => 2,
                    },
                );
                o.u32(4, p.device);
                o.u64(8, p.first_lba);
                o.u64(16, p.lbas);
                o.bytes(24, &p.type_guid);
                o.bytes(40, &p.unique_guid);
                o.u32(56, p.index);
            }
            Self::Claim(c) => {
                o.u8(0, KIND_CLAIM);
                match c.on {
                    Claimed::Class(class) => {
                        o.u8(1, 0);
                        o.u64(4, class as u64);
                    }
                    Claimed::Pci(at) => {
                        o.u8(1, 1);
                        o.addr(4, at);
                    }
                    Claimed::Partition { device, unique_guid } => {
                        o.u8(1, 2);
                        o.u32(4, device);
                        o.bytes(8, &unique_guid);
                    }
                }
                holder_out(&mut o, 28, &c.holder);
            }
        }
        RawRecord(o.0)
    }

    pub fn decode(raw: &RawRecord) -> Result<Self, Undecodable> {
        let r = In(&raw.0);
        Ok(match r.u8(0) {
            KIND_PCI => Self::Pci(Pci {
                at: r.addr(1),
                vendor: r.u16(6),
                device: r.u16(8),
                class: r.u8(10),
                subclass: r.u8(11),
                prog_if: r.u8(12),
                driven: r.variant(13, |b| match b {
                    0 => Some(Driven::Free),
                    1 => Some(Driven::Kernel),
                    2 => Some(Driven::Claimed),
                    _ => None,
                })?,
            }),
            KIND_USB => Self::Usb(Usb {
                controller: r.addr(1),
                port: r.u8(6),
                speed: r.variant(7, UsbSpeed::from_psiv)?,
                vendor: r.u16(8),
                product: r.u16(10),
                function: r.variant(12, |b| match b {
                    0 => Some(UsbFunction::Keyboard),
                    1 => Some(UsbFunction::Pointer),
                    2 => Some(UsbFunction::Storage),
                    _ => None,
                })?,
            }),
            KIND_BLOCK => Self::Block(Block { device: r.u32(4), blocks: r.u64(8) }),
            KIND_PARTITION => Self::Partition(Partition {
                state: r.variant(1, |b| match b {
                    0 => Some(PartState::Free),
                    1 => Some(PartState::Kernel),
                    2 => Some(PartState::Claimed),
                    _ => None,
                })?,
                device: r.u32(4),
                first_lba: r.u64(8),
                lbas: r.u64(16),
                type_guid: r.array(24),
                unique_guid: r.array(40),
                index: r.u32(56),
            }),
            KIND_CLAIM => Self::Claim(Claim {
                on: match r.variant(1, |b| (b <= 2).then_some(b))? {
                    0 => {
                        let raw = r.u64(4);
                        Claimed::Class(
                            DeviceType::from_raw(raw)
                                .ok_or(Undecodable::Variant { at: 4, value: raw })?,
                        )
                    }
                    1 => Claimed::Pci(r.addr(4)),
                    _ => Claimed::Partition { device: r.u32(4), unique_guid: r.array(8) },
                },
                holder: holder_in(&r, 28),
            }),
            kind => return Err(Undecodable::Kind(kind)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> [u8; NAME_BYTES] {
        let mut n = [0; NAME_BYTES];
        n[..s.len()].copy_from_slice(s.as_bytes());
        n
    }

    fn every_kind() -> [Record; 7] {
        let at = PciAddr { segment: 0x1234, bus: 0, dev: 0x1f, func: 6 };
        let holder = Holder { pid: 7, name: name("netd") };
        [
            Record::Pci(Pci {
                at,
                vendor: 0x8086,
                device: 0x15fc,
                class: 2,
                subclass: 0,
                prog_if: 0,
                driven: Driven::Claimed,
            }),
            Record::Usb(Usb {
                controller: PciAddr { segment: 0, bus: 0, dev: 4, func: 0 },
                port: 3,
                speed: UsbSpeed::SuperPlusGen2x2,
                vendor: 0x0627,
                product: 1,
                function: UsbFunction::Pointer,
            }),
            Record::Block(Block { device: 1, blocks: u64::MAX }),
            Record::Partition(Partition {
                device: 1,
                index: 128,
                type_guid: [0xab; 16],
                unique_guid: [0xcd; 16],
                first_lba: 2048,
                lbas: 4096,
                state: PartState::Kernel,
            }),
            Record::Claim(Claim { on: Claimed::Pci(at), holder }),
            Record::Claim(Claim { on: Claimed::Class(DeviceType::VirtioSound), holder }),
            Record::Claim(Claim {
                on: Claimed::Partition { device: 3, unique_guid: [0x11; 16] },
                holder,
            }),
        ]
    }

    #[test]
    fn every_record_round_trips() {
        for record in every_kind() {
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
        assert_eq!(Holder { pid: 1, name: name("soundd") }.name(), Some("soundd"));
        for psiv in 0..=u8::MAX {
            if let Some(speed) = UsbSpeed::from_psiv(psiv) {
                assert_eq!(speed.psiv(), psiv);
            }
        }
    }

    #[test]
    fn a_byte_that_names_nothing_is_refused_by_name() {
        assert_eq!(Record::decode(&RawRecord::EMPTY), Err(Undecodable::Kind(0)));
        let mut raw = RawRecord::EMPTY;
        raw.0[0] = 99;
        assert_eq!(Record::decode(&raw), Err(Undecodable::Kind(99)));
        for (kind, at) in [(KIND_PCI, 13), (KIND_USB, 12), (KIND_PARTITION, 1), (KIND_CLAIM, 1)] {
            let mut raw = RawRecord::EMPTY;
            raw.0[0] = kind;
            raw.0[at] = 9;
            if kind == KIND_USB {
                raw.0[7] = 1;
            }
            assert_eq!(Record::decode(&raw), Err(Undecodable::Variant { at, value: 9 }), "{kind}");
        }
        // A speed no default Protocol Speed ID names.
        let mut raw = Record::Usb(Usb {
            controller: PciAddr { segment: 0, bus: 0, dev: 4, func: 0 },
            port: 1,
            speed: UsbSpeed::Full,
            vendor: 0,
            product: 0,
            function: UsbFunction::Keyboard,
        })
        .encode();
        for psiv in [0, 8, 15] {
            raw.0[7] = psiv;
            assert_eq!(
                Record::decode(&raw),
                Err(Undecodable::Variant { at: 7, value: u64::from(psiv) })
            );
        }
        // A class number no `DeviceType` carries: 3 and 4 are retired.
        let mut raw = Record::Claim(Claim {
            on: Claimed::Class(DeviceType::Keyboard),
            holder: Holder { pid: 1, name: name("x") },
        })
        .encode();
        for class in [3u64, 4, 1 << 40] {
            raw.0[4..12].copy_from_slice(&class.to_le_bytes());
            assert_eq!(Record::decode(&raw), Err(Undecodable::Variant { at: 4, value: class }));
        }
    }

    #[test]
    fn a_record_is_one_fixed_width() {
        assert_eq!(core::mem::size_of::<RawRecord>(), RECORD_BYTES);
    }
}
