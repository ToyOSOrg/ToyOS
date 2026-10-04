//! Names (§5.3, §20.2.2): a 32-bit segment, and a path of them that is
//! absolute or relative to a scope by some number of parent prefixes.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::Error;

/// One NameSeg: `'A'-'Z'` or `'_'`, then three of `'A'-'Z'`, `'0'-'9'`, `'_'`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Seg(pub(crate) [u8; 4]);

impl Seg {
    pub(crate) fn new(b: [u8; 4]) -> Option<Seg> {
        let lead = matches!(b[0], b'A'..=b'Z' | b'_');
        let rest = b[1..].iter().all(|c| matches!(c, b'A'..=b'Z' | b'0'..=b'9' | b'_'));
        (lead && rest).then_some(Seg(b))
    }
}

impl fmt::Display for Seg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for &c in &self.0 {
            write!(f, "{}", char::from(c))?;
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Path {
    pub(crate) root: bool,
    /// Parent prefixes (`^`), each one scope up from the current one.
    pub(crate) up: usize,
    pub(crate) segs: Vec<Seg>,
}

impl Path {
    /// §5.3: the search toward the root applies to a lone NameSeg alone.
    pub(crate) fn searches(&self) -> bool {
        !self.root && self.up == 0 && self.segs.len() == 1
    }

    /// An absolute path written as text, `\_SB.PCI0._STA`; a segment shorter
    /// than four characters is padded with `_`, as §5.3 says compilers pad.
    pub(crate) fn absolute(text: &str) -> Result<Path, Error> {
        let rest = text.strip_prefix('\\').ok_or(Error::Rule("a path the caller names is not absolute"))?;
        let mut segs = Vec::new();
        if !rest.is_empty() {
            for part in rest.split('.') {
                let b = part.as_bytes();
                if b.is_empty() || b.len() > 4 {
                    return Err(Error::Rule("a segment of the caller's path is not one to four characters"));
                }
                let mut seg = [b'_'; 4];
                seg[..b.len()].copy_from_slice(b);
                segs.push(Seg::new(seg).ok_or(Error::Rule("a segment of the caller's path is not a NameSeg"))?);
            }
        }
        Ok(Path { root: true, up: 0, segs })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.root {
            f.write_str("\\")?;
        }
        for _ in 0..self.up {
            f.write_str("^")?;
        }
        let mut first = true;
        for s in &self.segs {
            if !first {
                f.write_str(".")?;
            }
            first = false;
            write!(f, "{s}")?;
        }
        Ok(())
    }
}

pub(crate) fn text(p: &Path) -> String {
    alloc::format!("{p}")
}
