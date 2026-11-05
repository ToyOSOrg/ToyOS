//! The function a recorded offset falls in, out of the file the record names,
//! marked when no build-id vouches for it. Panic-free on any bytes: the file
//! is whatever the record names.

use toyos::elf::{note, section, sym::SymTab, FileHeader};
use toyos_symbols::demangled;
use toyos_symbols::frame::BuildId;

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;

/// Why a recorded frame names no function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unnamed {
    /// The file is no ELF image `toyos::elf` reads.
    NotElf(toyos::elf::Error),
    /// The file's build-id is not the one the record carries: it is another
    /// build of that name, or one of the two has none.
    OtherBuild { file: Option<BuildId> },
    /// The file has no symbol table.
    NoSymbols,
    /// No function in the file's symbol table holds the offset.
    NoSymbol,
}

/// Whether a name is known to be of the build that ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Check {
    /// The file carries the build-id the record does.
    BuildId,
    /// Neither carries one, so nothing tells this build from another build of
    /// that path: the name is printed, and printed as unchecked.
    Unchecked,
}

/// A recorded offset named: the function it falls in, how far into it, and
/// what vouches that the file is the build that ran.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Named<'a> {
    pub function: &'a str,
    pub within: u64,
    pub check: Check,
}

/// `<demangled>+<within>`, and ` (unchecked: no build-id)` after a name
/// [`Check::Unchecked`].
impl core::fmt::Display for Named<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}+{:#x}", demangled(self.function), self.within)?;
        match self.check {
            Check::BuildId => Ok(()),
            Check::Unchecked => f.write_str(" (unchecked: no build-id)"),
        }
    }
}

/// The function `offset` falls in, and how far into it, out of `file`'s own
/// symbol table — when `file` is the build `id` names (`None`: a file with no
/// build-id).
pub fn name<'a>(file: &'a [u8], offset: u64, id: Option<&BuildId>) -> Result<Named<'a>, Unnamed> {
    FileHeader::parse(file).map_err(Unnamed::NotElf)?;
    let carried = note::build_id_in(file).and_then(BuildId::new);
    if carried.as_ref() != id {
        return Err(Unnamed::OtherBuild { file: carried });
    }
    let check = if carried.is_some() { Check::BuildId } else { Check::Unchecked };
    let (symtab, strtab) = section::symbol_tables(file).ok_or(Unnamed::NoSymbols)?;
    let (function, within) = SymTab::new(symtab, strtab).resolve(offset).ok_or(Unnamed::NoSymbol)?;
    Ok(Named { function, within, check })
}
