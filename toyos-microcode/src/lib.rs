//! Intel microcode update files, read as the SDM defines them, pure.
//!
//! A file Intel publishes for one processor signature is updates laid end to
//! end. [`select`] validates every one against SDM Vol. 3A §12.11 (order
//! number 325384-093US) — header, checksum and the optional extended signature
//! table — and answers with the newest that names a CPU's signature and
//! platform, or why it loads none. One malformed update refuses the whole file.
//!
//! Only the documented header, checksums and signature table are read. The
//! update data is the maker's signed payload: handed on whole, never
//! interpreted.
//!
//! Pure: no I/O, no allocation, no `unsafe`. The caller reads the CPU.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

/// The header's length, and so the offset of the update data (Table 12-7).
const HEADER: usize = 48;
/// The update data's length when the header's Data Size is 0 (Table 12-7).
const DEFAULT_DATA: usize = 2000;
/// The extended signature table's header: count, checksum, 12 reserved bytes
/// (Table 12-9).
const EXT_HEADER: usize = 20;
/// One extended signature: signature, processor flags, checksum (Table 12-10).
const EXT_SIGNATURE: usize = 12;
/// Total Size is "always a multiple of 1024" (Table 12-7).
const GRANULE: usize = 1024;

/// CPUID.01H:EAX, compared whole with an update's (§12.11.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Signature(pub u32);

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
        1 << self.0
    }
}

/// An update revision. Signed (Table 12-7): one update is newer than another
/// only when numerically larger (Example 12-10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(pub i32);

impl Revision {
    /// `IA32_BIOS_SIGN_ID` (MSR 8BH) read after writing it 0 and executing
    /// CPUID.01H: the revision is its upper dword (§12.11.7.1).
    pub const fn from_sign_id(msr: u64) -> Self {
        Self((msr >> 32) as u32 as i32)
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
    /// Header, data and extended signature table: Total Size bytes.
    bytes: &'a [u8],
    data: usize,
}

impl<'a> Update<'a> {
    pub fn revision(&self) -> Revision {
        Revision(dword(self.bytes, 4) as i32)
    }

    /// The update data. `IA32_BIOS_UPDT_TRIG` is written its linear address,
    /// which must be 16-byte aligned and mapped present (§12.11.6).
    pub fn data(&self) -> &'a [u8] {
        &self.bytes[HEADER..HEADER + self.data]
    }

    /// The header's signature and flags, then each extended signature's
    /// (Examples 12-5 and 12-6).
    fn names(&self, cpu: &Cpu) -> bool {
        let primary = (dword(self.bytes, 12), dword(self.bytes, 24));
        let table = self.bytes.get(HEADER + self.data + EXT_HEADER..).unwrap_or(&[]);
        let extended = table.chunks_exact(EXT_SIGNATURE).map(|s| (dword(s, 0), dword(s, 4)));
        core::iter::once(primary)
            .chain(extended)
            .any(|(sig, flags)| sig == cpu.signature.0 && flags & cpu.platform.flag() != 0)
    }
}

/// What a valid file holds for one CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice<'a> {
    /// The newest update naming the CPU, newer than the revision it runs.
    Load(Update<'a>),
    /// The newest update naming the CPU is this revision, and the CPU already
    /// runs it or a newer one.
    Current(Revision),
    /// No update in the file names the CPU.
    NoMatch,
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

/// The update in `file` to load on `cpu`, or why there is none.
pub fn select<'a>(file: &'a [u8], cpu: &Cpu) -> Result<Choice<'a>, Refused> {
    let mut newest: Option<Update<'a>> = None;
    let mut at = 0;
    while at < file.len() {
        let update = parse(&file[at..]).map_err(|why| Refused { at, why })?;
        at += update.bytes.len();
        if update.names(cpu) && newest.is_none_or(|n| update.revision() > n.revision()) {
            newest = Some(update);
        }
    }
    Ok(match newest {
        None => Choice::NoMatch,
        Some(update) if update.revision() > cpu.revision => Choice::Load(update),
        Some(update) => Choice::Current(update.revision()),
    })
}

/// The update `bytes` starts with.
fn parse(bytes: &[u8]) -> Result<Update<'_>, Refusal> {
    let have = bytes.len();
    let header = bytes.get(..HEADER).ok_or(Refusal::Truncated { need: HEADER, have })?;
    match dword(header, 0) {
        1 => {}
        version => return Err(Refusal::HeaderVersion(version)),
    }
    match dword(header, 20) {
        1 => {}
        loader => return Err(Refusal::LoaderRevision(loader)),
    }
    let (data, total) = match dword(header, 28) {
        0 => (DEFAULT_DATA, HEADER + DEFAULT_DATA),
        size => (size as usize, dword(header, 32) as usize),
    };
    if data % 4 != 0 {
        return Err(Refusal::DataSize(data));
    }
    if total % GRANULE != 0 {
        return Err(Refusal::TotalSize(total));
    }
    let table_len = total
        .checked_sub(HEADER + data)
        .ok_or(Refusal::TotalBelowData { total, data })?;
    let bytes = bytes.get(..total).ok_or(Refusal::Truncated { need: total, have })?;

    match sum(&bytes[..HEADER + data]) {
        0 => {}
        sum => return Err(Refusal::Checksum(sum)),
    }
    if table_len != 0 {
        let table = &bytes[HEADER + data..];
        let count = table.get(..EXT_HEADER).map(|h| dword(h, 0) as usize);
        if count.is_none_or(|n| table_len != EXT_HEADER + n * EXT_SIGNATURE) {
            return Err(Refusal::ExtendedTableSize { len: table_len });
        }
        match sum(table) {
            0 => {}
            sum => return Err(Refusal::ExtendedTableChecksum(sum)),
        }
        // An extended signature, flags and checksum replace the header's
        // three in the update it stands for, so the two triples sum alike.
        let header = dword(bytes, 12).wrapping_add(dword(bytes, 16)).wrapping_add(dword(bytes, 24));
        for (index, s) in table[EXT_HEADER..].chunks_exact(EXT_SIGNATURE).enumerate() {
            if dword(s, 0).wrapping_add(dword(s, 4)).wrapping_add(dword(s, 8)) != header {
                return Err(Refusal::ExtendedSignatureChecksum { index });
            }
        }
    }
    Ok(Update { bytes, data })
}

/// The little-endian dword at `at`.
fn dword(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0; 4];
    word.copy_from_slice(&bytes[at..at + 4]);
    u32::from_le_bytes(word)
}

/// Every dword of `bytes` summed, unsigned, with wrap (§12.11.5).
fn sum(bytes: &[u8]) -> u32 {
    bytes.chunks_exact(4).fold(0, |sum, w| sum.wrapping_add(dword(w, 0)))
}
