//! AMD's microcode containers, as AMD's own parser (`amd_ucode_info.py`) and
//! Linux v7.2's `amd.c` read them; AMD's APM documents no patch loader.
//!
//! A container is a 12-byte header — [`MAGIC`], the type of its first
//! section (0, the equivalence table) and that section's length — then the
//! table, then sections of type 1 laid end to end, each a type, a size and a
//! patch of that many bytes: a 64-byte header and its signed body. [`select`]
//! validates every section and answers with the patch a CPU loads, or why it
//! loads none. One malformed section refuses the whole container. A second
//! container behind the first is refused, by its magic standing where a
//! section's type does.
//!
//! **A Zen patch names its CPU in its patch ID** (Linux `94838d230a6c`): a
//! patch names the CPU whose CPUID.01H:EAX its family, model and stepping
//! decode to, and the equivalence table is skipped, as Linux skips it from
//! family 0x17 up.
//!
//! **Refused as unmodelled**, so that a container embedded later cannot load
//! what this loader does not understand: a chipset-specific patch, as Linux
//! refuses one; and a nonzero word at offset 12, which from Zen 3 on is the
//! lowest revision a patch may be loaded over (Xen's `check_min_rev`).
//!
//! **A patch at or above the revision the CPU runs loads**, an equal one
//! again on every thread, as Linux's early loader does (`amd.c:813-819,996`;
//! `e7ad18d1169c`: "there are microcode patches which do per-thread
//! modification … This is what the BIOS does too"). Xen refuses an equal
//! revision unless forced.
//!
//! **EntrySign's cutoff rule holds at build time.** Linux offers a CPU at or
//! below its cutoff revision only patches at or below that cutoff, and one
//! above it only patches above (`amd.c:554-572`). A patch below the running
//! revision never loads here, and every patch [`select`] can return out of
//! [`crate::AMD`] is held by a host test to its digest in Linux's
//! `amd_shas.c`, which records none above its CPU's cutoff: that test is the
//! rule's form here, and a container that drops it drops the rule.

use crate::{dwords, Choice, Revision, Signature};

#[cfg(test)]
mod tests;

/// A container's first dword (`UCODE_MAGIC`).
pub const MAGIC: u32 = 0x0041_4d44;
/// Magic, the table's type, the table's length.
const HEADER: usize = 12;
/// A section's type and size.
const SECTION: usize = 8;
const EQUIVALENCE_TABLE: u32 = 0;
const PATCH: u32 = 1;
/// `struct microcode_header_amd`.
const PATCH_HEADER: usize = 64;

/// One patch out of a container, validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch<'a> {
    revision: Revision,
    bytes: &'a [u8],
}

impl<'a> Patch<'a> {
    /// Its patch ID, which is the patch level a CPU reports once it runs it.
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// The patch as its section holds it, header first. MSR C001_0020 is
    /// written the linear address of its first byte (Linux `amd.c:700-705`).
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Whether the CPU that loads it must then drop the translations of its
    /// first and last byte: family 0x17's loader leaves the mapping of a
    /// patch's bytes in the TLB as uncacheable (Linux `c809b0d0e52d` and
    /// `amd.c:707-718`, Xen at AMD's instruction).
    pub fn leaves_its_mapping_uncached(&self) -> bool {
        family(self.revision.0) == 0x17
    }
}

/// Why a container is refused: the offset of the section that is malformed,
/// and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub at: usize,
    pub why: Refusal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes end before the container's header, the table, a section's
    /// header or its patch does.
    Truncated { need: usize, have: usize },
    /// The container does not open with [`MAGIC`].
    Magic(u32),
    /// The first section is not the equivalence table.
    TableType(u32),
    /// A section after the table is not a patch.
    SectionType(u32),
    /// The section is shorter than a patch's header.
    Short(u32),
    /// The patch is for one chipset (`nb_dev_id`, `sb_dev_id`).
    Chipset { north: u32, south: u32 },
    /// The patch names the lowest revision it may be loaded over.
    MinimumRevision(u32),
}

/// The patch in `container` a CPU with `signature` running `found` loads, or
/// why there is none.
pub fn select<'a>(
    container: &'a [u8],
    signature: Signature,
    found: Revision,
) -> Result<Choice<Patch<'a>>, Refused> {
    let have = container.len();
    let at_header = |why| Refused { at: 0, why };
    let (header, rest) = container
        .split_first_chunk::<HEADER>()
        .ok_or(at_header(Refusal::Truncated { need: HEADER, have }))?;
    let [magic, table, len] = dwords(header);
    if magic != MAGIC {
        return Err(at_header(Refusal::Magic(magic)));
    }
    if table != EQUIVALENCE_TABLE {
        return Err(at_header(Refusal::TableType(table)));
    }
    let need = HEADER.saturating_add(len as usize);
    let (_, mut rest) =
        rest.split_at_checked(len as usize).ok_or(at_header(Refusal::Truncated { need, have }))?;

    let mut newest: Option<Patch<'a>> = None;
    while !rest.is_empty() {
        // `rest` is a suffix of `container`.
        let at = have.wrapping_sub(rest.len());
        let (patch, after) = section(rest).map_err(|why| Refused { at, why })?;
        if named(patch.revision.0) == signature.0 && newest.is_none_or(|n| patch.revision > n.revision) {
            newest = Some(patch);
        }
        rest = after;
    }
    Ok(match newest {
        None => Choice::NoMatch,
        Some(patch) if patch.revision >= found => Choice::Load(patch),
        Some(patch) => Choice::Current(patch.revision),
    })
}

/// The patch the section `bytes` starts with, and the bytes after it.
fn section(bytes: &[u8]) -> Result<(Patch<'_>, &[u8]), Refusal> {
    let have = bytes.len();
    let (header, rest) =
        bytes.split_first_chunk::<SECTION>().ok_or(Refusal::Truncated { need: SECTION, have })?;
    let [kind, size] = dwords(header);
    if kind != PATCH {
        return Err(Refusal::SectionType(kind));
    }
    let need = SECTION.saturating_add(size as usize);
    let (bytes, rest) = rest.split_at_checked(size as usize).ok_or(Refusal::Truncated { need, have })?;
    let (patch, _) = bytes.split_first_chunk::<PATCH_HEADER>().ok_or(Refusal::Short(size))?;
    let [_, id, _, minimum, north, south, ..]: [u32; 16] = dwords(patch);
    if north != 0 || south != 0 {
        return Err(Refusal::Chipset { north, south });
    }
    if minimum != 0 {
        return Err(Refusal::MinimumRevision(minimum));
    }
    Ok((Patch { revision: Revision(id), bytes }, rest))
}

/// The CPUID.01H:EAX a Zen patch ID names (Linux's `ucode_rev_to_cpuid`): its
/// stepping, model, extended model and extended family, over base family 0xf.
const fn named(id: u32) -> u32 {
    let (stepping, model) = (id >> 8 & 0xf, id >> 12 & 0xf);
    let (ext_model, ext_family) = (id >> 20 & 0xf, id >> 24);
    stepping | model << 4 | 0xf << 8 | ext_model << 16 | ext_family << 20
}

/// The family a Zen patch ID names: base family 0xf plus its extended family.
const fn family(id: u32) -> u32 {
    (id >> 24).wrapping_add(0xf)
}
