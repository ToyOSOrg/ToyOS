//! The section header table, as a view over bytes.
//!
//! Sections are not needed to run a program — they are how the kernel finds
//! `.symtab` for a backtrace, and `.rela.dyn` in a file with no `PT_DYNAMIC`.
//! Nothing here refuses a file; a table that cannot be read simply names
//! nothing.

use core::fmt;

use crate::header::SECTION_HEADER_SIZE;
use crate::read;

pub const SHT_SYMTAB: u32 = 2;
pub const SHT_RELA: u32 = 4;
pub const SHT_DYNSYM: u32 = 11;
/// Relocations whose addend lives in the destination word.
pub const SHT_REL: u32 = 9;
/// Relative relocations packed as a bitmap (`-z pack-relative-relocs`).
pub const SHT_RELR: u32 = 19;

/// A relocation form a loader that applies `SHT_RELA` alone would leave
/// unapplied, so an image started with it runs with every pointer it covers
/// still its link-time one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unapplied {
    Rel,
    Relr,
}

impl fmt::Display for Unapplied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Rel => "SHT_REL",
            Self::Relr => "SHT_RELR (packed relative relocations)",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionHeader {
    pub name: u32,
    pub kind: u32,
    pub flags: u64,
    pub addr: u64,
    pub offset: u64,
    pub size: u64,
    /// `sh_link`: for `SHT_SYMTAB` and `SHT_DYNSYM`, the string table's index.
    pub link: u32,
    pub entry_size: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct SectionTable<'a> {
    data: &'a [u8],
}

impl<'a> SectionTable<'a> {
    pub const fn new(data: &'a [u8]) -> SectionTable<'a> {
        SectionTable { data }
    }

    /// Whole entries the bytes hold. A short read of the table leaves the
    /// entries it did cover usable, which is what the loader wants: a
    /// truncated table names fewer sections, not none.
    pub const fn len(&self) -> usize {
        self.data.len() / SECTION_HEADER_SIZE
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, i: usize) -> Option<SectionHeader> {
        if i >= self.len() {
            return None;
        }
        let off = i * SECTION_HEADER_SIZE;
        Some(SectionHeader {
            name: read::u32_at(self.data, off)?,
            kind: read::u32_at(self.data, off + 4)?,
            flags: read::u64_at(self.data, off + 8)?,
            addr: read::u64_at(self.data, off + 16)?,
            offset: read::u64_at(self.data, off + 24)?,
            size: read::u64_at(self.data, off + 32)?,
            link: read::u32_at(self.data, off + 40)?,
            entry_size: read::u64_at(self.data, off + 56)?,
        })
    }

    pub fn iter(self) -> impl Iterator<Item = SectionHeader> + 'a {
        (0..self.len()).filter_map(move |i| self.get(i))
    }

    /// The first section of this type.
    pub fn find(self, kind: u32) -> Option<SectionHeader> {
        self.iter().find(|sh| sh.kind == kind)
    }

    /// A symbol table and the string table it points at, as (symbols,
    /// strings) file extents.
    ///
    /// `None` when there is no such section or its `sh_link` names no section
    /// in this table — a symbol table whose names are unreachable resolves
    /// every name to `""`, which is worse than having no map at all.
    pub fn symbols(self, kind: u32) -> Option<(SectionHeader, SectionHeader)> {
        let syms = self.find(kind)?;
        let strs = self.get(syms.link as usize)?;
        Some((syms, strs))
    }

    /// Every `SHT_RELA` section, for a loader that applies those and nothing
    /// else: the only way to them, so no such loader can start an image whose
    /// other relocations it never read. Refused when the table also names one
    /// of another form.
    pub fn rela_sections(self) -> Result<impl Iterator<Item = SectionHeader> + 'a, Unapplied> {
        for sh in self.iter() {
            match sh.kind {
                SHT_REL => return Err(Unapplied::Rel),
                SHT_RELR => return Err(Unapplied::Relr),
                _ => {}
            }
        }
        Ok(self.iter().filter(|sh| sh.kind == SHT_RELA))
    }
}
