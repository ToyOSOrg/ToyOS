//! The CPU's own registers and instructions, as generic code names them.

use core::arch::asm;

/// The CPU's free-running counter: the generic timer's virtual count,
/// `CNTVCT_EL0`. The `ISB` keeps the read from being taken early, ahead of
/// the code it times (Arm ARM K.a, D12.2.2).
#[inline]
pub fn counter() -> u64 {
    let count: u64;
    // SAFETY: reads a counter EL1 may always read; the `ISB` touches nothing.
    unsafe { asm!("isb", "mrs {}, cntvct_el0", out(reg) count, options(nomem, nostack, preserves_flags)) };
    count
}

/// The counter's frequency as firmware states it in `CNTFRQ_EL0`, which the
/// Arm ARM makes firmware's duty to program. Zero states nothing.
pub fn stated_counter_hz() -> Option<u64> {
    let hz: u64;
    // SAFETY: reads a register EL1 may always read.
    unsafe { asm!("mrs {}, cntfrq_el0", out(reg) hz, options(nomem, nostack, preserves_flags)) };
    (hz != 0).then_some(hz)
}

/// This function's caller's frame pointer: `x29`, which `-Cforce-frame-pointers=yes` makes one.
#[inline(always)]
pub fn frame_pointer() -> u64 {
    let fp: u64;
    // SAFETY: register-to-register move only.
    unsafe { asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags)) };
    fp
}

/// The stack pointer this code is running on.
#[inline(always)]
pub fn stack_pointer() -> u64 {
    let sp: u64;
    // SAFETY: register-to-register move only.
    unsafe { asm!("mov {}, sp", out(reg) sp, options(nomem, nostack, preserves_flags)) };
    sp
}

/// Raise the architecture's undefined-instruction exception here: `UDF`, whose
/// synchronous exception the vectors catch as the kernel's own fault.
pub fn undefined_instruction() {
    // SAFETY: `UDF` reads and writes nothing and raises an exception the
    // installed vectors report.
    unsafe { asm!("udf #0", options(nomem, nostack)) };
}

/// The thread pointer this CPU is running with: `TPIDR_EL0`, which user TLS is addressed from.
#[inline]
pub fn thread_pointer() -> u64 {
    let tp: u64;
    // SAFETY: reads a register EL1 may always read.
    unsafe { asm!("mrs {}, tpidr_el0", out(reg) tp, options(nomem, nostack, preserves_flags)) };
    tp
}

/// Unmask interrupts on this CPU: `DAIF.I` and `DAIF.F`.
pub fn enable_interrupts() {
    // SAFETY: writes two `DAIF` bits; a compiler barrier, so no access moves across it.
    unsafe { asm!("msr daifclr, #3", options(nostack)) };
}

/// Mask interrupts on this CPU.
pub fn disable_interrupts() {
    // SAFETY: writes two `DAIF` bits; a compiler barrier, so no access moves across it.
    unsafe { asm!("msr daifset, #3", options(nostack)) };
}

/// Whether this CPU takes interrupts: `DAIF.I` clear.
pub fn interrupts_enabled() -> bool {
    let daif: u64;
    // SAFETY: reads `DAIF`.
    unsafe { asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack, preserves_flags)) };
    daif & (1 << 7) == 0
}

/// Stop this CPU for good: interrupts masked, then `WFI` forever. A wake
/// that arrives anyway lands back in the loop.
pub fn halt() -> ! {
    disable_interrupts();
    loop {
        // SAFETY: waits for an event; touches no memory.
        unsafe { asm!("wfi", options(nomem, nostack, preserves_flags)) };
    }
}

/// Leave the current stack for good and run `func` on the one ending at `top`,
/// with a zeroed frame chain so a panic there backtraces instead of walking off
/// the top.
/// # Safety
/// Nothing on the current stack is live past this call, and `top` is the end of
/// a 16-byte-aligned stack this CPU owns.
pub unsafe fn run_on_stack(top: u64, func: extern "C" fn() -> !) -> ! {
    // SAFETY: the caller's contract.
    unsafe {
        asm!(
            "mov sp, {sp}",
            "mov x29, xzr",
            "mov x30, xzr",
            "br {func}",
            sp = in(reg) top,
            func = in(reg) func as *const () as usize,
            options(noreturn),
        );
    }
}

/// This CPU's hardware identity: `MPIDR_EL1`'s four affinity fields, packed
/// into 32 bits (`Aff3:Aff2:Aff1:Aff0`), readable before any per-CPU state exists.
pub fn hardware_id() -> u32 {
    let mpidr: u64;
    // SAFETY: reads an ID register.
    unsafe { asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack, preserves_flags)) };
    ((mpidr & 0xFF_FFFF) | ((mpidr >> 8) & 0xFF00_0000)) as u32
}
