//! A cursor over a definition block's bytes: the fixed-size data, PkgLength
//! (§20.2.4) and NameString (§20.2.2) encodings. Every read is bounded by the
//! end of the term list it is in, and a read past it is a refusal.

use alloc::vec::Vec;

use crate::name::{Path, Seg};
use crate::Error;

#[derive(Clone)]
pub(crate) struct Cursor<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) at: usize,
    pub(crate) end: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(bytes: &'a [u8], at: usize, end: usize) -> Self {
        Cursor { bytes, at, end }
    }

    pub(crate) fn malformed(&self, why: &'static str) -> Error {
        Error::Malformed { at: self.at, why }
    }

    pub(crate) fn done(&self) -> bool {
        self.at >= self.end
    }

    pub(crate) fn peek(&self) -> Result<u8, Error> {
        if self.at < self.end {
            self.bytes.get(self.at).copied().ok_or(self.malformed("a term runs past the table"))
        } else {
            Err(self.malformed("a term runs past its enclosing package"))
        }
    }

    /// The byte after the next, for the two-byte `ExtOpPrefix` opcodes.
    pub(crate) fn peek2(&self) -> Result<u8, Error> {
        let at = self.at + 1;
        if at < self.end {
            self.bytes.get(at).copied().ok_or(self.malformed("a term runs past the table"))
        } else {
            Err(self.malformed("a term runs past its enclosing package"))
        }
    }

    pub(crate) fn byte(&mut self) -> Result<u8, Error> {
        let b = self.peek()?;
        self.at += 1;
        Ok(b)
    }

    fn le(&mut self, n: usize) -> Result<u64, Error> {
        let mut v = 0u64;
        for i in 0..n {
            v |= u64::from(self.byte()?) << (8 * i);
        }
        Ok(v)
    }

    pub(crate) fn word(&mut self) -> Result<u16, Error> {
        Ok(self.le(2)? as u16)
    }

    pub(crate) fn dword(&mut self) -> Result<u32, Error> {
        Ok(self.le(4)? as u32)
    }

    pub(crate) fn qword(&mut self) -> Result<u64, Error> {
        self.le(8)
    }

    /// The value of a PkgLength (§20.2.4), and the offset of its lead byte.
    pub(crate) fn pkg_value(&mut self) -> Result<(usize, usize), Error> {
        let start = self.at;
        let lead = self.byte()?;
        let follow = usize::from(lead >> 6);
        if follow == 0 {
            return Ok((start, usize::from(lead & 0x3F)));
        }
        // §20.2.4: bits 5-4 of a multi-byte lead are reserved and must be zero.
        if lead & 0x30 != 0 {
            return Err(Error::Malformed { at: start, why: "a multi-byte PkgLength sets bits 5-4 of its lead byte" });
        }
        let mut len = usize::from(lead & 0x0F);
        for i in 0..follow {
            len |= usize::from(self.byte()?) << (4 + 8 * i);
        }
        Ok((start, len))
    }

    /// A PkgLength that bounds the rest of its term: the offset it ends at,
    /// which lies within the enclosing term list (§5.4.1: "it is fatal for a
    /// package length to not fall on a logical boundary").
    pub(crate) fn pkg_end(&mut self) -> Result<usize, Error> {
        let (start, len) = self.pkg_value()?;
        let end = start.checked_add(len).ok_or(Error::Malformed { at: start, why: "a PkgLength overflows" })?;
        if end < self.at || end > self.end {
            return Err(Error::Malformed { at: start, why: "a PkgLength does not end within its enclosing term" });
        }
        Ok(end)
    }

    pub(crate) fn seg(&mut self) -> Result<Seg, Error> {
        let at = self.at;
        let b = [self.byte()?, self.byte()?, self.byte()?, self.byte()?];
        Seg::new(b).ok_or(Error::Malformed { at, why: "a NameSeg holds a character §5.3 does not allow" })
    }

    /// NameString := <RootChar NamePath> | <PrefixPath NamePath> (§20.2.2).
    pub(crate) fn name(&mut self) -> Result<Path, Error> {
        let mut root = false;
        let mut up = 0usize;
        if self.peek()? == b'\\' {
            self.at += 1;
            root = true;
        } else {
            while self.peek()? == b'^' {
                self.at += 1;
                up += 1;
            }
        }
        let count = match self.peek()? {
            0x00 => {
                self.at += 1;
                0
            }
            0x2E => {
                self.at += 1;
                2
            }
            0x2F => {
                self.at += 1;
                // §20.2.2: SegCount can be from 1 to 255.
                match self.byte()? {
                    0 => return Err(self.malformed("a MultiNamePath counts zero segments")),
                    n => usize::from(n),
                }
            }
            _ => 1,
        };
        let mut segs = Vec::with_capacity(count);
        for _ in 0..count {
            segs.push(self.seg()?);
        }
        Ok(Path { root, up, segs })
    }
}

/// The first byte of a NameString (§20.2.2), as opposed to an opcode.
pub(crate) fn starts_name(b: u8) -> bool {
    matches!(b, b'A'..=b'Z' | b'_' | b'\\' | b'^' | 0x2E | 0x2F)
}
