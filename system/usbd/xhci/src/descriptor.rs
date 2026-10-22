//! What a device says of itself over its default control pipe: the device
//! descriptor (USB 2.0 §9.6.1) and the interfaces of its configuration
//! (§9.6.3, §9.6.5), decoded from bytes the device chose.
//!
//! **Every length here is the device's, so every length is bounded twice**:
//! by what the descriptor claims and by what actually arrived. A descriptor
//! that claims more than arrived, a walk that would step zero bytes, and a
//! type that is not the one asked for are refused by name; nothing here
//! panics or loops on a device's word.

use alloc::vec::Vec;

/// The device descriptor's type, and the configuration's and an interface's
/// (USB 2.0 Table 9-5).
const DEVICE: u8 = 1;
const CONFIGURATION: u8 = 2;
const INTERFACE: u8 = 4;
const ENDPOINT: u8 = 5;
/// HID 1.11 §6.2.1's HID descriptor, and its report descriptor's type.
const HID: u8 = 0x21;
pub const REPORT: u8 = 0x22;
/// USB 3.2 §9.6.7's SuperSpeed Endpoint Companion.
const COMPANION: u8 = 0x30;
const ENDPOINT_BYTES: usize = 7;

/// A device descriptor's length (§9.6.1), and the eight bytes of it that every
/// device can deliver before its EP0 packet size is known.
pub const DEVICE_BYTES: usize = 18;
pub const PREFIX_BYTES: usize = 8;

/// A configuration descriptor's own header (§9.6.3), and an interface's (§9.6.5).
const CONFIGURATION_BYTES: usize = 9;
const INTERFACE_BYTES: usize = 9;

/// The interfaces one configuration may name here, and the endpoints one
/// interface may. A device offering more is not refused: the rest are not
/// named.
pub const MAX_INTERFACES: usize = 8;
pub const MAX_ENDPOINTS: usize = 4;

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
    /// bcdDevice.
    pub release: u16,
    /// iSerialNumber: the string descriptor naming its serial, 0 for none.
    pub serial: u8,
    pub configurations: u8,
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
        release: le16(12),
        serial: bytes[16],
        configurations: bytes[17],
    })
}

/// One endpoint of an interface (USB 2.0 §9.6.6), with its SuperSpeed
/// companion's burst (USB 3.2 §9.6.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
    /// bEndpointAddress: the number, and bit 7 for IN.
    pub address: u8,
    /// bmAttributes' transfer type: 0 control, 1 isochronous, 2 bulk, 3
    /// interrupt.
    pub kind: u8,
    /// wMaxPacketSize's size, bits 10:0.
    pub max_packet: u16,
    pub interval: u8,
    pub max_burst: u8,
}

pub const BULK: u8 = 2;
pub const INTERRUPT: u8 = 3;

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    /// Its Device Context Index (xHCI 1.2 §4.5.1): twice its number, and one
    /// more for IN.
    pub fn dci(&self) -> u8 {
        (self.address & 0x0F) * 2 + u8::from(self.is_in())
    }
}

/// One interface at alternate setting 0, and what follows it up to the next.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Interface {
    pub number: u8,
    pub class: Class,
    pub endpoints: Vec<Endpoint>,
    /// A HID interface's report descriptor length (HID 1.11 §6.2.1).
    pub report_bytes: Option<u16>,
}

/// A configuration (§9.6.3) and its interfaces, in the order it names them.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Configuration {
    /// bConfigurationValue, what SET_CONFIGURATION selects it by.
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

/// Walk a configuration descriptor, over `bytes` and no further than its
/// `wTotalLength` says the configuration reaches.
///
/// A walk that reaches the end of what arrived before `wTotalLength` is not
/// refused: a device asked for fewer bytes than its total answers fewer, and
/// what arrived is still a prefix of the configuration. Only alternate
/// setting 0 is read: an alternate is the same interface again.
pub fn configuration(bytes: &[u8]) -> Result<Configuration, Refused> {
    header(bytes, CONFIGURATION, CONFIGURATION_BYTES)?;
    let total = u16::from_le_bytes([bytes[2], bytes[3]]);
    if usize::from(total) < CONFIGURATION_BYTES {
        return Err(Refused::Total(total));
    }
    let end = bytes.len().min(usize::from(total));
    let mut found = Configuration { value: bytes[5], interfaces: Vec::new() };
    // Whether what follows belongs to an interface being read.
    let mut reading = false;
    let mut at = usize::from(bytes[0]);
    // Each step moves at least two bytes, so the walk ends within `end / 2`.
    while at + 2 <= end {
        let (len, kind) = (usize::from(bytes[at]), bytes[at + 1]);
        if len < 2 {
            return Err(Refused::Length { at, says: bytes[at] });
        }
        let need = match kind {
            INTERFACE => INTERFACE_BYTES,
            ENDPOINT => ENDPOINT_BYTES,
            _ => 2,
        };
        if len < need {
            return Err(Refused::Length { at, says: bytes[at] });
        }
        // Cut short by what arrived: a prefix names what it holds whole.
        if at + len.max(need) > end {
            break;
        }
        let d = &bytes[at..at + len];
        match kind {
            INTERFACE => {
                reading = d[3] == 0 && found.interfaces.len() < MAX_INTERFACES;
                if reading {
                    found.interfaces.push(Interface {
                        number: d[2],
                        class: Class { class: d[5], subclass: d[6], protocol: d[7] },
                        endpoints: Vec::new(),
                        report_bytes: None,
                    });
                }
            }
            ENDPOINT if reading => {
                let interface = found.interfaces.last_mut().expect("reading one");
                if interface.endpoints.len() < MAX_ENDPOINTS {
                    interface.endpoints.push(Endpoint {
                        address: d[2],
                        kind: d[3] & 0x3,
                        max_packet: u16::from_le_bytes([d[4], d[5]]) & 0x7FF,
                        interval: d[6],
                        max_burst: 0,
                    });
                }
            }
            COMPANION if reading && len >= 3 => {
                if let Some(endpoint) = found.interfaces.last_mut().and_then(|i| i.endpoints.last_mut()) {
                    endpoint.max_burst = d[2].min(15);
                }
            }
            HID if reading && len >= 9 => {
                // bNumDescriptors at 5, then (type, length) pairs from 6.
                let report = d[6..].as_chunks::<3>().0.iter().take(usize::from(d[5])).find(|c| c[0] == REPORT);
                if let Some(c) = report {
                    found.interfaces.last_mut().expect("reading one").report_bytes = Some(u16::from_le_bytes([c[1], c[2]]));
                }
            }
            _ => {}
        }
        at += len;
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
        let found = configuration(&QEMU_STORAGE_CONFIG).unwrap();
        assert_eq!(found.value, 1);
        let [msc] = &found.interfaces[..] else { panic!("{found:?}") };
        assert_eq!(msc.class, Class { class: 8, subclass: 6, protocol: 0x50 });
        let bulk = |address| Endpoint { address, kind: BULK, max_packet: 512, interval: 0, max_burst: 0 };
        assert_eq!(msc.endpoints, [bulk(0x81), bulk(0x02)]);
        assert_eq!((msc.endpoints[0].dci(), msc.endpoints[1].dci()), (3, 4));
        assert_eq!(msc.report_bytes, None);
    }

    #[test]
    fn a_short_answer_or_another_type_is_refused_by_name() {
        assert_eq!(device(&QEMU_STORAGE[..17]), Err(Refused::Short { arrived: 17, need: 18 }));
        assert_eq!(ep0_packet(&QEMU_STORAGE[..7]), Err(Refused::Short { arrived: 7, need: 8 }));
        assert_eq!(device(&QEMU_STORAGE_CONFIG[..18]), Err(Refused::Type { want: 1, got: 2 }));
        assert_eq!(configuration(&QEMU_STORAGE), Err(Refused::Type { want: 2, got: 1 }));
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
            assert_eq!(configuration(&bad), Err(Refused::Length { at: 18, says }));
        }
        let mut bad = QEMU_STORAGE_CONFIG;
        bad[9] = 8;
        assert_eq!(configuration(&bad), Err(Refused::Length { at: 9, says: 8 }));
        let mut bad = QEMU_STORAGE_CONFIG;
        bad[2] = 8;
        assert_eq!(configuration(&bad), Err(Refused::Total(8)));
    }

    /// The walk stops at `wTotalLength` whatever else arrived, and at what
    /// arrived whatever `wTotalLength` says.
    #[test]
    fn the_walk_is_bounded_by_the_total_and_by_what_arrived() {
        let mut two = [0u8; 41];
        two[..32].copy_from_slice(&QEMU_STORAGE_CONFIG);
        two[32..].copy_from_slice(&[9, 4, 1, 0, 0, 0x03, 0x01, 0x01, 0]);
        assert_eq!(configuration(&two).unwrap().interfaces.len(), 1, "past wTotalLength");
        two[2] = 41;
        assert_eq!(configuration(&two).unwrap().interfaces.len(), 2);
        assert_eq!(configuration(&two[..40]).unwrap().interfaces.len(), 1, "cut short by what arrived");
        assert_eq!(configuration(&QEMU_STORAGE_CONFIG[..9]).unwrap().interfaces.len(), 0);
    }

    /// An alternate setting is the same interface again, and is not named twice.
    #[test]
    fn only_alternate_setting_zero_is_named() {
        let mut alt = [0u8; 27];
        alt[..18].copy_from_slice(&QEMU_STORAGE_CONFIG[..18]);
        alt[18..].copy_from_slice(&[9, 4, 0, 1, 0, 0x08, 0x06, 0x50, 0]);
        alt[2] = 27;
        assert_eq!(configuration(&alt).unwrap().interfaces.len(), 1);
    }

    /// A HID interface's own descriptor names its report descriptor's length,
    /// and a SuperSpeed companion its endpoint's burst.
    #[test]
    fn a_hid_interface_names_its_report_length_and_a_companion_its_burst() {
        let bytes = [
            9, 2, 41, 0, 1, 1, 0, 0xa0, 50, //
            9, 4, 0, 0, 1, 0x03, 0x01, 0x01, 0, //
            9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, //
            7, 5, 0x81, 3, 8, 0, 10, //
            6, 0x30, 4, 0, 0, 0, //
            3, 0x99, 0,
        ];
        let found = configuration(&bytes).unwrap();
        let hid = &found.interfaces[0];
        assert_eq!(hid.report_bytes, Some(63));
        assert_eq!(hid.endpoints, [Endpoint { address: 0x81, kind: INTERRUPT, max_packet: 8, interval: 10, max_burst: 4 }]);
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
            let _ = configuration(&bytes[..len]);
        }
    }
}
