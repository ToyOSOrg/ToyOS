//! Little-endian reads out of a byte slice that came off a disk.
//!
//! Every accessor is bounds-checked and returns a refusal rather than
//! panicking: the slice is untrusted input, and the kernel links this.

use super::UpstreamError;

/// A window on disk bytes, named so a refusal can say which structure ran short.
#[derive(Clone, Copy)]
pub struct Raw<'a> {
    bytes: &'a [u8],
    what: &'static str,
}

impl<'a> Raw<'a> {
    pub fn new(bytes: &'a [u8], what: &'static str) -> Self {
        Self { bytes, what }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    fn short(&self) -> UpstreamError {
        UpstreamError::Refused(self.what)
    }

    pub fn slice(&self, off: usize, len: usize) -> Result<&'a [u8], UpstreamError> {
        let end = off.checked_add(len).ok_or_else(|| self.short())?;
        self.bytes.get(off..end).ok_or_else(|| self.short())
    }

    /// A window on part of this one, carrying its own name for refusals.
    pub fn sub(&self, off: usize, len: usize, what: &'static str) -> Result<Raw<'a>, UpstreamError> {
        Ok(Raw::new(self.slice(off, len)?, what))
    }

    pub fn u8(&self, off: usize) -> Result<u8, UpstreamError> {
        Ok(self.slice(off, 1)?[0])
    }

    pub fn u16(&self, off: usize) -> Result<u16, UpstreamError> {
        let b = self.slice(off, 2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&self, off: usize) -> Result<u32, UpstreamError> {
        let b = self.slice(off, 4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&self, off: usize) -> Result<u64, UpstreamError> {
        let b = self.slice(off, 8)?;
        Ok(u64::from_le_bytes(b.try_into().expect("an 8-byte window")))
    }

    pub fn uuid(&self, off: usize) -> Result<[u8; 16], UpstreamError> {
        Ok(self.slice(off, 16)?.try_into().expect("a 16-byte window"))
    }
}

/// The `LE64_BITMASK` accessor: bits `[start, end)` of `word`.
pub fn bits(word: u64, start: u32, end: u32) -> u64 {
    debug_assert!(start < end && end <= 64);
    (word >> start) & !(!0u64 << (end - start))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A structure that ends one byte short refuses by its own name instead of
    /// panicking, at every width.
    #[test]
    fn a_short_window_refuses_by_name() {
        let buf = [0u8; 7];
        let raw = Raw::new(&buf, "a truncated thing");
        assert_eq!(raw.u64(0), Err(UpstreamError::Refused("a truncated thing")));
        assert_eq!(raw.u32(4), Err(UpstreamError::Refused("a truncated thing")));
        assert_eq!(raw.u16(6), Err(UpstreamError::Refused("a truncated thing")));
        assert_eq!(raw.u8(7), Err(UpstreamError::Refused("a truncated thing")));
        assert_eq!(raw.uuid(0), Err(UpstreamError::Refused("a truncated thing")));
        assert!(raw.u32(3).is_ok());
    }

    /// An offset near `usize::MAX` must not wrap into a valid range.
    #[test]
    fn an_offset_that_would_wrap_refuses() {
        let buf = [0u8; 16];
        let raw = Raw::new(&buf, "wrapping");
        assert!(raw.slice(usize::MAX, 8).is_err());
        assert!(raw.slice(usize::MAX - 4, 8).is_err());
    }

    #[test]
    fn bitmask_matches_upstreams_accessor() {
        let flags = 0x8111_1100_8020_0107u64;
        assert_eq!(bits(flags, 0, 1), 1);
        assert_eq!(bits(flags, 1, 2), 1);
        assert_eq!(bits(flags, 12, 28), 512);
        assert_eq!(bits(flags, 40, 44), 1);
        assert_eq!(bits(flags, 63, 64), 1);
    }
}
