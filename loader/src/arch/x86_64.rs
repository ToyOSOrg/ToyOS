//! x86-64: the loader's every instruction Rust has no portable spelling for.

use toyos_abi::boot::KernelArgs;

/// `e_machine` of the kernel this loader starts (`toyos_kimage::Kernel::machine`).
pub const E_MACHINE: u16 = toyos_kimage::EM_X86_64;

/// How the boot map's entries are encoded.
pub use toyos_bootmap::x86_64 as encoding;

/// How the boot map types memory: by the MTRRs firmware programmed, beneath
/// entries that select plain memory.
pub fn typing(_write_back: &[(u64, u64)]) -> toyos_bootmap::Typing<'_> {
    toyos_bootmap::Typing::Firmware
}

/// The time-stamp counter, which counts from reset.
pub fn counter() -> u64 {
    // SAFETY: RDTSC reads a counter and nothing else; every x86-64 has it.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// [`counter`]'s rate in hertz as CPUID states it, or `None` on a part that
/// states none: the kernel's `stated_counter_hz`, read before the kernel runs.
pub fn counter_hz() -> Option<u64> {
    use core::arch::x86_64::__cpuid;
    // A leaf past the maximum is never asked for, so none is misread as data.
    let leaf = |at: u32| {
        let r = __cpuid(at);
        (r.eax, r.ebx, r.ecx, r.edx)
    };
    toyos_tsc::stated_hz((leaf(0).0 >= 0x15).then(|| leaf(0x15)))
}

/// This CPU as its vendor's microcode names it, or why it loads none.
pub fn microcode_cpu() -> Option<Result<toyos_microcode::Cpu, &'static str>> {
    use core::arch::x86_64::__cpuid;
    let id = __cpuid(0);
    let leaf_1 = __cpuid(1);
    // CPUID.01H:ECX[31], which a hypervisor sets for its guest (APM Vol. 3,
    // 24594 rev 3.36, §E.3.2).
    if leaf_1.ecx & 1 << 31 != 0 {
        return Some(Err("a hypervisor owns this CPU's microcode"));
    }
    let signature = leaf_1.eax;
    let vendor: [u8; 12] = core::array::from_fn(|i| [id.ebx, id.edx, id.ecx][i / 4].to_le_bytes()[i % 4]);
    Some(match &vendor {
        b"GenuineIntel" => {
            const IA32_PLATFORM_ID: u32 = 0x17;
            let (low, high): (u32, u32);
            // SAFETY: RDMSR of `IA32_PLATFORM_ID`, which every Intel part
            // the loader runs on has (SDM Vol. 4 Table 2-2), reads it and
            // nothing else.
            unsafe {
                core::arch::asm!("rdmsr", in("ecx") IA32_PLATFORM_ID, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
            }
            let platform = toyos_microcode::PlatformId::from_msr(u64::from(high) << 32 | u64::from(low));
            Ok(toyos_microcode::Cpu::Intel { signature, platform })
        }
        b"AuthenticAMD" => Ok(toyos_microcode::Cpu::Amd { signature }),
        _ => Err("ToyOS loads no microcode of this CPU's vendor"),
    })
}

/// What the loader says about the CPU as firmware handed it over, or why the
/// kernel cannot run on it. A UEFI x86-64 loader runs in long mode at CPL 0,
/// the state the kernel's entry takes; what is left to refuse is a CPU with
/// no execute-disable, which the boot map's data pages are mapped with.
pub fn cpu_as_entered() -> Result<Option<alloc::string::String>, alloc::string::String> {
    use core::arch::x86_64::__cpuid;
    // CPUID.80000001H:EDX[20] (SDM Vol. 2A, CPUID).
    let has_nx = __cpuid(0x8000_0000).eax >= 0x8000_0001 && __cpuid(0x8000_0001).edx & 1 << 20 != 0;
    if !has_nx {
        return Err(alloc::string::String::from(
            "CPU: no execute-disable (CPUID.80000001H:EDX[20]), so no boot map can keep its data from being executed",
        ));
    }
    Ok(None)
}

/// The pages the boot map holds executable beside the kernel's text: the one
/// the switch to it runs from, and the one each AP starts in.
pub fn code_pages() -> [u64; 2] {
    [switch_to_kernel as *const () as u64 & !0xFFF, encoding::AP_TRAMPOLINE]
}

// Sixteen-byte aligned and eight bytes long, so it lies in one page: the one
// `code_pages` names. `rdi` is the boot map's root, `rsi` the kernel's entry
// and `rdx` its argument.
core::arch::global_asm!(
    ".text",
    ".p2align 4",
    ".global switch_to_kernel",
    "switch_to_kernel:",
    "mov cr3, rdi",
    "mov rdi, rdx",
    "jmp rsi",
);

unsafe extern "sysv64" {
    /// Load `root` and jump to `entry` with `args` in `rdi`, as the boot
    /// protocol `toyos-abi::boot` and the kernel's entry define between them.
    ///
    /// # Safety
    /// `root` maps this function's page executable at identity and `entry`
    /// executable, with `IA32_EFER.NXE` set if it uses bit 63.
    fn switch_to_kernel(root: u64, entry: u64, args: &KernelArgs) -> !;
}

/// The I/O port space the chipset's TCO block answers in.
pub mod pio {
    /// Whether this architecture has an I/O port space at all.
    pub const EXISTS: bool = true;

    /// # Safety
    /// No fault in Ring 0; the caller owns which device answers at `port` and
    /// what the word commands it to do. `kernel/src/arch/x86_64/cpu.rs` states
    /// the same contract for the same instruction.
    pub unsafe fn outw(port: u16, value: u16) {
        // SAFETY: the caller's contract.
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags))
        };
    }

    /// One word from an I/O port; safe because a read has no value a caller can
    /// get wrong, as `kernel/src/arch/x86_64/cpu.rs`'s `inw` is.
    pub fn inw(port: u16) -> u16 {
        let value: u16;
        // SAFETY: one instruction into the declared output, no memory operand.
        unsafe {
            core::arch::asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        value
    }
}

/// Switch to the boot map at `args.boot_pml4_addr` and jump to the kernel
/// image's entry through the high half, handing it `args`.
///
/// # Safety
/// The boot map holds [`code_pages`] executable at identity and maps the
/// kernel image at `PHYS_OFFSET`; `image` is that relocated image,
/// `entry_offset` its entry point's offset in it, and `args` stays where it is
/// until the kernel copies it.
pub unsafe fn enter_kernel(image: (u64, u64), entry_offset: u64, args: &KernelArgs) -> ! {
    const IA32_EFER: u32 = 0xC000_0080;
    const NXE: u32 = 1 << 11;
    let root = args.boot_pml4_addr;
    let entry = crate::PHYS_OFFSET + image.0 + entry_offset;
    // SAFETY: `cpu_as_entered` found execute-disable, so the bit is this
    // CPU's to set (SDM Vol. 3A §2.2.1), and under firmware's tables it
    // changes nothing: an entry of theirs with bit 63 set already means it.
    unsafe {
        core::arch::asm!(
            "rdmsr",
            "or eax, {nxe}",
            "wrmsr",
            nxe = const NXE,
            in("ecx") IA32_EFER,
            out("eax") _,
            out("edx") _,
            options(nomem, nostack),
        );
    }
    // SAFETY: the caller's contract, and `NXE` is set above.
    unsafe { switch_to_kernel(root, entry, args) }
}
