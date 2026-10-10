//! What `IA32_EFER` holds on every x86-64 CPU once the kernel has declared it:
//! the value `arch::control_regs` writes whole and then asserts equal to what
//! the CPU reads back.
//!
//! `LMA` is in the value. The CPU sets it on entering long mode and it is 1
//! for as long as the kernel runs, so a value without it is not one this CPU
//! can hold. Intel's SDM marks the bit read-only, and Intel CPUs and QEMU
//! ignore a write that clears it; an AMD Zen 2 CPU faults on that write.

#![forbid(unsafe_code)]

/// `IA32_EFER`: Intel SDM Vol. 3A §2.2.1, "Extended Feature Enable Register",
/// address and bits from Vol. 4 Table 2-2; AMD APM Vol. 2 §3.1.7, "Extended
/// Feature Enable Register (EFER)".
pub const MSR: u32 = 0xC000_0080;
pub const SCE: u64 = 1 << 0;
pub const LME: u64 = 1 << 8;
/// Long mode active: SDM Vol. 4 Table 2-2 names it "IA-32e Mode Active (R)";
/// APM Vol. 2 §3.1.7, "Long Mode Active (LMA) Bit", has the processor set it
/// when long mode and paging are both enabled.
pub const LMA: u64 = 1 << 10;
pub const NXE: u64 = 1 << 11;

/// `IA32_EFER` on every CPU: `SCE`, `LME`, `LMA`, `NXE`. `SCE` is declared
/// only here, never by `arch::syscall::init`, so one register keeps one owner.
pub const DECLARED: u64 = SCE | LME | LMA | NXE;

#[cfg(test)]
mod tests {
    use super::*;

    /// The declaration as a number, so a bit gained or lost reads in review.
    #[test]
    fn the_declaration_is_sce_lme_lma_nxe() {
        assert_eq!(DECLARED, 0xD01);
    }

    /// What an AMD Zen 2 laptop's `IA32_EFER` held when the loader handed
    /// over — `LME` and `LMA`, read back before the kernel's write that
    /// faulted. Writing the declaration over it may add `SCE` and `NXE`, and
    /// must change neither bit that says the CPU is in long mode.
    #[test]
    fn writing_the_declaration_keeps_a_long_mode_cpus_mode_bits() {
        const HANDED_OVER: u64 = 0x500;
        assert_eq!(HANDED_OVER & (LME | LMA), LME | LMA);
        let changed = DECLARED ^ HANDED_OVER;
        assert_eq!(changed & (LME | LMA), 0, "the write changes {changed:#x}");
        assert_eq!(changed, SCE | NXE);
    }
}
