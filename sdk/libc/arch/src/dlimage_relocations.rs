//! The loader's image work on a shared object laid out here: what each
//! relocation it writes is worth, which ones it refuses, and the images it
//! refuses before any is laid out. The image is the host's machine's, so each
//! host checks its own architecture's relocation numbers.

use crate::dlimage::{self, Mapped, Pass, Scope};
use toyos_elf::RelocError;

const SIZE: usize = 0x2000;
const PHDRS: usize = 0x40;
const SYMTAB: usize = 0x200;
const STRTAB: usize = 0x400;
const RELA: usize = 0x600;
const DYNAMIC: usize = 0x800;
/// The writable segment, `[0x1000, 0x2000)` in memory, its first 0x100 bytes file bytes.
const DATA: u64 = 0x1000;
const TLS: u64 = 0x1080;

#[cfg(target_arch = "x86_64")]
mod machine {
    pub const EM: u16 = 62;
    pub const RELATIVE: u32 = 8;
    pub const GLOB_DAT: u32 = 6;
    pub const DTPMOD: u32 = 16;
    pub const DTPOFF: u32 = 17;
    pub const TPOFF: u32 = 18;
}
#[cfg(target_arch = "aarch64")]
mod machine {
    pub const EM: u16 = 183;
    pub const RELATIVE: u32 = 1027;
    pub const GLOB_DAT: u32 = 1025;
    pub const DTPMOD: u32 = 1028;
    pub const DTPOFF: u32 = 1029;
    pub const TPOFF: u32 = 1030;
}
use machine::*;

const GLOBAL_FUNC: u8 = (1 << 4) | 2;
const GLOBAL_TLS: u8 = (1 << 4) | 6;

/// Name, `st_info`, defined, value: 1 an import, 2 this object's
/// thread-local, 3 a function it defines, 4 another's thread-local.
const SYMBOLS: &[(&str, u8, bool, u64)] = &[
    ("", 0, false, 0),
    ("imported", GLOBAL_FUNC, false, 0),
    ("counter", GLOBAL_TLS, true, 8),
    ("exported", GLOBAL_FUNC, true, 0x500),
    ("other_tls", GLOBAL_TLS, false, 0),
];

/// One relocation: where, its type, its symbol and its addend.
type Entry = (u64, u32, u32, i64);

/// A shared object, its file bytes its image: text and tables in a read-only
/// page, a writable page whose start the relocations write and whose
/// `PT_TLS` is 0x20 bytes at 0x1080, and `relocs` in a `DT_RELA` at
/// `rela_at`. `edit` changes the bytes last.
fn object(relocs: &[Entry], rela_at: usize, edit: impl FnOnce(&mut [u8])) -> Vec<u8> {
    let mut bytes = vec![0u8; SIZE];
    let mut put = |at: usize, value: &[u8]| bytes[at..at + value.len()].copy_from_slice(value);
    put(0, &[0x7f, b'E', b'L', b'F', 2, 1, 1]);
    put(16, &3u16.to_le_bytes());
    put(18, &EM.to_le_bytes());
    put(20, &1u32.to_le_bytes());
    put(24, &0x500u64.to_le_bytes());
    put(32, &(PHDRS as u64).to_le_bytes());
    put(52, &64u16.to_le_bytes());
    put(54, &56u16.to_le_bytes());
    put(56, &4u16.to_le_bytes());
    // Type, flags, offset and vaddr, file size, memory size, alignment.
    let phdrs: [(u32, u32, u64, u64, u64, u64); 4] = [
        (1, 5, 0, 0x1000, 0x1000, 0x1000),
        (1, 6, DATA, 0x100, 0x1000, 0x1000),
        (2, 4, DYNAMIC as u64, 0x100, 0x100, 8),
        (7, 4, TLS, 0x10, 0x20, 8),
    ];
    for (i, (kind, flags, at, filesz, memsz, align)) in phdrs.into_iter().enumerate() {
        let p = PHDRS + i * 56;
        put(p, &kind.to_le_bytes());
        put(p + 4, &flags.to_le_bytes());
        for (field, value) in [(8, at), (16, at), (24, at), (32, filesz), (40, memsz), (48, align)] {
            put(p + field, &value.to_le_bytes());
        }
    }
    let mut names = vec![0u8];
    for (i, (name, info, defined, value)) in SYMBOLS.iter().enumerate() {
        let at = SYMTAB + i * 24;
        if !name.is_empty() {
            put(at, &(names.len() as u32).to_le_bytes());
            names.extend_from_slice(name.as_bytes());
            names.push(0);
        }
        put(at + 4, &[*info]);
        put(at + 6, &u16::from(*defined).to_le_bytes());
        put(at + 8, &value.to_le_bytes());
    }
    put(STRTAB, &names);
    for (i, &(offset, kind, sym, addend)) in relocs.iter().enumerate() {
        let at = rela_at + i * 24;
        put(at, &offset.to_le_bytes());
        put(at + 8, &(u64::from(sym) << 32 | u64::from(kind)).to_le_bytes());
        put(at + 16, &addend.to_le_bytes());
    }
    let tags = [
        (6u64, SYMTAB as u64),
        (5, STRTAB as u64),
        (10, names.len() as u64),
        (7, rela_at as u64),
        (8, (relocs.len() * 24) as u64),
        (0, 0),
    ];
    for (i, (tag, value)) in tags.into_iter().enumerate() {
        put(DYNAMIC + i * 16, &tag.to_le_bytes());
        put(DYNAMIC + i * 16 + 8, &value.to_le_bytes());
    }
    edit(&mut bytes);
    bytes
}

/// Defines `imported` and module 7's `other_tls` at 0x10 in its block.
struct Loaded;

impl Scope for Loaded {
    fn address(&self, name: &str) -> Option<u64> {
        (name == "imported").then_some(0xdead_0000)
    }

    fn tls(&self, name: &str, addend: i64) -> Option<Result<(usize, u64), RelocError>> {
        (name == "other_tls").then(|| Ok((7, 0x10u64.checked_add_signed(addend).unwrap())))
    }
}

/// Defines nothing.
struct Nothing;

impl Scope for Nothing {
    fn address(&self, _: &str) -> Option<u64> {
        None
    }

    fn tls(&self, _: &str, _: i64) -> Option<Result<(usize, u64), RelocError>> {
        None
    }
}

/// `file` laid out and relocated as module 3 against `scope`: the image's
/// words from `DATA` and how many symbols nothing defined, and the image's base.
fn load(file: &[u8], scope: &dyn Scope, pass: Pass) -> Result<(Vec<u64>, usize, u64), &'static str> {
    let plan = dlimage::plan(&file[..0x1000])?;
    // Whole words, so the image is 8-aligned as a mapping is page-aligned.
    let mut words = vec![0u64; plan.span / 8];
    // SAFETY: the buffer's own bytes.
    let image = unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), plan.span) };
    plan.lay(file, image)?;
    let base = image.as_ptr() as u64;
    // SAFETY: the buffer, laid out whole and writable, which nothing else reaches.
    let mapped = unsafe { Mapped::new(image.as_mut_ptr(), &plan) };
    let tables = mapped.tables(true)?.expect("a PT_DYNAMIC");
    let unresolved = dlimage::relocate(&mapped, &tables, 3, scope, pass)?;
    Ok((words[DATA as usize / 8..DATA as usize / 8 + 8].to_vec(), unresolved, base))
}

#[test]
fn each_relocation_is_worth_what_its_psabi_says() {
    let relocs = [
        (DATA, RELATIVE, 0, 0x500),
        (DATA + 8, GLOB_DAT, 1, 0),
        (DATA + 16, GLOB_DAT, 3, 0),
        (DATA + 24, DTPMOD, 0, 0),
        (DATA + 32, DTPOFF, 2, 4),
        (DATA + 40, DTPMOD, 4, 0),
        (DATA + 48, DTPOFF, 4, 2),
        (DATA + 56, DTPOFF, 0, 0x18),
    ];
    let file = object(&relocs, RELA, |_| ());
    let (words, unresolved, base) = load(&file, &Loaded, Pass::Library).unwrap();
    assert_eq!(words, [base + 0x500, 0xdead_0000, base + 0x500, 3, 8 + 4, 7, 0x12, 0x18]);
    assert_eq!(unresolved, 0);

    // With nothing loaded before it: an import is zero, another module's
    // thread-local names this one and offset zero, each counted.
    let (words, unresolved, _) = load(&file, &Nothing, Pass::Library).unwrap();
    assert_eq!((words[1], words[5], words[6]), (0, 3, 0));
    assert_eq!(unresolved, 3);
}

#[test]
fn an_executables_pass_leaves_the_relatives_it_applied_itself() {
    let file = object(&[(DATA, RELATIVE, 0, 0x500), (DATA + 8, GLOB_DAT, 1, 0)], RELA, |_| ());
    let (words, _, _) = load(&file, &Loaded, Pass::Executable).unwrap();
    assert_eq!(&words[..2], [0, 0xdead_0000]);
}

#[test]
fn what_it_cannot_honour_is_refused_before_any_write() {
    // Initial-exec TLS outside the executable's own block.
    let file = object(&[(DATA, RELATIVE, 0, 0x500), (DATA + 8, TPOFF, 2, 0)], RELA, |_| ());
    assert_eq!(load(&file, &Loaded, Pass::Library).map(|_| ()), Err("ELF: initial-exec TLS outside the executable's own block"));
    // A write outside the writable segment.
    let file = object(&[(0x100, RELATIVE, 0, 0)], RELA, |_| ());
    assert_eq!(load(&file, &Loaded, Pass::Library).map(|_| ()), Err(RelocError::OutsideWindow.as_str()));
    // An offset past this object's own thread-local storage.
    let file = object(&[(DATA, DTPOFF, 2, 0x20)], RELA, |_| ());
    assert_eq!(load(&file, &Loaded, Pass::Library).map(|_| ()), Err(RelocError::TlsOutsideSegment.as_str()));
}

#[test]
fn a_relocation_table_a_relocation_could_write_is_refused() {
    let file = object(&[(DATA + 8, RELATIVE, 0, 0)], DATA as usize + 0x40, |_| ());
    assert!(load(&file, &Loaded, Pass::Library).is_err_and(|why| why.contains(".rela.dyn")));
}

#[test]
fn an_image_off_zero_or_with_segments_sharing_a_page_is_refused() {
    // The read-only segment's vaddr moved to 0x400.
    let shifted = object(&[], RELA, |b| b[PHDRS + 16..PHDRS + 24].copy_from_slice(&0x400u64.to_le_bytes()));
    assert!(dlimage::plan(&shifted[..0x1000]).is_err());
    // The writable segment moved into the read-only one's last page.
    let shared = object(&[], RELA, |b| b[PHDRS + 56 + 16..PHDRS + 56 + 24].copy_from_slice(&0xf00u64.to_le_bytes()));
    assert_eq!(dlimage::plan(&shared[..0x1000]).map(|_| ()), Err("ELF: two PT_LOAD segments share a page"));
}
