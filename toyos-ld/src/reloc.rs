use crate::collect::{Arch, InputReloc, LinkState, RelocType, SectionIdx, SectionKind, SymbolDef, SymbolRef};
use crate::emit_pe::PeLayout;
use crate::LinkError;
use std::collections::{BTreeMap, HashSet};


pub(crate) struct RelocOutput {
    pub(crate) relatives: Vec<(u64, i64)>,
    /// Dynamic GOT entries needing GLOB_DAT relocations: (GOT slot vaddr, symbol name).
    pub(crate) glob_dats: Vec<(u64, String)>,
    /// GOTTPOFF GOT entries: (GOT slot vaddr, raw tpoff value). Written directly into
    /// the output (not as RELATIVE) because RELATIVE would incorrectly add the load base.
    pub(crate) tpoff_fills: Vec<(u64, i64)>,
    /// R_X86_64_TPOFF64 runtime relocations for shared libraries: (GOT slot vaddr, addend).
    /// The addend is the symbol's offset within its module's TLS segment. At load time,
    /// the kernel adds the module's TLS base tpoff to produce the final tpoff value.
    pub(crate) tpoff64s: Vec<(u64, i64)>,
    /// Named R_X86_64_TPOFF64 runtime relocations for cross-library TLS references:
    /// (GOT slot vaddr, symbol name). The kernel looks up the symbol across all loaded
    /// libraries and computes the correct tpoff.
    pub(crate) named_tpoff64s: Vec<(u64, String)>,
    /// R_X86_64_TPOFF32 runtime relocations for shared libraries: (vaddr of imm32, addend).
    /// Used for LD→LE relaxation in shared mode where DTPOFF32 immediates need load-time patching.
    pub(crate) tpoff32s: Vec<(u64, i64)>,
    /// R_X86_64_DTPMOD64 runtime relocations for DTV-based TLS in shared libraries:
    /// (GOT slot vaddr, addend). Kernel writes the module ID at load time.
    pub(crate) dtpmod64s: Vec<(u64, i64)>,
    /// R_X86_64_DTPOFF64 runtime relocations for DTV-based TLS in shared libraries:
    /// (GOT slot vaddr, addend). Addend is the symbol's offset within its module's TLS segment.
    pub(crate) dtpoff64s: Vec<(u64, i64)>,
    /// Named R_X86_64_DTPMOD64 for cross-library TLS: (GOT slot vaddr, symbol name).
    pub(crate) named_dtpmod64s: Vec<(u64, String)>,
    /// Named R_X86_64_DTPOFF64 for cross-library TLS: (GOT slot vaddr, symbol name).
    pub(crate) named_dtpoff64s: Vec<(u64, String)>,
    /// R_AARCH64_TLSDESC runtime relocations for a shared library's own TLS:
    /// (descriptor pair vaddr, the symbol's offset within the module's TLS
    /// segment). The loader writes the resolver and its argument.
    pub(crate) tlsdescs: Vec<(u64, i64)>,
    /// Named R_AARCH64_TLSDESC for cross-library TLS: (descriptor pair vaddr,
    /// symbol name).
    pub(crate) named_tlsdescs: Vec<(u64, String)>,
}

/// Resolve a symbol to its virtual address.
/// `plt` provides PLT stubs for dynamic symbols (PIE mode). Pass `None` for
/// static/PE modes where dynamic symbols are unsupported.
pub(crate) fn resolve_symbol(
    state: &LinkState,
    sym: &SymbolRef,
    plt: Option<&BTreeMap<SymbolRef, u64>>,
) -> Option<u64> {
    match sym {
        SymbolRef::Global(name) => {
            match state.globals.get(name)? {
                SymbolDef::Dynamic { .. } => plt.and_then(|p| p.get(&SymbolRef::Global(name.clone())).copied()),
                SymbolDef::Defined { section, value, .. } => {
                    let sec = &state.sections[*section];
                    Some(sec.vaddr.unwrap_or_else(|| panic!(
                        "symbol {name:?} in section {:?} ({:?}) has no vaddr",
                        sec.name, sec.kind,
                    )) + value)
                }
            }
        }
        SymbolRef::Local(obj_idx, name) => {
            // Local (static) symbols resolve to the local definition first.
            // Only fall back to global if no local exists (e.g., section symbols
            // that were promoted to global during merging).
            if let Some(SymbolDef::Defined { section, value, .. }) = state.locals.get(&(*obj_idx, name.clone())) {
                let sec = &state.sections[*section];
                return Some(sec.vaddr.unwrap_or_else(|| panic!(
                    "local symbol {name:?} in section {:?} ({:?}) has no vaddr",
                    sec.name, sec.kind,
                )) + value);
            }
            if let Some(def) = state.globals.get(name) {
                return match def {
                    SymbolDef::Dynamic { .. } => plt.and_then(|p| p.get(&SymbolRef::Global(name.clone())).copied()),
                    SymbolDef::Defined { section, value, .. } => {
                        let sec = &state.sections[*section];
                        Some(sec.vaddr.unwrap_or_else(|| panic!(
                            "symbol {name:?} in section {:?} ({:?}) has no vaddr",
                            sec.name, sec.kind,
                        )) + value)
                    }
                };
            }
            None
        }
    }
}

/// The alignment of every TLS segment this linker lays out: its start in the
/// image, its `PT_TLS` `p_align`, and so the gap variant I leaves after the TCB.
pub(crate) const TLS_ALIGN: u64 = 64;

/// A TLS symbol's offset from the thread pointer, in `arch`'s TLS variant.
///
/// x86-64, variant II: TP points to the end of the TLS block, so
/// TPOFF = symbol_vaddr - (tls_start + tls_memsz).
///
/// AArch64, variant I: TP points to a two-word TCB, and the executable's TLS
/// block follows it at the first offset the segment's alignment allows, so
/// TPOFF = align_up(16, TLS_ALIGN) + (symbol_vaddr - tls_start).
pub(crate) fn tpoff(arch: Arch, sym_addr: u64, tls_start: u64, tls_memsz: u64) -> i64 {
    match arch {
        Arch::X86_64 => sym_addr as i64 - (tls_start as i64 + tls_memsz as i64),
        Arch::Aarch64 => crate::align_up(16, TLS_ALIGN) as i64 + (sym_addr as i64 - tls_start as i64),
    }
}

fn check_i32(value: i64, reloc: &InputReloc) -> Result<(), LinkError> {
    if value < i32::MIN as i64 || value > i32::MAX as i64 {
        return Err(LinkError::RelocationOverflow {
            reloc_type: reloc.r_type,
            symbol: reloc.target.name().to_string(),
            value,
        });
    }
    Ok(())
}

fn check_u32(value: i64, reloc: &InputReloc) -> Result<(), LinkError> {
    if value < 0 || value > u32::MAX as i64 {
        return Err(LinkError::RelocationOverflow {
            reloc_type: reloc.r_type,
            symbol: reloc.target.name().to_string(),
            value,
        });
    }
    Ok(())
}

/// Apply a single relocation to section data, returning `true` if it's an absolute
/// reference (needs a runtime relocation / PE base fixup).
fn apply_one_reloc_x86(
    data: &mut [u8],
    reloc: &InputReloc,
    sym_addr: u64,
    reloc_vaddr: u64,
    got: &BTreeMap<SymbolRef, u64>,
    dyn_got: &BTreeMap<SymbolRef, u64>,
) -> Result<bool, LinkError> {
    match reloc.r_type {
        RelocType::X86_64 => {
            let value = (sym_addr as i64 + reloc.addend) as u64;
            write_u64_data(data, reloc.offset, value);
            Ok(true)
        }
        RelocType::X86Pc32 | RelocType::X86Plt32 => {
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            check_i32(value, reloc)?;
            write_i32_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        RelocType::X86_32 => {
            let value = sym_addr as i64 + reloc.addend;
            check_u32(value, reloc)?;
            write_u32_data(data, reloc.offset, value as u32);
            Ok(false)
        }
        RelocType::X86_32S => {
            let value = sym_addr as i64 + reloc.addend;
            check_i32(value, reloc)?;
            write_i32_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        RelocType::X86Gotpcrel | RelocType::X86Gotpcrelx
        | RelocType::X86RexGotpcrelx => {
            let got_slot = got.get(&reloc.target)
                .or_else(|| dyn_got.get(&reloc.target))
                .or_else(|| dyn_got.get(&SymbolRef::Global(reloc.target.name().to_string())))
                .ok_or_else(|| {
                    LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()])
                })?;
            let value = *got_slot as i64 + reloc.addend - reloc_vaddr as i64;
            check_i32(value, reloc)?;
            write_i32_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        RelocType::X86Tlv => {
            // Mach-O X86_64_RELOC_TLV: rewrite `movq disp(%rip), %reg` to
            // `leaq disp(%rip), %reg` so the register gets the ADDRESS of the
            // TLV descriptor, not its contents. Change opcode 0x8B → 0x8D.
            let op_off = reloc.offset as usize - 2;
            if data[op_off] == 0x8b {
                data[op_off] = 0x8d; // movq → leaq
            }
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            check_i32(value, reloc)?;
            write_i32_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        other => Err(LinkError::UnsupportedRelocation {
            reloc_type: other,
            symbol: reloc.target.name().to_string(),
        }),
    }
}

fn write_bytes_data(data: &mut [u8], offset: u64, bytes: &[u8]) {
    let off = offset as usize;
    data[off..off + bytes.len()].copy_from_slice(bytes);
}

fn write_u64_data(data: &mut [u8], offset: u64, value: u64) {
    write_bytes_data(data, offset, &value.to_le_bytes());
}

fn write_i32_data(data: &mut [u8], offset: u64, value: i32) {
    write_bytes_data(data, offset, &value.to_le_bytes());
}

fn write_u32_data(data: &mut [u8], offset: u64, value: u32) {
    write_bytes_data(data, offset, &value.to_le_bytes());
}

fn write_bytes(state: &mut LinkState, sec_idx: SectionIdx, offset: u64, bytes: &[u8]) {
    write_bytes_data(&mut state.sections[sec_idx].data, offset, bytes);
}

fn write_i32(state: &mut LinkState, sec_idx: SectionIdx, offset: u64, value: i32) {
    write_i32_data(&mut state.sections[sec_idx].data, offset, value);
}

fn write_u32(state: &mut LinkState, sec_idx: SectionIdx, offset: u64, value: u32) {
    write_u32_data(&mut state.sections[sec_idx].data, offset, value);
}

fn write_u64(state: &mut LinkState, sec_idx: SectionIdx, offset: u64, value: u64) {
    write_u64_data(&mut state.sections[sec_idx].data, offset, value);
}

/// Detect whether a TLS GD/LD relocation uses the 16-byte padded or 12-byte
/// unpadded instruction sequence by examining the byte before the leaq.
/// Padded: `data16; leaq ...` → byte at offset-4 is 0x66
/// Unpadded: `leaq ...`       → byte at offset-3 is 0x48 (REX.W)
fn is_padded_tls_sequence(sec_data: &[u8], reloc_offset: u64) -> bool {
    let off = reloc_offset as usize;
    off >= 4 && sec_data[off - 4] == 0x66
}

/// Parameters for ELF relocation application (shared between PIE and static modes).
pub(crate) struct ElfRelocParams<'a> {
    pub(crate) got: &'a BTreeMap<SymbolRef, u64>,
    pub(crate) tls_start: u64,
    pub(crate) tls_memsz: u64,
    pub(crate) plt: Option<&'a BTreeMap<SymbolRef, u64>>,
    pub(crate) dyn_got: &'a BTreeMap<SymbolRef, u64>,
    /// PIE mode: record R_X86_64_RELATIVE entries for runtime relocation.
    /// Static mode: addresses are fixed at link time, no RELATIVE needed.
    pub(crate) record_relatives: bool,
    pub(crate) allow_undefined: bool,
    /// Shared library mode: TLS offsets are not known at link time. GD→IE relaxation
    /// is used instead of GD→LE, and R_X86_64_TPOFF64 runtime relocs are emitted.
    pub(crate) is_shared: bool,
    /// Shared LD GOT pair for all TLSLD accesses (shared mode only).
    pub(crate) ld_got_pair: Option<u64>,
    /// Per-symbol GD GOT pairs for TLSGD accesses (shared mode only).
    pub(crate) gd_got: &'a BTreeMap<SymbolRef, u64>,
}

pub(crate) fn apply_relocs(
    state: &mut LinkState,
    params: &ElfRelocParams,
) -> Result<RelocOutput, LinkError> {
    let mut relatives = Vec::new();
    let mut undefined = HashSet::new();
    let mut tpoff64_entries: Vec<(u64, i64)> = Vec::new();
    let mut named_tpoff64_entries: Vec<(u64, String)> = Vec::new();
    let mut tpoff32_entries: Vec<(u64, i64)> = Vec::new();
    let mut dtpmod64_entries: Vec<(u64, i64)> = Vec::new();
    let mut dtpoff64_entries: Vec<(u64, i64)> = Vec::new();
    let mut named_dtpmod64_entries: Vec<(u64, String)> = Vec::new();
    let mut named_dtpoff64_entries: Vec<(u64, String)> = Vec::new();

    let relocs = std::mem::take(&mut state.relocs);

    // Pass 1: TLS GD/LD/DTPOFF relaxations. These rewrite instruction bytes
    // and overwrite the companion `call __tls_get_addr` instruction, so we
    // track which (section, offset) ranges were relaxed.
    let mut relaxed_calls: HashSet<(SectionIdx, u64)> = HashSet::new();

    for reloc in &relocs {
        match reloc.r_type {
            RelocType::X86Tlsgd => {
                let is_dynamic = matches!(
                    state.globals.get(reloc.target.name()),
                    Some(SymbolDef::Dynamic { .. })
                );
                let padded = is_padded_tls_sequence(
                    &state.sections[reloc.section].data,
                    reloc.offset,
                );
                if !padded {
                    return Err(LinkError::UnsupportedRelocation {
                        reloc_type: RelocType::X86Tlsgd,
                        symbol: reloc.target.name().to_string(),
                    });
                }
                if params.is_shared {
                    // Shared mode: preserve GD sequence for DTV-based TLS.
                    // Keep `call __tls_get_addr` intact. Emit DTPMOD64+DTPOFF64
                    // for the two consecutive GOT slots (TlsIndex pair).
                    // Use gd_got (separate from per-symbol got to avoid GOTTPOFF collisions).
                    let got_slot = *params.gd_got.get(&reloc.target).ok_or_else(|| {
                        LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()])
                    })?;
                    // Patch leaq's disp32 to point to GOT slot pair
                    let sec_vaddr = state.sections[reloc.section].vaddr.unwrap();
                    let rip = sec_vaddr + reloc.offset + 4; // end of leaq instruction
                    let disp = got_slot as i64 - rip as i64;
                    check_i32(disp, reloc)?;
                    write_i32(state, reloc.section, reloc.offset, disp as i32);
                    // Don't add to relaxed_calls — let the call __tls_get_addr PLT32 reloc fire
                    if is_dynamic {
                        // Cross-library TLS: named relocations for kernel to resolve
                        named_dtpmod64_entries.push((got_slot, reloc.target.name().to_string()));
                        named_dtpoff64_entries.push((got_slot + 8, reloc.target.name().to_string()));
                    } else {
                        // Same-module TLS: kernel writes module ID, offset is known
                        let sym_addr = resolve_symbol(state, &reloc.target, params.plt)
                            .ok_or_else(|| LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()]))?;
                        let sym_tls_offset = sym_addr as i64 - params.tls_start as i64;
                        dtpmod64_entries.push((got_slot, 0));
                        dtpoff64_entries.push((got_slot + 8, sym_tls_offset));
                    }
                } else if is_dynamic {
                    // PIE with dynamic TLS: GD → IE relaxation
                    // `data16; leaq; data16*2; rex64; call`
                    // → `mov %fs:0,%rax; add sym@GOTTPOFF(%rip),%rax`
                    #[rustfmt::skip]
                    let inst: [u8; 16] = [
                        0x64, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00, // mov %fs:0,%rax
                        0x48, 0x03, 0x05, 0x00, 0x00, 0x00, 0x00,             // add 0(%rip),%rax
                    ];
                    write_bytes(state, reloc.section, reloc.offset - 4, &inst);
                    let got_slot = *params.got.get(&reloc.target).ok_or_else(|| {
                        LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()])
                    })?;
                    let sec_vaddr = state.sections[reloc.section].vaddr.unwrap();
                    let rip = sec_vaddr + reloc.offset + 12; // end of `add` instruction
                    let disp = got_slot as i64 - rip as i64;
                    check_i32(disp, reloc)?;
                    write_i32(state, reloc.section, reloc.offset + 8, disp as i32);
                    relaxed_calls.insert((reloc.section, reloc.offset + 8));
                    // Cross-library TLS: emit named TPOFF64 for kernel to resolve
                    named_tpoff64_entries.push((got_slot, reloc.target.name().to_string()));
                } else {
                    let sym_addr = resolve_symbol(state, &reloc.target, params.plt)
                        .ok_or_else(|| LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()]))?;
                    // GD → LE (16-byte padded): `data16; leaq; data16*2; rex64; call`
                    // → `mov %fs:0,%rax; lea tpoff(%rax),%rax`
                    #[rustfmt::skip]
                    let inst: [u8; 16] = [
                        0x64, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00, // mov %fs:0,%rax
                        0x48, 0x8d, 0x80, 0x00, 0x00, 0x00, 0x00,             // lea 0(%rax),%rax
                    ];
                    write_bytes(state, reloc.section, reloc.offset - 4, &inst);
                    let tp_value = tpoff(state.arch, sym_addr, params.tls_start, params.tls_memsz);
                    check_i32(tp_value, reloc)?;
                    write_i32(state, reloc.section, reloc.offset + 8, tp_value as i32);
                    relaxed_calls.insert((reloc.section, reloc.offset + 8));
                }
            }
            RelocType::X86Tlsld => {
                if params.is_shared {
                    // Shared mode: preserve LD sequence for DTV-based TLS.
                    // Keep `call __tls_get_addr` intact. Emit DTPMOD64 for the
                    // dedicated LD GOT pair (separate from per-symbol GOTTPOFF slots).
                    let got_slot = params.ld_got_pair
                        .expect("TLSLD in shared mode but no LD GOT pair allocated");
                    let sec_vaddr = state.sections[reloc.section].vaddr.unwrap();
                    let rip = sec_vaddr + reloc.offset + 4; // end of leaq instruction
                    let disp = got_slot as i64 - rip as i64;
                    check_i32(disp, reloc)?;
                    write_i32(state, reloc.section, reloc.offset, disp as i32);
                    // Don't add to relaxed_calls — keep the call __tls_get_addr
                    // Emit DTPMOD64 for module ID (offset slot is 0 for LD)
                    dtpmod64_entries.push((got_slot, 0));
                    dtpoff64_entries.push((got_slot + 8, 0));
                } else {
                    // LD → LE: get thread pointer
                    let padded = is_padded_tls_sequence(
                        &state.sections[reloc.section].data,
                        reloc.offset,
                    );
                    if padded {
                        // LD → LE (16-byte padded)
                        #[rustfmt::skip]
                        let inst: [u8; 16] = [
                            0x66, 0x66, 0x66,                                           // 3x data16
                            0x64, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00,     // mov %fs:0,%rax
                            0x0f, 0x1f, 0x40, 0x00,                                     // nopl 0(%rax)
                        ];
                        write_bytes(state, reloc.section, reloc.offset - 4, &inst);
                        relaxed_calls.insert((reloc.section, reloc.offset + 8));
                    } else {
                        // LD → LE (12-byte unpadded)
                        #[rustfmt::skip]
                        let inst: [u8; 12] = [
                            0x66, 0x66, 0x66,                                           // 3x data16
                            0x64, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00,     // mov %fs:0,%rax
                        ];
                        write_bytes(state, reloc.section, reloc.offset - 3, &inst);
                        relaxed_calls.insert((reloc.section, reloc.offset + 5));
                    }
                }
            }
            RelocType::X86Dtpoff32 => {
                let sym_addr = resolve_symbol(state, &reloc.target, params.plt)
                    .ok_or_else(|| LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()]))?;
                if params.is_shared {
                    // Shared mode: LD is not relaxed, so __tls_get_addr returned the
                    // module's TLS base. DTPOFF32 is just a literal offset from there.
                    let value = sym_addr as i64 - params.tls_start as i64 + reloc.addend;
                    check_i32(value, reloc)?;
                    write_i32(state, reloc.section, reloc.offset, value as i32);
                } else {
                    let value = tpoff(state.arch, sym_addr, params.tls_start, params.tls_memsz) + reloc.addend;
                    check_i32(value, reloc)?;
                    write_i32(state, reloc.section, reloc.offset, value as i32);
                }
            }
            // Handled in pass 2
            RelocType::X86_64 | RelocType::X86Pc32 | RelocType::X86Plt32
            | RelocType::X86_32 | RelocType::X86_32S
            | RelocType::X86Gotpcrel | RelocType::X86Gotpcrelx
            | RelocType::X86RexGotpcrelx
            | RelocType::X86Tpoff32 | RelocType::X86Gottpoff
            | RelocType::X86Tlv
            | RelocType::Aarch64Abs64 | RelocType::Aarch64Abs32
            | RelocType::Aarch64Prel32 | RelocType::Aarch64Prel64
            | RelocType::Aarch64LdPrelLo19 | RelocType::Aarch64AdrPrelLo21
            | RelocType::Aarch64Condbr19 | RelocType::Aarch64Tstbr14
            | RelocType::Aarch64Call26 | RelocType::Aarch64Jump26
            | RelocType::Aarch64AdrPrelPgHi21 | RelocType::Aarch64AddAbsLo12Nc
            | RelocType::Aarch64Ldst8AbsLo12Nc | RelocType::Aarch64Ldst16AbsLo12Nc
            | RelocType::Aarch64Ldst32AbsLo12Nc | RelocType::Aarch64Ldst64AbsLo12Nc
            | RelocType::Aarch64Ldst128AbsLo12Nc
            | RelocType::Aarch64MovwUabsG0Nc | RelocType::Aarch64MovwUabsG1Nc
            | RelocType::Aarch64MovwUabsG2Nc | RelocType::Aarch64MovwUabsG3
            | RelocType::Aarch64AdrGotPage | RelocType::Aarch64Ld64GotLo12Nc
            | RelocType::Aarch64GotPcrel32
            | RelocType::Aarch64TlvpLoadPage21
            | RelocType::Aarch64TlvpLoadPageoff12
            | RelocType::Aarch64TlsdescAdrPage21 | RelocType::Aarch64TlsdescLd64Lo12
            | RelocType::Aarch64TlsdescAddLo12 | RelocType::Aarch64TlsdescCall
            | RelocType::Aarch64TlsieAdrGottprelPage21 | RelocType::Aarch64TlsieLd64GottprelLo12Nc
            | RelocType::Aarch64TlsleAddTprelHi12 | RelocType::Aarch64TlsleAddTprelLo12Nc => {}
        }
    }

    // Pass 2: all other relocations.
    for reloc in &relocs {
        if matches!(reloc.r_type, RelocType::X86Tlsgd | RelocType::X86Tlsld | RelocType::X86Dtpoff32) {
            continue;
        }
        if relaxed_calls.contains(&(reloc.section, reloc.offset)) {
            continue;
        }

        let sec_vaddr = state.sections[reloc.section].vaddr.unwrap_or_else(|| panic!(
            "reloc section {:?} ({:?}) has no vaddr",
            state.sections[reloc.section].name, state.sections[reloc.section].kind,
        ));
        let reloc_vaddr = sec_vaddr + reloc.offset;

        // Gottpoff relocations only need the GOT slot, not the symbol address.
        if reloc.r_type == RelocType::X86Gottpoff {
            let got_slot = params.got.get(&reloc.target)
                .ok_or_else(|| LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()]))?;
            let value = *got_slot as i64 + reloc.addend - reloc_vaddr as i64;
            check_i32(value, reloc)?;
            write_i32(state, reloc.section, reloc.offset, value as i32);
            continue;
        }

        if is_aarch64_tls(reloc.r_type) {
            apply_aarch64_tls(state, reloc, reloc_vaddr, params)?;
            continue;
        }

        let sym_addr = match resolve_symbol(state, &reloc.target, params.plt) {
            Some(a) => a,
            None => {
                undefined.insert(reloc.target.name().to_string());
                continue;
            }
        };

        let data = &mut state.sections[reloc.section].data;
        match reloc.r_type {
            RelocType::X86Tpoff32 => {
                if params.is_shared {
                    let sym_tls_offset = sym_addr as i64 - params.tls_start as i64 + reloc.addend;
                    write_i32_data(data, reloc.offset, 0);
                    tpoff32_entries.push((reloc_vaddr, sym_tls_offset));
                } else {
                    let value = tpoff(state.arch, sym_addr, params.tls_start, params.tls_memsz) + reloc.addend;
                    check_i32(value, reloc)?;
                    write_i32_data(data, reloc.offset, value as i32);
                }
            }
            RelocType::X86Gottpoff => unreachable!("handled above"),
            RelocType::Aarch64TlsdescAdrPage21 | RelocType::Aarch64TlsdescLd64Lo12
            | RelocType::Aarch64TlsdescAddLo12 | RelocType::Aarch64TlsdescCall
            | RelocType::Aarch64TlsieAdrGottprelPage21 | RelocType::Aarch64TlsieLd64GottprelLo12Nc
            | RelocType::Aarch64TlsleAddTprelHi12 | RelocType::Aarch64TlsleAddTprelLo12Nc => {
                unreachable!("handled above")
            }
            RelocType::X86Tlsgd | RelocType::X86Tlsld | RelocType::X86Dtpoff32 => {
                unreachable!("handled in pass 1")
            }
            RelocType::X86_64 | RelocType::X86Pc32 | RelocType::X86Plt32
            | RelocType::X86_32 | RelocType::X86_32S
            | RelocType::X86Gotpcrel | RelocType::X86Gotpcrelx
            | RelocType::X86RexGotpcrelx
            | RelocType::X86Tlv => {
                let is_abs =
                    apply_one_reloc_x86(data, reloc, sym_addr, reloc_vaddr, params.got, params.dyn_got)?;
                if is_abs && params.record_relatives {
                    relatives.push((reloc_vaddr, sym_addr as i64 + reloc.addend));
                }
            }
            RelocType::Aarch64Abs64 | RelocType::Aarch64Abs32
            | RelocType::Aarch64Prel32 | RelocType::Aarch64Prel64
            | RelocType::Aarch64LdPrelLo19 | RelocType::Aarch64AdrPrelLo21
            | RelocType::Aarch64Condbr19 | RelocType::Aarch64Tstbr14
            | RelocType::Aarch64Call26 | RelocType::Aarch64Jump26
            | RelocType::Aarch64AdrPrelPgHi21 | RelocType::Aarch64AddAbsLo12Nc
            | RelocType::Aarch64Ldst8AbsLo12Nc | RelocType::Aarch64Ldst16AbsLo12Nc
            | RelocType::Aarch64Ldst32AbsLo12Nc | RelocType::Aarch64Ldst64AbsLo12Nc
            | RelocType::Aarch64Ldst128AbsLo12Nc
            | RelocType::Aarch64MovwUabsG0Nc | RelocType::Aarch64MovwUabsG1Nc
            | RelocType::Aarch64MovwUabsG2Nc | RelocType::Aarch64MovwUabsG3
            | RelocType::Aarch64AdrGotPage | RelocType::Aarch64Ld64GotLo12Nc
            | RelocType::Aarch64GotPcrel32
            | RelocType::Aarch64TlvpLoadPage21
            | RelocType::Aarch64TlvpLoadPageoff12 => {
                let is_abs =
                    apply_one_reloc_aarch64(data, reloc, sym_addr, reloc_vaddr, params.got, params.dyn_got)?;
                if is_abs && params.record_relatives {
                    relatives.push((reloc_vaddr, sym_addr as i64 + reloc.addend));
                }
            }
        }
    }

    // Fill GOT entries (PIE mode records as RELATIVE; static mode handles in emit)
    let mut tpoff_fills = Vec::new();
    if params.record_relatives {
        let gottpoff_syms: HashSet<SymbolRef> = relocs
            .iter()
            .filter(|r| r.r_type.is_gottprel() || (r.r_type.is_tlsdesc() && !params.is_shared))
            .map(|r| r.target.clone())
            .collect();

        for (sym_ref, &got_vaddr) in params.got {
            // Dynamic symbols are in dyn_got, resolved at load time via GLOB_DAT
            if params.dyn_got.contains_key(sym_ref) { continue; }
            // GD→IE GOT entries are handled via tpoff64/named_tpoff64 entries (collected in pass 1)
            if tpoff64_entries.iter().any(|&(v, _)| v == got_vaddr) { continue; }
            if named_tpoff64_entries.iter().any(|(v, _)| *v == got_vaddr) { continue; }
            // DTV GOT entries are handled via dtpmod64/dtpoff64 entries (collected in pass 1)
            if dtpmod64_entries.iter().any(|&(v, _)| v == got_vaddr) { continue; }
            if named_dtpmod64_entries.iter().any(|(v, _)| *v == got_vaddr) { continue; }
            // Dynamic TLS symbols: emit named TPOFF64 for kernel to resolve
            let is_dynamic_tls = gottpoff_syms.contains(sym_ref)
                && matches!(state.globals.get(sym_ref.name()), Some(SymbolDef::Dynamic { .. }));
            if is_dynamic_tls {
                named_tpoff64_entries.push((got_vaddr, sym_ref.name().to_string()));
                continue;
            }
            let sym_addr = resolve_symbol(state, sym_ref, params.plt)
                .ok_or_else(|| LinkError::UndefinedSymbols(vec![sym_ref.name().to_string()]))?;
            if gottpoff_syms.contains(sym_ref) {
                if params.is_shared {
                    // Shared mode: emit TPOFF64 runtime reloc instead of filling at link time
                    let sym_tls_offset = sym_addr as i64 - params.tls_start as i64;
                    tpoff64_entries.push((got_vaddr, sym_tls_offset));
                } else {
                    // PIE mode: write raw tpoff directly — NOT as RELATIVE (RELATIVE
                    // would add the load base, corrupting the TP-relative offset).
                    let tp = tpoff(state.arch, sym_addr, params.tls_start, params.tls_memsz);
                    tpoff_fills.push((got_vaddr, tp));
                }
            } else {
                relatives.push((got_vaddr, sym_addr as i64));
            }
        }
    }

    // A shared library's TLS descriptors: one pair per symbol, written by the
    // loader.
    let mut tlsdescs = Vec::new();
    let mut named_tlsdescs = Vec::new();
    if params.is_shared {
        let described: std::collections::BTreeSet<&SymbolRef> =
            relocs.iter().filter(|r| r.r_type.is_tlsdesc()).map(|r| &r.target).collect();
        for sym_ref in described {
            let pair = params.gd_got[sym_ref];
            if matches!(state.globals.get(sym_ref.name()), Some(SymbolDef::Dynamic { .. })) {
                named_tlsdescs.push((pair, sym_ref.name().to_string()));
            } else {
                let sym_addr = resolve_symbol(state, sym_ref, None)
                    .ok_or_else(|| LinkError::UndefinedSymbols(vec![sym_ref.name().to_string()]))?;
                tlsdescs.push((pair, sym_addr as i64 - params.tls_start as i64));
            }
        }
    }

    // Collect dynamic GOT entries as GLOB_DAT relocations (resolved at load time)
    let mut glob_dats = Vec::new();
    for (sym_ref, &got_vaddr) in params.dyn_got {
        glob_dats.push((got_vaddr, sym_ref.name().to_string()));
    }

    if !params.allow_undefined && !undefined.is_empty() {
        let mut syms: Vec<String> = undefined.into_iter().collect();
        syms.sort();
        return Err(LinkError::UndefinedSymbols(syms));
    }

    Ok(RelocOutput {
        relatives,
        glob_dats,
        tpoff_fills,
        tpoff64s: tpoff64_entries,
        named_tpoff64s: named_tpoff64_entries,
        tpoff32s: tpoff32_entries,
        dtpmod64s: dtpmod64_entries,
        dtpoff64s: dtpoff64_entries,
        named_dtpmod64s: named_dtpmod64_entries,
        named_dtpoff64s: named_dtpoff64_entries,
        tlsdescs,
        named_tlsdescs,
    })
}

// ── AArch64 relocation helpers ───────────────────────────────────────────

/// `nop`.
const A64_NOP: u32 = 0xd503_201f;
/// `movz x0, #0, lsl #16`, its immediate in bits 5..21.
const A64_MOVZ_X0_LSL16: u32 = 0xd2a0_0000;
/// `movk x0, #0`, its immediate in bits 5..21.
const A64_MOVK_X0: u32 = 0xf280_0000;
/// `ldr x0, [x0, #0]`, its scaled immediate in bits 10..22.
const A64_LDR_X0_X0: u32 = 0xf940_0000;

fn is_aarch64_tls(r_type: RelocType) -> bool {
    matches!(
        r_type,
        RelocType::Aarch64TlsdescAdrPage21
            | RelocType::Aarch64TlsdescLd64Lo12
            | RelocType::Aarch64TlsdescAddLo12
            | RelocType::Aarch64TlsdescCall
            | RelocType::Aarch64TlsieAdrGottprelPage21
            | RelocType::Aarch64TlsieLd64GottprelLo12Nc
            | RelocType::Aarch64TlsleAddTprelHi12
            | RelocType::Aarch64TlsleAddTprelLo12Nc
    )
}

/// The page delta an `adrp` at `pc` needs to reach `target`'s page.
fn adrp_page_delta(target: u64, pc: u64, reloc: &InputReloc) -> Result<i32, LinkError> {
    let delta = ((target as i64) & !0xFFF) - ((pc as i64) & !0xFFF);
    let pages = delta >> 12;
    if !(-(1 << 20)..(1 << 20)).contains(&pages) {
        return Err(LinkError::RelocationOverflow {
            reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value: pages,
        });
    }
    Ok(pages as i32)
}

/// Apply one AArch64 TLS relocation.
///
/// A TLS descriptor sequence (`adrp x0; ldr x1, [x0]; add x0, x0; blr x1`,
/// the result the variable's offset from TPIDR_EL0) is kept in a shared
/// library, pointed at the symbol's descriptor pair, which an
/// R_AARCH64_TLSDESC asks the loader to fill; in an executable it is relaxed,
/// as the AArch64 ELF ABI allows and as LLD does: to initial-exec (`adrp x0;
/// ldr x0, [x0]; nop; nop`, the GOT slot holding the offset) when another
/// module defines the symbol, and otherwise to local-exec (`movz x0; movk x0;
/// nop; nop`, the offset itself).
fn apply_aarch64_tls(
    state: &mut LinkState,
    reloc: &InputReloc,
    reloc_vaddr: u64,
    params: &ElfRelocParams,
) -> Result<(), LinkError> {
    let name = reloc.target.name().to_string();
    let undefined = || LinkError::UndefinedSymbols(vec![name.clone()]);
    let unsupported = || LinkError::UnsupportedRelocation { reloc_type: reloc.r_type, symbol: name.clone() };
    let dynamic = matches!(state.globals.get(reloc.target.name()), Some(SymbolDef::Dynamic { .. }));
    let tp_offset = |state: &LinkState| -> Result<i64, LinkError> {
        let sym_addr = resolve_symbol(state, &reloc.target, None).ok_or_else(undefined)?;
        Ok(tpoff(Arch::Aarch64, sym_addr, params.tls_start, params.tls_memsz) + reloc.addend)
    };
    // A GOT slot or descriptor pair is one per symbol, so it cannot carry an
    // addend of its own.
    let slot_of = |map: &BTreeMap<SymbolRef, u64>| -> Result<u64, LinkError> {
        if reloc.addend != 0 {
            return Err(unsupported());
        }
        let slot = *map.get(&reloc.target).ok_or_else(undefined)?;
        assert!(slot % 8 == 0, "a TLS GOT slot at {slot:#x} is not 8-byte aligned");
        Ok(slot)
    };
    let write = |state: &mut LinkState, insn: u32| write_u32(state, reloc.section, reloc.offset, insn);

    match reloc.r_type {
        RelocType::Aarch64TlsdescAdrPage21
        | RelocType::Aarch64TlsdescLd64Lo12
        | RelocType::Aarch64TlsdescAddLo12
        | RelocType::Aarch64TlsdescCall => {
            if params.is_shared {
                let pair = slot_of(params.gd_got)?;
                let sec = &mut state.sections[reloc.section].data;
                match reloc.r_type {
                    RelocType::Aarch64TlsdescAdrPage21 => {
                        patch_aarch64_adrp_data(sec, reloc.offset, adrp_page_delta(pair, reloc_vaddr, reloc)?)
                    }
                    RelocType::Aarch64TlsdescLd64Lo12 => {
                        patch_aarch64_ldr_imm12_data(sec, reloc.offset, (pair & 0xFFF) as u32, 3)
                    }
                    RelocType::Aarch64TlsdescAddLo12 => {
                        patch_aarch64_add_imm12_data(sec, reloc.offset, (pair & 0xFFF) as u32)
                    }
                    _ => {}
                }
            } else if dynamic {
                let slot = slot_of(params.got)?;
                match reloc.r_type {
                    RelocType::Aarch64TlsdescAdrPage21 => {
                        let sec = &mut state.sections[reloc.section].data;
                        patch_aarch64_adrp_data(sec, reloc.offset, adrp_page_delta(slot, reloc_vaddr, reloc)?)
                    }
                    RelocType::Aarch64TlsdescLd64Lo12 => {
                        write(state, A64_LDR_X0_X0 | ((((slot & 0xFFF) >> 3) as u32) << 10))
                    }
                    _ => write(state, A64_NOP),
                }
            } else {
                let offset = tp_offset(state)?;
                if !(0..=u32::MAX as i64).contains(&offset) {
                    return Err(LinkError::RelocationOverflow { reloc_type: reloc.r_type, symbol: name, value: offset });
                }
                match reloc.r_type {
                    RelocType::Aarch64TlsdescAdrPage21 => {
                        write(state, A64_MOVZ_X0_LSL16 | ((((offset >> 16) & 0xFFFF) as u32) << 5))
                    }
                    RelocType::Aarch64TlsdescLd64Lo12 => write(state, A64_MOVK_X0 | (((offset & 0xFFFF) as u32) << 5)),
                    _ => write(state, A64_NOP),
                }
            }
        }
        RelocType::Aarch64TlsieAdrGottprelPage21 => {
            let slot = slot_of(params.got)?;
            let sec = &mut state.sections[reloc.section].data;
            patch_aarch64_adrp_data(sec, reloc.offset, adrp_page_delta(slot, reloc_vaddr, reloc)?);
        }
        RelocType::Aarch64TlsieLd64GottprelLo12Nc => {
            let slot = slot_of(params.got)?;
            let sec = &mut state.sections[reloc.section].data;
            patch_aarch64_ldr_imm12_data(sec, reloc.offset, (slot & 0xFFF) as u32, 3);
        }
        RelocType::Aarch64TlsleAddTprelHi12 | RelocType::Aarch64TlsleAddTprelLo12Nc => {
            // Local-exec names an offset from this module's thread pointer,
            // which only the executable has.
            if params.is_shared || dynamic {
                return Err(unsupported());
            }
            let offset = tp_offset(state)?;
            if !(0..1 << 24).contains(&offset) {
                return Err(LinkError::RelocationOverflow { reloc_type: reloc.r_type, symbol: name, value: offset });
            }
            let imm = if reloc.r_type == RelocType::Aarch64TlsleAddTprelHi12 { offset >> 12 } else { offset } & 0xFFF;
            let sec = &mut state.sections[reloc.section].data;
            patch_aarch64_add_imm12_data(sec, reloc.offset, imm as u32);
        }
        _ => unreachable!("not an AArch64 TLS relocation: {}", reloc.r_type),
    }
    Ok(())
}

/// Apply a single AArch64 relocation to section data. Returns `true` if it
/// produced an absolute reference that needs a Mach-O rebase entry.
fn apply_one_reloc_aarch64(
    data: &mut [u8],
    reloc: &InputReloc,
    sym_addr: u64,
    reloc_vaddr: u64,
    got: &BTreeMap<SymbolRef, u64>,
    dyn_got: &BTreeMap<SymbolRef, u64>,
) -> Result<bool, LinkError> {
    // A dynamic import's slot is in `dyn_got`, filled by GLOB_DAT; every other
    // symbol's in `got`.
    let slot = || {
        got.get(&reloc.target)
            .or_else(|| dyn_got.get(&reloc.target))
            .or_else(|| dyn_got.get(&SymbolRef::Global(reloc.target.name().to_string())))
            .copied()
            .ok_or_else(|| LinkError::UndefinedSymbols(vec![reloc.target.name().to_string()]))
    };
    match reloc.r_type {
        RelocType::Aarch64Abs64 => {
            let value = (sym_addr as i64 + reloc.addend) as u64;
            write_u64_data(data, reloc.offset, value);
            Ok(true)
        }
        RelocType::Aarch64Abs32 => {
            let value = sym_addr as i64 + reloc.addend;
            write_u32_data(data, reloc.offset, value as u32);
            Ok(false)
        }
        RelocType::Aarch64Prel32 => {
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            write_i32_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        RelocType::Aarch64Prel64 => {
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            write_u64_data(data, reloc.offset, value as u64);
            Ok(false)
        }
        RelocType::Aarch64LdPrelLo19 | RelocType::Aarch64Condbr19 | RelocType::Aarch64Tstbr14 => {
            // A word offset in bits 5.., 19 bits wide for a literal load and a
            // conditional branch, 14 for a test-and-branch.
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            let bits = if reloc.r_type == RelocType::Aarch64Tstbr14 { 14 } else { 19 };
            let words = value >> 2;
            if value & 3 != 0 || !(-(1 << (bits - 1))..(1 << (bits - 1))).contains(&words) {
                return Err(LinkError::RelocationOverflow {
                    reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value,
                });
            }
            let mask = (1u32 << bits) - 1;
            modify_insn_data(data, reloc.offset, |insn| (insn & !(mask << 5)) | (((words as u32) & mask) << 5));
            Ok(false)
        }
        RelocType::Aarch64AdrPrelLo21 => {
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            if !(-(1 << 20)..(1 << 20)).contains(&value) {
                return Err(LinkError::RelocationOverflow {
                    reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value,
                });
            }
            patch_aarch64_adrp_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        RelocType::Aarch64Call26 | RelocType::Aarch64Jump26 => {
            let value = sym_addr as i64 + reloc.addend - reloc_vaddr as i64;
            let imm26 = value >> 2;
            if !(-(1 << 25)..(1 << 25)).contains(&imm26) {
                return Err(LinkError::RelocationOverflow {
                    reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value,
                });
            }
            patch_aarch64_insn_imm26_data(data, reloc.offset, imm26 as u32);
            Ok(false)
        }
        RelocType::Aarch64MovwUabsG0Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFFF) as u32;
            patch_aarch64_movw_data(data, reloc.offset, value);
            Ok(false)
        }
        RelocType::Aarch64MovwUabsG1Nc => {
            let value = (((sym_addr as i64 + reloc.addend) >> 16) & 0xFFFF) as u32;
            patch_aarch64_movw_data(data, reloc.offset, value);
            Ok(false)
        }
        RelocType::Aarch64MovwUabsG2Nc => {
            let value = (((sym_addr as i64 + reloc.addend) >> 32) & 0xFFFF) as u32;
            patch_aarch64_movw_data(data, reloc.offset, value);
            Ok(false)
        }
        RelocType::Aarch64MovwUabsG3 => {
            let value = (((sym_addr as i64 + reloc.addend) >> 48) & 0xFFFF) as u32;
            patch_aarch64_movw_data(data, reloc.offset, value);
            Ok(false)
        }
        RelocType::Aarch64AdrPrelPgHi21 => {
            let sym_page = (sym_addr as i64 + reloc.addend) & !0xFFF;
            let pc_page = reloc_vaddr as i64 & !0xFFF;
            let page_delta = (sym_page - pc_page) >> 12;
            if !(-(1 << 20)..(1 << 20)).contains(&page_delta) {
                return Err(LinkError::RelocationOverflow {
                    reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value: page_delta,
                });
            }
            patch_aarch64_adrp_data(data, reloc.offset, page_delta as i32);
            Ok(false)
        }
        RelocType::Aarch64AddAbsLo12Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            patch_aarch64_add_imm12_data(data, reloc.offset, value);
            Ok(false)
        }
        RelocType::Aarch64Ldst8AbsLo12Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 0);
            Ok(false)
        }
        RelocType::Aarch64Ldst16AbsLo12Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 1);
            Ok(false)
        }
        RelocType::Aarch64Ldst32AbsLo12Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 2);
            Ok(false)
        }
        RelocType::Aarch64Ldst64AbsLo12Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 3);
            Ok(false)
        }
        RelocType::Aarch64Ldst128AbsLo12Nc => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 4);
            Ok(false)
        }
        RelocType::Aarch64AdrGotPage => {
            let got_slot = slot()?;
            let sym_page = got_slot as i64 & !0xFFF;
            let pc_page = reloc_vaddr as i64 & !0xFFF;
            let page_delta = (sym_page - pc_page) >> 12;
            if !(-(1 << 20)..(1 << 20)).contains(&page_delta) {
                return Err(LinkError::RelocationOverflow {
                    reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value: page_delta,
                });
            }
            patch_aarch64_adrp_data(data, reloc.offset, page_delta as i32);
            Ok(false)
        }
        RelocType::Aarch64Ld64GotLo12Nc => {
            let got_slot = slot()?;
            let value = (got_slot & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 3);
            Ok(false)
        }
        RelocType::Aarch64TlvpLoadPageoff12 => {
            let value = ((sym_addr as i64 + reloc.addend) & 0xFFF) as u32;
            let insn_off = reloc.offset as usize;
            let old_insn = u32::from_le_bytes(data[insn_off..insn_off+4].try_into().unwrap());
            let rd = old_insn & 0x1F;
            let rn = (old_insn >> 5) & 0x1F;
            let new_insn: u32 = 0x91000000 | (value << 10) | (rn << 5) | rd;
            data[insn_off..insn_off+4].copy_from_slice(&new_insn.to_le_bytes());
            Ok(false)
        }
        RelocType::Aarch64TlvpLoadPage21 => {
            let sym_page = (sym_addr as i64 + reloc.addend) & !0xFFF;
            let pc_page = reloc_vaddr as i64 & !0xFFF;
            let page_delta = (sym_page - pc_page) >> 12;
            if !(-(1 << 20)..(1 << 20)).contains(&page_delta) {
                return Err(LinkError::RelocationOverflow {
                    reloc_type: reloc.r_type, symbol: reloc.target.name().to_string(), value: page_delta,
                });
            }
            patch_aarch64_adrp_data(data, reloc.offset, page_delta as i32);
            Ok(false)
        }
        RelocType::Aarch64GotPcrel32 => {
            let got_slot = slot()?;
            let value = got_slot as i64 + reloc.addend - reloc_vaddr as i64;
            check_i32(value, reloc)?;
            write_i32_data(data, reloc.offset, value as i32);
            Ok(false)
        }
        other => Err(LinkError::UnsupportedRelocation {
            reloc_type: other, symbol: reloc.target.name().to_string(),
        }),
    }
}

/// Read-modify-write a 32-bit LE instruction in section data.
fn modify_insn_data(data: &mut [u8], offset: u64, f: impl FnOnce(u32) -> u32) {
    let off = offset as usize;
    let insn = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
    data[off..off + 4].copy_from_slice(&f(insn).to_le_bytes());
}

fn patch_aarch64_adrp_data(data: &mut [u8], offset: u64, page_delta: i32) {
    let val = page_delta as u32;
    modify_insn_data(data, offset, |insn|
        (insn & 0x9F00_001F) | ((val & 0x3) << 29) | (((val >> 2) & 0x7FFFF) << 5));
}

fn patch_aarch64_insn_imm26_data(data: &mut [u8], offset: u64, imm26: u32) {
    modify_insn_data(data, offset, |insn| (insn & 0xFC00_0000) | (imm26 & 0x03FF_FFFF));
}

fn patch_aarch64_movw_data(data: &mut [u8], offset: u64, value: u32) {
    modify_insn_data(data, offset, |insn| (insn & !(0xFFFF << 5)) | ((value & 0xFFFF) << 5));
}

fn patch_aarch64_add_imm12_data(data: &mut [u8], offset: u64, value: u32) {
    modify_insn_data(data, offset, |insn| (insn & !(0xFFF << 10)) | ((value & 0xFFF) << 10));
}

fn patch_aarch64_ldr_imm12_data(data: &mut [u8], offset: u64, value: u32, scale: u32) {
    modify_insn_data(data, offset, |insn| {
        let scaled = (value >> scale) & 0xFFF;
        (insn & !(0xFFF << 10)) | (scaled << 10)
    });
}

/// Parameters for Mach-O relocation application.
pub(crate) struct MachORelocParams<'a> {
    pub(crate) got: &'a BTreeMap<SymbolRef, u64>,
    /// Start of the TLS template (__thread_data vmaddr). Pointers from __thread_vars
    /// into __thread_data/__thread_bss are stored as template-relative offsets.
    pub(crate) tls_template_start: u64,
}

/// Apply relocations for Mach-O output. Returns rebase entries (internal absolute
/// pointers) and bind entries (dynamic symbol references for dyld).
pub(crate) fn apply_relocs_macho(
    state: &mut LinkState,
    params: &MachORelocParams,
) -> Result<MachORelocOutput, LinkError> {
    let mut rebase_entries = Vec::new();
    let mut bind_entries = Vec::new();
    let mut undefined = std::collections::HashSet::new();

    let relocs = std::mem::take(&mut state.relocs);

    for reloc in &relocs {
        let sec = &state.sections[reloc.section];
        let reloc_vaddr = sec.vaddr.unwrap_or_else(|| panic!(
            "reloc section {:?} ({:?}) has no vaddr", sec.name, sec.kind,
        )) + reloc.offset;

        // GOT relocations don't need the symbol address — they use the GOT
        // slot address, which is looked up by name inside the reloc handler.
        let is_got_reloc = matches!(reloc.r_type,
            RelocType::Aarch64AdrGotPage | RelocType::Aarch64Ld64GotLo12Nc
            | RelocType::Aarch64GotPcrel32
            | RelocType::X86Gotpcrel | RelocType::X86Gotpcrelx
            | RelocType::X86RexGotpcrelx);

        // SUBTRACTOR pair: resolve both symbols and compute the difference.
        // The result is position-independent (no rebase entry needed).
        if let Some(ref sub_sym) = reloc.subtrahend {
            let minuend = match resolve_symbol(state, &reloc.target, None) {
                Some(a) => a,
                None => {
                    undefined.insert(reloc.target.name().to_string());
                    continue;
                }
            };
            let subtrahend = match resolve_symbol(state, sub_sym, None) {
                Some(a) => a,
                None => {
                    undefined.insert(sub_sym.name().to_string());
                    continue;
                }
            };
            let value = minuend as i64 - subtrahend as i64 + reloc.addend;
            match reloc.r_type {
                RelocType::Aarch64Abs64 | RelocType::X86_64 =>
                    write_u64(state, reloc.section, reloc.offset, value as u64),
                RelocType::Aarch64Abs32 | RelocType::X86_32 =>
                    write_u32(state, reloc.section, reloc.offset, value as u32),
                RelocType::X86_32S =>
                    write_i32(state, reloc.section, reloc.offset, value as i32),
                other => return Err(LinkError::UnsupportedRelocation {
                    reloc_type: other,
                    symbol: reloc.target.name().to_string(),
                }),
            }
            continue;
        }

        let sym_addr = match resolve_symbol(state, &reloc.target, None) {
            Some(a) => a,
            None if is_got_reloc && params.got.contains_key(&reloc.target) => 0,
            None if matches!(reloc.r_type, RelocType::Aarch64Abs64 | RelocType::X86_64) => {
                // Absolute pointer to an external symbol — dyld will bind it.
                bind_entries.push((reloc.target.name().to_string(), reloc_vaddr));
                continue;
            }
            None if is_movw_to_dynamic(reloc, params) => {
                // Non-PIC MOVW sequence targeting a dynamic symbol: rewrite
                // to ADRP+LDR from GOT (G0→ADRP, G1→LDR, G2/G3→NOP).
                rewrite_movw_to_got(state, reloc, reloc_vaddr, params.got)?;
                continue;
            }
            None => {
                undefined.insert(reloc.target.name().to_string());
                continue;
            }
        };

        // TLV descriptor offset fixup: pointers from __thread_vars into
        // __thread_data/__thread_bss must be template-relative offsets,
        // not absolute virtual addresses.
        let in_tlv_descriptors = sec.kind == SectionKind::TlsVariables;
        let target_is_tls_data = match &reloc.target {
            SymbolRef::Global(name) => matches!(
                state.globals.get(name),
                Some(SymbolDef::Defined { section, .. })
                    if matches!(state.sections[section.0].kind, SectionKind::Tls | SectionKind::TlsBss)
            ),
            SymbolRef::Local(oi, name) => {
                let def = state.globals.get(name)
                    .or_else(|| state.locals.get(&(*oi, name.clone())));
                matches!(
                    def,
                    Some(SymbolDef::Defined { section, .. })
                        if matches!(state.sections[section.0].kind, SectionKind::Tls | SectionKind::TlsBss)
                )
            }
        };
        let sym_addr = if in_tlv_descriptors && target_is_tls_data {
            // Convert absolute vaddr to TLS template offset
            let offset = sym_addr - params.tls_template_start;
            write_u64(state, reloc.section, reloc.offset, (offset as i64 + reloc.addend) as u64);
            // No rebase — this is a template offset, not a pointer
            continue;
        } else {
            sym_addr
        };

        let is_abs = match reloc.r_type {
            // AArch64 relocations
            RelocType::Aarch64Abs64 | RelocType::Aarch64Abs32
            | RelocType::Aarch64Prel32 | RelocType::Aarch64Prel64
            | RelocType::Aarch64LdPrelLo19 | RelocType::Aarch64AdrPrelLo21
            | RelocType::Aarch64Condbr19 | RelocType::Aarch64Tstbr14
            | RelocType::Aarch64Call26 | RelocType::Aarch64Jump26
            | RelocType::Aarch64AdrPrelPgHi21 | RelocType::Aarch64AddAbsLo12Nc
            | RelocType::Aarch64Ldst8AbsLo12Nc | RelocType::Aarch64Ldst16AbsLo12Nc
            | RelocType::Aarch64Ldst32AbsLo12Nc | RelocType::Aarch64Ldst64AbsLo12Nc
            | RelocType::Aarch64Ldst128AbsLo12Nc
            | RelocType::Aarch64MovwUabsG0Nc | RelocType::Aarch64MovwUabsG1Nc
            | RelocType::Aarch64MovwUabsG2Nc | RelocType::Aarch64MovwUabsG3
            | RelocType::Aarch64AdrGotPage | RelocType::Aarch64Ld64GotLo12Nc
            | RelocType::Aarch64GotPcrel32
            | RelocType::Aarch64TlvpLoadPage21
            | RelocType::Aarch64TlvpLoadPageoff12 => {
                apply_one_reloc_aarch64(&mut state.sections[reloc.section].data, reloc, sym_addr, reloc_vaddr, params.got, &BTreeMap::new())?
            }
            // x86-64 relocations
            RelocType::X86_64
            | RelocType::X86Pc32
            | RelocType::X86Plt32
            | RelocType::X86Gotpcrel
            | RelocType::X86Gotpcrelx
            | RelocType::X86RexGotpcrelx
            | RelocType::X86_32
            | RelocType::X86_32S
            | RelocType::X86Tlv => {
                apply_one_reloc_x86(&mut state.sections[reloc.section].data, reloc, sym_addr, reloc_vaddr, params.got, &BTreeMap::new())?
            }
            other => return Err(LinkError::UnsupportedRelocation {
                reloc_type: other, symbol: reloc.target.name().to_string(),
            }),
        };
        if is_abs {
            rebase_entries.push(reloc_vaddr);
        }
    }

    if !undefined.is_empty() {
        let mut syms: Vec<String> = undefined.into_iter().collect();
        syms.sort();
        return Err(LinkError::UndefinedSymbols(syms));
    }

    Ok(MachORelocOutput { rebase_entries, bind_entries })
}

pub(crate) struct MachORelocOutput {
    pub(crate) rebase_entries: Vec<u64>,
    pub(crate) bind_entries: Vec<(String, u64)>,
}

/// Check if a relocation is a MOVW type targeting a dynamic symbol with a GOT slot.
fn is_movw_to_dynamic(reloc: &InputReloc, params: &MachORelocParams) -> bool {
    matches!(reloc.r_type,
        RelocType::Aarch64MovwUabsG0Nc | RelocType::Aarch64MovwUabsG1Nc
        | RelocType::Aarch64MovwUabsG2Nc | RelocType::Aarch64MovwUabsG3)
    && params.got.contains_key(&reloc.target)
}

/// Rewrite a MOVW instruction targeting a dynamic symbol to use the GOT slot.
/// G0 (MOVZ) → ADRP xN, got_page
/// G1 (MOVK) → LDR xN, [xN, #got_lo12]
/// G2/G3     → NOP
fn rewrite_movw_to_got(
    state: &mut LinkState,
    reloc: &InputReloc,
    reloc_vaddr: u64,
    got: &BTreeMap<SymbolRef, u64>,
) -> Result<(), LinkError> {
    let got_slot = *got.get(&reloc.target).unwrap();

    // Read the current instruction to extract the destination register
    let data = &mut state.sections[reloc.section].data;
    let off = reloc.offset as usize;
    let insn = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
    let rd = insn & 0x1F; // destination register in bits [4:0]

    match reloc.r_type {
        RelocType::Aarch64MovwUabsG0Nc => {
            // MOVZ → ADRP xN, got_page
            let adrp = 0x9000_0000 | rd; // ADRP template
            write_u32_data(data, reloc.offset, adrp);
            let sym_page = got_slot as i64 & !0xFFF;
            let pc_page = reloc_vaddr as i64 & !0xFFF;
            let page_delta = (sym_page - pc_page) >> 12;
            patch_aarch64_adrp_data(data, reloc.offset, page_delta as i32);
        }
        RelocType::Aarch64MovwUabsG1Nc => {
            // MOVK → LDR xN, [xN, #got_lo12]   (64-bit load, scale=3)
            let ldr = 0xF940_0000 | (rd << 5) | rd; // LDR X template: Rt=Rd, Rn=Rd
            write_u32_data(data, reloc.offset, ldr);
            let value = (got_slot & 0xFFF) as u32;
            patch_aarch64_ldr_imm12_data(data, reloc.offset, value, 3);
        }
        RelocType::Aarch64MovwUabsG2Nc | RelocType::Aarch64MovwUabsG3 => {
            // NOP
            write_u32_data(data, reloc.offset, 0xD503_201F);
        }
        _ => unreachable!(),
    }
    Ok(())
}

pub(crate) fn apply_relocs_pe(
    state: &mut LinkState,
    layout: &PeLayout,
) -> Result<Vec<u32>, LinkError> {
    let mut undefined = HashSet::new();
    let mut abs_fixups: Vec<u32> = Vec::new(); // RVAs of absolute 64-bit fixups
    let relocs = std::mem::take(&mut state.relocs);

    for reloc in &relocs {
        if matches!(reloc.r_type,
            RelocType::X86Tlsgd | RelocType::X86Tlsld | RelocType::X86Dtpoff32
            | RelocType::X86Tpoff32 | RelocType::X86Gottpoff)
        {
            panic!(
                "TLS relocation {} for symbol {} not supported in PE output",
                reloc.r_type, reloc.target.name()
            );
        }

        let sec = &state.sections[reloc.section];
        let reloc_vaddr = sec.vaddr.unwrap_or_else(|| panic!(
            "reloc section {:?} ({:?}) has no vaddr", sec.name, sec.kind,
        )) + reloc.offset;

        let sym_addr = match resolve_symbol(state, &reloc.target, None) {
            Some(a) => a,
            None => {
                undefined.insert(reloc.target.name().to_string());
                continue;
            }
        };

        let arch = state.arch;
        let data = &mut state.sections[reloc.section].data;
        let is_abs = match arch {
            Arch::X86_64 => apply_one_reloc_x86(data, reloc, sym_addr, reloc_vaddr, &layout.got, &BTreeMap::new())?,
            Arch::Aarch64 => apply_one_reloc_aarch64(data, reloc, sym_addr, reloc_vaddr, &layout.got, &BTreeMap::new())?,
        };
        if is_abs {
            abs_fixups.push(reloc_vaddr as u32);
        }
    }

    // Fill GOT entries
    for &got_vaddr in layout.got.values() {
        abs_fixups.push(got_vaddr as u32);
    }

    if !undefined.is_empty() {
        let mut syms: Vec<String> = undefined.into_iter().collect();
        syms.sort();
        return Err(LinkError::UndefinedSymbols(syms));
    }

    abs_fixups.sort();
    Ok(abs_fixups)
}
