//! A program applies its own relocations, first thing in its `_start`.
//!
//! The kernel maps an executable's `PT_LOAD`s and jumps to it; it reads none
//! of its dynamic section. std's and libc's `_start` call [`relocate_self`]
//! with the address of the image's own ELF header (`__ehdr_start`, found
//! PC-relatively) before anything else runs.
//!
//! **Nothing here reads a relocated word, because nothing is relocated yet**:
//! no static holding a pointer, no `&dyn`, no formatting, and no panic, whose
//! message and `Location` are pointers. The lints below hold the panic half;
//! the rest is the shape of the code, a core over byte slices and integers
//! answering integers, and one `unsafe` wrapper that hands it the image's own
//! memory.
//!
//! A static PIE carries `RELATIVE` relocations and nothing else, so that is
//! all this applies. Every other form is refused by name: a library this
//! process would have to load (`DT_NEEDED`), a symbol it would have to bind
//! (`DT_JMPREL` with entries, any other type), a table form it does not read
//! (`DT_REL`, `DT_RELR`), and a write into text (`DT_TEXTREL`, `DF_TEXTREL`).
//! A write lands only inside a writable `PT_LOAD` and never in the `PT_TLS`
//! template, which the kernel copies from the file into every thread's block
//! and which a write here would never reach.

#![deny(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects
)]

#[cfg(test)]
mod tests;

use toyos_abi::RawHandle;

/// Why a program's relocations are refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The header is no little-endian ELF64 of a machine this knows, or its
    /// program headers are not 56 bytes each.
    Header,
    /// No non-writable `PT_LOAD` at file offset 0 holds the header and the
    /// program header table in its file bytes.
    HeaderSegment,
    /// `PT_DYNAMIC` lies in no `PT_LOAD`'s file bytes.
    Dynamic,
    /// `DT_NEEDED`: a library this process would have to load.
    Needed,
    /// `DT_JMPREL` with entries: slots this process would have to bind.
    JmpRel,
    /// `DT_REL`: a table form this does not read.
    Rel,
    /// `DT_RELR`: a table form this does not read.
    Relr,
    /// `DT_TEXTREL` or `DF_TEXTREL`: a write into text.
    TextRel,
    /// `DT_RELA` without its size, a size that is no whole number of entries,
    /// or an entry size other than 24.
    RelaShape,
    /// The `DT_RELA` table is outside every `PT_LOAD`'s file bytes, or a
    /// writable segment overlaps it.
    RelaPlacement,
    /// A relocation of a type other than this machine's `RELATIVE`.
    Kind(u32),
    /// A write outside every writable `PT_LOAD`.
    OutsideWritable,
    /// A write into the `PT_TLS` template.
    InTlsTemplate,
}

/// What a program whose relocations are refused writes to slot 2.
const REFUSED_LINE: &[u8] = b"this program's relocations are refused: it cannot start\n";

/// The status a program whose relocations are refused exits with.
pub const REFUSED_STATUS: i32 = 127;

/// Apply the running image's own relocations, or end the process with
/// [`REFUSED_STATUS`].
///
/// # Safety
/// Called once, first thing in `_start`, before anything reads a relocated
/// word, with `header` the address `__ehdr_start` names: the running image's
/// own ELF header, which the `PT_LOAD` at file offset 0 maps.
pub unsafe extern "C" fn relocate_self(header: *const u8) {
    // SAFETY: the caller's contract.
    if unsafe { apply_to_self(header) }.is_err() {
        // A literal in place: there is no table of names, whose entries would be relocated words.
        let _ = toyos_abi::syscall::write_nonblock(RawHandle(2), REFUSED_LINE);
        toyos_abi::syscall::exit(REFUSED_STATUS);
    }
}

/// # Safety
/// [`relocate_self`]'s.
unsafe fn apply_to_self(header: *const u8) -> Result<(), Refusal> {
    // SAFETY: the caller's contract: the header is mapped, and every ELF64 header is this long.
    let head = unsafe { core::slice::from_raw_parts(header, HEADER_SIZE) };
    let shape = Header::parse(head)?;
    // The image's other bytes are reached by address: `__ehdr_start` gives the
    // header's provenance alone.
    let at = |vaddr: u64, bias: u64| bias.wrapping_add(vaddr) as usize;
    // SAFETY: the kernel starts no image whose program header table no
    // `PT_LOAD` maps, and lld writes it in the header's own segment, right
    // behind it. An image that put it elsewhere faults on this read in its own
    // address space, or `Image::new` refuses it before any other use.
    let phdrs = unsafe {
        core::slice::from_raw_parts(
            core::ptr::with_exposed_provenance::<u8>(header.addr().wrapping_add(shape.phoff)),
            shape.phlen,
        )
    };
    let image = Image::new(shape, phdrs)?;
    let bias = (header.addr() as u64).wrapping_sub(image.header_vaddr);
    let Some(dynamic) = image.dynamic()? else { return Ok(()) };
    let rela = {
        // SAFETY: inside a `PT_LOAD`'s file bytes, which the kernel mapped at
        // `bias + vaddr`; read whole before the first write below.
        let dynamic = unsafe {
            core::slice::from_raw_parts(core::ptr::with_exposed_provenance::<u8>(at(dynamic.vaddr, bias)), dynamic.len)
        };
        image.rela(dynamic)?
    };
    let Some(rela) = rela else { return Ok(()) };
    // SAFETY: inside a `PT_LOAD`'s file bytes that no writable segment
    // overlaps, so no write below reaches it.
    let rela = unsafe {
        core::slice::from_raw_parts(core::ptr::with_exposed_provenance::<u8>(at(rela.vaddr, bias)), rela.len)
    };
    image.apply(rela, bias, |vaddr, value| {
        let slot = core::ptr::with_exposed_provenance_mut::<u64>(at(vaddr, bias));
        // SAFETY: `apply` names only a vaddr whose 8 bytes lie in a writable
        // `PT_LOAD`, which the kernel mapped writable at `bias + vaddr`, and
        // outside every table this reads.
        unsafe { slot.write_unaligned(value) }
    })
}

const HEADER_SIZE: usize = 64;
const PHDR_SIZE: usize = 56;
const DYN_SIZE: usize = 16;
const RELA_SIZE: usize = 24;

const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;
const R_X86_64_RELATIVE: u32 = 8;
const R_AARCH64_RELATIVE: u32 = 1027;

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_TLS: u32 = 7;
const PF_W: u32 = 2;

const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_PLTRELSZ: u64 = 2;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;
const DT_REL: u64 = 17;
const DT_TEXTREL: u64 = 22;
const DT_FLAGS: u64 = 30;
const DT_RELR: u64 = 36;
const DF_TEXTREL: u64 = 4;

/// A run of the image's bytes, at its link-time address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    vaddr: u64,
    len: usize,
}

/// What the ELF header says of the program header table and the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    phoff: usize,
    phlen: usize,
    relative: u32,
}

impl Header {
    fn parse(head: &[u8]) -> Result<Header, Refusal> {
        let ident = head.get(..6).ok_or(Refusal::Header)?;
        if ident != [0x7f, b'E', b'L', b'F', 2, 1] {
            return Err(Refusal::Header);
        }
        let relative = match u16_at(head, 18).ok_or(Refusal::Header)? {
            EM_X86_64 => R_X86_64_RELATIVE,
            EM_AARCH64 => R_AARCH64_RELATIVE,
            _ => return Err(Refusal::Header),
        };
        if usize::from(u16_at(head, 54).ok_or(Refusal::Header)?) != PHDR_SIZE {
            return Err(Refusal::Header);
        }
        let phoff = usize::try_from(u64_at(head, 32).ok_or(Refusal::Header)?).map_err(|_| Refusal::Header)?;
        let phnum = usize::from(u16_at(head, 56).ok_or(Refusal::Header)?);
        let phlen = phnum.checked_mul(PHDR_SIZE).ok_or(Refusal::Header)?;
        Ok(Header { phoff, phlen, relative })
    }
}

/// One program header, the fields this reads.
#[derive(Clone, Copy)]
struct Phdr {
    kind: u32,
    flags: u32,
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
}

impl Phdr {
    fn parse(entry: &[u8]) -> Option<Phdr> {
        Some(Phdr {
            kind: u32_at(entry, 0)?,
            flags: u32_at(entry, 4)?,
            offset: u64_at(entry, 8)?,
            vaddr: u64_at(entry, 16)?,
            filesz: u64_at(entry, 32)?,
            memsz: u64_at(entry, 40)?,
        })
    }

    fn writable_load(&self) -> bool {
        self.kind == PT_LOAD && self.flags & PF_W != 0
    }

    /// `[vaddr, vaddr + len)` lies in this segment's first `size` bytes.
    fn holds(&self, vaddr: u64, len: u64, size: u64) -> bool {
        let (Some(end), Some(own_end)) = (vaddr.checked_add(len), self.vaddr.checked_add(size)) else {
            return false;
        };
        self.vaddr <= vaddr && end <= own_end
    }

    /// `[vaddr, vaddr + len)` meets this segment's first `size` bytes.
    fn meets(&self, vaddr: u64, len: u64, size: u64) -> bool {
        vaddr < self.vaddr.saturating_add(size) && self.vaddr < vaddr.saturating_add(len)
    }
}

/// The running image as its program headers describe it.
struct Image<'a> {
    phdrs: &'a [u8],
    relative: u32,
    /// Where the header's own segment puts file offset 0.
    header_vaddr: u64,
}

impl<'a> Image<'a> {
    fn new(header: Header, phdrs: &'a [u8]) -> Result<Image<'a>, Refusal> {
        let mut image = Image { phdrs, relative: header.relative, header_vaddr: 0 };
        let table_end = u64::try_from(header.phoff.checked_add(header.phlen).ok_or(Refusal::HeaderSegment)?)
            .map_err(|_| Refusal::HeaderSegment)?;
        let first = image.loads().find(|p| p.offset == 0).ok_or(Refusal::HeaderSegment)?;
        if first.filesz < table_end.max(HEADER_SIZE as u64) {
            return Err(Refusal::HeaderSegment);
        }
        if image.meets_writable(first.vaddr, table_end) {
            return Err(Refusal::HeaderSegment);
        }
        image.header_vaddr = first.vaddr;
        Ok(image)
    }

    fn headers(&self) -> impl Iterator<Item = Phdr> + 'a {
        self.phdrs.chunks_exact(PHDR_SIZE).filter_map(Phdr::parse)
    }

    fn loads(&self) -> impl Iterator<Item = Phdr> + 'a {
        self.headers().filter(|p| p.kind == PT_LOAD)
    }

    /// Some `PT_LOAD`'s file bytes hold `[vaddr, vaddr + len)`.
    fn in_file_bytes(&self, vaddr: u64, len: u64) -> bool {
        self.loads().any(|p| p.holds(vaddr, len, p.filesz))
    }

    fn meets_writable(&self, vaddr: u64, len: u64) -> bool {
        self.headers().any(|p| p.writable_load() && p.meets(vaddr, len, p.memsz))
    }

    /// `PT_DYNAMIC`'s bytes, or `None` for an image with nothing to apply.
    fn dynamic(&self) -> Result<Option<Span>, Refusal> {
        let Some(dynamic) = self.headers().find(|p| p.kind == PT_DYNAMIC) else { return Ok(None) };
        if !self.in_file_bytes(dynamic.vaddr, dynamic.filesz) {
            return Err(Refusal::Dynamic);
        }
        let len = usize::try_from(dynamic.filesz).map_err(|_| Refusal::Dynamic)?;
        Ok(Some(Span { vaddr: dynamic.vaddr, len }))
    }

    /// The `DT_RELA` table `dynamic` names, or `None` when it names none.
    fn rela(&self, dynamic: &[u8]) -> Result<Option<Span>, Refusal> {
        let (mut rela, mut size, mut plt) = (None, None, 0);
        for entry in dynamic.chunks_exact(DYN_SIZE) {
            let (Some(tag), Some(value)) = (u64_at(entry, 0), u64_at(entry, 8)) else { break };
            match tag {
                DT_NULL => break,
                DT_NEEDED => return Err(Refusal::Needed),
                DT_PLTRELSZ => plt = value,
                DT_RELA => rela = Some(value),
                DT_RELASZ => size = Some(value),
                DT_RELAENT if value != RELA_SIZE as u64 => return Err(Refusal::RelaShape),
                DT_REL => return Err(Refusal::Rel),
                DT_RELR => return Err(Refusal::Relr),
                DT_TEXTREL => return Err(Refusal::TextRel),
                DT_FLAGS if value & DF_TEXTREL != 0 => return Err(Refusal::TextRel),
                _ => {}
            }
        }
        if plt != 0 {
            return Err(Refusal::JmpRel);
        }
        let (vaddr, size) = match (rela, size) {
            (None, None) => return Ok(None),
            (Some(vaddr), Some(size)) if size.is_multiple_of(RELA_SIZE as u64) => (vaddr, size),
            _ => return Err(Refusal::RelaShape),
        };
        if !self.in_file_bytes(vaddr, size) || self.meets_writable(vaddr, size) {
            return Err(Refusal::RelaPlacement);
        }
        let len = usize::try_from(size).map_err(|_| Refusal::RelaPlacement)?;
        Ok(Some(Span { vaddr, len }))
    }

    /// Write `bias + addend` at every entry of `rela` through `write`, which
    /// is handed the entry's link-time address; the first refusal stops it.
    fn apply(&self, rela: &[u8], bias: u64, mut write: impl FnMut(u64, u64)) -> Result<(), Refusal> {
        for entry in rela.chunks_exact(RELA_SIZE) {
            let (Some(offset), Some(info), Some(addend)) = (u64_at(entry, 0), u64_at(entry, 8), u64_at(entry, 16))
            else {
                return Err(Refusal::RelaShape);
            };
            // `r_info`'s low half is the type: the mask keeps it.
            let kind = (info & 0xffff_ffff) as u32;
            if kind != self.relative {
                return Err(Refusal::Kind(kind));
            }
            if !self.loads().any(|p| p.writable_load() && p.holds(offset, 8, p.memsz)) {
                return Err(Refusal::OutsideWritable);
            }
            if self.headers().any(|p| p.kind == PT_TLS && p.meets(offset, 8, p.filesz)) {
                return Err(Refusal::InTlsTemplate);
            }
            write(offset, bias.wrapping_add(addend));
        }
        Ok(())
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at.checked_add(8)?)?.try_into().ok()?))
}
