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
//! nothing vouched for. The header is the whole of what is signed, so a
//! section's bytes are judged by its hash and never by being signed
//! themselves — which is what lets a later format fetch them as
//! content-addressed chunks under this same header.

use crate::Digest;

/// The first eight bytes of every header.
pub const MAGIC: [u8; 8] = *b"TOYOSIMG";

/// This layout. A header of any other format is refused by name.
pub const FORMAT: u32 = 1;

/// The kernel ELF.
pub const KERNEL: &str = "kernel";
/// The boot parameter the loader hands the kernel.
pub const CMDLINE: &str = "cmdline";
/// ROOT's filesystem, whole.
pub const ROOT: &str = "root";

/// Every section an image carries, in the order it carries them.
pub const SECTIONS: [&str; 3] = [KERNEL, CMDLINE, ROOT];

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
/// any ROOT this tree builds and below what a slot is.
pub const MAX_BYTES: [u64; 3] = [1 << 30, 4096, 4 << 30];

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
    pub sections: [Section; 3],
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
        let mut sections = [Section { len: 0, sha256: [0; 32] }; 3];
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
        let root = sections[2].len;
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
    pub fn of(version: u64, kernel: &[u8], cmdline: &[u8], root: &[u8]) -> Self {
        let section = |bytes: &[u8]| Section { len: bytes.len() as u64, sha256: crate::sha256(bytes) };
        Self { version, sections: [section(kernel), section(cmdline), section(root)] }
    }

    pub fn kernel(&self) -> Section {
        self.sections[0]
    }

    pub fn cmdline(&self) -> Section {
        self.sections[1]
    }

    pub fn root(&self) -> Section {
        self.sections[2]
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

/// An image whose layout adds up, split into its parts. Nothing here is
/// vouched for until the header's signature and each section's hash are
/// checked ([`crate::sig::verify`], [`Parts::matches`]).
pub struct Parts<'a> {
    pub header: Header,
    pub signed: &'a [u8; SIGNED_BYTES],
    pub kernel: &'a [u8],
    pub cmdline: &'a [u8],
    pub root: &'a [u8],
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
        let (k, c) = (header.kernel().len as usize, header.cmdline().len as usize);
        let rest = &bytes[SIGNED_BYTES..];
        Ok(Self { header, signed, kernel: &rest[..k], cmdline: &rest[k..k + c], root: &rest[k + c..] })
    }

    /// The first section whose bytes are not the ones the header names, or
    /// `None` where every one is.
    pub fn mismatched(&self) -> Option<&'static str> {
        [self.kernel, self.cmdline, self.root]
            .iter()
            .zip(self.header.sections)
            .zip(SECTIONS)
            .find(|((bytes, section), _)| crate::sha256(bytes) != section.sha256)
            .map(|(_, name)| name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header::of(7, b"\x7fELF kernel", b"root=x", &[0x5A; 8192])
    }

    fn signed(header: &Header) -> [u8; SIGNED_BYTES] {
        let mut out = [0u8; SIGNED_BYTES];
        out[..HEADER_BYTES].copy_from_slice(&header.encode());
        out
    }

    #[test]
    fn a_header_reads_back_as_it_was_written() {
        let h = header();
        assert_eq!(Header::parse(&signed(&h)), Ok(h));
        assert_eq!(HEADER_BYTES, 192);
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
        assert_eq!(bend(8, &2u32.to_le_bytes()), Err(Malformed::Format(2)));
        assert_eq!(bend(12, &4u32.to_le_bytes()), Err(Malformed::SectionCount(4)));
        assert_eq!(bend(FIXED_BYTES, b"kernal"), Err(Malformed::SectionName(0)));
        // The name is the whole padded field, so a longer one is not a prefix match.
        assert_eq!(bend(FIXED_BYTES + ENTRY_BYTES + 7, b"x"), Err(Malformed::SectionName(1)));
        let at = FIXED_BYTES + ENTRY_BYTES + NAME_BYTES;
        assert_eq!(bend(at, &4097u64.to_le_bytes()), Err(Malformed::TooLong(1, 4097)));
        let root = FIXED_BYTES + 2 * ENTRY_BYTES + NAME_BYTES;
        assert_eq!(bend(root, &4095u64.to_le_bytes()), Err(Malformed::RootNotBlocks(4095)));
        assert_eq!(bend(root, &0u64.to_le_bytes()), Err(Malformed::RootNotBlocks(0)));
    }

    #[test]
    fn an_image_splits_into_exactly_its_sections() {
        let (kernel, cmdline, root) = (b"\x7fELF kernel".to_vec(), b"root=x".to_vec(), vec![0x5A; 8192]);
        let h = Header::of(7, &kernel, &cmdline, &root);
        let mut bytes = signed(&h).to_vec();
        bytes.extend_from_slice(&kernel);
        bytes.extend_from_slice(&cmdline);
        bytes.extend_from_slice(&root);
        let parts = Parts::split(&bytes).expect("an image");
        assert_eq!((parts.kernel, parts.cmdline, parts.root), (&kernel[..], &cmdline[..], &root[..]));
        assert_eq!(parts.mismatched(), None);

        let mut flipped = bytes.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert_eq!(Parts::split(&flipped).expect("an image").mismatched(), Some(ROOT));
        flipped[SIGNED_BYTES] ^= 1;
        assert_eq!(Parts::split(&flipped).expect("an image").mismatched(), Some(KERNEL));

        let want = bytes.len() as u64;
        bytes.push(0);
        assert_eq!(Parts::split(&bytes).err(), Some(Malformed::Length { have: want + 1, want }));
        bytes.truncate(bytes.len() - 2);
        assert_eq!(Parts::split(&bytes).err(), Some(Malformed::Length { have: want - 1, want }));
    }
}
