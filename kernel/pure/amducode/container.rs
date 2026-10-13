//! AMD's microcode container: an equivalence table mapping a CPUID Fn0000_0001
//! EAX to an equivalence ID, then patch sections whose payload is one patch
//! block naming that ID at offset 0x18. A file may hold several containers end
//! to end. Little-endian throughout.
//!
//! The file is refused whole on any bound a reader can trip: no AMD document
//! states the bounds, so these are this reader's own, and AMD's published
//! files pass every one.

use core::fmt;

/// `"DMA\0"`, which starts every container.
const MAGIC: [u8; 4] = *b"DMA\0";
const TABLE_TYPE: u32 = 0;
const PATCH_TYPE: u32 = 1;
const ENTRY_LEN: usize = 16;
/// A patch block's header (the Family 19h and 1Ah PPRs' MPB): its level at
/// 0x04 and its equivalence ID at 0x18.
const HEADER_LEN: usize = 32;
const LEVEL_AT: usize = 0x04;
const EQUIVALENCE_AT: usize = 0x18;

/// The one patch block a container holds for a CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch<'a> {
    /// Where the block starts in the file.
    pub offset: usize,
    /// The section payload, header first: what the loader MSR is handed.
    pub block: &'a [u8],
    /// The header's patch level, in MSR0000_008B's number space.
    pub level: u32,
    pub equivalence: u16,
}

/// Why a file yields no patch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Malformed at byte `at`.
    Malformed { at: usize, why: &'static str },
    /// The tables name the CPU and two patch blocks carry its ID.
    TwoPatches { at: usize },
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed { at, why } => write!(f, "the container is malformed at byte {at:#x}: {why}"),
            Self::TwoPatches { at } => write!(f, "a second patch block for this CPU at byte {at:#x}"),
        }
    }
}

fn word(file: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(file.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn half(file: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(file.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

/// The patch block `file` holds for the CPU whose CPUID Fn0000_0001 EAX is
/// `signature`, `None` where no table names it or no block carries its ID.
pub fn patch_for(file: &[u8], signature: u32) -> Result<Option<Patch<'_>>, Refused> {
    let malformed = |at, why| Refused::Malformed { at, why };
    let mut found: Option<Patch<'_>> = None;
    let mut at = 0;
    while at < file.len() {
        // A container: magic, the table's type and length, the table.
        if file.get(at..at + 4) != Some(&MAGIC[..]) {
            return Err(malformed(at, "no container magic"));
        }
        if word(file, at + 4) != Some(TABLE_TYPE) {
            return Err(malformed(at + 4, "the first section is not the equivalence table"));
        }
        let len = word(file, at + 8).ok_or(malformed(at + 8, "the table's length is cut short"))? as usize;
        let table_at = at + 12;
        let table = file
            .get(table_at..table_at.saturating_add(len))
            .ok_or(malformed(at + 8, "the table runs past the end of the file"))?;
        let id = equivalence(table, signature).map_err(|why| malformed(table_at, why))?;
        at = table_at + len;

        // Its patch sections, up to the end of the file or the next container.
        while at < file.len() && file.get(at..at + 4) != Some(&MAGIC[..]) {
            let kind = word(file, at).ok_or(malformed(at, "bytes after the last section"))?;
            if kind != PATCH_TYPE {
                return Err(malformed(at, "a section that is not a patch"));
            }
            let len = word(file, at + 4).ok_or(malformed(at, "bytes after the last section"))? as usize;
            let offset = at + 8;
            let block = file
                .get(offset..offset.saturating_add(len))
                .ok_or(malformed(at + 4, "a patch runs past the end of the file"))?;
            if block.len() < HEADER_LEN {
                return Err(malformed(at + 4, "a patch shorter than its header"));
            }
            let named = half(block, EQUIVALENCE_AT).ok_or(malformed(offset, "a patch shorter than its header"))?;
            if id.is_some_and(|id| id == named) {
                if found.is_some() {
                    return Err(Refused::TwoPatches { at: offset });
                }
                let level = word(block, LEVEL_AT).ok_or(malformed(offset, "a patch shorter than its header"))?;
                found = Some(Patch { offset, block, level, equivalence: named });
            }
            at = offset + len;
        }
    }
    Ok(found)
}

/// The equivalence ID `table` maps `signature` to. The table is whole
/// entries ending in an all-zero one, and ID 0 names nothing.
fn equivalence(table: &[u8], signature: u32) -> Result<Option<u16>, &'static str> {
    if table.len() < ENTRY_LEN || !table.len().is_multiple_of(ENTRY_LEN) {
        return Err("the table is not whole entries");
    }
    let (entries, last) = table.split_at(table.len() - ENTRY_LEN);
    if last.iter().any(|&b| b != 0) {
        return Err("the table does not end in an all-zero entry");
    }
    for entry in entries.as_chunks::<ENTRY_LEN>().0 {
        let installed = word(entry, 0).ok_or("an entry is cut short")?;
        if installed == 0 {
            return Err("an all-zero signature before the table's end");
        }
        let id = half(entry, 12).ok_or("an entry is cut short")?;
        if installed == signature && id != 0 {
            return Ok(Some(id));
        }
    }
    Ok(None)
}
