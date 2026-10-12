//! The x86-64 kernel executes no instruction that touches a thread's user FP
//! or vector state but where `arch::fpu` and `arch::entry` save, restore and
//! first load it. That state stays in the registers across every entry
//! (`kernel/src/arch/x86_64/fpu.rs`), so one stray instruction anywhere else
//! corrupts a thread silently; rustc refuses an `xmm` operand on the
//! soft-float target but not an instruction written as `asm!` text, and the
//! target's `x87` is the compiler's to use. So this reads the linked image.
//!
//! Every executable section is decoded linearly from its start, in the mode
//! [`MODES`] says, and a byte that does not decode or an instruction that runs
//! over a symbol's first byte is refused: the sweep checks that no data sits
//! in the code rather than assuming it. An instruction touches the state when
//! it is one of the [`UNMARKED`], when `iced-x86` names x87, MMX, 3DNow!, any
//! SSE, AVX, AVX-512, AMX, FMA, F16C, XOP or an extension on their registers
//! among its CPUID features, or when it uses an x87, MMX, XMM, YMM, ZMM,
//! opmask or tile register, explicitly or implicitly; the fences, whose
//! features say SSE and which touch no state, are the exception
//! ([`STATE_FREE`]). Of those instructions only the [`ALLOWED`] mnemonics
//! pass, and only in a function of an [`OWNERS`] module.

use iced_x86::{CpuidFeature, Decoder, DecoderOptions, Instruction, InstructionInfoFactory, Mnemonic, Register};
use toyos_elf::section::SectionHeader;
use toyos_elf::{FileHeader, Layout, Machine, SectionTable, SymTab};

/// `SHT_PROGBITS` and `SHF_EXECINSTR` (System V gABI, "Sections").
const SHT_PROGBITS: u32 = 1;
const SHF_EXECINSTR: u64 = 0x4;
/// `STT_NOTYPE`: what an `asm` label is, the AP trampoline's among them.
const STT_NOTYPE: u8 = 0;

/// Where the code changes mode: the AP trampoline
/// (`kernel/src/arch/x86_64/smp.rs`) starts in real mode, enters protected
/// mode at its second label and long mode at its third. Everything before the
/// first label is 64-bit.
const MODES: [(&str, u32); 3] = [("_trampoline_start", 16), ("_ap_pm32", 32), ("_ap_lm64", 64)];

/// Instructions that touch the state with neither a feature nor a register
/// that says so: the save family, which moves it whole, and `WAIT`, which reads
/// the x87 status word and raises a thread's pending x87 exception.
const UNMARKED: [Mnemonic; 17] = [
    Mnemonic::Fxsave,
    Mnemonic::Fxsave64,
    Mnemonic::Fxrstor,
    Mnemonic::Fxrstor64,
    Mnemonic::Xsave,
    Mnemonic::Xsave64,
    Mnemonic::Xsavec,
    Mnemonic::Xsavec64,
    Mnemonic::Xsaveopt,
    Mnemonic::Xsaveopt64,
    Mnemonic::Xsaves,
    Mnemonic::Xsaves64,
    Mnemonic::Xrstor,
    Mnemonic::Xrstor64,
    Mnemonic::Xrstors,
    Mnemonic::Xrstors64,
    Mnemonic::Wait,
];

/// What the owners use: the three save forms, their two restores, and
/// `fpu::init`'s `FXSAVE64` check of the declared state.
const ALLOWED: [Mnemonic; 6] = [
    Mnemonic::Xsaves64,
    Mnemonic::Xsavec64,
    Mnemonic::Xsaveopt64,
    Mnemonic::Xrstors64,
    Mnemonic::Xrstor64,
    Mnemonic::Fxsave64,
];

/// Instructions whose CPUID features say SSE or SSE2 and which read and
/// write no register state: the fences the kernel orders memory with.
const STATE_FREE: [Mnemonic; 3] = [Mnemonic::Lfence, Mnemonic::Mfence, Mnemonic::Sfence];

/// The modules whose functions may run an [`ALLOWED`] instruction, as their
/// demangled paths begin.
const OWNERS: [&str; 2] = ["kernel::arch::x86_64::fpu::", "kernel::arch::x86_64::entry::"];

/// How many refused instructions a refusal names before it counts the rest.
const NAMED: usize = 16;

/// Refuse a kernel image that touches the user FP or vector state outside its
/// owners, naming each instruction by function, offset and mnemonic.
pub fn judge(kernel: &[u8]) -> Result<(), String> {
    let header = FileHeader::parse(kernel).map_err(|e| format!("the kernel is no ELF image: {e:?}"))?;
    if header.machine != Machine::X86_64 {
        return Err(format!("the kernel is for {:?}, and this reads x86-64", header.machine));
    }
    let table = toyos_symbols::file_range(kernel, header.shoff, toyos_symbols::shdr_table_len(&header))
        .ok_or("the kernel's section headers lie outside it")?;
    let sections: Vec<SectionHeader> = SectionTable::new(table)
        .iter()
        .filter(|s| s.kind == SHT_PROGBITS && s.flags & SHF_EXECINSTR != 0)
        .collect();
    if sections.is_empty() {
        return Err("the kernel has no executable section".into());
    }
    let functions = Functions::of(kernel, &header, &sections)?;
    let mut refused = Vec::new();
    let mut boundaries: Vec<(u64, u32)> = Vec::new();
    for (label, bitness) in MODES {
        let at = functions.start_of(label).ok_or_else(|| format!("the kernel names no `{label}`"))?;
        boundaries.push((at, bitness));
    }
    for section in &sections {
        let bytes = toyos_symbols::file_range(kernel, section.offset, section.size)
            .ok_or("an executable section lies outside the kernel")?;
        let end = section.addr + section.size;
        let mut cuts: Vec<(u64, u32)> = vec![(section.addr, 64)];
        cuts.extend(boundaries.iter().copied().filter(|&(at, _)| at > section.addr && at < end));
        cuts.push((end, 64));
        for run in cuts.windows(2) {
            let ((from, bitness), (to, _)) = (run[0], run[1]);
            let code = &bytes[(from - section.addr) as usize..(to - section.addr) as usize];
            refused.extend(sweep(code, from, bitness, &functions));
        }
    }
    if refused.is_empty() {
        return Ok(());
    }
    let named: Vec<&str> = refused.iter().take(NAMED).map(String::as_str).collect();
    Err(format!(
        "the kernel touches the user FP or vector state outside `arch::fpu` and `arch::entry`, \
         which a thread keeps in its registers across every entry: {} instruction(s), the first \
         {}:\n  {}\nThe kernel is soft-float and saves that state only at a switch; an instruction \
         anywhere else corrupts a thread's registers with no fault.",
        refused.len(),
        named.len(),
        named.join("\n  "),
    ))
}

/// Every function's and label's first byte, sorted, with its raw name.
struct Functions<'a> {
    starts: Vec<(u64, &'a str)>,
}

impl<'a> Functions<'a> {
    fn of(kernel: &'a [u8], header: &FileHeader, sections: &[SectionHeader]) -> Result<Self, String> {
        let (syms, strs) = toyos_symbols::locate(kernel).ok_or("the kernel has no readable `.symtab`")?;
        let table = SymTab::new(syms, strs);
        let extent = Layout::parse(kernel, header.machine).map_err(|e| format!("{e}"))?.extent();
        let in_code = |at: u64| sections.iter().any(|s| at >= s.addr && at < s.addr + s.size);
        let mut starts: Vec<(u64, &str)> = table
            .defined()
            .filter(|(_, sym)| matches!(sym.kind(), toyos_elf::sym::STT_FUNC | STT_NOTYPE))
            .filter_map(|(i, sym)| Some((extent.min() + sym.address(extent)?.get(), table.name(i))))
            .filter(|&(at, name)| in_code(at) && !name.is_empty())
            .collect();
        starts.sort_unstable();
        Ok(Self { starts })
    }

    fn start_of(&self, name: &str) -> Option<u64> {
        self.starts.iter().find(|&&(_, n)| n == name).map(|&(at, _)| at)
    }

    /// The function `at` lies in, demangled, and how far into it.
    fn name(&self, at: u64) -> (String, u64) {
        match self.starts.partition_point(|&(start, _)| start <= at) {
            0 => ("(before every function)".into(), at),
            i => {
                let (start, raw) = self.starts[i - 1];
                (toyos_symbols::demangled(raw).to_string(), at - start)
            }
        }
    }
}

/// Every refusal in `code`, which runs at `at` in `bitness`-bit mode.
fn sweep(code: &[u8], at: u64, bitness: u32, functions: &Functions) -> Vec<String> {
    let mut decoder = Decoder::with_ip(bitness, code, at, DecoderOptions::NONE);
    let mut factory = InstructionInfoFactory::new();
    let mut instr = Instruction::default();
    let mut next = functions.starts.partition_point(|&(start, _)| start <= at);
    let mut refused = Vec::new();
    while decoder.can_decode() {
        decoder.decode_out(&mut instr);
        let (from, to) = (instr.ip(), instr.next_ip());
        let say = |why: &str| {
            let (function, within) = functions.name(from);
            let bytes = &code[(from - at) as usize..(to.min(at + code.len() as u64) - at) as usize];
            format!("{function}+{within:#x} ({from:#x}): {why} {bytes:02x?}")
        };
        while next < functions.starts.len() && functions.starts[next].0 <= from {
            next += 1;
        }
        if next < functions.starts.len() && functions.starts[next].0 < to {
            refused.push(say(&format!("runs over `{}`'s first byte", functions.starts[next].1)));
        }
        if instr.is_invalid() {
            refused.push(say("does not decode"));
            continue;
        }
        if !touches_state(&instr, &mut factory) {
            continue;
        }
        let owned = || OWNERS.iter().any(|owner| functions.name(from).0.starts_with(owner));
        if !(ALLOWED.contains(&instr.mnemonic()) && owned()) {
            refused.push(say(&format!("{:?}", instr.mnemonic())));
        }
    }
    refused
}

/// Whether `instr` reads or writes any of the user FP or vector state.
fn touches_state(instr: &Instruction, factory: &mut InstructionInfoFactory) -> bool {
    let mnemonic = instr.mnemonic();
    if STATE_FREE.contains(&mnemonic) {
        return false;
    }
    UNMARKED.contains(&mnemonic)
        || instr.cpuid_features().iter().any(|&feature| vector_feature(feature))
        || factory.info(instr).used_registers().iter().any(|used| vector_register(used.register()))
}

fn vector_feature(feature: CpuidFeature) -> bool {
    use CpuidFeature::*;
    matches!(
        feature,
        FPU | FPU287
            | FPU287XL_ONLY
            | FPU387
            | FPU387SL_ONLY
            | CYRIX_FPU
            | MMX
            | D3NOW
            | D3NOWEXT
            | CYRIX_D3NOW
            | SSE
            | SSE2
            | SSE3
            | SSSE3
            | SSE4_1
            | SSE4_2
            | SSE4A
            | AVX
            | AVX2
            | AVX_VNNI
            | AVX_IFMA
            | AVX_NE_CONVERT
            | AVX_VNNI_INT8
            | AVX512F
            | AVX512VL
            | AVX512BW
            | AVX512CD
            | AVX512DQ
            | AVX512ER
            | AVX512PF
            | AVX512_4FMAPS
            | AVX512_4VNNIW
            | AVX512_BF16
            | AVX512_BITALG
            | AVX512_FP16
            | AVX512_IFMA
            | AVX512_VBMI
            | AVX512_VBMI2
            | AVX512_VNNI
            | AVX512_VP2INTERSECT
            | AVX512_VPOPCNTDQ
            | FMA
            | FMA4
            | F16C
            | XOP
            | AVX_VNNI_INT16
            | KNC
            | AMX_TILE
            | AMX_INT8
            | AMX_BF16
            | AMX_FP16
            | AMX_COMPLEX
            | CYRIX_EMMI
            | AES
            | VAES
            | PCLMULQDQ
            | VPCLMULQDQ
            | GFNI
            | SHA
            | SHA512
            | SM3
            | SM4
            | KL
            | AESKLE
            | WIDE_KL
    )
}

fn vector_register(register: Register) -> bool {
    register.is_st()
        || register.is_mm()
        || register.is_xmm()
        || register.is_ymm()
        || register.is_zmm()
        || register.is_k()
        || register.is_tmm()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELSEWHERE: &str = "kernel::arch::x86_64::hw::halt";
    const FPU_SAVE: &str = "kernel::arch::x86_64::fpu::save";
    const AT: u64 = 0x1000;

    fn one(name: &'static str) -> Functions<'static> {
        Functions { starts: vec![(AT, name)] }
    }

    /// The one instruction `bytes` decodes to, so a case cannot test an
    /// encoding it does not name.
    fn decoded(bytes: &[u8]) -> Mnemonic {
        let mut decoder = Decoder::with_ip(64, bytes, AT, DecoderOptions::NONE);
        let instr = decoder.decode();
        assert_eq!(instr.len(), bytes.len(), "{bytes:02x?} is more than one instruction");
        instr.mnemonic()
    }

    fn refusals(bytes: &[u8], function: &'static str) -> Vec<String> {
        sweep(bytes, AT, 64, &one(function))
    }

    /// One encoding per class the gate names, each touching the state another
    /// way: by an explicit register, by an implicit one, or by a control or
    /// status word no operand names.
    const STATE: &[(&[u8], Mnemonic)] = &[
        (&[0xdd, 0x00], Mnemonic::Fld),
        (&[0xdb, 0xe3], Mnemonic::Fninit),
        (&[0xd9, 0x28], Mnemonic::Fldcw),
        (&[0x9b], Mnemonic::Wait),
        (&[0x0f, 0xfc, 0xc1], Mnemonic::Paddb),
        (&[0x0f, 0x77], Mnemonic::Emms),
        (&[0x0f, 0x0f, 0xc1, 0x9e], Mnemonic::Pfadd),
        (&[0x0f, 0x28, 0xc1], Mnemonic::Movaps),
        (&[0x0f, 0xae, 0x10], Mnemonic::Ldmxcsr),
        (&[0x66, 0x0f, 0xfe, 0xc1], Mnemonic::Paddd),
        (&[0x66, 0x0f, 0x38, 0x00, 0xc1], Mnemonic::Pshufb),
        (&[0x66, 0x0f, 0x38, 0xdc, 0xc1], Mnemonic::Aesenc),
        (&[0xc5, 0xf5, 0xfe, 0xc2], Mnemonic::Vpaddd),
        (&[0xc5, 0xf8, 0x77], Mnemonic::Vzeroupper),
        (&[0xc5, 0xf8, 0xae, 0x10], Mnemonic::Vldmxcsr),
        (&[0x62, 0xf1, 0x75, 0x48, 0xfe, 0xc2], Mnemonic::Vpaddd),
        (&[0xc5, 0xf8, 0x92, 0xc8], Mnemonic::Kmovw),
        (&[0xc4, 0xe2, 0x71, 0x98, 0xc2], Mnemonic::Vfmadd132ps),
        (&[0xc4, 0xe2, 0x79, 0x13, 0xc1], Mnemonic::Vcvtph2ps),
        (&[0x8f, 0xe8, 0x70, 0xa2, 0xc2, 0x30], Mnemonic::Vpcmov),
        (&[0xc4, 0xe2, 0x78, 0x49, 0xc0], Mnemonic::Tilerelease),
    ];

    #[test]
    fn every_class_of_state_instruction_is_refused_outside_the_owners() {
        let passed: Vec<Mnemonic> = STATE
            .iter()
            .filter(|&&(bytes, mnemonic)| {
                assert_eq!(decoded(bytes), mnemonic, "{bytes:02x?}");
                refusals(bytes, ELSEWHERE).is_empty()
            })
            .map(|&(_, mnemonic)| mnemonic)
            .collect();
        assert!(passed.is_empty(), "passed outside the owners: {passed:?}");
    }

    #[test]
    fn a_state_instruction_is_refused_inside_an_owner_too() {
        let passed: Vec<Mnemonic> = STATE
            .iter()
            .filter(|&&(bytes, _)| refusals(bytes, FPU_SAVE).is_empty())
            .map(|&(_, mnemonic)| mnemonic)
            .collect();
        assert!(passed.is_empty(), "passed inside `fpu`: {passed:?}");
    }

    /// The six the owners use, each on `[rax]`.
    const SAVES: &[(&[u8], Mnemonic)] = &[
        (&[0x48, 0x0f, 0xc7, 0x28], Mnemonic::Xsaves64),
        (&[0x48, 0x0f, 0xc7, 0x20], Mnemonic::Xsavec64),
        (&[0x48, 0x0f, 0xae, 0x30], Mnemonic::Xsaveopt64),
        (&[0x48, 0x0f, 0xc7, 0x18], Mnemonic::Xrstors64),
        (&[0x48, 0x0f, 0xae, 0x28], Mnemonic::Xrstor64),
        (&[0x48, 0x0f, 0xae, 0x00], Mnemonic::Fxsave64),
    ];

    #[test]
    fn the_saves_the_owners_use_pass_inside_them() {
        for owner in ["kernel::arch::x86_64::fpu::save", "kernel::arch::x86_64::entry::thread_start"] {
            for &(bytes, mnemonic) in SAVES {
                assert_eq!(decoded(bytes), mnemonic, "{bytes:02x?}");
                assert_eq!(refusals(bytes, owner), Vec::<String>::new(), "{mnemonic:?} in {owner}");
            }
        }
    }

    #[test]
    fn the_saves_the_owners_use_are_refused_anywhere_else() {
        let passed: Vec<Mnemonic> = SAVES
            .iter()
            .filter(|&&(bytes, _)| refusals(bytes, ELSEWHERE).is_empty())
            .map(|&(_, mnemonic)| mnemonic)
            .collect();
        assert!(passed.is_empty(), "passed outside the owners: {passed:?}");
    }

    #[test]
    fn a_save_the_owners_do_not_use_is_refused_inside_them() {
        let unused: &[(&[u8], Mnemonic)] = &[
            (&[0x48, 0x0f, 0xae, 0x20], Mnemonic::Xsave64),
            (&[0x48, 0x0f, 0xae, 0x08], Mnemonic::Fxrstor64),
            (&[0x0f, 0xc7, 0x28], Mnemonic::Xsaves),
        ];
        for &(bytes, mnemonic) in unused {
            assert_eq!(decoded(bytes), mnemonic, "{bytes:02x?}");
            assert_eq!(refusals(bytes, FPU_SAVE).len(), 1, "{mnemonic:?}");
        }
    }

    #[test]
    fn the_fences_and_xcr0_pass_anywhere() {
        let free: &[(&[u8], Mnemonic)] = &[
            (&[0x0f, 0xae, 0xe8], Mnemonic::Lfence),
            (&[0x0f, 0xae, 0xf0], Mnemonic::Mfence),
            (&[0x0f, 0xae, 0xf8], Mnemonic::Sfence),
            (&[0x0f, 0x01, 0xd0], Mnemonic::Xgetbv),
            (&[0x0f, 0x01, 0xd1], Mnemonic::Xsetbv),
        ];
        for &(bytes, mnemonic) in free {
            assert_eq!(decoded(bytes), mnemonic, "{bytes:02x?}");
            assert_eq!(refusals(bytes, ELSEWHERE), Vec::<String>::new(), "{mnemonic:?}");
        }
    }

    #[test]
    fn a_byte_that_does_not_decode_is_refused() {
        // `push es`, which long mode has no encoding for.
        assert_eq!(refusals(&[0x06], ELSEWHERE).len(), 1);
    }

    #[test]
    fn an_instruction_over_a_functions_first_byte_is_refused() {
        // `mov eax, imm32`, five bytes, with a function starting at its third.
        let functions = Functions { starts: vec![(AT, ELSEWHERE), (AT + 2, "kernel::later")] };
        let refused = sweep(&[0xb8, 0x90, 0x90, 0x90, 0x90], AT, 64, &functions);
        assert_eq!(refused.len(), 1, "{refused:?}");
    }
}
