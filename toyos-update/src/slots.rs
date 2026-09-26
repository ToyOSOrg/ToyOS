//! The slot table: which partitions make each slot, and which slot is marked.
//!
//! It lives on its own partition of type [`TYPE_TEXT`], in two copies at
//! blocks 0 and 1, and **a writer writes the copy that is not the current
//! one**, with a sequence one past it. A write that tears leaves that copy
//! unreadable and the current one standing, so moving the mark is atomic on a
//! device that tears a block: the machine boots the old mark or the new one,
//! never neither.
//!
//! ```text
//! magic "TOYOSLOT" | format u32 | marked u32 | sequence u64
//! then per slot: present u32 | 0 u32 | boot guid [16] | root guid [16] | version u64
//! then crc32 u32 over everything before it                   (TABLE_BYTES)
//! ```
//!
//! The version a slot records is what its writer installed, and is the
//! updater's to compare against; the loader trusts nothing here but which
//! partitions to read and which slot is marked, and judges each slot by its
//! own signed header.

/// `94464329-E06E-4288-A9DA-7FC7154F5E92`, the slot table's partition type.
pub const TYPE_TEXT: &str = "94464329-E06E-4288-A9DA-7FC7154F5E92";

/// `037719D7-DEA5-481A-AA07-6AF8BE6D51E2`, a slot's FAT partition type.
pub const BOOT_TYPE_TEXT: &str = "037719D7-DEA5-481A-AA07-6AF8BE6D51E2";

/// The unit the table's copies are written in.
pub const BLOCK: usize = 4096;

/// Blocks the table's partition must hold: its two copies.
pub const COPIES: u64 = 2;

const MAGIC: [u8; 8] = *b"TOYOSLOT";
const FORMAT: u32 = 1;
const SLOT_BYTES: usize = 4 + 4 + 16 + 16 + 8;
const BODY_BYTES: usize = 8 + 4 + 4 + 8 + 2 * SLOT_BYTES;
/// A copy's bytes, checksum included; the rest of its block is zero.
pub const TABLE_BYTES: usize = BODY_BYTES + 4;

/// Where each slot's kernel, boot parameter and signed header are on its FAT
/// partition, as paths from the volume's root.
pub const KERNEL_FILE: &str = "toyos/kernel.elf";
pub const CMDLINE_FILE: &str = "toyos/cmdline";
pub const SIGNED_FILE: &str = "toyos/image.sig";

/// A slot's two partitions, by their unique GUIDs as a GPT entry stores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub boot: [u8; 16],
    pub root: [u8; 16],
    /// The version its writer installed; `0` for a slot nothing has been installed in.
    pub version: u64,
}

/// Slot `A` or slot `B`, by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    A,
    B,
}

impl Which {
    pub const fn index(self) -> usize {
        match self {
            Self::A => 0,
            Self::B => 1,
        }
    }

    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    pub const fn letter(self) -> char {
        match self {
            Self::A => 'A',
            Self::B => 'B',
        }
    }

    pub const fn from_letter(c: char) -> Option<Self> {
        match c {
            'A' => Some(Self::A),
            'B' => Some(Self::B),
            _ => None,
        }
    }
}

/// One copy of the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Table {
    pub sequence: u64,
    pub marked: Which,
    /// Slot `A` then slot `B`; `None` for a machine built with one slot.
    pub slots: [Option<Slot>; 2],
}

/// Why a copy is not a table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreadable {
    Magic,
    Format(u32),
    Checksum,
    /// It marks a slot it does not carry, or a mark that is no slot.
    Mark(u32),
}

impl core::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Magic => write!(f, "no slot table"),
            Self::Format(n) => write!(f, "a slot table of format {n}, and this reads {FORMAT}"),
            Self::Checksum => write!(f, "a slot table whose checksum does not hold, which is a torn write"),
            Self::Mark(n) => write!(f, "a slot table marking slot {n}, which it does not carry"),
        }
    }
}

impl Table {
    pub fn slot(&self, which: Which) -> Option<Slot> {
        self.slots[which.index()]
    }

    pub fn encode(&self) -> [u8; BLOCK] {
        let mut out = [0u8; BLOCK];
        out[..8].copy_from_slice(&MAGIC);
        out[8..12].copy_from_slice(&FORMAT.to_le_bytes());
        out[12..16].copy_from_slice(&(self.marked.index() as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.sequence.to_le_bytes());
        for (i, slot) in self.slots.iter().enumerate() {
            let at = 24 + i * SLOT_BYTES;
            if let Some(slot) = slot {
                out[at..at + 4].copy_from_slice(&1u32.to_le_bytes());
                out[at + 8..at + 24].copy_from_slice(&slot.boot);
                out[at + 24..at + 40].copy_from_slice(&slot.root);
                out[at + 40..at + 48].copy_from_slice(&slot.version.to_le_bytes());
            }
        }
        let crc = crc32(&out[..BODY_BYTES]);
        out[BODY_BYTES..TABLE_BYTES].copy_from_slice(&crc.to_le_bytes());
        out
    }

    pub fn decode(block: &[u8; BLOCK]) -> Result<Self, Unreadable> {
        if block[..8] != MAGIC {
            return Err(Unreadable::Magic);
        }
        let word = |at: usize| u32::from_le_bytes(block[at..at + 4].try_into().expect("four bytes"));
        let format = word(8);
        if format != FORMAT {
            return Err(Unreadable::Format(format));
        }
        if crc32(&block[..BODY_BYTES]) != word(BODY_BYTES) {
            return Err(Unreadable::Checksum);
        }
        let mut slots = [None; 2];
        for (i, slot) in slots.iter_mut().enumerate() {
            let at = 24 + i * SLOT_BYTES;
            if word(at) == 1 {
                *slot = Some(Slot {
                    boot: block[at + 8..at + 24].try_into().expect("sixteen bytes"),
                    root: block[at + 24..at + 40].try_into().expect("sixteen bytes"),
                    version: u64::from_le_bytes(block[at + 40..at + 48].try_into().expect("eight bytes")),
                });
            }
        }
        let marked = match word(12) {
            0 if slots[0].is_some() => Which::A,
            1 if slots[1].is_some() => Which::B,
            other => return Err(Unreadable::Mark(other)),
        };
        let sequence = u64::from_le_bytes(block[16..24].try_into().expect("eight bytes"));
        Ok(Self { sequence, marked, slots })
    }
}

/// The table two copies make: the readable one with the higher sequence, and
/// which copy that is. Both unreadable is the first copy's reason.
pub fn current(copies: [&[u8; BLOCK]; 2]) -> Result<(Table, usize), Unreadable> {
    match (Table::decode(copies[0]), Table::decode(copies[1])) {
        (Ok(a), Ok(b)) if b.sequence > a.sequence => Ok((b, 1)),
        (Ok(a), _) => Ok((a, 0)),
        (Err(_), Ok(b)) => Ok((b, 1)),
        (Err(why), Err(_)) => Err(why),
    }
}

/// What a writer puts where, to make `next` the table: the copy that is not
/// `current`'s, one sequence past it.
pub fn next_write(current: (Table, usize), mut next: Table) -> (usize, [u8; BLOCK]) {
    next.sequence = current.0.sequence + 1;
    (1 - current.1, next.encode())
}

/// CRC-32 (IEEE 802.3, reflected), the checksum GPT uses; a torn write is what
/// it catches, not an adversary.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(marked: Which, sequence: u64) -> Table {
        let slot = |n: u8| Some(Slot { boot: [n; 16], root: [n + 1; 16], version: u64::from(n) });
        Table { sequence, marked, slots: [slot(1), slot(3)] }
    }

    /// The check value every CRC-32 is held to.
    #[test]
    fn the_checksum_is_crc32() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_table_reads_back_and_a_bent_one_is_refused() {
        let t = table(Which::B, 9);
        assert_eq!(Table::decode(&t.encode()), Ok(t));
        let mut torn = t.encode();
        torn[40] ^= 1;
        assert_eq!(Table::decode(&torn), Err(Unreadable::Checksum));
        assert_eq!(Table::decode(&[0; BLOCK]), Err(Unreadable::Magic));
        let one = Table { slots: [t.slots[0], None], ..t };
        let mut marks_absent = one.encode();
        // Re-encode with a mark on the absent slot, checksum and all.
        marks_absent[12..16].copy_from_slice(&1u32.to_le_bytes());
        let crc = crc32(&marks_absent[..BODY_BYTES]);
        marks_absent[BODY_BYTES..TABLE_BYTES].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(Table::decode(&marks_absent), Err(Unreadable::Mark(1)));
    }

    /// **The write that tears is never the one read**: the writer writes the
    /// copy that is not current, so a torn one leaves the old table, and a
    /// whole one is the new table by its sequence.
    #[test]
    fn moving_the_mark_is_atomic_against_a_torn_copy() {
        let old = table(Which::A, 4);
        let (a, b) = (old.encode(), table(Which::A, 3).encode());
        let now = current([&a, &b]).expect("a table");
        assert_eq!(now, (old, 0));
        let (copy, written) = next_write(now, table(Which::B, 0));
        assert_eq!(copy, 1);
        assert_eq!(current([&a, &written]).expect("a table").0.marked, Which::B);
        assert_eq!(current([&a, &written]).expect("a table").0.sequence, 5);
        let mut torn = written;
        torn[100] ^= 0xFF;
        assert_eq!(current([&a, &torn]).expect("a table"), (old, 0));
        assert_eq!(current([&[0; BLOCK], &[0; BLOCK]]), Err(Unreadable::Magic));
    }
}
