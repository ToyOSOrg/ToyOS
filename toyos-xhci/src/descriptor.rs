//! What a device says of itself over its default control pipe: the device
//! descriptor (USB 2.0 §9.6.1) and the interfaces of its configuration
//! (§9.6.3, §9.6.5), decoded from bytes the device chose.
//!
//! **Every length here is the device's, so every length is bounded twice**:
//! by what the descriptor claims and by what actually arrived. A descriptor
//! that claims more than arrived, a walk that would step zero bytes, and a
//! type that is not the one asked for are refused by name; nothing here
//! panics or loops on a device's word.

/// The device descriptor's type, and the configuration's and an interface's
/// (USB 2.0 Table 9-5).
const DEVICE: u8 = 1;
const CONFIGURATION: u8 = 2;
const INTERFACE: u8 = 4;

/// A device descriptor's length (§9.6.1), and the eight bytes of it that every
/// device can deliver before its EP0 packet size is known.
pub const DEVICE_BYTES: usize = 18;
pub const PREFIX_BYTES: usize = 8;

/// A configuration descriptor's own header (§9.6.3), and an interface's (§9.6.5).
const CONFIGURATION_BYTES: usize = 9;
const INTERFACE_BYTES: usize = 9;

/// The interfaces one configuration may name here. A device offering more is
/// not refused: the rest are not named.
pub const MAX_INTERFACES: usize = 8;

/// Why a device's answer is not the descriptor it was asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Fewer bytes arrived than the descriptor's own header needs.
    Short { arrived: usize, need: usize },
    /// The descriptor names another type than the one asked for.
    Type { want: u8, got: u8 },
    /// A length field below the descriptor's own size, which would make a
    /// walk step backwards or not at all.
    Length { at: usize, says: u8 },
    /// `wTotalLength` is shorter than the configuration's own header.
    Total(u16),
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Short { arrived, need } => write!(f, "{arrived} byte(s) arrived where {need} are needed"),
            Self::Type { want, got } => write!(f, "descriptor type {got} where {want} was asked for"),
            Self::Length { at, says } => write!(f, "the descriptor at byte {at} says it is {says} byte(s) long"),
            Self::Total(total) => write!(f, "wTotalLength {total} is shorter than the configuration's own header"),
        }
    }
}

/// The fields of a device descriptor this crate's callers read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Device {
    pub class: Class,
    pub vendor: u16,
    pub product: u16,
}

/// A class triple, the device's or an interface's (USB-IF class codes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Class {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// The descriptor's `bMaxPacketSize0`, from the first [`PREFIX_BYTES`] of it.
pub fn ep0_packet(prefix: &[u8]) -> Result<u8, Refused> {
    header(prefix, DEVICE, PREFIX_BYTES)?;
    Ok(prefix[7])
}

/// The whole device descriptor.
pub fn device(bytes: &[u8]) -> Result<Device, Refused> {
    header(bytes, DEVICE, DEVICE_BYTES)?;
    let le16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    Ok(Device {
        class: Class { class: bytes[4], subclass: bytes[5], protocol: bytes[6] },
        vendor: le16(8),
        product: le16(10),
    })
}

/// The interfaces a configuration names, at alternate setting 0, in the order
/// it names them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Interfaces {
    found: [Class; MAX_INTERFACES],
    count: usize,
}

impl Interfaces {
    pub fn iter(&self) -> impl Iterator<Item = Class> + '_ {
        self.found[..self.count].iter().copied()
    }
}

/// Walk a configuration descriptor's interfaces, over `bytes` and no further
/// than its `wTotalLength` says the configuration reaches.
///
/// A walk that reaches the end of what arrived before `wTotalLength` is not
/// refused: a device asked for fewer bytes than its total answers fewer, and
/// what arrived is still a prefix of the configuration.
pub fn interfaces(bytes: &[u8]) -> Result<Interfaces, Refused> {
    header(bytes, CONFIGURATION, CONFIGURATION_BYTES)?;
    let total = u16::from_le_bytes([bytes[2], bytes[3]]);
    if usize::from(total) < CONFIGURATION_BYTES {
        return Err(Refused::Total(total));
    }
    let end = bytes.len().min(usize::from(total));
    let mut found = Interfaces { found: [Class { class: 0, subclass: 0, protocol: 0 }; MAX_INTERFACES], count: 0 };
    let mut at = usize::from(bytes[0]);
    // Each step moves at least two bytes, so the walk ends within `end / 2`.
    while at + 2 <= end {
        let (len, kind) = (bytes[at], bytes[at + 1]);
        if len < 2 {
            return Err(Refused::Length { at, says: len });
        }
        if kind == INTERFACE {
            if usize::from(len) < INTERFACE_BYTES {
                return Err(Refused::Length { at, says: len });
            }
            // Cut short by what arrived: a prefix names what it holds whole.
            if at + INTERFACE_BYTES > end {
                break;
            }
            let alternate = bytes[at + 3];
            if alternate == 0 && found.count < MAX_INTERFACES {
                found.found[found.count] =
                    Class { class: bytes[at + 5], subclass: bytes[at + 6], protocol: bytes[at + 7] };
                found.count += 1;
            }
        }
        at += usize::from(len);
    }
    Ok(found)
}

/// The descriptor's first two bytes against what was asked for: its type, and
/// a length no shorter than `need` that arrived whole.
fn header(bytes: &[u8], want: u8, need: usize) -> Result<(), Refused> {
    if bytes.len() < need {
        return Err(Refused::Short { arrived: bytes.len(), need });
    }
    if bytes[1] != want {
        return Err(Refused::Type { want, got: bytes[1] });
    }
    if usize::from(bytes[0]) < need {
        return Err(Refused::Length { at: 0, says: bytes[0] });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QEMU 11.1's `usb-storage` device descriptor (`hw/usb/dev-storage.c`'s
    /// `desc_device_high` through `desc-msd`): USB 2.0, class at the
    /// interface, EP0 of 64, vendor 46f4 product 0001 release 0.00.
    const QEMU_STORAGE: [u8; 18] =
        [18, 1, 0x00, 0x02, 0, 0, 0, 64, 0xf4, 0x46, 0x01, 0x00, 0x00, 0x00, 1, 2, 3, 1];

    /// Its configuration: one interface, class 08 subclass 06 protocol 50
    /// (SCSI over Bulk-Only), with its two bulk endpoints.
    const QEMU_STORAGE_CONFIG: [u8; 32] = [
        9, 2, 32, 0, 1, 1, 0, 0xc0, 50, //
        9, 4, 0, 0, 2, 0x08, 0x06, 0x50, 0, //
        7, 5, 0x81, 2, 0x00, 0x02, 0, //
        7, 5, 0x02, 2, 0x00, 0x02, 0,
    ];

    #[test]
    fn a_device_descriptor_names_its_maker_its_product_and_its_class() {
        let d = device(&QEMU_STORAGE).unwrap();
        assert_eq!((d.vendor, d.product), (0x46f4, 0x0001));
        assert_eq!(d.class, Class { class: 0, subclass: 0, protocol: 0 }, "named per interface");
        assert_eq!(ep0_packet(&QEMU_STORAGE[..PREFIX_BYTES]), Ok(64));
    }

    #[test]
    fn a_configuration_names_its_interfaces_and_steps_over_its_endpoints() {
        let found = interfaces(&QEMU_STORAGE_CONFIG).unwrap();
        let mut named = found.iter();
        let msc = Class { class: 8, subclass: 6, protocol: 0x50 };
        assert_eq!(named.next(), Some(msc));
        assert_eq!(named.next(), None);
    }

    #[test]
    fn a_short_answer_or_another_type_is_refused_by_name() {
        assert_eq!(device(&QEMU_STORAGE[..17]), Err(Refused::Short { arrived: 17, need: 18 }));
        assert_eq!(ep0_packet(&QEMU_STORAGE[..7]), Err(Refused::Short { arrived: 7, need: 8 }));
        assert_eq!(device(&QEMU_STORAGE_CONFIG[..18]), Err(Refused::Type { want: 1, got: 2 }));
        assert_eq!(interfaces(&QEMU_STORAGE), Err(Refused::Type { want: 2, got: 1 }));
        let mut short = QEMU_STORAGE;
        short[0] = 17;
        assert_eq!(device(&short), Err(Refused::Length { at: 0, says: 17 }));
    }

    /// A descriptor that says it is zero or one bytes long would have the walk
    /// stand still or read its own type as the next length.
    #[test]
    fn a_length_that_would_not_move_the_walk_is_refused() {
        for says in [0u8, 1] {
            let mut bad = QEMU_STORAGE_CONFIG;
            bad[18] = says;
            assert_eq!(interfaces(&bad), Err(Refused::Length { at: 18, says }));
        }
        let mut bad = QEMU_STORAGE_CONFIG;
        bad[9] = 8;
        assert_eq!(interfaces(&bad), Err(Refused::Length { at: 9, says: 8 }));
        let mut bad = QEMU_STORAGE_CONFIG;
        bad[2] = 8;
        assert_eq!(interfaces(&bad), Err(Refused::Total(8)));
    }

    /// The walk stops at `wTotalLength` whatever else arrived, and at what
    /// arrived whatever `wTotalLength` says.
    #[test]
    fn the_walk_is_bounded_by_the_total_and_by_what_arrived() {
        let mut two = [0u8; 41];
        two[..32].copy_from_slice(&QEMU_STORAGE_CONFIG);
        two[32..].copy_from_slice(&[9, 4, 1, 0, 0, 0x03, 0x01, 0x01, 0]);
        assert_eq!(interfaces(&two).unwrap().iter().count(), 1, "past wTotalLength");
        two[2] = 41;
        assert_eq!(interfaces(&two).unwrap().iter().count(), 2);
        assert_eq!(interfaces(&two[..40]).unwrap().iter().count(), 1, "cut short by what arrived");
        assert_eq!(interfaces(&QEMU_STORAGE_CONFIG[..9]).unwrap().iter().count(), 0);
    }

    /// An alternate setting is the same interface again, and is not named twice.
    #[test]
    fn only_alternate_setting_zero_is_named() {
        let mut alt = [0u8; 27];
        alt[..18].copy_from_slice(&QEMU_STORAGE_CONFIG[..18]);
        alt[18..].copy_from_slice(&[9, 4, 0, 1, 0, 0x08, 0x06, 0x50, 0]);
        alt[2] = 27;
        assert_eq!(interfaces(&alt).unwrap().iter().count(), 1);
    }

    /// Every arrangement of a short buffer is refused or walked, never a panic:
    /// the bytes are the device's.
    #[test]
    fn no_answer_panics_the_decoder() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..200_000 {
            let len = (next() % 64) as usize;
            let mut bytes = [0u8; 64];
            for b in bytes.iter_mut().take(len) {
                *b = next() as u8;
            }
            // Half of them start like a configuration, so the walk is reached.
            if next() & 1 == 0 && len >= 4 {
                bytes[1] = CONFIGURATION;
                bytes[0] = 9;
            }
            let _ = device(&bytes[..len]);
            let _ = ep0_packet(&bytes[..len]);
            let _ = interfaces(&bytes[..len]);
        }
    }
}
