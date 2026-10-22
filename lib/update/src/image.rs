//! The signed image: a header, its signature, and the sections it names.
//!
//! ```text
//! header   magic "TOYOSIMG" | format u32 | sections u32 | version u64
//!          then per section: name [u8; 16] | length u64 | sha256 [u8; 32]
//! signed   header | signature [u8; 64]                  (SIGNED_BYTES)
//! image    signed | the sections' bytes, in header order
//! ```
//!
//! Little-endian throughout. **The sections are exactly [`SECTIONS`], in that
//! order**, and a header naming anything else is refused rather than read
//! around: a loader that skipped a section it did not know would boot bytes
//! nothing vouched for. A section's name is its role, so one version binds the
//! kernel, its parameter, the two programs the kernel starts the machine with
//! and ROOT, each by role, length and hash. The header is the whole of what is
//! signed, so a section's bytes are judged by its hash and never by being
//! signed themselves — which is what lets a later format fetch them as
//! content-addressed chunks under this same header.

use crate::Digest;

/// The first eight bytes of every header.
pub const MAGIC: [u8; 8] = *b"TOYOSIMG";

/// This layout. A header of any other format is refused by name.
pub const FORMAT: u32 = 2;

/// The kernel ELF.
pub const KERNEL: &str = "kernel";
/// The boot parameter the loader hands the kernel.
pub const CMDLINE: &str = "cmdline";
/// The supervisor, the one program the kernel starts.
pub const SUPERVISOR: &str = "supervisor";
/// The file server the supervisor serves ROOT with before it reads a file.
pub const FILESERVER: &str = "fileserver";
/// ROOT's filesystem, whole.
pub const ROOT: &str = "root";

/// Every section an image carries, in the order it carries them. ROOT is last,
/// so an update streams it after every other section is held.
pub const SECTIONS: [&str; 5] = [KERNEL, CMDLINE, SUPERVISOR, FILESERVER, ROOT];

/// Bytes a section's name takes, NUL-padded.
const NAME_BYTES: usize = 16;
/// One section's entry.
const ENTRY_BYTES: usize = NAME_BYTES + 8 + 32;
/// The fixed part ahead of the entries.
const FIXED_BYTES: usize = 8 + 4 + 4 + 8;

/// The header's length, which is fixed by [`SECTIONS`].
pub const HEADER_BYTES: usize = FIXED_BYTES + SECTIONS.len() * ENTRY_BYTES;
/// An Ed25519 signature.
pub const SIGNATURE_BYTES: usize = 64;
/// The header and its signature: what a slot keeps as its signed header.
pub const SIGNED_BYTES: usize = HEADER_BYTES + SIGNATURE_BYTES;

/// The largest section of each kind an image may carry, so a header is a
/// bound on what a reader allocates before a single hash is checked. The
/// kernel's is the loader's own bound on an ESP file; ROOT's is well above
/// any ROOT this tree builds and below what a slot is; each program's is far
/// above what this tree builds (2 MiB) and a bound on the pages the kernel keeps.
pub const MAX_BYTES: [u64; 5] = [1 << 30, 4096, 64 << 20, 64 << 20, 4 << 30];

/// ROOT is whole 4 KiB blocks: it is written to a partition and read back in them.
pub const ROOT_BLOCK: u64 = 4096;

/// One section as the header names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub len: u64,
    pub sha256: Digest,
}

/// A header, decoded. Every field was checked by [`Header::parse`]; nothing in
/// it has been vouched for until [`crate::sig::verify`] says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u64,
    /// In [`SECTIONS`] order.
    pub sections: [Section; 5],
}

/// Why bytes are not an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// Fewer bytes than a signed header.
    Short(usize),
    Magic,
    Format(u32),
    SectionCount(u32),
    /// The section at this index is not the one [`SECTIONS`] puts there.
    SectionName(usize),
    /// The section at this index is longer than [`MAX_BYTES`] allows.
    TooLong(usize, u64),
    /// The program section at this index is empty.
    Empty(usize),
    /// ROOT is not whole [`ROOT_BLOCK`]s, or is empty.
    RootNotBlocks(u64),
    /// The image's bytes do not add up to the header's sections.
    Length { have: u64, want: u64 },
}

impl core::fmt::Display for Malformed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::Short(n) => write!(f, "{n} bytes is shorter than a signed header ({SIGNED_BYTES})"),
            Self::Magic => write!(f, "the header does not begin TOYOSIMG"),
            Self::Format(n) => write!(f, "the header is format {n}, and this reads format {FORMAT}"),
            Self::SectionCount(n) => write!(f, "the header names {n} sections, and an image has {}", SECTIONS.len()),
            Self::SectionName(i) => write!(f, "section {i} is not `{}`", SECTIONS[i]),
            Self::TooLong(i, len) => write!(f, "the `{}` section is {len} bytes, past its bound of {}", SECTIONS[i], MAX_BYTES[i]),
            Self::Empty(i) => write!(f, "the `{}` section is empty", SECTIONS[i]),
            Self::RootNotBlocks(len) => write!(f, "ROOT is {len} bytes, which is not whole {ROOT_BLOCK}-byte blocks"),
            Self::Length { have, want } => write!(f, "the image is {have} bytes and its header adds up to {want}"),
        }
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

/// `name` NUL-padded to [`NAME_BYTES`].
fn padded(name: &str) -> [u8; NAME_BYTES] {
    let mut out = [0u8; NAME_BYTES];
    out[..name.len()].copy_from_slice(name.as_bytes());
    out
}

impl Header {
    /// The header at the front of `bytes`, which must be at least a signed
    /// header long: the signature is not checked here, only the layout.
    pub fn parse(bytes: &[u8]) -> Result<Self, Malformed> {
        if bytes.len() < SIGNED_BYTES {
            return Err(Malformed::Short(bytes.len()));
        }
        if bytes[..8] != MAGIC {
            return Err(Malformed::Magic);
        }
        let format = u32_at(bytes, 8);
        if format != FORMAT {
            return Err(Malformed::Format(format));
        }
        let count = u32_at(bytes, 12);
        if count as usize != SECTIONS.len() {
            return Err(Malformed::SectionCount(count));
        }
        let version = u64_at(bytes, 16);
        let mut sections = [Section { len: 0, sha256: [0; 32] }; 5];
        for (i, section) in sections.iter_mut().enumerate() {
            let at = FIXED_BYTES + i * ENTRY_BYTES;
            if bytes[at..at + NAME_BYTES] != padded(SECTIONS[i]) {
                return Err(Malformed::SectionName(i));
            }
            let len = u64_at(bytes, at + NAME_BYTES);
            if len > MAX_BYTES[i] {
                return Err(Malformed::TooLong(i, len));
            }
            section.len = len;
            section.sha256.copy_from_slice(&bytes[at + NAME_BYTES + 8..at + ENTRY_BYTES]);
        }
        for program in [2, 3] {
            if sections[program].len == 0 {
                return Err(Malformed::Empty(program));
            }
        }
        let root = sections[4].len;
        if root == 0 || !root.is_multiple_of(ROOT_BLOCK) {
            return Err(Malformed::RootNotBlocks(root));
        }
        Ok(Self { version, sections })
    }

    /// The header's bytes, which are what is signed.
    pub fn encode(&self) -> [u8; HEADER_BYTES] {
        let mut out = [0u8; HEADER_BYTES];
        out[..8].copy_from_slice(&MAGIC);
        out[8..12].copy_from_slice(&FORMAT.to_le_bytes());
        out[12..16].copy_from_slice(&(SECTIONS.len() as u32).to_le_bytes());
        out[16..24].copy_from_slice(&self.version.to_le_bytes());
        for (i, section) in self.sections.iter().enumerate() {
            let at = FIXED_BYTES + i * ENTRY_BYTES;
            out[at..at + NAME_BYTES].copy_from_slice(&padded(SECTIONS[i]));
            out[at + NAME_BYTES..at + NAME_BYTES + 8].copy_from_slice(&section.len.to_le_bytes());
            out[at + NAME_BYTES + 8..at + ENTRY_BYTES].copy_from_slice(&section.sha256);
        }
        out
    }

    /// The header naming exactly these sections' bytes.
    pub fn of(version: u64, sections: &Sections<'_>) -> Self {
        Self { version, sections: sections.all().map(|bytes| Section { len: bytes.len() as u64, sha256: crate::sha256(bytes) }) }
    }

    pub fn kernel(&self) -> Section {
        self.sections[0]
    }

    pub fn cmdline(&self) -> Section {
        self.sections[1]
    }

    pub fn supervisor(&self) -> Section {
        self.sections[2]
    }

    pub fn fileserver(&self) -> Section {
        self.sections[3]
    }

    pub fn root(&self) -> Section {
        self.sections[4]
    }

    /// The bytes the sections take after the signed header.
    pub fn payload_len(&self) -> u64 {
        self.sections.iter().map(|s| s.len).sum()
    }
}

/// The signature at the end of a signed header.
pub fn signature_of(signed: &[u8; SIGNED_BYTES]) -> [u8; SIGNATURE_BYTES] {
    signed[HEADER_BYTES..].try_into().expect("a signed header ends in a signature")
}

/// Each section's bytes, by role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sections<'a> {
    pub kernel: &'a [u8],
    pub cmdline: &'a [u8],
    pub supervisor: &'a [u8],
    pub fileserver: &'a [u8],
    pub root: &'a [u8],
}

impl<'a> Sections<'a> {
    /// In [`SECTIONS`] order.
    pub fn all(&self) -> [&'a [u8]; 5] {
        [self.kernel, self.cmdline, self.supervisor, self.fileserver, self.root]
    }
}

/// An image whose layout adds up, split into its parts. Nothing here is
/// vouched for until the header's signature ([`crate::sig::verify`]) and
/// each section's hash against its header entry are checked.
pub struct Parts<'a> {
    pub header: Header,
    pub signed: &'a [u8; SIGNED_BYTES],
    pub sections: Sections<'a>,
}

impl<'a> Parts<'a> {
    /// `bytes` as an image: a signed header and exactly the bytes its sections add up to.
    pub fn split(bytes: &'a [u8]) -> Result<Self, Malformed> {
        let header = Header::parse(bytes)?;
        let want = SIGNED_BYTES as u64 + header.payload_len();
        if bytes.len() as u64 != want {
            return Err(Malformed::Length { have: bytes.len() as u64, want });
        }
        let signed: &[u8; SIGNED_BYTES] = bytes[..SIGNED_BYTES].try_into().expect("checked above");
        let mut rest = &bytes[SIGNED_BYTES..];
        let mut take = |section: Section| {
            let (bytes, after) = rest.split_at(section.len as usize);
            rest = after;
            bytes
        };
        let [kernel, cmdline, supervisor, fileserver, root] = header.sections.map(&mut take);
        Ok(Self { header, signed, sections: Sections { kernel, cmdline, supervisor, fileserver, root } })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_BYTES: [u8; 8192] = [0x5A; 8192];

    fn sections() -> Sections<'static> {
        Sections {
            kernel: b"\x7fELF kernel",
            cmdline: b"slot=A",
            supervisor: b"\x7fELF supervisor",
            fileserver: b"\x7fELF fileserver",
            root: &ROOT_BYTES,
        }
    }

    fn header() -> Header {
        Header::of(7, &sections())
    }

    fn signed(header: &Header) -> [u8; SIGNED_BYTES] {
        let mut out = [0u8; SIGNED_BYTES];
        out[..HEADER_BYTES].copy_from_slice(&header.encode());
        out
    }

    /// The entry of section `i`: where its name, then its length, begin.
    fn entry(i: usize) -> usize {
        FIXED_BYTES + i * ENTRY_BYTES
    }

    #[test]
    fn a_header_reads_back_as_it_was_written() {
        let h = header();
        assert_eq!(Header::parse(&signed(&h)), Ok(h));
        assert_eq!(HEADER_BYTES, 304);
        assert_eq!(h.supervisor().sha256, crate::sha256(b"\x7fELF supervisor"));
        assert_eq!(h.fileserver().len, 15);
    }

    /// Every field a reader decides on, bent one at a time, is refused by the
    /// word for it.
    #[test]
    fn a_bent_header_is_refused_by_name() {
        let good = signed(&header());
        let bend = |at: usize, value: &[u8]| {
            let mut bent = good;
            bent[at..at + value.len()].copy_from_slice(value);
            Header::parse(&bent)
        };
        assert_eq!(Header::parse(&good[..SIGNED_BYTES - 1]), Err(Malformed::Short(SIGNED_BYTES - 1)));
        assert_eq!(bend(0, b"TOYOSIMH"), Err(Malformed::Magic));
        assert_eq!(bend(8, &1u32.to_le_bytes()), Err(Malformed::Format(1)));
        assert_eq!(bend(12, &3u32.to_le_bytes()), Err(Malformed::SectionCount(3)));
        assert_eq!(bend(entry(0), b"kernal"), Err(Malformed::SectionName(0)));
        // The name is the whole padded field, so a longer one is not a prefix match.
        assert_eq!(bend(entry(1) + 7, b"x"), Err(Malformed::SectionName(1)));
        assert_eq!(bend(entry(1) + NAME_BYTES, &4097u64.to_le_bytes()), Err(Malformed::TooLong(1, 4097)));
        let len = |i| entry(i) + NAME_BYTES;
        assert_eq!(bend(len(4), &4095u64.to_le_bytes()), Err(Malformed::RootNotBlocks(4095)));
        assert_eq!(bend(len(4), &0u64.to_le_bytes()), Err(Malformed::RootNotBlocks(0)));
        for program in [2, 3] {
            assert_eq!(bend(len(program), &0u64.to_le_bytes()), Err(Malformed::Empty(program)));
            let past = MAX_BYTES[program] + 1;
            assert_eq!(bend(len(program), &past.to_le_bytes()), Err(Malformed::TooLong(program, past)));
        }
    }

    /// **A role is its place**: the two programs' entries swapped are refused
    /// by name, so a header cannot hand the kernel the file server as the
    /// supervisor.
    #[test]
    fn two_programs_swapped_are_refused() {
        let good = signed(&header());
        let mut swapped = good;
        swapped[entry(2)..entry(3)].copy_from_slice(&good[entry(3)..entry(4)]);
        swapped[entry(3)..entry(4)].copy_from_slice(&good[entry(2)..entry(3)]);
        assert_eq!(Header::parse(&swapped), Err(Malformed::SectionName(2)));
    }

    #[test]
    fn an_image_splits_into_exactly_its_sections() {
        let s = sections();
        let h = Header::of(7, &s);
        let mut bytes = signed(&h).to_vec();
        for section in s.all() {
            bytes.extend_from_slice(section);
        }
        let parts = Parts::split(&bytes).expect("an image");
        assert_eq!(parts.sections, s);
        let want = bytes.len() as u64;
        bytes.push(0);
        assert_eq!(Parts::split(&bytes).err(), Some(Malformed::Length { have: want + 1, want }));
        bytes.truncate(bytes.len() - 2);
        assert_eq!(Parts::split(&bytes).err(), Some(Malformed::Length { have: want - 1, want }));
    }
}
