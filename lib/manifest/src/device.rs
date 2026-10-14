//! What a `devices` or `drives` entry names.
//!
//! A class, a partition or an ISA set is the ABI's own spelling
//! ([`DeviceRequest::parse`]). A PCI function is matched here, by its class
//! code and optionally its card: `pci:CC:SS:PP` or `pci:CC:SS:PP/VVVV:DDDD`,
//! lowercase hex. **Every matcher carries its class**, so whether two entries
//! can name one function is decided from their text alone ([`Entry::overlaps`]),
//! and a claim names the function a matcher found by its arrival.

use core::fmt;

use toyos_abi::inventory::Pci;
use toyos_abi::syscall::{DeviceRequest, PciId};

/// One entry of a row's `devices` or `drives`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// A class, a partition or an ISA set: one device by its own name.
    Fixed(DeviceRequest),
    Pci(PciMatch),
}

impl Entry {
    pub fn parse(text: &str) -> Option<Self> {
        match text.strip_prefix(PciMatch::PREFIX) {
            Some(_) => PciMatch::parse(text).map(Self::Pci),
            None => DeviceRequest::parse(text).map(Self::Fixed),
        }
    }

    /// Whether one device can answer both: the same fixed name, or two
    /// matchers of one class whose cards do not differ.
    pub fn overlaps(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fixed(a), Self::Fixed(b)) => a == b,
            (Self::Pci(a), Self::Pci(b)) => a.class == b.class && (a.id.is_none() || b.id.is_none() || a.id == b.id),
            _ => false,
        }
    }
}

/// Which PCI functions an entry names: every function of a class code, or of
/// one card within it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PciMatch {
    /// Base class, subclass and programming interface.
    pub class: [u8; 3],
    pub id: Option<PciId>,
}

impl PciMatch {
    pub const PREFIX: &'static str = "pci:";

    /// Every function of class `class`, of card `id` where it names one.
    pub const fn new(class: [u8; 3], id: Option<PciId>) -> Self {
        Self { class, id }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(Self::PREFIX)?;
        let (code, card) = match rest.split_once('/') {
            Some((code, card)) => {
                let (vendor, device) = card.split_once(':')?;
                (code, Some(PciId { vendor: hex16(vendor)?, device: hex16(device)? }))
            }
            None => (rest, None),
        };
        let mut class = [0u8; 3];
        let mut fields = code.split(':');
        for byte in &mut class {
            *byte = hex8(fields.next()?)?;
        }
        if fields.next().is_some() {
            return None;
        }
        Some(Self { class, id: card })
    }

    pub fn matches(&self, pci: &Pci) -> bool {
        [pci.class, pci.subclass, pci.prog_if] == self.class
            && self.id.is_none_or(|id| id.vendor == pci.vendor && id.device == pci.device)
    }
}

impl fmt::Display for PciMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [class, subclass, prog_if] = self.class;
        write!(f, "{}{class:02x}:{subclass:02x}:{prog_if:02x}", Self::PREFIX)?;
        match self.id {
            Some(id) => write!(f, "/{:04x}:{:04x}", id.vendor, id.device),
            None => Ok(()),
        }
    }
}

/// Exactly two lowercase hex digits: one spelling per value, because the
/// label a claim is endowed under is the entry's text.
fn hex8(text: &str) -> Option<u8> {
    lower_hex(text, 2).and_then(|v| u8::try_from(v).ok())
}

/// Exactly four lowercase hex digits, for [`hex8`]'s reason.
fn hex16(text: &str) -> Option<u16> {
    lower_hex(text, 4).and_then(|v| u16::try_from(v).ok())
}

fn lower_hex(text: &str, digits: usize) -> Option<u32> {
    let lowercase = text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if text.len() != digits || !lowercase {
        return None;
    }
    u32::from_str_radix(text, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_abi::inventory::{Arrival, Driven, PciAddr};
    use toyos_abi::syscall::DeviceType;

    const NVME: PciMatch = PciMatch::new([0x01, 0x08, 0x02], Some(PciId { vendor: 0x1b36, device: 0x0010 }));

    fn function(class: [u8; 3], vendor: u16, device: u16) -> Pci {
        Pci {
            arrival: Arrival(1),
            at: PciAddr { segment: 0, bus: 0, dev: 3, func: 0 },
            vendor,
            device,
            class: class[0],
            subclass: class[1],
            prog_if: class[2],
            driven: Driven::Free,
        }
    }

    #[test]
    fn a_matcher_writes_back_the_text_it_parsed() {
        for text in ["pci:01:08:02/1b36:0010", "pci:0c:03:30", "pci:ff:00:00/1234:11e8", "pci:00:00:00"] {
            let parsed = PciMatch::parse(text).unwrap_or_else(|| panic!("{text} did not parse"));
            assert_eq!(parsed.to_string(), text);
            assert_eq!(Entry::parse(text), Some(Entry::Pci(parsed)));
        }
        assert_eq!(PciMatch::parse("pci:01:08:02/1b36:0010"), Some(NVME));
    }

    /// One spelling per matcher, because the label a claim is endowed under is
    /// the entry's text and a driver looks it up by its own.
    #[test]
    fn a_matcher_has_one_spelling() {
        for bad in [
            "pci:1b36:0010", // a card without its class
            "pci:01:08", "pci:01:08:02:00", "pci:1:08:02", "pci:01:08:0A", "pci:01:08:02/",
            "pci:01:08:02/1B36:0010", "pci:01:08:02/1b36", "pci:01:08:02/1b36:0010/x", "pci:+1:08:02",
            "pci:", "pci",
            // The card is four lowercase hex digits each and nothing else: two
            // spellings would be two labels for one claim.
            "pci:02:00:00/1Af4:1041", "pci:02:00:00/1af4:41", "pci:02:00:00/1af4", "pci:02:00:00/1af4:1041:",
            "pci:02:00:00/:", "pci:02:00:00/zzzz:1041", "pci:02:00:00/1af4:104g", "pci:02:00:00/+af4:1041",
        ] {
            assert_eq!(PciMatch::parse(bad), None, "{bad} parsed");
            assert_eq!(Entry::parse(bad), None, "{bad} parsed as an entry");
        }
    }

    #[test]
    fn a_fixed_entry_is_the_abis_spelling() {
        assert_eq!(Entry::parse("keyboard"), Some(Entry::Fixed(DeviceRequest::Class(DeviceType::Keyboard))));
        assert!(matches!(Entry::parse("isa:0060,0064:1,12"), Some(Entry::Fixed(DeviceRequest::Isa(_)))));
        assert_eq!(Entry::parse("gpu"), None);
    }

    #[test]
    fn a_matcher_matches_its_class_and_its_card() {
        assert!(NVME.matches(&function([0x01, 0x08, 0x02], 0x1b36, 0x0010)));
        assert!(!NVME.matches(&function([0x01, 0x08, 0x02], 0x144d, 0xa808)));
        assert!(!NVME.matches(&function([0x01, 0x08, 0x01], 0x1b36, 0x0010)));
        let any_nvme = PciMatch::new([0x01, 0x08, 0x02], None);
        assert!(any_nvme.matches(&function([0x01, 0x08, 0x02], 0x144d, 0xa808)));
    }

    /// Whether two entries can name one device, decided from their text: the
    /// build refuses a manifest where they can.
    #[test]
    fn two_matchers_overlap_where_one_function_can_answer_both() {
        let entry = |text| Entry::parse(text).unwrap();
        let overlapping = [
            ("pci:01:08:02/1b36:0010", "pci:01:08:02/1b36:0010"),
            ("pci:01:08:02", "pci:01:08:02/1b36:0010"),
            ("pci:01:08:02/1b36:0010", "pci:01:08:02"),
            ("pci:0c:03:30", "pci:0c:03:30"),
            ("framebuffer", "framebuffer"),
            ("isa:0060,0064:1,12", "isa:0060,0064:1,12"),
            ("part:0FC63DAF-8483-4772-8E79-3D69D8477DE4", "part:0FC63DAF-8483-4772-8E79-3D69D8477DE4"),
        ];
        for (a, b) in overlapping {
            assert!(entry(a).overlaps(&entry(b)), "{a} and {b}");
        }
        let apart = [
            ("pci:01:08:02/1b36:0010", "pci:01:08:02/144d:a808"),
            ("pci:01:08:02", "pci:01:08:01"),
            ("pci:01:08:02/1b36:0010", "pci:01:08:01/1b36:0010"),
            ("framebuffer", "keyboard"),
            ("pci:0c:03:30", "keyboard"),
        ];
        for (a, b) in apart {
            assert!(!entry(a).overlaps(&entry(b)), "{a} and {b}");
        }
    }
}
