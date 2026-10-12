//! Intel's microcode update files, read as the SDM defines them.
//!
//! A file Intel publishes for one processor signature is updates laid end to
//! end, and so is any number of such files laid end to end. [`select`]
//! validates every update against SDM Vol. 3A §12.11 (order number
//! 325384-093US) — header, checksum and the optional extended signature table
//! — and answers with the newest that names a CPU's signature and platform,
//! or why it loads none. One malformed update refuses the whole file.
//!
//! Only an update newer than the revision the CPU runs loads (Example 12-10):
//! the CPU "may or may not reload" an equal one, and loading an update on one
//! logical processor loads it on every other in its core.

use crate::{dwords, Choice, Revision, Signature};

#[cfg(test)]
mod tests;

/// The header's length, and so the offset of the update data (Table 12-7).
const HEADER: usize = 48;
/// The extended signature table's header: count, checksum, 12 reserved bytes
/// (Table 12-9).
const EXT_HEADER: usize = 20;
/// One extended signature: signature, processor flags, checksum (Table 12-10).
const EXT_SIGNATURE: usize = 12;
/// Total Size is "always a multiple of 1024" (Table 12-7).
const GRANULE: usize = 1024;

/// Which bit of an update's processor flags names this CPU's platform
/// (§12.11.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformId(u8);

impl PlatformId {
    /// `IA32_PLATFORM_ID` (MSR 17H): bits 52:50 (Table 12-11).
    pub const fn from_msr(msr: u64) -> Self {
        Self((msr >> 50) as u8 & 7)
    }

    const fn flag(self) -> u32 {
        // `self.0` < 8: the field is private and `from_msr` masks it.
        1 << self.0
    }
}

impl core::fmt::Display for PlatformId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.0, f)
    }
}

/// What one CPU reports, read on that CPU.
#[derive(Clone, Copy, Debug)]
pub struct Cpu {
    pub signature: Signature,
    pub platform: PlatformId,
    pub revision: Revision,
}

/// One update out of a file, validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Update<'a> {
    revision: Revision,
    signature: u32,
    flags: u32,
    data: &'a [u8],
    extended: &'a [[u8; EXT_SIGNATURE]],
}

impl<'a> Update<'a> {
    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// The update data. `IA32_BIOS_UPDT_TRIG` is written its linear address,
    /// which must be 16-byte aligned and mapped present (§12.11.6).
    pub fn data(&self) -> &'a [u8] {
        self.data
    }

    /// The header's signature and flags, then each extended signature's
    /// (Examples 12-5 and 12-6).
    fn names(&self, cpu: &Cpu) -> bool {
        let extended = self.extended.iter().map(|entry| {
            let [signature, flags, _] = dwords(entry);
            (signature, flags)
        });
        core::iter::once((self.signature, self.flags))
            .chain(extended)
            .any(|(signature, flags)| signature == cpu.signature.0 && flags & cpu.platform.flag() != 0)
    }
}

/// Why a file is refused: the offset of the update that is malformed, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused {
    pub at: usize,
    pub why: Refusal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes end before the header does, or before Total Size says the
    /// update does.
    Truncated { need: usize, have: usize },
    /// Header Version is not 1, the only version §12.11 defines.
    HeaderVersion(u32),
    /// Loader Revision is not 1, the only loader §12.11.6 describes.
    LoaderRevision(u32),
    /// Data Size is 0, which Table 12-7 reads as 2000 bytes: the form of
    /// updates for CPUs older than any ToyOS runs on.
    ZeroDataSize,
    /// Data Size is not a multiple of a dword.
    DataSize(usize),
    /// Total Size is not a multiple of 1024.
    TotalSize(usize),
    /// Total Size is smaller than the header and data it holds.
    TotalBelowData { total: usize, data: usize },
    /// The header's and data's dwords sum to this, not to 0 (§12.11.5).
    Checksum(u32),
    /// The extended signature table is not 20 bytes and 12 for each signature
    /// its count names.
    ExtendedTableSize { len: usize },
    /// The extended signature table's dwords sum to this, not to 0 (§12.11.2).
    ExtendedTableChecksum(u32),
    /// This extended signature's checksum does not stand in for the header's
    /// (Table 12-10).
    ExtendedSignatureChecksum { index: usize },
}

/// The update in `file` to load on `cpu`, or why there is none. A file holds
/// at least one update.
pub fn select<'a>(file: &'a [u8], cpu: &Cpu) -> Result<Choice<Update<'a>>, Refused> {
    let mut newest: Option<Update<'a>> = None;
    let mut rest = file;
    loop {
        // `rest` is a suffix of `file`.
        let at = file.len().wrapping_sub(rest.len());
        let (update, after) = parse(rest).map_err(|why| Refused { at, why })?;
        if update.names(cpu) && newest.is_none_or(|n| update.revision > n.revision) {
            newest = Some(update);
        }
        if after.is_empty() {
            break;
        }
        rest = after;
    }
    Ok(match newest {
        None => Choice::NoMatch,
        Some(update) if update.revision > cpu.revision => Choice::Load(update),
        Some(update) => Choice::Current(update.revision),
    })
}

/// The update `bytes` starts with, and the bytes after it.
fn parse(bytes: &[u8]) -> Result<(Update<'_>, &[u8]), Refusal> {
    let have = bytes.len();
    let (header, rest) =
        bytes.split_first_chunk::<HEADER>().ok_or(Refusal::Truncated { need: HEADER, have })?;
    let [version, revision, _, signature, checksum, loader, flags, data, total, ..]: [u32; 12] =
        dwords(header);
    if version != 1 {
        return Err(Refusal::HeaderVersion(version));
    }
    if loader != 1 {
        return Err(Refusal::LoaderRevision(loader));
    }
    let (data, total) = (data as usize, total as usize);
    if data == 0 {
        return Err(Refusal::ZeroDataSize);
    }
    if !data.is_multiple_of(4) {
        return Err(Refusal::DataSize(data));
    }
    if !total.is_multiple_of(GRANULE) {
        return Err(Refusal::TotalSize(total));
    }
    let table = HEADER
        .checked_add(data)
        .and_then(|signed| total.checked_sub(signed))
        .ok_or(Refusal::TotalBelowData { total, data })?;
    let truncated = Refusal::Truncated { need: total, have };
    let (data, rest) = rest.split_at_checked(data).ok_or(truncated)?;
    let (table, rest) = rest.split_at_checked(table).ok_or(truncated)?;

    match sum(header).wrapping_add(sum(data)) {
        0 => {}
        sum => return Err(Refusal::Checksum(sum)),
    }
    let extended = match table {
        [] => &[],
        table => extended(table, signature.wrapping_add(checksum).wrapping_add(flags))?,
    };
    let update = Update { revision: Revision(revision), signature, flags, data, extended };
    Ok((update, rest))
}

/// The entries of an extended signature table whose update's header
/// signature, checksum and flags sum to `primary`.
fn extended(table: &[u8], primary: u32) -> Result<&[[u8; EXT_SIGNATURE]], Refusal> {
    let size = Refusal::ExtendedTableSize { len: table.len() };
    let (header, entries) = table.split_first_chunk::<EXT_HEADER>().ok_or(size)?;
    let [count, ..]: [u32; 5] = dwords(header);
    if (count as usize).checked_mul(EXT_SIGNATURE) != Some(entries.len()) {
        return Err(size);
    }
    match sum(table) {
        0 => {}
        sum => return Err(Refusal::ExtendedTableChecksum(sum)),
    }
    let (entries, _) = entries.as_chunks::<EXT_SIGNATURE>();
    // An extended signature, flags and checksum replace the header's three in
    // the update it stands for, so the two triples sum alike.
    for (index, entry) in entries.iter().enumerate() {
        let [signature, flags, checksum] = dwords(entry);
        if signature.wrapping_add(flags).wrapping_add(checksum) != primary {
            return Err(Refusal::ExtendedSignatureChecksum { index });
        }
    }
    Ok(entries)
}

/// Every dword of `bytes` summed, unsigned, with wrap (§12.11.5).
fn sum(bytes: &[u8]) -> u32 {
    bytes.as_chunks::<4>().0.iter().fold(0, |sum, &w| sum.wrapping_add(u32::from_le_bytes(w)))
}
