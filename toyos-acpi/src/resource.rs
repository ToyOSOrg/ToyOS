//! ACPI resource descriptors, decoded into the memory windows a PCI root
//! bridge decodes.
//!
//! The bytes are the ones ACPI 6.5 §6.4.3.5 defines and firmware emits in two
//! places for the same bridge: `_CRS` in the DSDT, which is what Linux reads,
//! and `EFI_PCI_ROOT_BRIDGE_IO_PROTOCOL::Configuration` (UEFI 2.10 §14.2),
//! which is what the bootloader reads and this decodes. **An address outside
//! every window here is not free space, it is unrouted** — a read of it answers
//! all-ones, which an absent device answers too — so nothing may derive a
//! window from what is merely unused.
//!
//! Input is firmware-supplied and untrusted: no path panics, the walk
//! terminates on every input, and every refusal is a [`ResourceError`].

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
/// Its fields, from the descriptor's first byte. Granularity at 6 and the two
/// flag bytes at 4 and 5 are not read: nothing here decides on them.
const RESOURCE_TYPE: usize = 3;
const QWORD_MINIMUM: usize = 14;
const QWORD_MAXIMUM: usize = 22;
const QWORD_TRANSLATION: usize = 30;
const QWORD_LENGTH: usize = 38;
/// The whole descriptor, which is the last field's end.
const QWORD_BYTES: usize = QWORD_LENGTH + 8;

/// ACPI 6.5 Table 6.44: what a resource type byte names.
const TYPE_MEMORY: u8 = 0;

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
    /// An address space descriptor whose declared length cannot hold the fields
    /// its own tag defines.
    Short { tag: u8, declared: usize, needed: usize },
    /// A window whose two accounts of its extent disagree: firmware named a
    /// maximum that is not its minimum plus its length.
    Inconsistent { min: u64, max: u64, length: u64 },
    /// A window whose address on the bridge's two sides differs. What this
    /// answers is which addresses a *CPU* may issue, and translating one is a
    /// machine nothing here has read.
    Translated { min: u64, offset: u64 },
    /// More memory windows than the caller has room for.
    TooMany { room: usize },
}

impl core::fmt::Display for ResourceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable { at, len } => write!(f, "{len} bytes at {at:#x} are unreadable"),
            Self::Unterminated => write!(f, "no End Tag inside {MAX_LIST_BYTES} bytes"),
            Self::UnknownTag { tag } => write!(f, "descriptor tag {tag:#04x} is not decoded here"),
            Self::Short { tag, declared, needed } => {
                write!(f, "descriptor tag {tag:#04x} declares {declared} bytes, needs {needed}")
            }
            Self::Inconsistent { min, max, length } => {
                write!(f, "{min:#x}..={max:#x} is not {length:#x} bytes long")
            }
            Self::Translated { min, offset } => {
                write!(f, "the window at {min:#x} is translated by {offset:#x}")
            }
            Self::TooMany { room } => write!(f, "more memory windows than the {room} there is room for"),
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
    let head = at + offset as u64;
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

/// How far the list at `at` runs, to and including its End Tag — or, where the
/// walk cannot reach one, as far as it read.
///
/// This is what the bootloader logs the raw bytes of, so it answers for a list
/// this decoder refuses as well as for one it reads.
pub fn list_len<P: Phys>(phys: P, at: u64) -> usize {
    let mut offset = 0;
    while offset < MAX_LIST_BYTES {
        let Ok(item) = item(phys, at, offset) else { break };
        offset += item.len;
        if item.tag == END_TAG {
            break;
        }
    }
    offset.min(MAX_LIST_BYTES)
}

/// Eight bytes little-endian at `offset` into the descriptor at `at`.
fn u64le<P: Phys>(phys: P, at: u64, offset: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= u64::from(phys.byte(at + offset as u64 + i)) << (8 * i);
    }
    v
}

/// Every memory window the list at `at` names, written into `out`; the count,
/// or the refusal.
///
/// **A refusal carries no windows at all**: a caller handed the ones decoded
/// before the refusal would be holding an aperture with a hole in it, and would
/// place a BAR in the hole.
pub fn memory_windows<P: Phys>(
    phys: P,
    at: u64,
    out: &mut [RootBridgeWindow],
) -> Result<usize, ResourceError> {
    let mut offset = 0usize;
    let mut found = 0usize;
    loop {
        if offset >= MAX_LIST_BYTES {
            return Err(ResourceError::Unterminated);
        }
        let item = item(phys, at, offset)?;
        if item.tag == END_TAG {
            return Ok(found);
        }
        let head = at + offset as u64;
        if item.tag != QWORD_ADDRESS_SPACE {
            return Err(ResourceError::UnknownTag { tag: item.tag });
        }
        if item.len < QWORD_BYTES {
            return Err(ResourceError::Short {
                tag: item.tag,
                declared: item.len,
                needed: QWORD_BYTES,
            });
        }
        if phys.byte(head + RESOURCE_TYPE as u64) == TYPE_MEMORY {
            let min = u64le(phys, head, QWORD_MINIMUM);
            let max = u64le(phys, head, QWORD_MAXIMUM);
            let length = u64le(phys, head, QWORD_LENGTH);
            let translation = u64le(phys, head, QWORD_TRANSLATION);
            // A window of no length decodes nothing, and firmware emits one
            // where a range is declared and unused.
            if length != 0 {
                if min.checked_add(length - 1) != Some(max) {
                    return Err(ResourceError::Inconsistent { min, max, length });
                }
                if translation != 0 {
                    return Err(ResourceError::Translated { min, offset: translation });
                }
                let room = out.len();
                let slot = out.get_mut(found).ok_or(ResourceError::TooMany { room })?;
                *slot = RootBridgeWindow { base: min, length };
                found += 1;
            }
        }
        offset += item.len;
    }
}
