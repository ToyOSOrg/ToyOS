//! The Display Micro-Controller's firmware package, and loading its main
//! program, from Vol 12 "DMC Firmware Package" and "Sequences for Display C5
//! and C6".
//!
//! A package is a CSS header, a package header mapping steppings to programs,
//! and the programs, each a DMC header and its payload. Loading is MMIO in the
//! display engine's own registers: the payload is copied into the CSR program
//! storage at the header's start address, then the header's writes are made.
//! The package is pinned and parsed whole on the host, so [`Package::parse`]
//! failing is a bug in this code or a wrong file, not a machine's state.

use toyos_device_memory::Registers;

/// Intel's Tiger Lake DMC package, `i915/tgl_dmc_ver2_12.bin` from
/// linux-firmware, unmodified and pinned in `NOTICE`.
pub const TGL_PACKAGE: &[u8] = include_bytes!("../../firmware/tgl_dmc_ver2_12.bin");

/// The main DMC's MMIO window, 80000h-8FFFFh: Vol 12 bounds a program's
/// header writes to it, and the program storage holding its payload is held
/// to it too.
pub const WINDOW: core::ops::Range<usize> = 0x8_0000..0x9_0000;

/// CSS header 1.0, package header 2 and DMC header 3, each checked at its
/// header.
const CSS_MODULE_DMC: u32 = 0x09;
const CSS_HEADER_VERSION: u32 = 0x1_0000;
const CSS_VERSION_AT: usize = 0x58;
const PACKAGE_HEADER_VERSION: u8 = 0x02;
const PACKAGE_ENTRIES: usize = 32;
const PACKAGE_ENTRY_BYTES: usize = 12;
const NO_PROGRAM: u32 = 0xFFFF_FFFF;
const DMC_SIGNATURE: u32 = 0x4040_3E3E;
const DMC_HEADER_VERSION: u8 = 0x03;
/// `_DMCHeader` version 3 through `mmiodata[20]`.
const DMC_HEADER_BYTES: usize = 256;
const MMIO_COUNT_AT: usize = 0x5C;
const MMIO_WRITES: usize = 20;
/// The main DMC's ID in a package entry; pipe DMCs are 1-4.
const MAIN: u8 = 0;

/// A display stepping as a package entry names it: a letter and a digit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stepping {
    pub letter: u8,
    pub digit: u8,
}

impl Stepping {
    /// The display stepping of a `8086:9a49` function at PCI revision
    /// `revision`: Vol 4 "Steppings and Device IDs" gives RevID 1h as display
    /// C0 and 3h as D0.
    pub fn of_9a49(revision: u8) -> Option<Self> {
        match revision {
            0x1 => Some(Self { letter: b'C', digit: b'0' }),
            0x3 => Some(Self { letter: b'D', digit: b'0' }),
            _ => None,
        }
    }
}

/// Why a package is not one this code reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// The CSS header is short, not a DMC module's, not version 1.0, or does
    /// not size the file.
    Css,
    /// The package header is not version 2, or its entries run off it.
    PackageHeader,
    /// An entry's program runs off the file, has no DMC header 3 signature,
    /// or names more writes than the header has.
    Program { entry: usize },
    /// A program's storage or one of its writes is outside [`WINDOW`], or not
    /// dword aligned.
    OutsideWindow { entry: usize, at: u32 },
}

/// A parsed package.
pub struct Package<'a> {
    file: &'a [u8],
    /// The package's version, major in 31:16 and minor in 15:0.
    pub version: u32,
    entries: [Option<Entry>; PACKAGE_ENTRIES],
    programs_at: usize,
}

#[derive(Clone, Copy)]
struct Entry {
    dmc: u8,
    stepping: u8,
    substepping: u8,
    /// Dwords past the package header; [`NO_PROGRAM`] for none.
    offset: u32,
}

/// One DMC program, whole and inside [`WINDOW`].
#[derive(Clone, Copy, Debug)]
pub struct Program<'a> {
    /// The program's version, major in 31:16 and minor in 15:0.
    pub version: u32,
    /// Where its payload is stored.
    pub start: usize,
    payload: &'a [u8],
    header: &'a [u8],
    writes: usize,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

impl<'a> Package<'a> {
    /// The package's headers, and every program its entries name, checked.
    pub fn parse(file: &'a [u8]) -> Result<Self, Malformed> {
        let css = |at| u32_at(file, at).ok_or(Malformed::Css);
        let css_bytes = css(0x04)? as usize * 4;
        if css(0x00)? != CSS_MODULE_DMC || css(0x08)? != CSS_HEADER_VERSION || css(0x18)? as usize * 4 != file.len() {
            return Err(Malformed::Css);
        }
        let version = css(CSS_VERSION_AT)?;
        if css_bytes <= CSS_VERSION_AT {
            return Err(Malformed::Css);
        }
        let header = file.get(css_bytes..).ok_or(Malformed::Css)?;
        let (Some(&length), Some(&header_version)) = (header.first(), header.get(1)) else {
            return Err(Malformed::PackageHeader);
        };
        let header_bytes = usize::from(length) * 4;
        let count = u32_at(header, 12).ok_or(Malformed::PackageHeader)? as usize;
        if header_version != PACKAGE_HEADER_VERSION
            || count > PACKAGE_ENTRIES
            || header_bytes < 16 + PACKAGE_ENTRIES * PACKAGE_ENTRY_BYTES
            || header.len() < header_bytes
        {
            return Err(Malformed::PackageHeader);
        }
        let mut entries = [None; PACKAGE_ENTRIES];
        for (n, slot) in entries.iter_mut().enumerate().take(count) {
            let at = 16 + n * PACKAGE_ENTRY_BYTES;
            let offset = u32_at(header, at + 4).ok_or(Malformed::PackageHeader)?;
            *slot = Some(Entry { dmc: header[at + 1], stepping: header[at + 2], substepping: header[at + 3], offset });
        }
        let package = Self { file, version, entries, programs_at: css_bytes + header_bytes };
        for (entry, found) in entries.iter().enumerate() {
            if let Some(found) = found {
                package.program(entry, found)?;
            }
        }
        Ok(package)
    }

    fn program(&self, entry: usize, found: &Entry) -> Result<Option<Program<'a>>, Malformed> {
        if found.offset == NO_PROGRAM {
            return Ok(None);
        }
        let malformed = Malformed::Program { entry };
        let start = (found.offset as usize).checked_mul(4).and_then(|at| at.checked_add(self.programs_at)).ok_or(malformed)?;
        let header = self.file.get(start..).ok_or(malformed)?;
        let word = |at| u32_at(header, at).ok_or(malformed);
        let header_bytes = usize::from(*header.get(4).ok_or(malformed)?) * 4;
        if word(0)? != DMC_SIGNATURE || header.get(5) != Some(&DMC_HEADER_VERSION) || header_bytes < DMC_HEADER_BYTES {
            return Err(malformed);
        }
        let payload_bytes = (word(0x0C)? as usize).checked_mul(4).ok_or(malformed)?;
        let payload = header.get(header_bytes..header_bytes.checked_add(payload_bytes).ok_or(malformed)?).ok_or(malformed)?;
        let writes = word(MMIO_COUNT_AT)? as usize;
        if writes > MMIO_WRITES {
            return Err(malformed);
        }
        let storage = word(0x14)?;
        let program = Program { version: word(0x10)?, start: storage as usize, payload, header: &header[..header_bytes], writes };
        // Only the main DMC's window is Vol 12's; a pipe DMC's is past it.
        if found.dmc == MAIN {
            let outside = |at: usize, bytes: usize| !at.is_multiple_of(4) || at < WINDOW.start || at + bytes > WINDOW.end;
            if outside(program.start, payload_bytes) {
                return Err(Malformed::OutsideWindow { entry, at: storage });
            }
            if let Some((at, _)) = program.writes().find(|(at, _)| outside(*at, 4)) {
                return Err(Malformed::OutsideWindow { entry, at: at as u32 });
            }
        }
        Ok(Some(program))
    }

    /// The main DMC's program for `stepping`: the first entry that names it,
    /// exactly or by the `*` wildcard, since Vol 12 lists the exceptions
    /// before the defaults. `None` where that entry says there is none, or no
    /// entry names it.
    pub fn main_program(&self, stepping: Stepping) -> Option<Program<'a>> {
        let names = |field: u8, value: u8| field == value || field == b'*';
        let (entry, found) = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(n, e)| Some((n, e.as_ref()?)))
            .find(|(_, e)| e.dmc == MAIN && names(e.stepping, stepping.letter) && names(e.substepping, stepping.digit))?;
        self.program(entry, found).expect("parse checked every entry's program")
    }
}

impl Program<'_> {
    /// The payload's dwords, in storage order.
    pub fn words(&self) -> impl Iterator<Item = u32> + '_ {
        self.payload.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w))
    }

    /// The header's writes, address then data, in order.
    pub fn writes(&self) -> impl Iterator<Item = (usize, u32)> + '_ {
        let word = |at| u32_at(self.header, at).expect("parse checked the header's length");
        (0..self.writes).map(move |n| (word(MMIO_COUNT_AT + 4 + 4 * n) as usize, word(MMIO_COUNT_AT + 84 + 4 * n)))
    }
}

/// The program stored and its header's writes made (Vol 12 "Sequence to Allow
/// DC5 or DC6" steps 1b-1d). The storage is read back: the first word that
/// does not hold is returned, as its address, what was written and what reads.
pub fn load<R: Registers>(regs: &R, program: &Program<'_>) -> Result<(), (usize, u32, u32)> {
    for (n, word) in program.words().enumerate() {
        regs.write32(program.start + 4 * n, word);
    }
    if let Some(differs) = program
        .words()
        .enumerate()
        .map(|(n, word)| (program.start + 4 * n, word, regs.read32(program.start + 4 * n)))
        .find(|(_, wrote, read)| wrote != read)
    {
        return Err(differs);
    }
    for (at, data) in program.writes() {
        regs.write32(at, data);
    }
    Ok(())
}
