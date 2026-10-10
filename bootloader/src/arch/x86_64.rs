//! x86-64: the loader's every instruction Rust has no portable spelling for.

use toyos_abi::boot::KernelArgs;

/// The machine the kernel image must be built for: the loader's own.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::X86_64;

/// How the boot map's entries are encoded.
pub use toyos_bootmap::x86_64 as encoding;

/// How the boot map types memory: by the MTRRs firmware programmed, beneath
/// entries that select plain memory.
pub fn typing(_write_back: &[(u64, u64)]) -> toyos_bootmap::Typing<'_> {
    toyos_bootmap::Typing::Firmware
}

/// The CPU's physical address width, `CPUID.80000008H:EAX[7:0]` (SDM Vol. 2A,
/// CPUID; AMD APM Vol. 3, E.4.7), or why it cannot be read.
pub fn physical_bits() -> Result<u32, alloc::string::String> {
    use core::arch::x86_64::__cpuid;
    let max = __cpuid(0x8000_0000).eax;
    if max < 0x8000_0008 {
        return Err(alloc::format!("CPUID answers extended leaves to {max:#x}, short of 0x80000008's physical address width"));
    }
    Ok(__cpuid(0x8000_0008).eax & 0xFF)
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
    let max = leaf(0).0;
    toyos_tsc::stated_hz((max >= 0x15).then(|| leaf(0x15)), (max >= 0x16).then(|| leaf(0x16)))
}

/// CPUID `leaf`, subleaf `sub`, as `[eax, ebx, ecx, edx]`; `None` on an
/// architecture with no CPUID. The caller asks no leaf past the maximum its
/// range's base leaf states.
pub fn cpuid(leaf: u32, sub: u32) -> Option<[u32; 4]> {
    let r = core::arch::x86_64::__cpuid_count(leaf, sub);
    Some([r.eax, r.ebx, r.ecx, r.edx])
}

/// `CLFLUSH`'s line on every x86-64 part.
const LINE: u64 = 64;

/// Every line of `[at, at + len)` written back out of this CPU's caches and
/// every other CPU's, before this returns.
pub fn write_back(at: u64, len: usize) {
    let mut line = at & !(LINE - 1);
    while line < at + len as u64 {
        // SAFETY: `CLFLUSH` writes back and invalidates the line containing the
        // address and touches nothing else; the caller names memory it
        // allocated, and the instruction faults on nothing a canonical address
        // can be.
        unsafe {
            core::arch::asm!("clflush [{addr}]", addr = in(reg) line as *const u8, options(nostack, preserves_flags));
        }
        line += LINE;
    }
    // SAFETY: `SFENCE` orders those writebacks ahead of whatever ends this
    // machine; it touches no memory or register.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}

/// What the loader says about the CPU as firmware handed it over, or why the
/// kernel cannot run on it. A UEFI x86-64 loader runs in long mode at CPL 0,
/// the state the kernel's entry takes, so there is nothing to say or refuse.
pub fn cpu_as_entered() -> Result<Option<alloc::string::String>, alloc::string::String> {
    Ok(None)
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

    /// One byte from an I/O port, safe as [`inw`] is.
    pub fn inb(port: u16) -> u8 {
        let value: u8;
        // SAFETY: one instruction into the declared output, no memory operand.
        unsafe {
            core::arch::asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
        }
        value
    }
}

/// Switch to the boot map at `args.boot_pml4_addr` and jump to the kernel
/// image's entry through the high half, handing it `args`.
///
/// # Safety
/// The boot map identity-maps the memory this code and its stack run from and
/// maps the kernel image at `PHYS_OFFSET`; `image` is that relocated image,
/// `entry_offset` its entry point's offset in it, and `args` stays where it is
/// until the kernel copies it.
pub unsafe fn enter_kernel(image: (u64, u64), entry_offset: u64, args: &KernelArgs) -> ! {
    let root = args.boot_pml4_addr;
    let entry = crate::PHYS_OFFSET + image.0 + entry_offset;
    // SAFETY: the caller's contract: the switch keeps this code and stack
    // mapped, and the jump lands in the image it mapped.
    unsafe { core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack)) };
    // SAFETY: `kernel.elf`'s entry point takes `&KernelArgs` in `rdi` by the boot
    // protocol `toyos-abi::boot` and the kernel side of it define between them —
    // `sysv64`, because this target's own `"C"` is the Microsoft convention —
    // and this bootloader has no way to check the callee's signature, only to
    // keep its own side of that contract.
    let entry: extern "sysv64" fn(&KernelArgs) -> ! = unsafe { core::mem::transmute(entry) };
    entry(args)
}
