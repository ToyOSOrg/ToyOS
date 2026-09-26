//! AArch64: the loader's every instruction Rust has no portable spelling for.

use toyos_abi::boot::KernelArgs;
use toyos_bootmap::Typing;

/// The machine the kernel image must be built for: the loader's own.
pub const ELF_MACHINE: toyos_elf::Machine = toyos_elf::Machine::Aarch64;

/// How the boot map's descriptors are encoded.
pub use toyos_bootmap::aarch64 as encoding;

/// How the boot map types memory: by firmware's map, since AArch64 has no
/// range registers to do it and every descriptor names its own type.
pub fn typing(write_back: &[(u64, u64)]) -> Typing<'_> {
    Typing::ByMap(write_back)
}

/// The generic timer's virtual count, `CNTVCT_EL0`.
pub fn counter() -> u64 {
    let count: u64;
    // SAFETY: reads a counter EL1 and EL2 may always read; the `ISB` keeps the
    // read in program order.
    unsafe { core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) count, options(nomem, nostack, preserves_flags)) };
    count
}

/// What the loader's report says beside the counter: its rate. Where it counts
/// from is firmware's to say and no register here does.
pub fn counter_origin() -> alloc::string::String {
    let hz: u64;
    // SAFETY: reads a register EL1 and EL2 may always read.
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) hz, options(nomem, nostack, preserves_flags)) };
    alloc::format!("CNTFRQ_EL0 {hz} Hz; the counter's origin is firmware's")
}

/// What the loader says about the CPU as firmware handed it over, or why the
/// kernel cannot run on it: entered at EL2 on a CPU without FEAT_E2H0,
/// `HCR_EL2.E2H` is RES1, so the kernel's entry cannot clear it and every
/// `_el1` register its drop programs would be EL2's own. Refused here, where
/// the console still prints; the entry's read-back of `HCR_EL2` stays as the
/// last line of defence.
pub fn cpu_as_entered() -> Result<Option<alloc::string::String>, alloc::string::String> {
    let current: u64;
    // SAFETY: reads `CurrentEL`, which EL1 and above may read.
    unsafe { core::arch::asm!("mrs {}, currentel", out(reg) current, options(nomem, nostack, preserves_flags)) };
    let el = (current >> 2) & 0b11;
    if el != 2 {
        return Ok(Some(alloc::format!("CPU: entered at EL{el}")));
    }
    let (hcr, mmfr4): (u64, u64);
    // SAFETY: at EL2 both are readable. `ID_AA64MMFR4_EL1` by its encoding,
    // `S3_0_C0_C7_4`, which sits in the ID space an older CPU reads as zero.
    unsafe {
        core::arch::asm!("mrs {}, hcr_el2", out(reg) hcr, options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {}, S3_0_C0_C7_4", out(reg) mmfr4, options(nomem, nostack, preserves_flags));
    }
    let e2h = (hcr >> 34) & 1;
    let e2h0 = (mmfr4 >> 24) & 0xF;
    let state = alloc::format!("CPU: entered at EL2, HCR_EL2.E2H {e2h}, ID_AA64MMFR4_EL1.E2H0 {e2h0:#x}");
    if e2h0 != 0 {
        return Err(alloc::format!(
            "{state}: REFUSED, HCR_EL2.E2H is RES1 on a CPU without FEAT_E2H0, and the kernel's \
             drop to EL1 needs it clear"
        ));
    }
    Ok(Some(alloc::format!("{state}: the kernel's entry writes E2H clear")))
}

/// The smallest data cache line on this machine, from `CTR_EL0.DminLine`.
fn line() -> u64 {
    let ctr: u64;
    // SAFETY: reads `CTR_EL0`, which every level may read.
    unsafe { core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack, preserves_flags)) };
    4 << ((ctr >> 16) & 0xF)
}

/// Every line of `[at, at + len)` cleaned to the point of coherency and
/// invalidated, then `DSB SY` for their completion.
pub fn write_back(at: u64, len: usize) {
    let step = line();
    let mut addr = at & !(step - 1);
    while addr < at + len as u64 {
        // SAFETY: `DC CIVAC` cleans and invalidates the line holding an
        // address the caller allocated; it changes no memory's contents.
        unsafe { core::arch::asm!("dc civac, {}", in(reg) addr, options(nostack, preserves_flags)) };
        addr += step;
    }
    // SAFETY: a barrier; waits for the maintenance above to complete.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// AArch64 has no I/O port space: every caller checks [`pio::EXISTS`] first.
pub mod pio {
    pub const EXISTS: bool = false;

    /// # Safety
    /// Never called: [`EXISTS`] is false.
    pub unsafe fn outw(_port: u16, _value: u16) {
        unreachable!("AArch64 has no I/O port space")
    }

    pub fn inw(_port: u16) -> u16 {
        unreachable!("AArch64 has no I/O port space")
    }
}

/// Hand the CPU to the kernel as firmware left it — its exception level, its
/// identity tables — at the image's physical entry, with `x0 = args`. The
/// kernel's entry switches to the boot map (`args.boot_pml4_addr`) itself (`kernel/src/arch/aarch64/boot.rs`),
/// because at EL2 only the kernel's own drop to EL1 can install it.
///
/// The image is cleaned to the point of coherency first: that entry fetches
/// instructions with the MMU off for a few of them, straight from memory.
///
/// # Safety
/// `args.boot_pml4_addr` is the boot map, `image` is the relocated kernel image, `entry_offset`
/// is its entry point's offset in it, and `args` stays where it is until the
/// kernel copies it.
pub unsafe fn enter_kernel(image: (u64, u64), entry_offset: u64, args: &KernelArgs) -> ! {
    write_back(image.0, image.1 as usize);
    let entry = image.0 + entry_offset;
    // SAFETY: interrupts masked for good — the kernel's vectors are not
    // installed yet — then every instruction cache line invalidated against
    // the image just cleaned, and a branch to its entry with `x0 = args`, the
    // boot protocol `toyos-abi::boot` and the kernel's `_start` define.
    unsafe {
        core::arch::asm!(
            "msr daifset, #0xf",
            "ic iallu",
            "dsb ish",
            "isb",
            "br {entry}",
            entry = in(reg) entry,
            in("x0") args as *const KernelArgs,
            options(noreturn),
        );
    }
}
