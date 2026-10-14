//! Intel's microcode update files, validated whole and read one update at a
//! time.
//!
//! A file Intel publishes for one processor signature is updates laid end to
//! end. [`updates`] validates every one against SDM Vol. 3A §12.11 (order
//! number 325384-093US) — header, checksum and the optional extended
//! signature table — and one malformed update refuses the whole file. Only
//! the documented headers are read: an update's data is Intel's signed
//! payload, handed on whole and never interpreted.

#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

use toyos_microcode::{Cpu, PlatformId};

use super::Patch;

#[cfg(test)]
mod tests;

/// The header's length, and so the offset of the update data (Table 12-7).
pub const HEADER: usize = 48;
/// The extended signature table's header: count, checksum, 12 reserved bytes
/// (Table 12-9).
const EXT_HEADER: usize = 20;
/// One extended signature: signature, processor flags, checksum (Table 12-10).
const EXT_SIGNATURE: usize = 12;
/// Total Size is "always a multiple of 1024" (Table 12-7).
const GRANULE: usize = 1024;

/// One update out of a file, validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Update<'a> {
    revision: u32,
    signature: u32,
    flags: u32,
    /// The whole update: header, data and extended signature table.
    bytes: &'a [u8],
    extended: &'a [[u8; EXT_SIGNATURE]],
}

impl<'a> Update<'a> {
    /// The update as the CPUs it names load it, and every one of them: the
    /// header's signature and each extended signature's, with each platform
    /// its processor flags name (Examples 12-5 and 12-6).
    pub fn patches(&self) -> impl Iterator<Item = (Cpu, Patch<'a>)> + '_ {
        let extended = self.extended.iter().map(|entry| {
            let [signature, flags, _] = dwords(entry);
            (signature, flags)
        });
        let patch = Patch { revision: self.revision, bytes: self.bytes };
        core::iter::once((self.signature, self.flags)).chain(extended).flat_map(move |(signature, flags)| {
            PlatformId::all()
                .filter(move |platform| flags & platform.flag() != 0)
                .map(move |platform| (Cpu::Intel { signature, platform }, patch))
        })
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

/// Every update in `file`, which holds at least one.
pub fn updates(file: &[u8]) -> Result<Vec<Update<'_>>, Refused> {
    let mut updates = Vec::new();
    let mut rest = file;
    loop {
        // `rest` is a suffix of `file`.
        let at = file.len().wrapping_sub(rest.len());
        let (update, after) = parse(rest).map_err(|why| Refused { at, why })?;
        updates.push(update);
        if after.is_empty() {
            return Ok(updates);
        }
        rest = after;
    }
}

/// The update `bytes` starts with, and the bytes after it.
fn parse(bytes: &[u8]) -> Result<(Update<'_>, &[u8]), Refusal> {
    let have = bytes.len();
    let (header, _) =
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
    let signed = HEADER
        .checked_add(data)
        .filter(|&signed| signed <= total)
        .ok_or(Refusal::TotalBelowData { total, data })?;
    let truncated = Refusal::Truncated { need: total, have };
    let (whole, after) = bytes.split_at_checked(total).ok_or(truncated)?;
    let (signed, table) = whole.split_at_checked(signed).ok_or(truncated)?;
    match sum(signed) {
        0 => {}
        sum => return Err(Refusal::Checksum(sum)),
    }
    let extended = match table {
        [] => &[],
        table => extended(table, signature.wrapping_add(checksum).wrapping_add(flags))?,
    };
    Ok((Update { revision, signature, flags, bytes: whole, extended }, after))
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

/// `bytes` as little-endian dwords; a `LEN` that is not `4 * N` does not build.
pub(super) fn dwords<const LEN: usize, const N: usize>(bytes: &[u8; LEN]) -> [u32; N] {
    const { assert!(LEN == 4 * N) };
    let mut dwords = [0; N];
    for (dword, bytes) in dwords.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *dword = u32::from_le_bytes(*bytes);
    }
    dwords
}

/// Every dword of `bytes` summed, unsigned, with wrap (§12.11.5).
fn sum(bytes: &[u8]) -> u32 {
    bytes.as_chunks::<4>().0.iter().fold(0, |sum, &w| sum.wrapping_add(u32::from_le_bytes(w)))
}
