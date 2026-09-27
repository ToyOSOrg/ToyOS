//! The firmware's boot entries, as the loader writes them on the running
//! system's request ([`crate::slots::Request`]): a load option naming an EFI
//! system partition's loader, the order with one entry first, and the entry
//! the firmware would have tried after the one that booted this pass.
//!
//! **The loader writes one shape of entry and no other**: a GPT partition by
//! its unique GUID and a file on it, `HD(…)/File(…)`, which is the short form
//! `efibootmgr --disk … --part …` writes and firmware expands against every
//! disk it sees (UEFI 2.10 §3.1.2). It never writes an entry for a path the
//! running system names — only for an ESP the loader found and its
//! removable-media file — so the request can ask for a boot of an ESP and for
//! nothing else.
//!
//! What firmware stores is untrusted here: an option is walked as bytes,
//! bounded by the slice, and a node claiming a length of zero or one past the
//! option ends the walk rather than being stepped over.

/// `LOAD_OPTION_ACTIVE` (UEFI 2.10 §3.1.3): the boot manager may boot it.
pub const LOAD_OPTION_ACTIVE: u32 = 0x1;

/// `EFI_LOAD_OPTION`'s fixed head: a `UINT32` of attributes and a `UINT16`
/// device-path length, then a null-terminated `CHAR16` description, then the
/// device path itself (UEFI 2.10 §3.1.3).
const LOAD_OPTION_HEAD: usize = 6;

/// A device-path node's type, subtype and length (UEFI 2.10 §10.2).
const NODE_HEADER: usize = 4;

/// MEDIA/HARD_DRIVE (UEFI 2.10 §10.3.6.1): partition number, start and size
/// in blocks, the signature, its format and its type — 42 bytes.
const HARD_DRIVE: (u8, u8) = (0x04, 0x01);
const HARD_DRIVE_BYTES: usize = 42;
/// `MBRType` 2 is a GPT partition, and `SignatureType` 2 a GUID signature.
const GPT: u8 = 0x02;
const SIGNATURE_GUID: u8 = 0x02;

/// MEDIA/FILE_PATH (UEFI 2.10 §10.3.6.4): a null-terminated `CHAR16` path.
const FILE_PATH: (u8, u8) = (0x04, 0x04);

/// END_ENTIRE_DEVICE_PATH (UEFI 2.10 §10.3.1).
const END: (u8, u8) = (0x7F, 0xFF);

/// A GPT partition as a HARDDRIVE node names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Partition {
    /// The entry's index in the partition array, counted from 1.
    pub number: u32,
    /// Its first block and its length in blocks, in the disk's own block size.
    pub start: u64,
    pub size: u64,
    /// Its unique GUID, as the GPT entry stores it.
    pub guid: [u8; 16],
}

/// Why an option could not be written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unwritable {
    /// A description or a path holds a character outside ASCII, which this
    /// writes as UCS-2 one byte at a time and so refuses rather than mangles.
    NotAscii,
    /// The option does not fit the buffer it was asked into.
    TooLong { needs: usize },
}

impl core::fmt::Display for Unwritable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAscii => write!(f, "a description or path outside ASCII"),
            Self::TooLong { needs } => write!(f, "an option of {needs} bytes, past the buffer it was asked into"),
        }
    }
}

/// `text` as null-terminated UCS-2 at `out[at..]`, and where it ends.
fn ucs2(text: &str, out: &mut [u8], at: usize) -> Result<usize, Unwritable> {
    if !text.is_ascii() {
        return Err(Unwritable::NotAscii);
    }
    let end = at + 2 * (text.len() + 1);
    let room = out.len();
    let span = out.get_mut(at..end).ok_or(Unwritable::TooLong { needs: end.max(room + 1) })?;
    for (unit, byte) in span.as_chunks_mut::<2>().0.iter_mut().zip(text.bytes().chain(core::iter::once(0))) {
        unit.copy_from_slice(&u16::from(byte).to_le_bytes());
    }
    Ok(end)
}

/// The load option `HD(part)/File(path)`, active, described as `description`,
/// written into `out`; its length.
pub fn load_option(description: &str, part: &Partition, path: &str, out: &mut [u8]) -> Result<usize, Unwritable> {
    if !path.is_ascii() || !description.is_ascii() {
        return Err(Unwritable::NotAscii);
    }
    let file_bytes = NODE_HEADER + 2 * (path.len() + 1);
    let path_bytes = HARD_DRIVE_BYTES + file_bytes + NODE_HEADER;
    let needs = LOAD_OPTION_HEAD + 2 * (description.len() + 1) + path_bytes;
    if needs > out.len() {
        return Err(Unwritable::TooLong { needs });
    }
    let path_len = u16::try_from(path_bytes).map_err(|_| Unwritable::TooLong { needs })?;
    let file_len = u16::try_from(file_bytes).map_err(|_| Unwritable::TooLong { needs })?;
    out[..needs].fill(0);
    out[..4].copy_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    out[4..6].copy_from_slice(&path_len.to_le_bytes());
    let mut at = ucs2(description, out, LOAD_OPTION_HEAD)?;

    let hd = &mut out[at..at + HARD_DRIVE_BYTES];
    hd[0] = HARD_DRIVE.0;
    hd[1] = HARD_DRIVE.1;
    hd[2..4].copy_from_slice(&(HARD_DRIVE_BYTES as u16).to_le_bytes());
    hd[4..8].copy_from_slice(&part.number.to_le_bytes());
    hd[8..16].copy_from_slice(&part.start.to_le_bytes());
    hd[16..24].copy_from_slice(&part.size.to_le_bytes());
    hd[24..40].copy_from_slice(&part.guid);
    hd[40] = GPT;
    hd[41] = SIGNATURE_GUID;
    at += HARD_DRIVE_BYTES;

    out[at] = FILE_PATH.0;
    out[at + 1] = FILE_PATH.1;
    out[at + 2..at + 4].copy_from_slice(&file_len.to_le_bytes());
    at = ucs2(path, out, at + NODE_HEADER)?;

    out[at] = END.0;
    out[at + 1] = END.1;
    out[at + 2..at + 4].copy_from_slice(&(NODE_HEADER as u16).to_le_bytes());
    Ok(at + NODE_HEADER)
}

/// The device path an option carries, bounded by the option; `None` where its
/// head, its description or its path runs off the end.
fn device_path(option: &[u8]) -> Option<&[u8]> {
    let head = option.get(..LOAD_OPTION_HEAD)?;
    let path_len = usize::from(u16::from_le_bytes([head[4], head[5]]));
    // The description is `CHAR16` and null-terminated, so the path starts after
    // the first pair of zero bytes on an even offset from the head.
    let mut at = LOAD_OPTION_HEAD;
    loop {
        let pair = option.get(at..at + 2)?;
        at += 2;
        if pair == [0, 0] {
            break;
        }
    }
    option.get(at..at.checked_add(path_len)?)
}

/// The GPT signature of the first HARDDRIVE node in a device path, or `None`
/// where it has none, names an MBR partition, or a node cannot be stepped
/// over.
pub fn hard_drive_guid(mut path: &[u8]) -> Option<[u8; 16]> {
    while let Some(node) = path.get(..NODE_HEADER) {
        let len = usize::from(u16::from_le_bytes([node[2], node[3]]));
        // A node shorter than its own header, or longer than what is left, ends
        // the walk: neither can be stepped over.
        let this = path.get(..len).filter(|_| len >= NODE_HEADER)?;
        if (this[0], this[1]) == END {
            return None;
        }
        if (this[0], this[1]) == HARD_DRIVE {
            if this.get(40) != Some(&GPT) || this.get(41) != Some(&SIGNATURE_GUID) {
                return None;
            }
            return this.get(24..40)?.try_into().ok();
        }
        path = &path[len..];
    }
    None
}

/// Whether an option boots off the GPT partition `guid`.
pub fn names(option: &[u8], guid: &[u8; 16]) -> bool {
    device_path(option).and_then(hard_drive_guid).is_some_and(|found| found == *guid)
}

/// Whether an option is active: the boot manager skips one that is not.
pub fn active(option: &[u8]) -> bool {
    option.get(..4).is_some_and(|a| u32::from_le_bytes([a[0], a[1], a[2], a[3]]) & LOAD_OPTION_ACTIVE != 0)
}

/// `BootOrder` with `ours` first and every other entry after it in the order
/// it had, into `out`; how many entries that is. An `out` shorter than the
/// order keeps the order's head.
pub fn first(order: &[u16], ours: u16, out: &mut [u16]) -> usize {
    let mut n = 0;
    for entry in core::iter::once(ours).chain(order.iter().copied().filter(|&e| e != ours)) {
        let Some(slot) = out.get_mut(n) else { break };
        *slot = entry;
        n += 1;
    }
    n
}

/// The entry the firmware would have tried after `current`: the first after it
/// in `order` that `skip` does not refuse — never `current` again, and never
/// an entry before it, so a machine whose every later entry is skipped has
/// nothing to fall to rather than a loop.
///
/// `current` not in the order at all (a `BootNext` boot) falls to the order's
/// first entry that `skip` does not refuse: that is what the firmware tries
/// after a `BootNext` boot fails.
pub fn after(order: &[u16], current: u16, skip: impl Fn(u16) -> bool) -> Option<u16> {
    let rest = match order.iter().position(|&e| e == current) {
        Some(at) => &order[at + 1..],
        None => order,
    };
    rest.iter().copied().find(|&e| e != current && !skip(e))
}

/// The lowest `Boot####` number no entry in `used` holds.
pub fn free(used: &[u16]) -> Option<u16> {
    (0..=u16::MAX).find(|n| !used.contains(n))
}

/// A GPT GUID in the text the GPT tools print, for a line that names one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuidText(pub [u8; 16]);

impl core::fmt::Display for GuidText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let b = &self.0;
        write!(
            f,
            "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
            b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
        )
    }
}

/// A GPT GUID's text, `C12A7328-F81F-11D2-BA4B-00A0C93EC93B`, as the bytes a
/// partition entry stores: the first three fields little-endian, the last two
/// as written (UEFI 2.10 appendix A). Either case; nothing but the 36
/// characters.
pub fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let bytes = text.as_bytes();
    if bytes.len() != 36 || [8, 13, 18, 23].iter().any(|&at| bytes[at] != b'-') {
        return None;
    }
    let mut hex = [0u8; 16];
    let mut digits = bytes.iter().enumerate().filter(|(at, _)| ![8, 13, 18, 23].contains(at)).map(|(_, b)| *b);
    for out in hex.iter_mut() {
        let high = (digits.next()? as char).to_digit(16)?;
        let low = (digits.next()? as char).to_digit(16)?;
        *out = (high * 16 + low) as u8;
    }
    Some([
        hex[3], hex[2], hex[1], hex[0], hex[5], hex[4], hex[7], hex[6], hex[8], hex[9], hex[10], hex[11], hex[12], hex[13],
        hex[14], hex[15],
    ])
}

/// `Boot0003`'s number; `None` for any other variable name.
pub fn number(name: &str) -> Option<u16> {
    let hex = name.strip_prefix("Boot")?;
    if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PART: Partition = Partition { number: 1, start: 2048, size: 67_584, guid: [0xE5; 16] };

    /// The bytes UEFI 2.10 §3.1.3 and §10.3.6 lay down for
    /// `HD(1,GPT,<guid>,0x800,0x10800)/\EFI\BOOT\BOOTX64.EFI`, described as
    /// `ToyOS`, written out by hand from the tables rather than by the writer.
    fn by_hand() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&1u32.to_le_bytes());
        let path = r"\EFI\BOOT\BOOTX64.EFI";
        let file_len = 4 + 2 * (path.len() + 1);
        out.extend_from_slice(&((42 + file_len + 4) as u16).to_le_bytes());
        for unit in "ToyOS".encode_utf16().chain([0]) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[4, 1, 42, 0]);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&2048u64.to_le_bytes());
        out.extend_from_slice(&67_584u64.to_le_bytes());
        out.extend_from_slice(&[0xE5; 16]);
        out.extend_from_slice(&[2, 2]);
        out.extend_from_slice(&[4, 4]);
        out.extend_from_slice(&(file_len as u16).to_le_bytes());
        for unit in path.encode_utf16().chain([0]) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0x7F, 0xFF, 4, 0]);
        out
    }

    #[test]
    fn an_option_is_the_bytes_the_specification_lays_down() {
        let mut buffer = [0u8; 512];
        let n = load_option("ToyOS", &PART, r"\EFI\BOOT\BOOTX64.EFI", &mut buffer).expect("fits");
        assert_eq!(&buffer[..n], by_hand().as_slice());
        assert!(names(&buffer[..n], &[0xE5; 16]));
        assert!(!names(&buffer[..n], &[0xE6; 16]));
        assert!(active(&buffer[..n]));
    }

    #[test]
    fn an_option_that_does_not_fit_or_is_not_ascii_is_refused() {
        let mut small = [0u8; 40];
        assert!(matches!(load_option("ToyOS", &PART, r"\EFI\BOOT\BOOTX64.EFI", &mut small), Err(Unwritable::TooLong { .. })));
        let mut buffer = [0u8; 512];
        assert_eq!(load_option("ToyÖS", &PART, r"\x", &mut buffer), Err(Unwritable::NotAscii));
    }

    /// **Firmware's bytes are walked, never trusted**: a node of length zero,
    /// one past the option, a path running off the end and an MBR partition
    /// each name nothing rather than looping, reading past the slice or
    /// matching.
    #[test]
    fn a_bent_option_names_nothing() {
        let good = by_hand();
        let desc_end = 6 + 2 * ("ToyOS".len() + 1);
        let mut zero = good.clone();
        zero[desc_end + 2] = 0;
        assert!(!names(&zero, &[0xE5; 16]), "a zero-length node");
        let mut long = good.clone();
        long[desc_end + 2] = 0xFF;
        assert!(!names(&long, &[0xE5; 16]), "a node past the option");
        let mut short = good.clone();
        short.truncate(desc_end + 10);
        assert!(!names(&short, &[0xE5; 16]), "a path running off the end");
        let mut mbr = good.clone();
        mbr[desc_end + 40] = 1;
        assert!(!names(&mbr, &[0xE5; 16]), "an MBR partition");
        assert!(!names(&good[..5], &[0xE5; 16]), "a head alone");
        let mut unterminated = good;
        unterminated.truncate(8);
        assert!(!names(&unterminated, &[0xE5; 16]), "a description with no end");
    }

    #[test]
    fn the_order_puts_ours_first_once() {
        let mut out = [0u16; 8];
        let n = first(&[3, 1, 7], 7, &mut out);
        assert_eq!(&out[..n], &[7, 3, 1]);
        let n = first(&[3, 1], 9, &mut out);
        assert_eq!(&out[..n], &[9, 3, 1], "an entry the order lacked is added at its head");
        let n = first(&[], 2, &mut out);
        assert_eq!(&out[..n], &[2]);
        let mut two = [0u16; 2];
        assert_eq!(first(&[3, 1, 7], 7, &mut two), 2);
        assert_eq!(two, [7, 3]);
    }

    /// The firmware's own fall-through: the next entry after the one that
    /// booted, never it again and never one before it.
    #[test]
    fn the_entry_after_is_later_in_the_order_and_never_current() {
        let order = [4, 2, 9, 5];
        assert_eq!(after(&order, 2, |_| false), Some(9));
        assert_eq!(after(&order, 2, |e| e == 9), Some(5));
        assert_eq!(after(&order, 5, |_| false), None, "nothing after the last");
        assert_eq!(after(&order, 7, |_| false), Some(4), "a BootNext boot falls to the order's head");
        assert_eq!(after(&order, 7, |e| e == 4), Some(2));
        assert_eq!(after(&[3, 3, 3], 3, |_| false), None, "never the entry that booted");
    }

    /// The GPT tools' own spelling, read back by the crate that writes it.
    #[test]
    fn a_guid_is_read_as_the_gpt_crate_writes_it() {
        for guid in [toyos_gpt::Guid::EFI_SYSTEM, toyos_gpt::Guid::TOYOS_SLOTS, toyos_gpt::Guid([0xA5; 16])] {
            let text = std::format!("{guid}");
            assert_eq!(parse_guid(&text), Some(guid.0), "{text}");
            assert_eq!(parse_guid(&text.to_ascii_lowercase()), Some(guid.0), "{text}");
            assert_eq!(std::format!("{}", GuidText(guid.0)), text);
        }
        assert_eq!(parse_guid("C12A7328F81F-11D2-BA4B-00A0C93EC93B0"), None);
        assert_eq!(parse_guid("C12A7328-F81F-11D2-BA4B-00A0C93EC93"), None);
        assert_eq!(parse_guid("G12A7328-F81F-11D2-BA4B-00A0C93EC93B"), None);
    }

    #[test]
    fn a_number_is_four_hex_digits() {
        assert_eq!(number("Boot0003"), Some(3));
        assert_eq!(number("Boot00aF"), Some(0xAF));
        assert_eq!(number("BootOrder"), None);
        assert_eq!(number("BootNext"), None);
        assert_eq!(number("Boot00031"), None);
        assert_eq!(free(&[0, 1, 3]), Some(2));
        assert_eq!(free(&[]), Some(0));
    }
}
