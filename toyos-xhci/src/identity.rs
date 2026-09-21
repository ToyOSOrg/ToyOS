//! Whether a mass-storage device that has just bound is a disk this driver lost
//! to its own reset, come back; and how long such a disk is waited for.
//!
//! **A reset can move a device off its port.** A SuperSpeed stick enumerated on
//! the USB2 half of its receptacle can answer a bus reset by training on the
//! USB3 half, which is another root-hub port, a new slot and a new enumeration.
//! A disk whose device left its port within [`RETURN_WINDOW`] of a reset this
//! driver made is held for that long ([`returns_by`]), and a device that binds
//! meanwhile takes its number only if [`same`] says it is that device.
//!
//! **Where it arrives is not evidence and is not checked.** xHCI 1.2 §4.19.7:
//! the mapping of root-hub ports to physical connectors "is defined by platform
//! implementations and outside the scope of this specification"; the Supported
//! Protocol capability (§7.2) says which ports speak USB2 and which USB3, never
//! which two share a connector, and Appendix D leaves that to ACPI `_PLD` group
//! tokens. A companion port could only say where to look, and the port machine
//! already enumerates whatever connects anywhere; it could never say that the
//! device is the same one, which is the whole question.
//!
//! **What the device says of itself is the evidence, and every field must
//! agree**: the device descriptor's idVendor, idProduct and bcdDevice and the
//! string its iSerialNumber names (USB 2.0 §9.6.1, §9.6.7), INQUIRY's vendor,
//! product and revision (SPC-4 §6.4.2), and READ CAPACITY's sector count and
//! size. Speed, port, slot and endpoint descriptors are not identity: the move
//! this exists for changes all of them.
//!
//! **A device with no readable serial number is never adopted**, as the one
//! that left or as the one that arrived: every other field is its model's, so
//! nothing would tell it from another unit of the same model plugged in during
//! the window, and serving one stick's volume from another is the loss this
//! refuses.

/// Nanoseconds since boot.
pub type Nanos = u64;

/// How long after this driver reset a disk's port the disk is held for its
/// device to come back, once the device has been seen to leave.
///
/// T14 run 79 measured 0.98 s from the port rung's reset (1.897 s) to the same
/// stick bound on its companion port (2.878 s): the old port going empty, the
/// SuperSpeed link training, the enumeration and the bind's own commands. Two
/// seconds is twice that, and is the port rung's and the last rung's bounds
/// together — what the ladder already gave a reset device to answer, which a
/// device that left made moot.
pub const RETURN_WINDOW: Nanos = 2_000_000_000;

/// When a disk whose device was seen gone at `now` is given up on, if it is to
/// be waited for at all: only a disk whose port this driver reset at
/// `reset_at`, and only inside [`RETURN_WINDOW`] of that reset. A device that
/// leaves without a reset of ours was unplugged.
pub fn returns_by(reset_at: Option<Nanos>, now: Nanos) -> Option<Nanos> {
    let by = reset_at?.saturating_add(RETURN_WINDOW);
    (now < by).then_some(by)
}

/// The device descriptor's own name for what it is (USB 2.0 §9.6.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct UsbId {
    pub vendor: u16,
    pub product: u16,
    /// bcdDevice.
    pub release: u16,
}

/// The longest string a descriptor can carry: bLength is one byte, less the
/// two-byte header, in whole UTF-16 code units.
pub const SERIAL_MAX: usize = 252;

/// The serial number a device publishes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Serial {
    /// iSerialNumber is zero, or names an empty string.
    Absent,
    /// It names one, and the string descriptor did not arrive whole and well
    /// formed.
    Unread,
    /// The string's UTF-16LE code units as the descriptor carried them.
    Read { len: u8, units: [u8; SERIAL_MAX] },
}

impl Serial {
    /// A string descriptor (USB 2.0 §9.6.7) as it arrived: `bLength`,
    /// `bDescriptorType` 3, then UTF-16LE code units. Anything else is
    /// [`Serial::Unread`], never a guess at what was meant.
    pub fn from_descriptor(arrived: &[u8]) -> Self {
        let [length, kind, ..] = *arrived else { return Self::Unread };
        let length = usize::from(length);
        if kind != 3 || length < 2 || length > arrived.len() || length % 2 != 0 {
            return Self::Unread;
        }
        let carried = &arrived[2..length];
        if carried.is_empty() {
            return Self::Absent;
        }
        let mut units = [0u8; SERIAL_MAX];
        units[..carried.len()].copy_from_slice(carried);
        Self::Read { len: carried.len() as u8, units }
    }
}

impl core::fmt::Display for Serial {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Absent => f.write_str("none published"),
            Self::Unread => f.write_str("named and not read"),
            Self::Read { len, units } => {
                f.write_str("\"")?;
                for unit in units[..usize::from(*len)].chunks_exact(2) {
                    let unit = u16::from_le_bytes([unit[0], unit[1]]);
                    // Device-supplied: rendered without letting it choose what the
                    // log looks like.
                    let c = match u8::try_from(unit) {
                        Ok(b) if (0x20..0x7F).contains(&b) && b != b'"' => char::from(b),
                        _ => '.',
                    };
                    write!(f, "{c}")?;
                }
                f.write_str("\"")
            }
        }
    }
}

/// Everything a disk says of itself that another unit could not also say.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Identity {
    pub usb: UsbId,
    pub serial: Serial,
    /// INQUIRY bytes 8 to 36: T10 vendor, product and product revision.
    pub inquiry: [u8; 28],
    pub sectors: u64,
    pub sector_bytes: u32,
}

/// The first reason the device that bound is not the disk that left.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Differs {
    /// The disk that left had no readable serial number.
    LeftUnnamed,
    /// The device that bound has none.
    ArrivedUnnamed,
    Vendor,
    Product,
    Release,
    Serial,
    Inquiry,
    Capacity,
}

impl core::fmt::Display for Differs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::LeftUnnamed => {
                "the disk that left published no serial number it could be told apart by"
            }
            Self::ArrivedUnnamed => "it publishes no serial number it could be told apart by",
            Self::Vendor => "its USB vendor differs",
            Self::Product => "its USB product differs",
            Self::Release => "its USB release (bcdDevice) differs",
            Self::Serial => "its serial number differs",
            Self::Inquiry => "its INQUIRY vendor, product or revision differs",
            Self::Capacity => "its capacity differs",
        })
    }
}

/// Whether `arrived` is the device that was `left`: every field, and a serial
/// number on both.
pub fn same(left: &Identity, arrived: &Identity) -> Result<(), Differs> {
    if !matches!(left.serial, Serial::Read { .. }) {
        return Err(Differs::LeftUnnamed);
    }
    if !matches!(arrived.serial, Serial::Read { .. }) {
        return Err(Differs::ArrivedUnnamed);
    }
    let checks = [
        (left.usb.vendor == arrived.usb.vendor, Differs::Vendor),
        (left.usb.product == arrived.usb.product, Differs::Product),
        (left.usb.release == arrived.usb.release, Differs::Release),
        (left.serial == arrived.serial, Differs::Serial),
        (left.inquiry == arrived.inquiry, Differs::Inquiry),
        (
            left.sectors == arrived.sectors && left.sector_bytes == arrived.sector_bytes,
            Differs::Capacity,
        ),
    ];
    checks.into_iter().find(|(agrees, _)| !agrees).map_or(Ok(()), |(_, why)| Err(why))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Nanos = 1_000_000;

    fn serial(text: &str) -> Serial {
        let mut descriptor = [0u8; 256];
        let mut at = 2;
        for unit in text.encode_utf16() {
            descriptor[at..at + 2].copy_from_slice(&unit.to_le_bytes());
            at += 2;
        }
        descriptor[0] = at as u8;
        descriptor[1] = 3;
        Serial::from_descriptor(&descriptor[..at])
    }

    /// The T14's stick as run 79 printed it, with a serial of its shape.
    fn stick() -> Identity {
        let mut inquiry = [b' '; 28];
        inquiry[..8].copy_from_slice(b"SanDisk ");
        inquiry[8..13].copy_from_slice(b"Ultra");
        inquiry[24..].copy_from_slice(b"1.00");
        Identity {
            usb: UsbId { vendor: 0x0781, product: 0x5581, release: 0x0100 },
            serial: serial("4C530001230821115363"),
            inquiry,
            sectors: 7_507_812 * 8,
            sector_bytes: 512,
        }
    }

    #[test]
    fn the_same_device_is_the_same_disk() {
        assert_eq!(same(&stick(), &stick()), Ok(()));
    }

    /// Each field alone refuses, and names itself: a stick of the same model
    /// plugged in during the window differs in its serial number at least.
    #[test]
    fn a_device_that_differs_in_any_field_is_a_new_disk() {
        let changes: [(fn(&mut Identity), Differs); 7] = [
            (|i| i.usb.vendor ^= 1, Differs::Vendor),
            (|i| i.usb.product ^= 1, Differs::Product),
            (|i| i.usb.release ^= 1, Differs::Release),
            (|i| i.serial = serial("4C530001230821115364"), Differs::Serial),
            (|i| i.inquiry[27] ^= 1, Differs::Inquiry),
            (|i| i.sectors -= 1, Differs::Capacity),
            (|i| i.sector_bytes = 4096, Differs::Capacity),
        ];
        for (change, why) in changes {
            let mut arrived = stick();
            change(&mut arrived);
            assert_eq!(same(&stick(), &arrived), Err(why));
        }
    }

    /// With no serial on either side the rest is the model's, which a second
    /// unit shares field for field.
    #[test]
    fn a_device_with_no_readable_serial_is_never_adopted() {
        for unnamed in [Serial::Absent, Serial::Unread] {
            let mut left = stick();
            left.serial = unnamed;
            assert_eq!(same(&left, &left), Err(Differs::LeftUnnamed), "{unnamed:?}");
            let mut arrived = stick();
            arrived.serial = unnamed;
            assert_eq!(same(&stick(), &arrived), Err(Differs::ArrivedUnnamed), "{unnamed:?}");
        }
    }

    #[test]
    fn a_string_descriptor_is_read_only_whole_and_well_formed() {
        assert!(matches!(serial("1"), Serial::Read { len: 2, .. }));
        assert_eq!(serial(""), Serial::Absent);
        // Not a string descriptor.
        assert_eq!(Serial::from_descriptor(&[4, 1, b'1', 0]), Serial::Unread);
        // Claims more than arrived.
        assert_eq!(Serial::from_descriptor(&[6, 3, b'1', 0]), Serial::Unread);
        // Half a code unit.
        assert_eq!(Serial::from_descriptor(&[3, 3, b'1']), Serial::Unread);
        assert_eq!(Serial::from_descriptor(&[1]), Serial::Unread);
        assert_eq!(Serial::from_descriptor(&[]), Serial::Unread);
        // Bytes past bLength are not the string's.
        assert_eq!(Serial::from_descriptor(&[4, 3, b'1', 0, b'2', 0]), serial("1"));
        // The longest a descriptor can say.
        let mut longest = [b'A'; 254];
        longest[0] = 254;
        longest[1] = 3;
        assert!(matches!(Serial::from_descriptor(&longest), Serial::Read { len: 252, .. }));
    }

    #[test]
    fn a_serial_prints_as_the_characters_it_carries_and_nothing_else() {
        extern crate std;
        use std::string::ToString;
        assert_eq!(serial("AB\"1\u{e9}").to_string(), "\"AB.1.\"");
        assert_eq!(Serial::Absent.to_string(), "none published");
    }

    /// Run 79's own numbers: the reset at 1.897 s, the port empty by 1.948 s,
    /// the same stick bound at 2.878 s.
    #[test]
    fn a_disk_is_waited_for_only_after_a_reset_of_ours_and_only_inside_the_window() {
        let reset = 1_897 * MS;
        let by = returns_by(Some(reset), 1_948 * MS).expect("inside the window");
        assert!(2_878 * MS < by, "run 79's stick came back inside it");
        assert_eq!(by, reset + RETURN_WINDOW);
        assert_eq!(returns_by(None, 1_948 * MS), None, "a device that left unreset was unplugged");
        assert_eq!(returns_by(Some(reset), reset + RETURN_WINDOW), None);
    }
}
