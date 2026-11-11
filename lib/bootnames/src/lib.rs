//! What the loader names of where this boot came from, for the supervisor: the
//! partitions it read by role, and the devices they came off. The loader
//! writes the record ([`Names::encode`]) and the kernel carries its bytes
//! unread; [`Names::parse`] is the one place they become names.
//!
//! **Provenance, not authority**: a source says where the loader read, never
//! that a device found there is the one it read; what was read is held to the
//! slot's signature regardless.
//!
//! The record is little-endian and exact. Absence is a tag, so nothing absent
//! has bytes to disagree with it:
//!
//! ```text
//! MAGIC | root[16] | slot[16] | log[16] | boot: 0 or 1,guid[16] | loader | slot
//! source: 0
//!       | 1, len u16, path[len]
//!       | 2, segment u16, bus, dev, func, len u16, path[len]
//! ```

#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

use toyos_devicepath::{Malformed, Path};

/// The record's first bytes, its last the format's version: a supervisor
/// refuses another loader's record by name.
pub const MAGIC: [u8; 8] = *b"ToyBoot\x01";

/// A partition's unique GUID, raw as in its GPT entry.
pub type Guid = [u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Names<'a> {
    /// The ROOT this boot runs, read whole by the loader.
    pub root: Guid,
    /// The booted slot's volume: the signed header, the kernel, its parameter
    /// and the two boot programs.
    pub slot: Guid,
    /// The log partition.
    pub log: Guid,
    /// The partition firmware loaded the loader from, where it named one: a
    /// network boot or an unpartitioned device has none.
    pub boot: Option<Guid>,
    /// The device [`Self::boot`] is on, where firmware gave it a path.
    pub loader_source: Option<Source<'a>>,
    /// The device [`Self::slot`] is on.
    pub slot_source: Option<Source<'a>>,
}

/// One device a boot read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source<'a> {
    /// Firmware's device path for its handle (UEFI 2.10 §10).
    pub path: Path<'a>,
    /// The PCI function the path runs through last, where firmware's
    /// `GetLocation()` (§14.4.13) placed one: a path's PCI hops name no bus.
    pub controller: Option<Function>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Function {
    pub segment: u16,
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
}

/// Why bytes are no record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The bytes do not begin with [`MAGIC`].
    Magic,
    /// The field at this offset runs past the bytes.
    Short { at: usize },
    /// The tag at this offset is none of its field's.
    Tag { at: usize, tag: u8 },
    /// A controller at device 32 or above, or function 8 or above.
    Address { dev: u8, func: u8 },
    /// The path at this offset is no device path.
    Path { at: usize, why: Malformed },
    /// Bytes follow the record, from this offset.
    Trailing { at: usize },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Magic => write!(f, "it does not begin as this format's record does"),
            Self::Short { at } => write!(f, "the field at byte {at} runs past its end"),
            Self::Tag { at, tag } => write!(f, "the tag at byte {at} is {tag}"),
            Self::Address { dev, func } => write!(f, "a controller at device {dev} function {func} is no PCI address"),
            Self::Path { at, why } => write!(f, "the device path at byte {at} is refused: {why}"),
            Self::Trailing { at } => write!(f, "bytes follow it from byte {at}"),
        }
    }
}

const NO_SOURCE: u8 = 0;
const PATH: u8 = 1;
const PATH_AND_CONTROLLER: u8 = 2;

impl<'a> Names<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Refused> {
        let mut r = Reader { bytes, at: 0 };
        if r.take(MAGIC.len()).ok() != Some(&MAGIC[..]) {
            return Err(Refused::Magic);
        }
        let (root, slot, log) = (r.guid()?, r.guid()?, r.guid()?);
        let boot = match r.tag()? {
            (_, 0) => None,
            (_, 1) => Some(r.guid()?),
            (at, tag) => return Err(Refused::Tag { at, tag }),
        };
        let (loader_source, slot_source) = (r.source()?, r.source()?);
        if r.at != bytes.len() {
            return Err(Refused::Trailing { at: r.at });
        }
        Ok(Self { root, slot, log, boot, loader_source, slot_source })
    }

    /// The record [`Self::parse`] reads back as `self`.
    ///
    /// # Panics
    /// On a path past `u16::MAX` bytes, or a controller that is no PCI address.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::from(MAGIC);
        for guid in [self.root, self.slot, self.log] {
            out.extend_from_slice(&guid);
        }
        match self.boot {
            None => out.push(0),
            Some(guid) => {
                out.push(1);
                out.extend_from_slice(&guid);
            }
        }
        for source in [self.loader_source, self.slot_source] {
            let Some(Source { path, controller }) = source else {
                out.push(NO_SOURCE);
                continue;
            };
            match controller {
                None => out.push(PATH),
                Some(Function { segment, bus, dev, func }) => {
                    assert!(dev < 32 && func < 8, "a controller at device {dev} function {func}");
                    out.push(PATH_AND_CONTROLLER);
                    out.extend_from_slice(&segment.to_le_bytes());
                    out.extend_from_slice(&[bus, dev, func]);
                }
            }
            let len = u16::try_from(path.bytes().len()).expect("a device path within a u16 of bytes");
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(path.bytes());
        }
        out
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], Refused> {
        let field = self.bytes[self.at..].get(..len).ok_or(Refused::Short { at: self.at })?;
        self.at += len;
        Ok(field)
    }

    /// A tag, and where it is.
    fn tag(&mut self) -> Result<(usize, u8), Refused> {
        let at = self.at;
        Ok((at, self.take(1)?[0]))
    }

    fn guid(&mut self) -> Result<Guid, Refused> {
        Ok(self.take(16)?.try_into().expect("sixteen bytes"))
    }

    fn u16(&mut self) -> Result<u16, Refused> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("two bytes")))
    }

    fn source(&mut self) -> Result<Option<Source<'a>>, Refused> {
        let controller = match self.tag()? {
            (_, NO_SOURCE) => return Ok(None),
            (_, PATH) => None,
            (_, PATH_AND_CONTROLLER) => {
                let segment = self.u16()?;
                let &[bus, dev, func] = self.take(3)? else { unreachable!("three bytes") };
                if dev >= 32 || func >= 8 {
                    return Err(Refused::Address { dev, func });
                }
                Some(Function { segment, bus, dev, func })
            }
            (at, tag) => return Err(Refused::Tag { at, tag }),
        };
        let len = usize::from(self.u16()?);
        let at = self.at;
        let path = Path::parse(self.take(len)?).map_err(|why| Refused::Path { at, why })?;
        Ok(Some(Source { path, controller }))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;

    /// A stick's ESP below a USB controller, with a made-up partition.
    const STICK: [u8; 76] = [
        2, 1, 12, 0, 0xd0, 0x41, 0x03, 0x0a, 0, 0, 0, 0, // PciRoot(0x0)
        1, 1, 6, 0, 1, 8, // Pci(0x8,0x1)
        1, 1, 6, 0, 4, 0, // Pci(0x0,0x4)
        3, 5, 6, 0, 4, 0, // USB(0x4,0x0)
        4, 1, 42, 0, 1, 0, 0, 0, // HD(1,
        0, 8, 0, 0, 0, 0, 0, 0, // 0x800,
        0, 0x10, 0, 0, 0, 0, 0, 0, // 0x1000)
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 2, 2, // GPT, its GUID
        0x7f, 0xff, 4, 0, // End
    ];
    const END: [u8; 4] = [0x7f, 0xff, 4, 0];
    const AT: Function = Function { segment: 0x1234, bus: 3, dev: 31, func: 7 };

    /// Where the fixed fields end: the magic, three GUIDs.
    const BOOT_TAG: usize = 8 + 48;

    fn stick() -> Path<'static> {
        Path::parse(&STICK).unwrap()
    }

    fn full() -> Names<'static> {
        Names {
            root: [1; 16],
            slot: [2; 16],
            log: [3; 16],
            boot: Some([4; 16]),
            loader_source: Some(Source { path: stick(), controller: Some(AT) }),
            slot_source: Some(Source { path: Path::parse(&END).unwrap(), controller: None }),
        }
    }

    #[test]
    fn every_shape_the_loader_writes_reads_back() {
        let bare = Names { boot: None, loader_source: None, slot_source: None, ..full() };
        let swapped = Names { loader_source: full().slot_source, slot_source: full().loader_source, ..full() };
        for names in [full(), bare, swapped] {
            let bytes = names.encode();
            assert_eq!(Names::parse(&bytes), Ok(names), "{bytes:02x?}");
        }
        assert_eq!(bare.encode().len(), BOOT_TAG + 3);
    }

    /// The offsets below are the layout: a change to it changes [`MAGIC`].
    #[test]
    fn the_record_is_laid_out_as_the_header_says() {
        let bytes = full().encode();
        assert_eq!(bytes[..8], MAGIC);
        assert_eq!((&bytes[8..24], &bytes[24..40], &bytes[40..56]), (&[1; 16][..], &[2; 16][..], &[3; 16][..]));
        assert_eq!((bytes[56], &bytes[57..73]), (1, &[4; 16][..]));
        assert_eq!(bytes[73..81], [2, 0x34, 0x12, 3, 31, 7, 76, 0]);
        assert_eq!(bytes[81..157], STICK);
        assert_eq!(bytes[157..], [1, 4, 0, 0x7f, 0xff, 4, 0]);
    }

    /// Every way the bytes are not a record names itself and where, and no
    /// prefix of a record is one.
    #[test]
    fn hostile_bytes_are_refused_by_name() {
        let good = full().encode();
        let loader = BOOT_TAG + 17;
        let path = loader + 8;
        let slot = path + STICK.len();
        let with = |at: usize, byte: u8| {
            let mut bytes = good.clone();
            bytes[at] = byte;
            bytes
        };
        let cases: [(Vec<u8>, Refused); 12] = [
            (Vec::new(), Refused::Magic),
            (with(7, 2), Refused::Magic),
            (with(0, b't'), Refused::Magic),
            (with(BOOT_TAG, 2), Refused::Tag { at: BOOT_TAG, tag: 2 }),
            (with(loader, 3), Refused::Tag { at: loader, tag: 3 }),
            (with(slot, 0xff), Refused::Tag { at: slot, tag: 0xff }),
            (with(loader + 4, 32), Refused::Address { dev: 32, func: 7 }),
            (with(loader + 5, 8), Refused::Address { dev: 31, func: 8 }),
            // The length says 77: the path takes the slot source's tag.
            (with(loader + 6, 77), Refused::Path { at: path, why: Malformed::Trailing { at: 72 } }),
            (with(path + 2, 0), Refused::Path { at: path, why: Malformed::Short { at: 0, len: 0 } }),
            (with(loader + 7, 1), Refused::Short { at: path }),
            ([good.clone(), Vec::from([0])].concat(), Refused::Trailing { at: good.len() }),
        ];
        for (bytes, want) in cases {
            assert_eq!(Names::parse(&bytes), Err(want), "{bytes:02x?}");
        }
        for len in MAGIC.len()..good.len() {
            let cut = Names::parse(&good[..len]);
            assert!(matches!(cut, Err(Refused::Short { .. } | Refused::Path { .. })), "{len} bytes: {cut:?}");
        }
        // A source with no path has no controller to carry: tag 0 ends it.
        let none = Names { loader_source: None, ..full() }.encode();
        assert_eq!(none[loader], 0);
        assert_eq!(Names::parse(&none).unwrap().loader_source, None);
    }

    #[test]
    fn a_refusal_says_what_and_where() {
        let said = Refused::Path { at: 81, why: Malformed::Unterminated { at: 72 } }.to_string();
        assert_eq!(said, "the device path at byte 81 is refused: the path ends at byte 72 with no End node");
    }
}
