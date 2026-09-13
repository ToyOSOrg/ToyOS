//! ACPI resource descriptors, decoded into the memory windows a PCI root
//! bridge decodes.
//!
//! The bytes are the ones ACPI 6.5 §6.4.3.5 defines and firmware emits in two
//! places for the same bridge: `_CRS` in the DSDT, which is what Linux reads,
//! and `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL::Configuration` (UEFI 2.10 §14.2),
//! which is what the bootloader reads and this decodes. What a window is, and
//! what an address outside every one of them is, is
//! [`toyos_abi::boot::KernelArgs::root_bridge_windows`].
//!
//! Input is firmware-supplied and untrusted: no path panics, the walk
//! terminates on every input, and every refusal is a [`ResourceError`].
//! `tests/corpus.rs` holds that claim over both firmwares' committed bytes.

use crate::Phys;
use toyos_abi::boot::RootBridgeWindow;

/// The furthest a descriptor list is walked. The protocol answers with a
/// pointer and no length, so this is what bounds the walk of a list whose End
/// Tag is missing, and it is what a reader's [`Phys::readable`] bounds itself
/// by.
pub const MAX_LIST_BYTES: usize = 1024;

/// ACPI 6.5 §6.4.2.9: the small item that ends every resource list, over one
/// checksum byte.
const END_TAG: u8 = 0x79;
/// ACPI 6.5 §6.4.3.5.1: the QWORD Address Space Descriptor.
const QWORD_ADDRESS_SPACE: u8 = 0x8A;
/// Its fields, from the descriptor's first byte.
const RESOURCE_TYPE: usize = 3;
const GENERAL_FLAGS: usize = 4;
const QWORD_MINIMUM: usize = 14;
const QWORD_MAXIMUM: usize = 22;
const QWORD_TRANSLATION: usize = 30;
const QWORD_LENGTH: usize = 38;
const QWORD_BYTES: usize = QWORD_LENGTH + 8;

/// ACPI 6.5 Table 6.44: what a resource type byte names. Every other value is
/// reserved or vendor-defined.
const TYPE_MEMORY: u8 = 0;
const TYPE_IO: u8 = 1;
const TYPE_BUS: u8 = 2;
/// ACPI 6.5 Table 6.43, bit 0 of the General Flags: set where the device
/// consumes the range itself rather than forwarding it downstream.
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
    /// A window whose address on the bridge's two sides differs. What this
    /// answers is which addresses a *CPU* may issue, and translating one is a
    /// machine nothing here has read.
    Translated { min: u64, offset: u64 },
    /// A memory range the bridge consumes rather than forwards — its own
    /// registers, not a range anything behind it decodes.
    Consumed { min: u64 },
    /// More memory windows than the caller has room for.
    TooMany { room: usize },
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
                write!(f, "more memory windows than the {room} there is room for")
            }
        }
    }
}

/// What one walk of a descriptor list found.
pub struct Walk {
    /// How far the list ran: to and including its End Tag, or through the
    /// descriptor the walk refused, or as far as the reader would go. **This is
    /// the evidence** — what the bootloader logs the raw bytes of, on a list
    /// this decoder reads and on one it refuses alike.
    pub bytes: usize,
    /// How many memory windows were written into the caller's slice, or why the
    /// list cannot be used.
    pub windows: Result<usize, ResourceError>,
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

fn u64le<P: Phys>(phys: P, at: u64, offset: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= u64::from(phys.byte(at + offset as u64 + i)) << (8 * i);
    }
    v
}

/// Every memory window the list at `at` names, written into `out`.
///
/// **A refusal carries no windows at all**: a caller handed the ones decoded
/// before the refusal would be holding an aperture with a hole in it, and would
/// place a BAR in the hole.
pub fn memory_windows<P: Phys>(phys: P, at: u64, out: &mut [RootBridgeWindow]) -> Walk {
    let mut offset = 0usize;
    let mut found = 0usize;
    loop {
        if offset >= MAX_LIST_BYTES {
            return Walk { bytes: offset.min(MAX_LIST_BYTES), windows: Err(ResourceError::Unterminated) };
        }
        let item = match item(phys, at, offset) {
            Ok(item) => item,
            Err(why) => return Walk { bytes: offset, windows: Err(why) },
        };
        let head = at + offset as u64;
        // Stepped over the refusing descriptor as well, so the bytes logged
        // beside a refusal are the ones the refusal is about.
        let through = offset + item.len;
        if item.tag == END_TAG {
            return Walk { bytes: through.min(MAX_LIST_BYTES), windows: Ok(found) };
        }
        if item.tag != QWORD_ADDRESS_SPACE {
            return Walk { bytes: through, windows: Err(ResourceError::UnknownTag { tag: item.tag }) };
        }
        if item.len < QWORD_BYTES {
            let why = ResourceError::Short { tag: item.tag, whole: item.len, needed: QWORD_BYTES };
            return Walk { bytes: through, windows: Err(why) };
        }
        let kind = phys.byte(head + RESOURCE_TYPE as u64);
        if !matches!(kind, TYPE_MEMORY | TYPE_IO | TYPE_BUS) {
            let why = ResourceError::UnknownResourceType { kind };
            return Walk { bytes: through, windows: Err(why) };
        }
        if kind == TYPE_MEMORY {
            let min = u64le(phys, head, QWORD_MINIMUM);
            let max = u64le(phys, head, QWORD_MAXIMUM);
            let length = u64le(phys, head, QWORD_LENGTH);
            let translation = u64le(phys, head, QWORD_TRANSLATION);
            let flags = phys.byte(head + GENERAL_FLAGS as u64);
            // A window of no length decodes nothing, and firmware emits one
            // where a range is declared and unused.
            if length != 0 {
                let refusal = if min.checked_add(length - 1) != Some(max) {
                    Some(ResourceError::Inconsistent { min, max, length })
                } else if translation != 0 {
                    Some(ResourceError::Translated { min, offset: translation })
                } else if flags & CONSUMER != 0 {
                    Some(ResourceError::Consumed { min })
                } else {
                    None
                };
                if let Some(why) = refusal {
                    return Walk { bytes: through, windows: Err(why) };
                }
                let room = out.len();
                match out.get_mut(found) {
                    Some(slot) => *slot = RootBridgeWindow { base: min, length },
                    None => {
                        return Walk { bytes: through, windows: Err(ResourceError::TooMany { room }) }
                    }
                }
                found += 1;
            }
        }
        offset = through;
    }
}
