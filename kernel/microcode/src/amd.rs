//! AMD's microcode container, and the patch it holds for one CPU.
//!
//! No AMD document describes the container. Its layout is that of the files
//! AMD publishes, all little-endian: the magic `DMA\0`, a section of type 0
//! holding the equivalence table, then sections of type 1, each a type, a
//! length and that many bytes of patch. A patch is a Microcode Patch Block
//! (MPB), a 32-byte header and AMD's encrypted patch data, which the Family
//! 19h and 1Ah PPRs define (55901 §2.1.5.2, 57896 §2.1.3.2) and no Family 17h
//! document does.
//!
//! A patch names a CPU in two steps: a table entry maps CPUID Fn0000_0001 EAX
//! to an equivalence ID, and the MPB header's processor ID is the equivalence
//! ID it is for. [`select`] answers with the patch of the highest level
//! naming the CPU, never an older one, or why it loads none. The header's
//! level only chooses: the CPU checks it against nothing and reports the
//! level its patch data carries, so only MSR0000_008B read after a load says
//! what the CPU runs.
//!
//! The whole file is read, and anything it holds that this module does not
//! understand refuses it by name: a bound, a second container, an ambiguous
//! mapping, or a header field that would change whether a patch may be
//! loaded.

use super::{dwords, Choice, Revision, Signature};

#[cfg(test)]
mod tests;

/// `DMA\0`, a container's first four bytes.
const MAGIC: u32 = 0x0041_4d44;
const TABLE: u32 = 0;
const PATCH: u32 = 1;
/// The magic, the table's type and its length.
const CONTAINER: usize = 12;
/// A section's type and length.
const SECTION: usize = 8;
/// Signature, errata mask, errata compare, equivalence ID and 16 reserved bits.
const ENTRY: usize = 16;
const MPB_HEADER: usize = 32;

/// What one AMD CPU reports, read on that CPU.
#[derive(Clone, Copy, Debug)]
pub struct Cpu {
    pub signature: Signature,
    pub level: Revision,
}

/// One patch out of a container, its header validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch<'a> {
    level: Revision,
    block: &'a [u8],
}

impl<'a> Patch<'a> {
    /// The level its header claims.
    pub fn level(&self) -> Revision {
        self.level
    }

    /// The MPB, header first. MSRC001_0020 is written the linear address of
    /// its first byte (57896 §2.1.13.3).
    pub fn block(&self) -> &'a [u8] {
        self.block
    }
}

/// Why a file is refused: the offset of the container, table entry or
/// section that is malformed, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub at: usize,
    pub why: Refusal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes end before the container's header, its table or a section
    /// does.
    Truncated { need: usize, have: usize },
    /// The file does not begin `DMA\0`.
    Magic(u32),
    /// The first section is not the equivalence table.
    TableType(u32),
    /// The table's length is 0 or not a whole number of entries.
    TableSize(usize),
    /// The table's last entry is not all zero, which ends every table AMD
    /// publishes.
    Unterminated,
    /// An entry before the last has signature or equivalence ID 0, which a
    /// reader may take for the table's end or skip.
    EmptyEntry,
    /// An entry's errata mask or compare is not 0: what they mean is
    /// documented nowhere.
    Errata,
    /// A signature an earlier entry already maps.
    DuplicateSignature(u32),
    /// A section after the table is not a patch.
    SectionType(u32),
    /// A second container follows the first: AMD publishes one per file.
    SecondContainer,
    /// A patch shorter than an MPB header.
    PatchSize(usize),
    /// MPB_NB_ID or MPB_SB_ID names a chipset, which the PPRs have the BIOS
    /// match before loading by IDs they do not name.
    ChipsetSpecific,
    /// The upper half of the header's 32-bit processor ID is not 0, so its
    /// 16-bit equivalence ID is not the whole of it.
    ProcessorIdHigh(u16),
    /// NEXT_MPB_OFFSET chains another MPB behind this one (57896 §2.1.3.2).
    Chained,
    /// Two patches name the CPU at its newest level.
    TwoNewest(Revision),
}

/// The patch in `file` to load on `cpu`, or why there is none.
///
/// A patch at the level the CPU already reads is loaded too: the PPRs ask a
/// patch "loaded on both threads on each core" unless its Load Control says
/// otherwise (57896 §2.1.3.2), and the level is per core (55922 §2.1.14.1),
/// so the second thread of a core reads the first one's level before its own
/// write.
pub fn select<'a>(file: &'a [u8], cpu: &Cpu) -> Result<Choice<Patch<'a>>, Refused> {
    let have = file.len();
    let refused = |at, why| Refused { at, why };
    let (header, rest) = file
        .split_first_chunk::<CONTAINER>()
        .ok_or(refused(0, Refusal::Truncated { need: CONTAINER, have }))?;
    let [magic, kind, len]: [u32; 3] = dwords(header);
    if magic != MAGIC {
        return Err(refused(0, Refusal::Magic(magic)));
    }
    if kind != TABLE {
        return Err(refused(0, Refusal::TableType(kind)));
    }
    let len = len as usize;
    if len == 0 || !len.is_multiple_of(ENTRY) {
        return Err(refused(0, Refusal::TableSize(len)));
    }
    let (table, mut sections) = rest
        .split_at_checked(len)
        .ok_or(refused(0, Refusal::Truncated { need: CONTAINER.saturating_add(len), have }))?;
    let id = equivalence(table, cpu.signature)?;

    let mut newest: Option<Patch<'a>> = None;
    // Where a second patch at `newest`'s level sits.
    let mut tied = None;
    while !sections.is_empty() {
        // `sections` is a suffix of `file`.
        let at = have.wrapping_sub(sections.len());
        let (named, patch, after) = section(sections).map_err(|why| refused(at, why))?;
        sections = after;
        if Some(named) != id {
            continue;
        }
        match newest {
            Some(n) if patch.level == n.level => tied = Some(at),
            Some(n) if patch.level < n.level => {}
            _ => (newest, tied) = (Some(patch), None),
        }
    }
    if let (Some(patch), Some(at)) = (newest, tied) {
        return Err(refused(at, Refusal::TwoNewest(patch.level)));
    }
    Ok(match newest {
        None => Choice::NoMatch,
        Some(patch) if patch.level >= cpu.level => Choice::Load(patch),
        Some(patch) => Choice::Current(patch.level),
    })
}

/// The equivalence ID `table` maps `signature` to, if any, once every entry
/// is valid.
fn equivalence(table: &[u8], signature: Signature) -> Result<Option<u16>, Refused> {
    let at = |index: usize| CONTAINER.wrapping_add(ENTRY.wrapping_mul(index));
    let (entries, _) = table.as_chunks::<ENTRY>();
    let Some((last, entries)) = entries.split_last() else {
        return Err(Refused { at: 0, why: Refusal::TableSize(table.len()) });
    };
    if *last != [0; ENTRY] {
        return Err(Refused { at: at(entries.len()), why: Refusal::Unterminated });
    }
    let mut id = None;
    for (index, entry) in entries.iter().enumerate() {
        let [named, mask, compare, ids]: [u32; 4] = dwords(entry);
        let refused = |why| Err(Refused { at: at(index), why });
        if named == 0 || ids as u16 == 0 {
            return refused(Refusal::EmptyEntry);
        }
        if mask != 0 || compare != 0 {
            return refused(Refusal::Errata);
        }
        if entries.iter().take(index).any(|earlier| dwords::<ENTRY, 4>(earlier)[0] == named) {
            return refused(Refusal::DuplicateSignature(named));
        }
        if named == signature.0 {
            id = Some(ids as u16);
        }
    }
    Ok(id)
}

/// The patch section `bytes` starts with, the equivalence ID its header
/// names, and the bytes after it.
fn section(bytes: &[u8]) -> Result<(u16, Patch<'_>, &[u8]), Refusal> {
    let have = bytes.len();
    let (header, rest) = bytes.split_first_chunk::<SECTION>().ok_or(Refusal::Truncated { need: SECTION, have })?;
    let [kind, len]: [u32; 2] = dwords(header);
    match kind {
        PATCH => {}
        MAGIC => return Err(Refusal::SecondContainer),
        other => return Err(Refusal::SectionType(other)),
    }
    let len = len as usize;
    let (block, rest) =
        rest.split_at_checked(len).ok_or(Refusal::Truncated { need: SECTION.saturating_add(len), have })?;
    let mpb = block.first_chunk::<MPB_HEADER>().ok_or(Refusal::PatchSize(len))?;
    // MPB_DATE, the level, MPB_LOADER_ID and a reserved half, a reserved
    // dword, MPB_NB_ID, MPB_SB_ID, the processor ID, and MPB_BIOS_REVISION,
    // Load Control and NEXT_MPB_OFFSET. Load Control is not read: it asks
    // for loads one core at a time or on both threads, which a loader that
    // loads every CPU in turn does whatever it says.
    let [_, level, _, _, nb, sb, processor, last]: [u32; 8] = dwords(mpb);
    if nb != 0 || sb != 0 {
        return Err(Refusal::ChipsetSpecific);
    }
    let high = (processor >> 16) as u16;
    if high != 0 {
        return Err(Refusal::ProcessorIdHigh(high));
    }
    if last >> 16 != 0 {
        return Err(Refusal::Chained);
    }
    Ok((processor as u16, Patch { level: Revision(level), block }, rest))
}
