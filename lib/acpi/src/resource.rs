//! ACPI resource descriptors (ACPI 6.5 §6.4), decoded into the bus range and
//! memory windows a PCI root bridge's `_CRS` says it forwards ([`root_bridge`]),
//! and into the I/O ports a device names its registers by ([`io_ports`]).
//!
//! Input is firmware-supplied and untrusted: no path panics, the walk
//! terminates on every input, and every refusal is a [`ResourceError`].

use crate::Phys;
use toyos_abi::acpi::RootBridgeWindow;

/// The furthest a descriptor list is walked: what bounds the walk of a list
/// whose End Tag is missing, beside its reader's own [`Phys::readable`].
pub const MAX_LIST_BYTES: usize = 4096;

/// ACPI 6.5 §6.4.2.9: the small item that ends every resource list, over one
/// checksum byte.
const END_TAG: u8 = 0x79;
/// ACPI 6.5 §6.4.3.5: the four Address Space Descriptors, each a large item
/// whose fields from the resource type to the length have one order and
/// differ in width ([`Layout`]).
const QWORD_ADDRESS_SPACE: u8 = 0x8A;
const DWORD_ADDRESS_SPACE: u8 = 0x87;
const WORD_ADDRESS_SPACE: u8 = 0x88;
const EXTENDED_ADDRESS_SPACE: u8 = 0x8B;
/// The fields every one of them carries at the same place.
const RESOURCE_TYPE: usize = 3;
const GENERAL_FLAGS: usize = 4;

/// Where one Address Space Descriptor keeps its granularity, minimum,
/// maximum, translation offset and length, each `width` bytes wide and in
/// that order from `first` (ACPI 6.5 Tables 6.47, 6.48, 6.49 and 6.50).
struct Layout {
    first: usize,
    width: usize,
}

impl Layout {
    const fn of(tag: u8) -> Option<Self> {
        match tag {
            WORD_ADDRESS_SPACE => Some(Self { first: 6, width: 2 }),
            DWORD_ADDRESS_SPACE => Some(Self { first: 6, width: 4 }),
            QWORD_ADDRESS_SPACE => Some(Self { first: 6, width: 8 }),
            // A revision and a reserved byte before the granularity.
            EXTENDED_ADDRESS_SPACE => Some(Self { first: 8, width: 8 }),
            _ => None,
        }
    }

    /// The `n`th of the five fields, from the descriptor's first byte.
    const fn field(&self, n: usize) -> usize {
        self.first + n * self.width
    }

    /// The bytes the five fields need.
    const fn bytes(&self) -> usize {
        self.field(5)
    }
}

/// ACPI 6.5 Table 6.44: what a resource type byte names. Every other value is
/// reserved or vendor-defined.
const TYPE_MEMORY: u8 = 0;
const TYPE_IO: u8 = 1;
const TYPE_BUS: u8 = 2;
/// ACPI 6.5 Table 6.47, bit 0 of the General Flags: set where the device
/// consumes the range itself rather than producing it for what lies below.
const CONSUMER: u8 = 1;

/// Why a list of resource descriptors cannot be used.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResourceError {
    /// The reader would not give up the bytes a descriptor's own header names.
    Unreadable { at: u64, len: usize },
    /// No End Tag inside [`MAX_LIST_BYTES`].
    Unterminated,
    /// A descriptor this decoder does not implement. Refused by name rather
    /// than stepped over: a window silently dropped is an aperture the kernel
    /// would then place a BAR outside of.
    UnknownTag { tag: u8 },
    /// An address space descriptor naming a resource type this decoder does
    /// not implement. Refused for [`Self::UnknownTag`]'s reason: a range whose
    /// kind is unread may be memory.
    UnknownResourceType { kind: u8 },
    /// An address space descriptor whose whole length cannot hold the fields
    /// its own tag defines.
    Short { tag: u8, whole: usize, needed: usize },
    /// A window whose two accounts of its extent disagree: firmware named a
    /// maximum that is not its minimum plus its length.
    Inconsistent { min: u64, max: u64, length: u64 },
    /// A memory window whose address on the bridge's two sides differs: a
    /// non-zero `_TRA`. A window is declared in the addresses a CPU issues,
    /// and a device's above the CPU's own rest on the two being one.
    Translated { min: u64, offset: u64 },
    /// A memory range the bridge consumes rather than produces — its own
    /// registers, not a range anything behind it decodes. Refused rather than
    /// stepped over: a root bridge's `_CRS` that names one says something
    /// about its memory this decoder does not read.
    Consumed { min: u64 },
    /// A bus number range past bus 0xff, or a second one: a root's buses
    /// are one run of one segment's.
    Buses { min: u64, max: u64 },
    /// More memory windows, or I/O runs, than the caller has room for.
    TooMany { room: usize },
    /// An I/O Port Descriptor naming a range of bases the OS chooses among,
    /// not one fixed run.
    Relocatable { min: u16, max: u16 },
    /// An I/O run of no ports.
    NoPorts { port: u16 },
}

impl core::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable { at, len } => write!(f, "{len} bytes at {at:#x} are unreadable"),
            Self::Unterminated => write!(f, "no End Tag inside {MAX_LIST_BYTES} bytes"),
            Self::UnknownTag { tag } => write!(f, "descriptor tag {tag:#04x} is not decoded here"),
            Self::UnknownResourceType { kind } => {
                write!(f, "resource type {kind:#04x} is not decoded here")
            }
            Self::Short { tag, whole, needed } => {
                write!(f, "descriptor tag {tag:#04x} is {whole} bytes, and its fields need {needed}")
            }
            Self::Inconsistent { min, max, length } => {
                write!(f, "{min:#x}..={max:#x} is not {length:#x} bytes long")
            }
            Self::Translated { min, offset } => {
                write!(f, "the window at {min:#x} is translated by {offset:#x}")
            }
            Self::Consumed { min } => {
                write!(f, "the range at {min:#x} is consumed by the bridge, not forwarded")
            }
            Self::TooMany { room } => {
                write!(f, "more memory windows or I/O runs than the {room} there is room for")
            }
            Self::Buses { min, max } => write!(f, "the bus range {min:#x}..={max:#x} is not the root's one run of buses"),
            Self::Relocatable { min, max } => write!(f, "the I/O run is relocatable between {min:#x} and {max:#x}"),
            Self::NoPorts { port } => write!(f, "the I/O run at {port:#x} is no ports long"),
        }
    }
}

/// One descriptor's tag and its whole length, header included.
struct Item {
    tag: u8,
    len: usize,
}

/// The descriptor `offset` bytes into the list at `at`, whole.
///
/// ACPI 6.5 §6.4: bit 7 of the first byte picks the encoding. A large item's
/// tag is that whole byte and the two bytes after it are its *body* length; a
/// small item carries both in the one byte. Every tag steps the same way, so a
/// tag this decoder refuses is still one it knows the length of.
///
/// The whole descriptor is checked readable here, so no caller reads a byte of
/// one whose own header put it past what the reader has.
fn item<P: Phys>(phys: P, at: u64, offset: usize) -> Result<Item, ResourceError> {
    let Some(head) = at.checked_add(offset as u64) else {
        return Err(ResourceError::Unreadable { at, len: offset });
    };
    if !phys.readable(head, 1) {
        return Err(ResourceError::Unreadable { at: head, len: 1 });
    }
    let tag = phys.byte(head);
    let len = if tag & 0x80 == 0 {
        1 + (tag & 0x07) as usize
    } else {
        if !phys.readable(head, 3) {
            return Err(ResourceError::Unreadable { at: head, len: 3 });
        }
        3 + (u16::from(phys.byte(head + 1)) | u16::from(phys.byte(head + 2)) << 8) as usize
    };
    if !phys.readable(head, len) {
        return Err(ResourceError::Unreadable { at: head, len });
    }
    Ok(Item { tag, len })
}

/// The `width`-byte little-endian field `offset` bytes into the descriptor at `at`.
fn field<P: Phys>(phys: P, at: u64, offset: usize, width: usize) -> u64 {
    (0..width).fold(0u64, |v, i| v | u64::from(phys.byte(at + (offset + i) as u64)) << (8 * i))
}

/// What a root bridge's `_CRS` produces: its bus range, where it names one,
/// and how many memory windows [`root_bridge`] wrote.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RootResources {
    pub buses: Option<(u8, u8)>,
    pub windows: usize,
}

/// The bus range and every memory window the root bridge's `_CRS` list at
/// `at` says it forwards, the windows written into `out`.
///
/// **A window is a memory range an Address Space Descriptor produces**
/// (ACPI 6.5 §6.4.3.5); its buses are the one bus number range one produces,
/// inside bus 0xff. Its I/O ranges, in those descriptors and in the small I/O
/// Port and Fixed I/O Port descriptors (§6.4.2.5, §6.4.2.6), are stepped
/// over; every other descriptor is refused by name,
/// as is a memory range whose two accounts of its extent disagree, that the
/// bridge consumes, or that a non-zero `_TRA` translates. A range of no
/// length is none.
///
/// **A refusal carries no windows at all**: a caller handed the ones decoded
/// before the refusal would be holding an aperture with a hole in it, and would
/// place a BAR in the hole.
pub fn root_bridge<P: Phys>(phys: P, at: u64, out: &mut [RootBridgeWindow]) -> Result<RootResources, ResourceError> {
    let mut offset = 0usize;
    let mut found = 0usize;
    let mut buses = None;
    loop {
        if offset >= MAX_LIST_BYTES {
            return Err(ResourceError::Unterminated);
        }
        let item = item(phys, at, offset)?;
        let head = at + offset as u64;
        offset += item.len;
        let layout = match item.tag {
            END_TAG => return Ok(RootResources { buses, windows: found }),
            IO_PORT | FIXED_IO_PORT => continue,
            tag => Layout::of(tag).ok_or(ResourceError::UnknownTag { tag })?,
        };
        if item.len < layout.bytes() {
            return Err(ResourceError::Short { tag: item.tag, whole: item.len, needed: layout.bytes() });
        }
        let kind = phys.byte(head + RESOURCE_TYPE as u64);
        if !matches!(kind, TYPE_MEMORY | TYPE_IO | TYPE_BUS) {
            return Err(ResourceError::UnknownResourceType { kind });
        }
        let read = |n| field(phys, head, layout.field(n), layout.width);
        let (min, max, translation, length) = (read(1), read(2), read(3), read(4));
        if kind == TYPE_IO || length == 0 {
            continue;
        }
        if min.checked_add(length - 1) != Some(max) {
            return Err(ResourceError::Inconsistent { min, max, length });
        }
        if kind == TYPE_BUS {
            buses = match (buses, u8::try_from(min), u8::try_from(max)) {
                (None, Ok(first), Ok(last)) => Some((first, last)),
                _ => return Err(ResourceError::Buses { min, max }),
            };
            continue;
        }
        if translation != 0 {
            return Err(ResourceError::Translated { min, offset: translation });
        }
        if phys.byte(head + GENERAL_FLAGS as u64) & CONSUMER != 0 {
            return Err(ResourceError::Consumed { min });
        }
        let room = out.len();
        *out.get_mut(found).ok_or(ResourceError::TooMany { room })? = RootBridgeWindow { base: min, length };
        found += 1;
    }
}

/// ACPI 6.5 §6.4.2.5: the I/O Port Descriptor, a small item of seven bytes:
/// its decode, its range's minimum and maximum base, alignment and length.
const IO_PORT: u8 = 0x47;
/// ACPI 6.5 §6.4.2.6: the Fixed Location I/O Port Descriptor, a small item of
/// three bytes: a 10-bit base and a length.
const FIXED_IO_PORT: u8 = 0x4B;

/// The first port of each I/O run the list at `at` names, in its order,
/// written into `out`, and how many: what a device that names its own
/// registers by `_CRS` and by nothing else names them with (ACPI 6.5 §12.11,
/// the embedded controller's).
///
/// **Only a run firmware fixed is a port**: an I/O Port Descriptor whose
/// minimum and maximum base differ names a range the OS chooses within, and
/// is refused, as is a run of no ports and any descriptor but these two.
pub fn io_ports<P: Phys>(phys: P, at: u64, out: &mut [u16]) -> Result<usize, ResourceError> {
    let mut offset = 0usize;
    let mut found = 0usize;
    loop {
        if offset >= MAX_LIST_BYTES {
            return Err(ResourceError::Unterminated);
        }
        let item = item(phys, at, offset)?;
        let head = at + offset as u64;
        let byte = |i: u64| phys.byte(head + i);
        let (min, max, length) = match item.tag {
            END_TAG => return Ok(found),
            IO_PORT => {
                let base = |i| u16::from(byte(i)) | u16::from(byte(i + 1)) << 8;
                (base(2), base(4), byte(7))
            }
            FIXED_IO_PORT => {
                let base = (u16::from(byte(1)) | u16::from(byte(2)) << 8) & 0x3FF;
                (base, base, byte(3))
            }
            tag => return Err(ResourceError::UnknownTag { tag }),
        };
        if min != max {
            return Err(ResourceError::Relocatable { min, max });
        }
        if length == 0 {
            return Err(ResourceError::NoPorts { port: min });
        }
        let room = out.len();
        match out.get_mut(found) {
            Some(slot) => *slot = min,
            None => return Err(ResourceError::TooMany { room }),
        }
        found += 1;
        offset += item.len;
    }
}
