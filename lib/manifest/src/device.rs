//! What a `devices` or `drives` entry names.
//!
//! A class or an ISA set is the ABI's own spelling
//! ([`DeviceRequest::parse`]). A PCI function is matched here, by its class
//! code and optionally its card and its place: `pci:CC:SS:PP`, then
//! `/VVVV:DDDD`, then `@BB:DD.F` or `@boot`, lowercase hex, where `PP` may be
//! `*` for every programming interface of the subclass. A place tells apart two
//! functions of one card, as two controllers on one bridge are; `@boot` is the
//! controller of the device firmware loaded the loader from, wherever that is
//! ([`toyos_abi::boot::BootSources::loader`]). **Every matcher carries its class**, so whether two entries
//! can name one function is decided from their text alone ([`Entry::overlaps`]),
//! and a claim names the function a matcher found by its arrival.

use core::fmt;

use toyos_abi::inventory::{Pci, PciAddr};
use toyos_abi::syscall::{DeviceRequest, PciId};

/// One entry of a row's `devices` or `drives`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Entry {
    /// A class or an ISA set: one device by its own name.
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
            (Self::Pci(a), Self::Pci(b)) => {
                let places = match (a.at, b.at) {
                    (Some(Place::At(x)), Some(Place::At(y))) => x == y,
                    _ => true,
                };
                a.class == b.class && either(a.prog_if, b.prog_if) && either(a.id, b.id) && places
            }
            _ => false,
        }
    }
}

/// Whether two optional fields can hold one value: either is unset, or both
/// are the same.
fn either<T: PartialEq>(x: Option<T>, y: Option<T>) -> bool {
    x.is_none() || y.is_none() || x == y
}

/// Which PCI functions an entry names: every function of a class code, or of
/// one card within it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PciMatch {
    /// Base class and subclass.
    pub class: [u8; 2],
    /// The programming interface, or `None` for every one: a subclass whose
    /// interfaces are one register model told apart by vendor extensions (HD
    /// Audio's 00 and 80) is driven alike whichever a function reports.
    pub prog_if: Option<u8>,
    pub id: Option<PciId>,
    pub at: Option<Place>,
}

/// What the supervisor says the boot controller is under, followed by its
/// address as the kernel's `pcidev:` lines spell one, `bb:dd.f`.
pub const BOOT_SAID: &str = "supervisor: `@boot` is PCI ";

/// Where a matched function is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// Bus, device and function, on segment 0.
    At((u8, u8, u8)),
    /// The boot controller, where the loader named one.
    Boot,
}

impl PciMatch {
    pub const PREFIX: &'static str = "pci:";

    /// Every function of class code `class`, of card `id` where it names one.
    pub const fn new(class: [u8; 3], id: Option<PciId>) -> Self {
        Self { class: [class[0], class[1]], prog_if: Some(class[2]), id, at: None }
    }

    /// Every function of `class`'s base class and subclass, whatever its
    /// programming interface.
    pub const fn any_interface(class: [u8; 2], id: Option<PciId>) -> Self {
        Self { class, prog_if: None, id, at: None }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(Self::PREFIX)?;
        let (rest, at) = match rest.split_once('@') {
            Some((rest, "boot")) => (rest, Some(Place::Boot)),
            Some((rest, place)) => {
                let (bus, slot) = place.split_once(':')?;
                let (dev, func) = slot.split_once('.')?;
                let func = match func.as_bytes() {
                    [digit @ b'0'..=b'7'] => digit - b'0',
                    _ => return None,
                };
                (rest, Some(Place::At((hex8(bus)?, hex8(dev).filter(|d| *d < 32)?, func))))
            }
            None => (rest, None),
        };
        let (code, card) = match rest.split_once('/') {
            Some((code, card)) => {
                let (vendor, device) = card.split_once(':')?;
                (code, Some(PciId { vendor: hex16(vendor)?, device: hex16(device)? }))
            }
            None => (rest, None),
        };
        let mut fields = code.split(':');
        let class = [hex8(fields.next()?)?, hex8(fields.next()?)?];
        let prog_if = match fields.next()? {
            "*" => None,
            byte => Some(hex8(byte)?),
        };
        if fields.next().is_some() {
            return None;
        }
        Some(Self { class, prog_if, id: card, at })
    }

    /// Whether `pci` is a function this names, on a machine whose boot
    /// controller is `boot`: with none, `@boot` names nothing.
    pub fn matches(&self, pci: &Pci, boot: Option<PciAddr>) -> bool {
        [pci.class, pci.subclass] == self.class
            && self.prog_if.is_none_or(|prog_if| prog_if == pci.prog_if)
            && self.id.is_none_or(|id| id.vendor == pci.vendor && id.device == pci.device)
            && self.at.is_none_or(|place| match place {
                Place::At(at) => pci.at.segment == 0 && (pci.at.bus, pci.at.dev, pci.at.func) == at,
                Place::Boot => boot == Some(pci.at),
            })
    }
}

impl fmt::Display for PciMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [class, subclass] = self.class;
        write!(f, "{}{class:02x}:{subclass:02x}:", Self::PREFIX)?;
        match self.prog_if {
            Some(prog_if) => write!(f, "{prog_if:02x}")?,
            None => f.write_str("*")?,
        }
        if let Some(id) = self.id {
            write!(f, "/{:04x}:{:04x}", id.vendor, id.device)?;
        }
        match self.at {
            Some(Place::At((bus, dev, func))) => write!(f, "@{bus:02x}:{dev:02x}.{func}"),
            Some(Place::Boot) => f.write_str("@boot"),
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
    use toyos_abi::inventory::{Arrival, Driven};
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
        for text in
            ["pci:01:08:02/1b36:0010", "pci:0c:03:30", "pci:ff:00:00/1234:11e8", "pci:00:00:00", "pci:04:03:*", "pci:04:03:*/8086:a0c8",
             "pci:0c:03:30/1022:1639@03:00.3", "pci:0c:03:30@00:14.0", "pci:0c:03:30@boot"]
        {
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
            "pci:", "pci", "pci:04:03:**", "pci:04:*:*", "pci:04:03:*:00", "pci:04:03:",
            // The card is four lowercase hex digits each and nothing else: two
            // spellings would be two labels for one claim.
            "pci:02:00:00/1Af4:1041", "pci:02:00:00/1af4:41", "pci:02:00:00/1af4", "pci:02:00:00/1af4:1041:",
            "pci:02:00:00/:", "pci:02:00:00/zzzz:1041", "pci:02:00:00/1af4:104g", "pci:02:00:00/+af4:1041",
            // A place is two digits, two digits below 0x20, and one octal one.
            "pci:0c:03:30@3:00.3", "pci:0c:03:30@03:20.0", "pci:0c:03:30@03:00.8", "pci:0c:03:30@03:00", "pci:0c:03:30@03:00.3/1022:1639",
            "pci:0c:03:30@", "pci:0c:03:30@03:0A.0", "pci:0c:03:30@Boot", "pci:0c:03:30@boot/1022:1639",
        ] {
            assert_eq!(PciMatch::parse(bad), None, "{bad} parsed");
            assert_eq!(Entry::parse(bad), None, "{bad} parsed as an entry");
        }
    }

    #[test]
    fn a_fixed_entry_is_the_abis_spelling() {
        assert_eq!(Entry::parse("framebuffer"), Some(Entry::Fixed(DeviceRequest::Class(DeviceType::Framebuffer))));
        assert!(matches!(Entry::parse("isa:0060,0064:1,12"), Some(Entry::Fixed(DeviceRequest::Isa(_)))));
        assert_eq!(Entry::parse("gpu"), None);
    }

    #[test]
    fn a_matcher_matches_its_class_and_its_card() {
        assert!(NVME.matches(&function([0x01, 0x08, 0x02], 0x1b36, 0x0010), None));
        assert!(!NVME.matches(&function([0x01, 0x08, 0x02], 0x144d, 0xa808), None));
        assert!(!NVME.matches(&function([0x01, 0x08, 0x01], 0x1b36, 0x0010), None));
        let any_nvme = PciMatch::new([0x01, 0x08, 0x02], None);
        assert!(any_nvme.matches(&function([0x01, 0x08, 0x02], 0x144d, 0xa808), None));
    }

    /// A place tells two functions of one card apart, and two matchers of
    /// one card at two places name no function in common.
    #[test]
    fn a_place_matches_its_function_alone() {
        let at = |func: u8| Pci { at: PciAddr { segment: 0, bus: 3, dev: 0, func }, ..function([0x0c, 0x03, 0x30], 0x1022, 0x1639) };
        let three = PciMatch::parse("pci:0c:03:30/1022:1639@03:00.3").unwrap();
        let four = PciMatch::parse("pci:0c:03:30/1022:1639@03:00.4").unwrap();
        assert!(three.matches(&at(3), None) && !three.matches(&at(4), None));
        assert!(!Entry::Pci(three).overlaps(&Entry::Pci(four)));
        assert!(Entry::Pci(three).overlaps(&Entry::Pci(PciMatch::parse("pci:0c:03:30").unwrap())));
        let elsewhere = Pci { at: PciAddr { segment: 1, bus: 3, dev: 0, func: 3 }, ..at(3) };
        assert!(!three.matches(&elsewhere, None), "a place is on segment 0");
    }

    /// `@boot` matches the boot controller, wherever it is, and nothing else;
    /// on a machine the loader named none for it matches nothing. It names one
    /// function, so it may be the one a place names and overlaps it.
    #[test]
    fn the_boot_place_matches_the_boot_controller() {
        let at = |func: u8| PciAddr { segment: 0, bus: 3, dev: 0, func };
        let xhci = |func: u8| Pci { at: at(func), ..function([0x0c, 0x03, 0x30], 0x1022, 0x1639) };
        let boot = PciMatch::parse("pci:0c:03:30@boot").unwrap();
        assert!(boot.matches(&xhci(3), Some(at(3))) && !boot.matches(&xhci(4), Some(at(3))));
        assert!(boot.matches(&xhci(4), Some(at(4))) && !boot.matches(&xhci(3), Some(at(4))));
        assert!(!boot.matches(&xhci(3), None) && !boot.matches(&xhci(4), None));
        let nvme = function([0x01, 0x08, 0x02], 0x1b36, 0x0010);
        assert!(!boot.matches(&nvme, Some(nvme.at)), "its class still holds");
        let four = PciMatch::parse("pci:0c:03:30/1022:1639@03:00.4").unwrap();
        assert!(Entry::Pci(boot).overlaps(&Entry::Pci(four)));
        assert!(!Entry::Pci(boot).overlaps(&Entry::Pci(PciMatch::parse("pci:01:08:02@boot").unwrap())));
    }

    /// The T14's HD Audio controller reports programming interface 0x80 and
    /// QEMU's 0x00: one matcher names both, and nothing outside the subclass.
    #[test]
    fn a_wildcard_interface_matches_every_interface_of_its_subclass() {
        let hda = PciMatch::parse("pci:04:03:*").unwrap();
        assert!(hda.matches(&function([0x04, 0x03, 0x80], 0x8086, 0xa0c8), None));
        assert!(hda.matches(&function([0x04, 0x03, 0x00], 0x8086, 0x293e), None));
        assert!(!hda.matches(&function([0x04, 0x01, 0x00], 0x1af4, 0x1059), None));
        assert!(!PciMatch::parse("pci:04:03:00").unwrap().matches(&function([0x04, 0x03, 0x80], 0x8086, 0xa0c8), None));
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
            ("pci:04:03:*", "pci:04:03:80"),
            ("pci:04:03:00/8086:293e", "pci:04:03:*"),
            ("framebuffer", "framebuffer"),
            ("isa:0060,0064:1,12", "isa:0060,0064:1,12"),
        ];
        for (a, b) in overlapping {
            assert!(entry(a).overlaps(&entry(b)), "{a} and {b}");
        }
        let apart = [
            ("pci:01:08:02/1b36:0010", "pci:01:08:02/144d:a808"),
            ("pci:01:08:02", "pci:01:08:01"),
            ("pci:04:03:*", "pci:04:01:00"),
            ("pci:01:08:02/1b36:0010", "pci:01:08:01/1b36:0010"),
            ("framebuffer", "acpi"),
            ("pci:0c:03:30", "acpi"),
        ];
        for (a, b) in apart {
            assert!(!entry(a).overlaps(&entry(b)), "{a} and {b}");
        }
    }
}
