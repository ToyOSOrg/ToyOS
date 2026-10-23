//! UEFI device paths as bytes (UEFI 2.10 §10): validated once by [`Path::parse`],
//! then walked node by node.
//!
//! A path is firmware's, so its bytes are external input wherever they are
//! read: a node shorter than its header or running past the bytes, a path with
//! no End of Entire node or bytes after it, and a multi-instance path are each
//! refused by name ([`Malformed`]). A [`Path`] is only made by that check.
//!
//! A node is decoded only at the exact length its definition gives; any other
//! is rendered whole, as is every kind no boot source has needed.

#![no_std]

use core::fmt;

/// A node's header: type, subtype and a little-endian length counting it.
const HEADER: usize = 4;

/// End of Entire Device Path.
const END_ENTIRE: (u8, u8) = (0x7f, 0xff);
/// End of This Instance: a path of several instances names several devices.
const END_INSTANCE: (u8, u8) = (0x7f, 0x01);

/// Why bytes are no device path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// The node at this offset has no whole header before the bytes end, so
    /// the path has no End of Entire node.
    Unterminated { at: usize },
    /// The node at this offset claims a length shorter than its header.
    Short { at: usize, len: usize },
    /// The node at this offset claims a length past the bytes.
    Overrun { at: usize, len: usize },
    /// An End of This Instance node at this offset: more than one device.
    Instances { at: usize },
    /// The End of Entire node at this offset is not the path's last four bytes.
    Trailing { at: usize },
}

impl fmt::Display for Malformed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Unterminated { at } => write!(f, "the path ends at byte {at} with no End node"),
            Self::Short { at, len } => write!(f, "the node at byte {at} claims {len} bytes, short of its header"),
            Self::Overrun { at, len } => write!(f, "the node at byte {at} claims {len} bytes, past the path"),
            Self::Instances { at } => write!(f, "the path has a second instance after byte {at}"),
            Self::Trailing { at } => write!(f, "bytes follow the End node at byte {at}"),
        }
    }
}

/// A single-instance device path, End of Entire node included.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Path<'a>(&'a [u8]);

impl<'a> Path<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Malformed> {
        let mut at = 0;
        loop {
            let Some(header) = bytes.get(at..at + HEADER) else { return Err(Malformed::Unterminated { at }) };
            let len = usize::from(u16::from_le_bytes([header[2], header[3]]));
            if len < HEADER {
                return Err(Malformed::Short { at, len });
            }
            if bytes.len() - at < len {
                return Err(Malformed::Overrun { at, len });
            }
            match (header[0], header[1]) {
                END_ENTIRE if at + len == bytes.len() && len == HEADER => return Ok(Self(bytes)),
                END_ENTIRE => return Err(Malformed::Trailing { at }),
                END_INSTANCE => return Err(Malformed::Instances { at }),
                _ => at += len,
            }
        }
    }

    /// Every byte, End node included.
    pub fn bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Each node before the End node.
    pub fn nodes(&self) -> impl Iterator<Item = Node<'a>> + 'a {
        let mut rest = &self.0[..self.0.len() - HEADER];
        core::iter::from_fn(move || {
            let len = usize::from(u16::from_le_bytes([*rest.get(2)?, rest[3]]));
            let (node, after) = rest.split_at(len);
            rest = after;
            Some(Node(node))
        })
    }
}

/// One node, header included.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Node<'a>(&'a [u8]);

impl<'a> Node<'a> {
    pub fn kind(&self) -> (u8, u8) {
        (self.0[0], self.0[1])
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.0
    }

    fn decode(&self) -> Known {
        let n = self.0;
        let u32_at = |at: usize| u32::from_le_bytes(n[at..at + 4].try_into().expect("four bytes"));
        match (self.kind(), n.len()) {
            ((2, 1), 12) => Known::Acpi { hid: u32_at(4), uid: u32_at(8) },
            ((1, 1), 6) => Known::Pci { function: n[4], device: n[5] },
            ((3, 5), 6) => Known::Usb { port: n[4], interface: n[5] },
            ((3, 0x17), 16) => Known::Nvme { namespace: u32_at(4), eui64: n[8..16].try_into().expect("eight bytes") },
            (HardDrive::TYPE, _) => HardDrive::parse(n).map_or(Known::Other, Known::HardDrive),
            _ => Known::Other,
        }
    }
}

/// What a node is, of the kinds a boot source's path is made of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    /// An ACPI device by its `_HID` and `_UID`: a root bridge, at the head of
    /// a PCI device's path.
    Acpi { hid: u32, uid: u32 },
    /// A PCI function below the node before it, by device and function number
    /// on that node's bus. No bus number: it is the bridge's, which only
    /// configuration space says.
    Pci { device: u8, function: u8 },
    /// A USB device on its parent hub's or root's port.
    Usb { port: u8, interface: u8 },
    Nvme { namespace: u32, eui64: [u8; 8] },
    HardDrive(HardDrive),
    Other,
}

/// `_HID` of a PCI root bridge, and of a PCI Express one, in EISA form.
const PNP0A03: u32 = 0x0a03_41d0;
const PNP0A08: u32 = 0x0a08_41d0;

/// The MEDIA/HARDDRIVE node: one partition of the disk the nodes before it
/// name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardDrive {
    /// The partition's number in its table, from 1.
    pub number: u32,
    pub start: u64,
    pub size: u64,
    /// The unique partition GUID, raw as in the GPT entry, under a GPT
    /// signature; an MBR one's four bytes and zeroes otherwise.
    pub signature: [u8; 16],
    /// `MBRType`: [`Self::GPT`] for a GPT partition.
    pub format: u8,
    /// `SignatureType`: [`Self::GUID_SIGNATURE`] for a GUID one.
    pub signature_type: u8,
}

impl HardDrive {
    pub const TYPE: (u8, u8) = (4, 1);
    pub const GPT: u8 = 2;
    pub const GUID_SIGNATURE: u8 = 2;
    const LEN: usize = 42;

    /// `node`, header included, where it is a HARDDRIVE node of its defined
    /// length.
    pub fn parse(node: &[u8]) -> Option<HardDrive> {
        if node.len() != Self::LEN || (node[0], node[1]) != Self::TYPE {
            return None;
        }
        let u64_at = |at: usize| u64::from_le_bytes(node[at..at + 8].try_into().expect("eight bytes"));
        Some(HardDrive {
            number: u32::from_le_bytes(node[4..8].try_into().expect("four bytes")),
            start: u64_at(8),
            size: u64_at(16),
            signature: node[24..40].try_into().expect("sixteen bytes"),
            format: node[40],
            signature_type: node[41],
        })
    }

    /// The unique GUID, where this is a GPT partition named by one.
    pub fn guid(&self) -> Option<[u8; 16]> {
        (self.format == Self::GPT && self.signature_type == Self::GUID_SIGNATURE).then_some(self.signature)
    }
}

/// The text form (§10.6) for the nodes decoded, and
/// `Path(type,subtype,hex)` for every other.
impl fmt::Display for Path<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, node) in self.nodes().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            write!(f, "{node}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Node<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.decode() {
            Known::Acpi { hid: PNP0A03, uid } => write!(f, "PciRoot({uid:#x})"),
            Known::Acpi { hid: PNP0A08, uid } => write!(f, "PcieRoot({uid:#x})"),
            Known::Acpi { hid, uid } => write!(f, "Acpi({hid:#x},{uid:#x})"),
            Known::Pci { device, function } => write!(f, "Pci({device:#x},{function:#x})"),
            Known::Usb { port, interface } => write!(f, "USB({port:#x},{interface:#x})"),
            Known::Nvme { namespace, eui64: e } => write!(
                f,
                "NVMe({namespace:#x},{:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X})",
                e[0], e[1], e[2], e[3], e[4], e[5], e[6], e[7]
            ),
            Known::HardDrive(hd) => match hd.guid() {
                Some(guid) => write!(f, "HD({},GPT,{},{:#x},{:#x})", hd.number, toyos_gpt::Guid(guid), hd.start, hd.size),
                None => write!(f, "HD({},{},{:#x},{:#x})", hd.number, hd.format, hd.start, hd.size),
            },
            Known::Other => {
                let (kind, subtype) = self.kind();
                write!(f, "Path({kind},{subtype},")?;
                for byte in &self.0[HEADER..] {
                    write!(f, "{byte:02X}")?;
                }
                f.write_str(")")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::string::ToString;
    use std::vec::Vec;

    fn node(kind: (u8, u8), data: &[u8]) -> Vec<u8> {
        let len = u16::try_from(HEADER + data.len()).unwrap();
        let mut n = Vec::from([kind.0, kind.1]);
        n.extend_from_slice(&len.to_le_bytes());
        n.extend_from_slice(data);
        n
    }

    fn end() -> Vec<u8> {
        node(END_ENTIRE, &[])
    }

    fn hard_drive() -> Vec<u8> {
        let mut data = Vec::from(1u32.to_le_bytes());
        data.extend_from_slice(&0x800u64.to_le_bytes());
        data.extend_from_slice(&0x10_0000u64.to_le_bytes());
        data.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 9, 10, 11, 12, 13, 14, 15, 16]);
        data.extend_from_slice(&[HardDrive::GPT, HardDrive::GUID_SIGNATURE]);
        node(HardDrive::TYPE, &data)
    }

    /// The T14's ESP on a stick on 00:14.0, its nodes as that firmware named
    /// them on a bench run; the partition's GUID, start and size are made up.
    fn stick() -> Vec<u8> {
        let mut acpi = Vec::from(PNP0A03.to_le_bytes());
        acpi.extend_from_slice(&0u32.to_le_bytes());
        [node((2, 1), &acpi), node((1, 1), &[0, 0x14]), node((3, 5), &[0, 0]), hard_drive(), end()].concat()
    }

    #[test]
    fn a_firmware_path_decodes_node_by_node_and_reads_back_as_text() {
        let bytes = stick();
        let path = Path::parse(&bytes).unwrap();
        let known: Vec<Known> = path.nodes().map(|n| n.decode()).collect();
        assert_eq!(known[..3], [Known::Acpi { hid: PNP0A03, uid: 0 }, Known::Pci { device: 0x14, function: 0 }, Known::Usb { port: 0, interface: 0 }]);
        let Known::HardDrive(hd) = known[3] else { panic!("{:?}", known[3]) };
        assert_eq!((hd.number, hd.start, hd.size), (1, 0x800, 0x10_0000));
        assert_eq!(hd.guid(), Some([0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 9, 10, 11, 12, 13, 14, 15, 16]));
        assert_eq!(
            path.to_string(),
            "PciRoot(0x0)/Pci(0x14,0x0)/USB(0x0,0x0)/HD(1,GPT,44332211-6655-8877-090A-0B0C0D0E0F10,0x800,0x100000)"
        );
        assert_eq!(path.bytes(), &bytes[..]);
    }

    #[test]
    fn a_node_off_its_defined_length_is_kept_whole_and_not_decoded() {
        let bytes = [node((1, 1), &[0, 0x14, 0xaa]), node((3, 0x0b), &[0xde, 0xad]), end()].concat();
        let path = Path::parse(&bytes).unwrap();
        assert!(path.nodes().all(|n| n.decode() == Known::Other));
        assert_eq!(path.to_string(), "Path(1,1,0014AA)/Path(3,11,DEAD)");
    }

    #[test]
    fn the_end_node_alone_is_a_path_of_no_nodes() {
        let bytes = end();
        assert_eq!(Path::parse(&bytes).unwrap().nodes().count(), 0);
    }

    /// Every way a hostile path is refused, with where: nothing short of a
    /// whole path, ending in its one End node, parses.
    #[test]
    fn hostile_bytes_are_refused_by_name() {
        let good = stick();
        let pci = node((1, 1), &[0, 0x14]);
        let mut zero_len = pci.clone();
        zero_len[2..4].copy_from_slice(&0u16.to_le_bytes());
        let mut three = pci.clone();
        three[2..4].copy_from_slice(&3u16.to_le_bytes());
        let mut long = pci.clone();
        long[2..4].copy_from_slice(&u16::MAX.to_le_bytes());
        let cases: [(Vec<u8>, Malformed); 9] = [
            (Vec::new(), Malformed::Unterminated { at: 0 }),
            (good[..good.len() - HEADER].to_vec(), Malformed::Unterminated { at: good.len() - HEADER }),
            (good[..good.len() - 1].to_vec(), Malformed::Unterminated { at: good.len() - HEADER }),
            ([zero_len, end()].concat(), Malformed::Short { at: 0, len: 0 }),
            ([three, end()].concat(), Malformed::Short { at: 0, len: 3 }),
            ([long, end()].concat(), Malformed::Overrun { at: 0, len: usize::from(u16::MAX) }),
            ([pci.clone(), node(END_INSTANCE, &[]), pci.clone(), end()].concat(), Malformed::Instances { at: 6 }),
            ([pci.clone(), end(), pci.clone()].concat(), Malformed::Trailing { at: 6 }),
            ([pci, node(END_ENTIRE, &[0])].concat(), Malformed::Trailing { at: 6 }),
        ];
        for (bytes, want) in cases {
            assert_eq!(Path::parse(&bytes).map(|_| ()), Err(want), "{bytes:02x?}");
        }
    }
}
