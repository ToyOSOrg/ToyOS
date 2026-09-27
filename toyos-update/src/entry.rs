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

/// `text` as null-terminated UCS-2 at `out[at..]`, and where it ends.
fn ucs2(text: &str, out: &mut [u8], mut at: usize) -> usize {
    for unit in text.encode_utf16().chain(core::iter::once(0)) {
        out[at..at + 2].copy_from_slice(&unit.to_le_bytes());
        at += 2;
    }
    at
}

/// A node's length as its header carries it.
fn node_len(bytes: usize) -> [u8; 2] {
    u16::try_from(bytes).expect("a device path this writes is far below 64 KiB").to_le_bytes()
}

/// The load option `HD(part)/File(path)`, active, described as `description`,
/// written into `out`; its length.
pub fn load_option(description: &str, part: &Partition, path: &str, out: &mut [u8]) -> usize {
    out[..4].copy_from_slice(&LOAD_OPTION_ACTIVE.to_le_bytes());
    let path_at = ucs2(description, out, LOAD_OPTION_HEAD);

    let hd = &mut out[path_at..path_at + HARD_DRIVE_BYTES];
    hd[0] = HARD_DRIVE.0;
    hd[1] = HARD_DRIVE.1;
    hd[2..4].copy_from_slice(&node_len(HARD_DRIVE_BYTES));
    hd[4..8].copy_from_slice(&part.number.to_le_bytes());
    hd[8..16].copy_from_slice(&part.start.to_le_bytes());
    hd[16..24].copy_from_slice(&part.size.to_le_bytes());
    hd[24..40].copy_from_slice(&part.guid);
    hd[40] = GPT;
    hd[41] = SIGNATURE_GUID;

    let file_at = path_at + HARD_DRIVE_BYTES;
    let end_at = ucs2(path, out, file_at + NODE_HEADER);
    out[file_at] = FILE_PATH.0;
    out[file_at + 1] = FILE_PATH.1;
    out[file_at + 2..file_at + 4].copy_from_slice(&node_len(end_at - file_at));

    out[end_at] = END.0;
    out[end_at + 1] = END.1;
    out[end_at + 2..end_at + 4].copy_from_slice(&node_len(NODE_HEADER));
    let len = end_at + NODE_HEADER;
    out[4..6].copy_from_slice(&node_len(len - path_at));
    len
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
fn hard_drive_guid(mut path: &[u8]) -> Option<[u8; 16]> {
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
fn names(option: &[u8], guid: &[u8; 16]) -> bool {
    device_path(option).and_then(hard_drive_guid).is_some_and(|found| found == *guid)
}

/// Whether an option is active: the boot manager skips one that is not.
fn active(option: &[u8]) -> bool {
    option.get(..4).is_some_and(|a| u32::from_le_bytes([a[0], a[1], a[2], a[3]]) & LOAD_OPTION_ACTIVE != 0)
}

/// The lowest active entry of `held` that boots off the GPT partition `guid`.
pub fn naming<B: AsRef<[u8]>>(held: &[(u16, B)], guid: &[u8; 16]) -> Option<u16> {
    held.iter().filter(|(_, o)| active(o.as_ref()) && names(o.as_ref(), guid)).map(|(n, _)| *n).min()
}

/// `BootOrder` with `ours` first and every other entry after it in the order
/// it had, into `out`; how many entries that is, or `None` where `out` cannot
/// hold them.
pub fn first(order: &[u16], ours: u16, out: &mut [u16]) -> Option<usize> {
    let mut n = 0;
    for entry in core::iter::once(ours).chain(order.iter().copied().filter(|&e| e != ours)) {
        *out.get_mut(n)? = entry;
        n += 1;
    }
    Some(n)
}

/// The first active entry of `held` after `current`'s last place in `order`
/// (all of it where `current` is not there) that is not `current` and does
/// not boot off `ours`: never an earlier one, so a fall cannot loop.
pub fn after<B: AsRef<[u8]>>(order: &[u16], current: u16, held: &[(u16, B)], ours: Option<&[u8; 16]>) -> Option<u16> {
    let rest = match order.iter().rposition(|&e| e == current) {
        Some(at) => &order[at + 1..],
        None => order,
    };
    let boots = |number: u16| {
        held.iter().any(|(n, o)| {
            let o = o.as_ref();
            *n == number && active(o) && !ours.is_some_and(|guid| names(o, guid))
        })
    };
    rest.iter().copied().find(|&e| e != current && boots(e))
}

/// The lowest `Boot####` number neither `held` nor `order` names.
pub fn free<B>(held: &[(u16, B)], order: &[u16]) -> Option<u16> {
    (0..=u16::MAX).find(|n| !order.contains(n) && !held.iter().any(|(e, _)| e == n))
}

/// `Boot0003`'s number; `None` for any other variable name, lowercase hex
/// among them (UEFI 2.10 §3.3).
pub fn number(name: &str) -> Option<u16> {
    let hex = name.strip_prefix("Boot")?;
    if hex.len() != 4 || !hex.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)) {
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

    /// An entry for the partition `guid`, active or not.
    fn option(guid: u8, is_active: bool) -> Vec<u8> {
        let mut out = [0xAAu8; 512];
        let n = load_option("ToyOS", &Partition { guid: [guid; 16], ..PART }, r"\EFI\BOOT\BOOTX64.EFI", &mut out);
        out[0] = u8::from(is_active);
        out[..n].to_vec()
    }

    #[test]
    fn an_option_is_the_bytes_the_specification_lays_down() {
        let mut buffer = [0xAAu8; 512];
        let n = load_option("ToyOS", &PART, r"\EFI\BOOT\BOOTX64.EFI", &mut buffer);
        assert_eq!(&buffer[..n], by_hand().as_slice());
        assert!(names(&buffer[..n], &[0xE5; 16]));
        assert!(!names(&buffer[..n], &[0xE6; 16]));
        assert!(active(&buffer[..n]));
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
    fn an_esp_is_booted_by_its_lowest_active_entry() {
        let held = [(2, option(0xE5, false)), (5, option(0xE5, true)), (7, option(0xE5, true)), (1, option(0xE6, true))];
        assert_eq!(naming(&held, &[0xE5; 16]), Some(5));
        assert_eq!(naming(&held, &[0xE7; 16]), None);
        assert_eq!(naming(&held[..1], &[0xE5; 16]), None, "an inactive entry alone boots nothing");
    }

    #[test]
    fn the_order_puts_ours_first_once() {
        let mut out = [0u16; 8];
        let n = first(&[3, 1, 7], 7, &mut out).expect("fits");
        assert_eq!(&out[..n], &[7, 3, 1]);
        let n = first(&[3, 1], 9, &mut out).expect("fits");
        assert_eq!(&out[..n], &[9, 3, 1], "an entry the order lacked is added at its head");
        let n = first(&[], 2, &mut out).expect("fits");
        assert_eq!(&out[..n], &[2]);
        let mut three = [0u16; 3];
        assert_eq!(first(&[3, 1, 7], 7, &mut three), Some(3), "ours was in the order: no longer");
        assert_eq!(first(&[3, 1, 7], 9, &mut three), None, "one more than the order holds is refused");
    }

    /// The firmware's own fall-through: the next entry after the one that
    /// booted, never it again and never one before it.
    #[test]
    fn the_entry_after_is_later_in_the_order_and_boots_something_else() {
        const OURS: u8 = 0x0E;
        let held = [(4, option(1, true)), (2, option(OURS, true)), (9, option(3, true)), (5, option(4, true))];
        let order = [4, 2, 9, 5];
        let ours = Some(&[OURS; 16]);
        assert_eq!(after(&order, 4, &held, None), Some(2));
        assert_eq!(after(&order, 4, &held, ours), Some(9), "an entry naming this loader's own ESP is passed over");
        assert_eq!(after(&order, 2, &held, ours), Some(9));
        assert_eq!(after(&order, 5, &held, ours), None, "nothing after the last");
        assert_eq!(after(&order, 7, &held, ours), Some(4), "a BootNext boot falls to the order's head");
        let inactive = [(4, option(1, true)), (2, option(2, false)), (9, option(3, true))];
        assert_eq!(after(&order, 4, &inactive, ours), Some(9), "an inactive entry is passed over");
        assert_eq!(after(&order, 4, &held[2..], ours), Some(9), "an entry no variable holds is passed over");
        assert_eq!(after(&[3, 3, 3], 3, &held, None), None, "never the entry that booted");
        let twice = [(3, option(1, true)), (5, option(2, true))];
        assert_eq!(after(&[3, 5, 3], 3, &twice, None), None);
        assert_eq!(after(&[3, 5, 3], 5, &twice, None), Some(3));
    }

    #[test]
    fn a_number_is_four_uppercase_hex_digits_and_a_free_one_is_named_by_nothing() {
        assert_eq!(number("Boot0003"), Some(3));
        assert_eq!(number("Boot00AF"), Some(0xAF));
        assert_eq!(number("Boot00aF"), None, "lowercase is no load option");
        assert_eq!(number("BootOrder"), None);
        assert_eq!(number("BootNext"), None);
        assert_eq!(number("Boot00031"), None);
        let held = [(0, [0u8; 0]), (1, []), (3, [])];
        assert_eq!(free(&held, &[]), Some(2));
        assert_eq!(free(&held, &[2, 4]), Some(5), "a number the order still names is not free");
        assert_eq!(free::<[u8; 0]>(&[], &[]), Some(0));
    }
}
