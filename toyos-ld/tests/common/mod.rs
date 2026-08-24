//! Synthetic linker inputs and a run harness, shared by the test files here.
//!
//! One copy: two would drift, and both are asked to build the same shapes of
//! object.

#![allow(dead_code)]

use object::write::{Object, Relocation, StandardSection, Symbol, SymbolSection};
use object::{
    elf, Architecture, BinaryFormat, Endianness, Object as _, ObjectSymbol as _, RelocationFlags,
    SymbolFlags, SymbolKind, SymbolScope,
};
use std::path::{Path, PathBuf};
use std::process::Command;

// ── Input synthesis ──────────────────────────────────────────────────────

/// How many samples a determinism case takes. Two suffices for a case whose
/// hazard is wide — each of them is, deliberately, so that a hash order and a
/// sorted order essentially never coincide. Eight is the margin for the narrow
/// case somebody adds later: `toyos-cc`'s gate has one hazard of a single stack
/// slot, and two runs caught it 39 times in 40 where eight caught it 40.
pub const RUNS: usize = 8;

/// `mov rax, [rip + disp32]` — the byte sequence Cranelift emits for a GOT load.
pub const MOV_RAX_RIP: [u8; 3] = [0x48, 0x8B, 0x05];
/// `call rel32`
pub const CALL_REL32: u8 = 0xE8;
pub const RET: u8 = 0xC3;

pub struct ObjBuilder {
    obj: Object<'static>,
}

impl ObjBuilder {
    pub fn new() -> Self {
        ObjBuilder {
            obj: Object::new(BinaryFormat::Elf, Architecture::X86_64, Endianness::Little),
        }
    }

    pub fn text(&mut self, name: &str, code: &[u8], scope: SymbolScope) -> object::write::SymbolId {
        let section = self.obj.section_id(StandardSection::Text);
        let offset = self.obj.append_section_data(section, code, 16);
        self.obj.add_symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value: offset,
            size: code.len() as u64,
            kind: SymbolKind::Text,
            scope,
            weak: false,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        })
    }

    pub fn data(&mut self, name: &str, bytes: &[u8], scope: SymbolScope) -> object::write::SymbolId {
        let section = self.obj.section_id(StandardSection::Data);
        let offset = self.obj.append_section_data(section, bytes, 8);
        self.obj.add_symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value: offset,
            size: bytes.len() as u64,
            kind: SymbolKind::Data,
            scope,
            weak: false,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        })
    }

    pub fn undefined(&mut self, name: &str, kind: SymbolKind) -> object::write::SymbolId {
        self.obj.add_symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value: 0,
            size: 0,
            kind,
            scope: SymbolScope::Dynamic,
            weak: false,
            section: SymbolSection::Undefined,
            flags: SymbolFlags::None,
        })
    }

    /// Append a function that GOT-loads each of `targets`, then returns. The
    /// GOT slot order is what `.rela.dyn` is built from.
    pub fn got_loader(
        &mut self,
        name: &str,
        targets: &[object::write::SymbolId],
        scope: SymbolScope,
    ) -> object::write::SymbolId {
        let mut code = Vec::new();
        for _ in targets {
            code.extend_from_slice(&MOV_RAX_RIP);
            code.extend_from_slice(&0i32.to_le_bytes());
        }
        code.push(RET);
        let sym = self.text(name, &code, scope);
        let section = self.obj.section_id(StandardSection::Text);
        let base = self.symbol_offset(sym);
        for (i, &target) in targets.iter().enumerate() {
            self.obj.add_relocation(
                section,
                Relocation {
                    offset: base + (i * 7) as u64 + MOV_RAX_RIP.len() as u64,
                    symbol: target,
                    addend: -4,
                    flags: RelocationFlags::Elf { r_type: elf::R_X86_64_REX_GOTPCRELX },
                },
            )
            .unwrap();
        }
        sym
    }

    /// Append a function that calls each of `targets` through PLT32.
    pub fn caller(
        &mut self,
        name: &str,
        targets: &[object::write::SymbolId],
        scope: SymbolScope,
    ) -> object::write::SymbolId {
        let mut code = Vec::new();
        for _ in targets {
            code.push(CALL_REL32);
            code.extend_from_slice(&0i32.to_le_bytes());
        }
        code.push(RET);
        let sym = self.text(name, &code, scope);
        let section = self.obj.section_id(StandardSection::Text);
        let base = self.symbol_offset(sym);
        for (i, &target) in targets.iter().enumerate() {
            self.obj.add_relocation(
                section,
                Relocation {
                    offset: base + (i * 5) as u64 + 1,
                    symbol: target,
                    addend: -4,
                    flags: RelocationFlags::Elf { r_type: elf::R_X86_64_PLT32 },
                },
            )
            .unwrap();
        }
        sym
    }

    pub fn symbol_offset(&self, id: object::write::SymbolId) -> u64 {
        self.obj.symbol(id).value
    }

    pub fn finish(self) -> Vec<u8> {
        self.obj.write().unwrap()
    }
}

/// A GNU (System V) `ar` archive with the given members.
///
/// The member's name lives in the header's 16-byte name field, terminated by a
/// slash so it may contain spaces, and the data follows the header directly.
/// A name too long for the field goes in a `//` member instead and the header
/// carries `/<byte offset into it>`; the symbol table is a first member named
/// `/`, holding big-endian offsets of the member headers. Members are padded to
/// an even byte.
pub fn archive(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    // GNU's long-name table: every name too long for the header field, each
    // terminated by "/\n", and the offset each header will point at.
    let mut long_names: Vec<u8> = Vec::new();
    let mut name_field: Vec<String> = Vec::new();
    for (name, _) in members {
        if name.len() < 16 {
            name_field.push(format!("{name}/"));
        } else {
            name_field.push(format!("/{}", long_names.len()));
            long_names.extend_from_slice(name.as_bytes());
            long_names.extend_from_slice(b"/\n");
        }
    }

    let mut symbols: Vec<(String, usize)> = Vec::new();
    for (i, (_, data)) in members.iter().enumerate() {
        for name in global_definitions(data) {
            symbols.push((name, i));
        }
    }

    let symtab_len = 4 + 4 * symbols.len() + symbols.iter().map(|(n, _)| n.len() + 1).sum::<usize>();
    let mut pos = 8 + 60 + symtab_len + symtab_len % 2;
    if !long_names.is_empty() {
        pos += 60 + long_names.len() + long_names.len() % 2;
    }
    let mut member_offsets = Vec::with_capacity(members.len());
    for (_, data) in members {
        member_offsets.push(pos as u32);
        pos += 60 + data.len() + data.len() % 2;
    }

    // The GNU index is in member order and big-endian, where BSD's is sorted
    // and little-endian. Neither is read by toyos-ld, which scans the members.
    let mut symtab: Vec<u8> = (symbols.len() as u32).to_be_bytes().to_vec();
    for (_, member) in &symbols {
        symtab.extend_from_slice(&member_offsets[*member].to_be_bytes());
    }
    for (name, _) in &symbols {
        symtab.extend_from_slice(name.as_bytes());
        symtab.push(0);
    }
    assert_eq!(symtab.len(), symtab_len);

    let mut out = b"!<arch>\n".to_vec();
    push_gnu_member(&mut out, "/", &symtab);
    if !long_names.is_empty() {
        push_gnu_member(&mut out, "//", &long_names);
    }
    for (i, ((_, data), field)) in members.iter().zip(&name_field).enumerate() {
        assert_eq!(
            out.len(),
            member_offsets[i] as usize,
            "member {i} is not where the symbol table says it is",
        );
        push_gnu_member(&mut out, field, data);
    }
    out
}

fn push_gnu_member(out: &mut Vec<u8>, name_field: &str, data: &[u8]) {
    out.extend_from_slice(&member_header(name_field, data.len()));
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(b'\n');
    }
}

/// Every global a member defines — what an archive's symbol table indexes.
fn global_definitions(data: &[u8]) -> Vec<String> {
    let obj = object::File::parse(data).unwrap();
    obj.symbols()
        .filter(|s| s.is_global() && !s.is_undefined())
        .filter_map(|s| s.name().ok().filter(|n| !n.is_empty()).map(str::to_string))
        .collect()
}

/// A BSD (cctools/Darwin) `ar` archive with the same members `archive` takes.
///
/// The two dialects share `!<arch>\n` and the 60-byte member header and agree
/// on nothing else that a reader has to know:
///
/// * **`#1/<n>`** in the name field means the real name is the first `n` bytes
///   of the *member data*, NUL-padded, and `n` is counted inside the member's
///   size — so both the data pointer and the data length move. A reader that
///   takes the header field as the name sees `#1/24` and a member whose first
///   bytes are its own name.
/// * **`__.SYMDEF SORTED`** (or `__.SYMDEF`) is the symbol table, where GNU
///   writes a member named `/`. It is not an object and carries a name long
///   enough that it arrives through `#1/` as well.
/// * Padding is to 8 bytes, not 2: the name is padded so the data that follows
///   starts 8-aligned, and the member is padded so the next header does.
///
/// The paddings are LLVM's `printBSDMemberHeader`
/// (`llvm/lib/Object/ArchiveWriter.cpp`): `#1/` carries
/// `name.len() + pad_to_8(pos + 60 + name.len())`. Apple's `ar` pads the name
/// differently — it rounds `60 + name.len()` up to 8 — and both land the data
/// on the same alignment; a reader that stops the name at its first NUL takes
/// either. Measured against `/usr/bin/ar` on macOS 26 (cctools, the same inode
/// as `/usr/bin/libtool` and `/usr/bin/ranlib`): `ar cr m.a f.o` over one
/// 512-byte Mach-O object wrote `#1/20` + `__.SYMDEF SORTED` at offset 8 and
/// `#1/12` + `f.o` at offset 0x70, member data at 0x58 and 0xb8 — both
/// 8-aligned, name lengths 20 and 12 for names of 16 and 3.
///
/// The symbol table this writes is a real one — `__.SYMDEF SORTED`'s ranlib
/// array over every global the members define, sorted by name, each entry
/// naming the offset of its member's header. toyos-ld does not read it (it
/// scans the members itself), which is exactly why it is written correctly
/// here: a table nothing checks is a fixture that could rot into a shape no
/// archiver produces.
pub fn bsd_archive(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
    // (symbol, index of the member defining it), sorted — `SORTED` is a claim
    // about this order and a reader may binary-search on it.
    let mut symbols: Vec<(String, usize)> = Vec::new();
    for (i, (_, data)) in members.iter().enumerate() {
        for name in global_definitions(data) {
            symbols.push((name, i));
        }
    }
    symbols.sort();

    // The string table, and each symbol's offset into it.
    let mut strtab: Vec<u8> = Vec::new();
    let mut str_offsets: Vec<u32> = Vec::new();
    for (name, _) in &symbols {
        str_offsets.push(strtab.len() as u32);
        strtab.extend_from_slice(name.as_bytes());
        strtab.push(0);
    }
    while !strtab.len().is_multiple_of(8) {
        strtab.push(0);
    }

    // The table's own size is fixed before any member is placed, so the member
    // offsets it has to carry can be computed ahead of writing it.
    let ranlib_bytes = symbols.len() * 8;
    let symdef_payload = 4 + ranlib_bytes + 4 + strtab.len();
    let symdef_name = b"__.SYMDEF SORTED";
    let symdef_name_len = bsd_name_len(8, symdef_name.len());
    let mut pos = 8 + 60 + symdef_name_len + symdef_payload;
    pos += pad_to(pos, 8);

    let mut member_offsets = Vec::with_capacity(members.len());
    for (name, data) in members {
        member_offsets.push(pos as u32);
        let name_len = bsd_name_len(pos, name.len());
        pos += 60 + name_len + data.len();
        pos += pad_to(pos, 8);
    }

    let mut symdef: Vec<u8> = Vec::new();
    symdef.extend_from_slice(&(ranlib_bytes as u32).to_le_bytes());
    for (str_off, (_, member)) in str_offsets.iter().zip(&symbols) {
        symdef.extend_from_slice(&str_off.to_le_bytes());
        symdef.extend_from_slice(&member_offsets[*member].to_le_bytes());
    }
    symdef.extend_from_slice(&(strtab.len() as u32).to_le_bytes());
    symdef.extend_from_slice(&strtab);
    assert_eq!(symdef.len(), symdef_payload);

    let mut out = b"!<arch>\n".to_vec();
    push_bsd_member(&mut out, symdef_name, &symdef);
    for (name, data) in members {
        push_bsd_member(&mut out, name.as_bytes(), data);
    }
    for (i, offset) in member_offsets.iter().enumerate() {
        assert_eq!(
            &out[*offset as usize..*offset as usize + 3],
            b"#1/",
            "member {i} is not where __.SYMDEF says it is",
        );
    }
    out
}

/// The 60-byte header every `ar` dialect shares: name, mtime, uid, gid, mode,
/// size, and the `` `\n `` terminator that lets a reader tell it found one.
fn member_header(name: &str, size: usize) -> [u8; 60] {
    let header =
        format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, 0, 0, 0, "100644", size);
    header.as_bytes().try_into().expect("an ar member header is 60 bytes")
}

fn pad_to(pos: usize, align: usize) -> usize {
    (align - pos % align) % align
}

/// `#1/<n>`'s `n`: the name, padded so that the data behind it starts 8-aligned.
fn bsd_name_len(pos: usize, name_len: usize) -> usize {
    name_len + pad_to(pos + 60 + name_len, 8)
}

fn push_bsd_member(out: &mut Vec<u8>, name: &[u8], data: &[u8]) {
    let name_len = bsd_name_len(out.len(), name.len());
    out.extend_from_slice(&member_header(&format!("#1/{name_len}"), name_len + data.len()));
    out.extend_from_slice(name);
    out.resize(out.len() + (name_len - name.len()), 0);
    assert_eq!(out.len() % 8, 0, "BSD member data must start 8-aligned");
    out.extend_from_slice(data);
    let pad = pad_to(out.len(), 8);
    out.resize(out.len() + pad, 0);
}

// ── Harness ──────────────────────────────────────────────────────────────

pub struct Case {
    dir: PathBuf,
    inputs: Vec<PathBuf>,
    args: Vec<String>,
}

impl Case {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("toyos-ld-det-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Case { dir, inputs: Vec::new(), args: Vec::new() }
    }

    pub fn input(mut self, name: &str, bytes: Vec<u8>) -> Self {
        let path = self.dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        self.inputs.push(path);
        self
    }

    pub fn arg(mut self, a: &str) -> Self {
        self.args.push(a.to_string());
        self
    }

    pub fn link_once(&self, out: &Path) {
        let status = Command::new(env!("CARGO_BIN_EXE_toyos-ld"))
            .args(&self.args)
            .arg("-o")
            .arg(out)
            .args(&self.inputs)
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "link failed: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// Link once and return the output bytes.
    pub fn link(&self) -> Vec<u8> {
        let out = self.dir.join("out");
        self.link_once(&out);
        std::fs::read(&out).unwrap()
    }

    /// Link once, expecting it to fail, and return what the linker said.
    pub fn link_expecting_failure(&self) -> String {
        let out = self.dir.join("out");
        let result = Command::new(env!("CARGO_BIN_EXE_toyos-ld"))
            .args(&self.args)
            .arg("-o")
            .arg(&out)
            .args(&self.inputs)
            .output()
            .unwrap();
        assert!(!result.status.success(), "expected the link to fail, and it succeeded");
        String::from_utf8_lossy(&result.stderr).into_owned()
    }

    /// Link `RUNS` times and return the index of the first run whose output
    /// differs from run 0, with the number of differing bytes.
    pub fn diff(&self) -> Option<(usize, usize)> {
        let mut first: Option<Vec<u8>> = None;
        for run in 0..RUNS {
            let out = self.dir.join(format!("out.{run}"));
            self.link_once(&out);
            let bytes = std::fs::read(&out).unwrap();
            match &first {
                None => first = Some(bytes),
                Some(f) => {
                    if *f != bytes {
                        let differing = if f.len() == bytes.len() {
                            f.iter().zip(&bytes).filter(|(a, b)| a != b).count()
                        } else {
                            usize::MAX
                        };
                        return Some((run, differing));
                    }
                }
            }
        }
        None
    }

    pub fn assert_identical(&self, what: &str) {
        if let Some((run, bytes)) = self.diff() {
            panic!("{what}: run {run} of {RUNS} differs from run 0 in {bytes} bytes");
        }
    }
}

