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

    /// A path written as ASL text, `\_SB.PCI0._STA` or `^ABC`: a segment
    /// shorter than four characters is padded with `_`, as §5.3 says
    /// compilers pad. DerefOf of a String reads one (§19.6.30), and so does a
    /// caller naming an object.
    pub(crate) fn text(s: &[u8]) -> Option<Path> {
        let mut rest = s;
        let mut root = false;
        let mut up = 0;
        if let [b'\\', r @ ..] = rest {
            root = true;
            rest = r;
        }
        while let [b'^', r @ ..] = rest {
            up += 1;
            rest = r;
        }
        let mut segs = Vec::new();
        if !rest.is_empty() {
            for part in rest.split(|&c| c == b'.') {
                if part.is_empty() || part.len() > 4 {
                    return None;
                }
                let mut seg = [b'_'; 4];
                seg[..part.len()].copy_from_slice(part);
                segs.push(Seg::new(seg)?);
            }
        }
        Some(Path { root, up, segs })
    }

    /// An absolute path written as text, as the caller names an object.
    pub(crate) fn absolute(text: &str) -> Result<Path, Error> {
        Path::text(text.as_bytes())
            .filter(|p| p.root)
            .ok_or(Error::Rule("a path the caller names is not an absolute path of NameSegs"))
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
