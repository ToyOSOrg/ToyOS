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
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct PciAddr {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

/// A PCI function's number for this boot, never given to another: what a
/// claim names it by ([`crate::syscall::DeviceRequest::Function`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Arrival(pub u32);

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
    pub arrival: Arrival,
    pub at: PciAddr,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub driven: Driven,
}

/// What a claim is on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Claimed {
    /// A device class.
    Class(DeviceType),
    Pci(PciAddr),
}

/// A claim, and the process holding its handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Claim {
    pub on: Claimed,
    pub holder: Holder,
}

/// Which of the machine's partitions the loader named for a role.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// The ROOT this boot runs, read whole by the loader.
    Root,
    /// The partition firmware loaded the loader from.
    Boot,
    /// The log partition.
    Log,
    /// The booted slot's volume: the kernel and its parameter.
    Slot,
}

/// A partition the loader named, by its unique GUID: what a file server for
/// that role serves, wherever the disk it is on is driven.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Loaded {
    pub role: Role,
    pub unique_guid: [u8; 16],
}

/// One record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Record {
    Pci(Pci),
    Claim(Claim),
    Loaded(Loaded),
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
const KIND_CLAIM: u8 = 5;
const KIND_LOADED: u8 = 6;

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
                o.u32(16, p.arrival.0);
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
                }
                holder_out(&mut o, 28, &c.holder);
            }
            Self::Loaded(l) => {
                o.u8(0, KIND_LOADED);
                o.u8(
                    1,
                    match l.role {
                        Role::Root => 0,
                        Role::Boot => 1,
                        Role::Log => 2,
                        Role::Slot => 3,
                    },
                );
                o.bytes(8, &l.unique_guid);
            }
        }
        RawRecord(o.0)
    }

    pub fn decode(raw: &RawRecord) -> Result<Self, Undecodable> {
        let r = In(&raw.0);
        Ok(match r.u8(0) {
            KIND_PCI => Self::Pci(Pci {
                arrival: Arrival(r.u32(16)),
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
            KIND_CLAIM => Self::Claim(Claim {
                on: match r.variant(1, |b| (b <= 1).then_some(b))? {
                    0 => {
                        let raw = r.u64(4);
                        Claimed::Class(
                            DeviceType::from_raw(raw)
                                .ok_or(Undecodable::Variant { at: 4, value: raw })?,
                        )
                    }
                    _ => Claimed::Pci(r.addr(4)),
                },
                holder: holder_in(&r, 28),
            }),
            KIND_LOADED => Self::Loaded(Loaded {
                role: r.variant(1, |b| match b {
                    0 => Some(Role::Root),
                    1 => Some(Role::Boot),
                    2 => Some(Role::Log),
                    3 => Some(Role::Slot),
                    _ => None,
                })?,
                unique_guid: r.array(8),
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

    fn every_kind() -> [Record; 4] {
        let at = PciAddr { segment: 0x1234, bus: 0, dev: 0x1f, func: 6 };
        let holder = Holder { pid: 7, name: name("netstack") };
        [
            Record::Pci(Pci {
                arrival: Arrival(0x0102_0304),
                at,
                vendor: 0x8086,
                device: 0x15fc,
                class: 2,
                subclass: 0,
                prog_if: 0,
                driven: Driven::Claimed,
            }),
            Record::Claim(Claim { on: Claimed::Pci(at), holder }),
            Record::Claim(Claim { on: Claimed::Class(DeviceType::Framebuffer), holder }),
            Record::Loaded(Loaded { role: Role::Log, unique_guid: [0x22; 16] }),
        ]
    }

    #[test]
    fn every_record_round_trips() {
        for record in every_kind() {
            assert_eq!(Record::decode(&record.encode()), Ok(record));
        }
        assert_eq!(Holder { pid: 1, name: name("soundserver") }.name(), Some("soundserver"));
    }

    #[test]
    fn a_byte_that_names_nothing_is_refused_by_name() {
        assert_eq!(Record::decode(&RawRecord::EMPTY), Err(Undecodable::Kind(0)));
        let mut raw = RawRecord::EMPTY;
        raw.0[0] = 99;
        assert_eq!(Record::decode(&raw), Err(Undecodable::Kind(99)));
        for (kind, at) in [(KIND_PCI, 13), (KIND_CLAIM, 1), (KIND_LOADED, 1)] {
            let mut raw = RawRecord::EMPTY;
            raw.0[0] = kind;
            raw.0[at] = 9;
            assert_eq!(Record::decode(&raw), Err(Undecodable::Variant { at, value: 9 }), "{kind}");
        }
        let mut raw = Record::Claim(Claim {
            on: Claimed::Class(DeviceType::Framebuffer),
            holder: Holder { pid: 1, name: name("x") },
        })
        .encode();
        let past_the_last = DeviceType::ALL.iter().map(|&class| class as u64).max().expect("a class") + 1;
        for class in [past_the_last, 1 << 40] {
            raw.0[4..12].copy_from_slice(&class.to_le_bytes());
            assert_eq!(Record::decode(&raw), Err(Undecodable::Variant { at: 4, value: class }));
        }
    }

    #[test]
    fn a_record_is_one_fixed_width() {
        assert_eq!(core::mem::size_of::<RawRecord>(), RECORD_BYTES);
    }
}
