//! The loader's image work on a shared object laid out here: what each
//! relocation it writes is worth, which ones it refuses, and the images it
//! refuses before any is laid out. The image is the host's machine's, so each
//! host checks its own architecture's relocation numbers.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

use alloc::string::ToString;
use alloc::vec;
use alloc::vec::Vec;

use super::{plan, relocate, Mapped, Pass, Scope};
use crate::dl::Refused;
use crate::elf::RelocError;

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
    pub const ABS64: u32 = 1;
    pub const JUMP_SLOT: u32 = 7;
    pub const IRELATIVE: u32 = 37;
    pub const RELATIVE: u32 = 8;
    pub const GLOB_DAT: u32 = 6;
    pub const DTPMOD: u32 = 16;
    pub const DTPOFF: u32 = 17;
    pub const TPOFF: u32 = 18;
}
#[cfg(target_arch = "aarch64")]
mod machine {
    pub const EM: u16 = 183;
    pub const ABS64: u32 = 257;
    pub const JUMP_SLOT: u32 = 1026;
    pub const IRELATIVE: u32 = 1032;
    pub const RELATIVE: u32 = 1027;
    pub const GLOB_DAT: u32 = 1025;
    pub const DTPMOD: u32 = 1028;
    pub const DTPOFF: u32 = 1029;
    pub const TPOFF: u32 = 1030;
}
use machine::*;

const GLOBAL_FUNC: u8 = (1 << 4) | 2;
const WEAK_FUNC: u8 = (2 << 4) | 2;
const GLOBAL_TLS: u8 = (1 << 4) | 6;

/// Name, `st_info`, defined, value: 1 an import, 2 this object's
/// thread-local, 3 a function it defines, 4 another's thread-local, 5 a weak
/// import.
const SYMBOLS: &[(&str, u8, bool, u64)] = &[
    ("", 0, false, 0),
    ("imported", GLOBAL_FUNC, false, 0),
    ("counter", GLOBAL_TLS, true, 8),
    ("exported", GLOBAL_FUNC, true, 0x500),
    ("other_tls", GLOBAL_TLS, false, 0),
    ("maybe", WEAK_FUNC, false, 0),
];

/// One relocation: where, its type, its symbol and its addend.
type Entry = (u64, u32, u32, i64);

/// A shared object, its file bytes its image: text and tables in a read-only
/// page, a writable page whose start the relocations write and whose
/// `PT_TLS` is 0x20 bytes at 0x1080, 0x10 of them its template, and `relocs`
/// in a `DT_RELA` at `rela_at`. `edit` changes the bytes last.
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

/// The words a load leaves from `DATA`, as many as the relocations write.
const WORDS: usize = 20;

/// `file` laid out and relocated as module 3 against `scope`: the image's
/// words from `DATA`, whether or not it was refused, the refusal, and the
/// image's base.
fn load(file: &[u8], scope: &dyn Scope, pass: Pass<'_>) -> (Vec<u64>, Result<(), Refused>, u64) {
    let plan = plan(&file[..0x1000]).unwrap();
    // Whole words, so the image is 8-aligned as a mapping is page-aligned.
    let mut words = vec![0u64; plan.span / 8];
    // SAFETY: the buffer's own bytes.
    let image = unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), plan.span) };
    plan.lay(file, image).unwrap();
    let base = image.as_ptr() as u64;
    // SAFETY: the buffer, laid out whole and writable, which nothing else reaches.
    let mapped = unsafe { Mapped::new(image.as_mut_ptr(), &plan) };
    let relocated = mapped.tables(true).and_then(|tables| relocate(&mapped, &tables.expect("a PT_DYNAMIC"), 3, scope, pass));
    (words[DATA as usize / 8..DATA as usize / 8 + WORDS].to_vec(), relocated, base)
}

/// The words `file` relocates to as a library against `scope`, or why it was refused.
fn library(file: &[u8], scope: &dyn Scope) -> Result<(Vec<u64>, u64), String> {
    let (words, relocated, base) = load(file, scope, Pass::Library);
    relocated.map(|()| (words, base)).map_err(|why| why.to_string())
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
        (DATA + 64, ABS64, 1, 8),
        (DATA + 72, ABS64, 0, 0x1234),
        (DATA + 80, ABS64, 3, -0x10),
    ];
    let file = object(&relocs, RELA, |_| ());
    let (words, base) = library(&file, &Loaded).unwrap();
    assert_eq!(
        words[..11],
        [base + 0x500, 0xdead_0000, base + 0x500, 3, 8 + 4, 7, 0x12, 0x18, 0xdead_0008, 0x1234, base + 0x4f0]
    );
}

/// A strong name nothing defines would leave a wrong pointer, or alias
/// another module's thread-local onto this one's: each is refused, naming
/// the symbol, before any write. A weak one is 0.
#[test]
fn a_symbol_nothing_defines_is_refused_by_name_unless_it_is_weak() {
    for (entry, name) in [
        ((DATA + 8, GLOB_DAT, 1, 0), "imported"),
        ((DATA + 8, ABS64, 1, 0), "imported"),
        ((DATA + 8, DTPMOD, 4, 0), "other_tls"),
        ((DATA + 8, DTPOFF, 4, 0), "other_tls"),
    ] {
        let file = object(&[(DATA, RELATIVE, 0, 0x500), entry], RELA, |_| ());
        let (words, relocated, _) = load(&file, &Nothing, Pass::Library);
        assert!(matches!(&relocated, Err(Refused::Undefined(n)) if n == name), "{entry:?}: {relocated:?}");
        assert_eq!(relocated.unwrap_err().to_string(), alloc::format!("symbol `{name}` is defined by no loaded module"));
        assert_eq!(words[0], 0, "{entry:?} wrote before its refusal");
    }
    let file = object(&[(DATA, GLOB_DAT, 5, 0), (DATA + 8, ABS64, 5, 4)], RELA, |_| ());
    assert_eq!(library(&file, &Nothing).unwrap().0[..2], [0, 4]);
}

/// A call's slot nothing defines is bound lazily, as the psABI lets a loader:
/// it fails at the call, not the load. An image that binds now
/// (`DT_BIND_NOW`) is refused at the load, naming it.
#[test]
fn a_call_nothing_defines_fails_at_the_call_unless_the_image_binds_now() {
    let file = object(&[(DATA, JUMP_SLOT, 1, 0), (DATA + 8, JUMP_SLOT, 3, 0)], RELA, |_| ());
    let (words, base) = library(&file, &Nothing).unwrap();
    assert_eq!(words[..2], [(crate::dl::undefined_function as *const ()).addr() as u64, base + 0x500]);
    // `DT_BIND_NOW` where `DT_NULL` was, and `DT_NULL` after it.
    let now = object(&[(DATA, JUMP_SLOT, 1, 0)], RELA, |b| {
        b[DYNAMIC + 5 * 16..DYNAMIC + 6 * 16].copy_from_slice(&[24u64.to_le_bytes(), [0; 8]].concat());
        b[DYNAMIC + 6 * 16..DYNAMIC + 7 * 16].fill(0);
    });
    assert_eq!(library(&now, &Nothing).err().as_deref(), Some("symbol `imported` is defined by no loaded module"));
}

#[test]
fn an_executables_pass_leaves_the_relatives_it_applied_itself() {
    let file = object(&[(DATA, RELATIVE, 0, 0x500), (DATA + 8, GLOB_DAT, 1, 0)], RELA, |_| ());
    let (words, relocated, _) = load(&file, &Loaded, Pass::Executable { block: &mut [] });
    relocated.unwrap();
    assert_eq!(words[..2], [0, 0xdead_0000]);
}

/// The main thread's block was copied from the template before the
/// executable's pass, so a value the pass writes into the template is written
/// into that block too, at the same offset; one beside the template is not.
#[test]
fn an_executables_pass_writes_into_the_main_threads_block_what_it_writes_into_the_template() {
    let file = object(&[(TLS, GLOB_DAT, 1, 0), (TLS + 8, ABS64, 1, 4), (TLS + 0x10, GLOB_DAT, 1, 0)], RELA, |_| ());
    let mut block = [0u8; 0x10];
    let (words, relocated, _) = load(&file, &Loaded, Pass::Executable { block: &mut block });
    relocated.unwrap();
    let image_at = (TLS - DATA) as usize / 8;
    assert_eq!(words[image_at..image_at + 3], [0xdead_0000, 0xdead_0004, 0xdead_0000]);
    assert_eq!(block[..8], 0xdead_0000u64.to_le_bytes());
    assert_eq!(block[8..], 0xdead_0004u64.to_le_bytes());
}

#[test]
fn what_it_cannot_honour_is_refused_before_any_write() {
    for (entry, why) in [
        // Initial-exec TLS outside the executable's own block.
        ((DATA + 8, TPOFF, 2, 0), "ELF: initial-exec TLS outside the executable's own block".to_string()),
        // A write outside the writable segment.
        ((0x100, RELATIVE, 0, 0), RelocError::OutsideWindow.to_string()),
        // An offset past this object's own thread-local storage.
        ((DATA + 8, DTPOFF, 2, 0x20), RelocError::TlsOutsideSegment.to_string()),
        // A type this loader does not apply.
        ((DATA + 8, IRELATIVE, 0, 0x500), RelocError::Unsupported(IRELATIVE).to_string()),
    ] {
        let file = object(&[(DATA, RELATIVE, 0, 0x500), entry], RELA, |_| ());
        let (words, relocated, _) = load(&file, &Loaded, Pass::Library);
        assert_eq!(relocated.map_err(|e| e.to_string()), Err(why));
        assert_eq!(words[0], 0, "{entry:?} wrote before its refusal");
    }
}

#[test]
fn a_relocation_table_a_relocation_could_write_is_refused() {
    let file = object(&[(DATA + 8, RELATIVE, 0, 0)], DATA as usize + 0x40, |_| ());
    assert!(library(&file, &Loaded).is_err_and(|why| why.contains(".rela.dyn")));
}

#[test]
fn an_image_off_zero_or_with_segments_sharing_a_page_is_refused() {
    // The read-only segment's vaddr moved to 0x400.
    let shifted = object(&[], RELA, |b| b[PHDRS + 16..PHDRS + 24].copy_from_slice(&0x400u64.to_le_bytes()));
    assert!(plan(&shifted[..0x1000]).is_err());
    // The writable segment moved into the read-only one's last page.
    let shared = object(&[], RELA, |b| b[PHDRS + 56 + 16..PHDRS + 56 + 24].copy_from_slice(&0xf00u64.to_le_bytes()));
    assert_eq!(plan(&shared[..0x1000]).map(|_| ()).map_err(|e| e.to_string()), Err("ELF: two PT_LOAD segments share a page".to_string()));
}

/// A library's segment both writable and executable is refused rather than
/// given either protection.
#[test]
fn a_library_segment_both_writable_and_executable_is_refused() {
    assert!(plan(&object(&[], RELA, |_| ())[..0x1000]).unwrap().library().is_ok());
    // The writable segment's flags, `PF_R | PF_W | PF_X`.
    let wx = object(&[], RELA, |b| b[PHDRS + 56 + 4..PHDRS + 56 + 8].copy_from_slice(&7u32.to_le_bytes()));
    let refused = plan(&wx[..0x1000]).unwrap().library().map_err(|e| e.to_string());
    assert_eq!(refused, Err("ELF: a library's PT_LOAD is both writable and executable".to_string()));
}

/// `dlsym` with a handle answers any symbol the module defines, but a
/// thread-local, whose address differs per thread.
#[test]
fn a_modules_thread_local_has_no_one_address() {
    let file = object(&[], RELA, |_| ());
    let plan = plan(&file[..0x1000]).unwrap();
    let mut words = vec![0u64; plan.span / 8];
    // SAFETY: the buffer's own bytes.
    let image = unsafe { core::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), plan.span) };
    plan.lay(&file, image).unwrap();
    // SAFETY: the buffer, laid out whole, which nothing else reaches.
    let mapped = unsafe { Mapped::new(image.as_mut_ptr(), &plan) };
    let tables = mapped.tables(true).unwrap().unwrap();
    assert_eq!(tables.defined("exported", 0x10_0000), Some(0x10_0500));
    assert_eq!(tables.defined("counter", 0x10_0000), None);
    assert_eq!(tables.defined("imported", 0x10_0000), None);
}
