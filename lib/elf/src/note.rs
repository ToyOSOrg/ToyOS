//! `PT_NOTE`, for the one note ToyOS reads: the build-id a linker stamps into an
//! image, which is what lets a reader of a crash record say the file it opened
//! is the file that crashed.
//!
//! The layout is the gABI's note section: a 12-byte header (`namesz`, `descsz`,
//! `type`), the name, the descriptor, each padded to the segment's alignment —
//! 4, or 8 for a segment that declares it. Nothing here refuses a file: a note
//! that cannot be read names no build-id.

use crate::header::{FileHeader, ProgramHeader, PROGRAM_HEADER_SIZE, PT_NOTE};
use crate::read;

/// `n_type` of a GNU build-id note.
pub const NT_GNU_BUILD_ID: u32 = 3;

/// The longest build-id this crate answers. lld's is 20
/// bytes, a `uuid` or `md5` one 16; past this a descriptor is no build-id any
/// linker writes.
pub const MAX_BUILD_ID: usize = 32;

/// One `PT_NOTE`: where its bytes are in the file, and the alignment its notes
/// are padded to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoteSegment {
    pub offset: u64,
    pub filesz: u64,
    pub align: u64,
}

/// Every `PT_NOTE` in the program header table `data` holds, in table order;
/// none when `data` holds no readable table.
pub fn segments(data: &[u8]) -> impl Iterator<Item = NoteSegment> + '_ {
    let table = FileHeader::parse(data).and_then(|h| h.program_headers(data)).unwrap_or(&[]);
    (0..table.len() / PROGRAM_HEADER_SIZE)
        .filter_map(move |i| ProgramHeader::parse(table, i))
        .filter(|p| p.kind == PT_NOTE)
        .map(|p| NoteSegment { offset: p.offset, filesz: p.filesz, align: p.align })
}

/// The descriptor of the first `NT_GNU_BUILD_ID` note named `"GNU"` in `notes`,
/// a `PT_NOTE`'s bytes padded to `align`, when it is 1 to [`MAX_BUILD_ID`]
/// bytes long.
pub fn build_id(notes: &[u8], align: u64) -> Option<&[u8]> {
    let align = match align {
        0..=4 => 4,
        8 => 8,
        _ => return None,
    };
    let mut rest = notes;
    loop {
        let namesz = read::u32_at(rest, 0)? as usize;
        let descsz = read::u32_at(rest, 4)? as usize;
        let kind = read::u32_at(rest, 8)?;
        let name_end = 12usize.checked_add(namesz)?;
        let name = rest.get(12..name_end)?;
        let desc_start = align_up(name_end, align)?;
        let desc_end = desc_start.checked_add(descsz)?;
        let desc = rest.get(desc_start..desc_end)?;
        if kind == NT_GNU_BUILD_ID && name == b"GNU\0" {
            return (1..=MAX_BUILD_ID).contains(&desc.len()).then_some(desc);
        }
        rest = rest.get(align_up(desc_end, align)?..)?;
    }
}

fn align_up(at: usize, align: usize) -> Option<usize> {
    Some(at.checked_add(align - 1)? & !(align - 1))
}
