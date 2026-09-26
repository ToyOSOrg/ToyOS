//! x86-64: the entry, the string instructions, and SSE2's square roots.

/// Entry point for C programs. Stack layout at entry (set up by kernel):
///   [RSP]   = argc
///   [RSP+8] = argv[0], argv[1], ..., NULL
#[cfg(not(feature = "std-runtime"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "mov rdi, [rsp]",      // argc
        "lea rsi, [rsp + 8]",  // argv
        "call {start_c}",
        "ud2",
        start_c = sym crate::runtime::start_c,
    );
}

/// Copy `n` bytes from `src` to `dest`, lowest address first.
pub(crate) unsafe fn copy_forward(dest: *mut u8, src: *const u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "rep movsb",
            inout("rdi") dest => _,
            inout("rsi") src => _,
            inout("rcx") n => _,
            options(nostack),
        );
    }
}

/// Copy `n` bytes from `src` to `dest`, highest address first: the order a
/// `dest` overlapping `src` from above needs.
pub(crate) unsafe fn copy_backward(dest: *mut u8, src: *const u8, n: usize) {
    if n == 0 {
        return;
    }
    // The direction flag is set only across this one `rep`; every Ring 0 entry
    // clears it for the kernel's own sake.
    unsafe {
        core::arch::asm!(
            "std",
            "rep movsb",
            "cld",
            inout("rdi") dest.add(n - 1) => _,
            inout("rsi") src.add(n - 1) => _,
            inout("rcx") n => _,
            options(nostack),
        );
    }
}

/// Set `n` bytes at `dest` to `byte`.
pub(crate) unsafe fn fill(dest: *mut u8, byte: u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "rep stosb",
            inout("rdi") dest => _,
            in("al") byte,
            inout("rcx") n => _,
            options(nostack),
        );
    }
}

pub(crate) fn sqrt_f64(x: f64) -> f64 {
    let result: f64;
    unsafe { core::arch::asm!("sqrtsd {0}, {0}", inout(xmm_reg) x => result, options(pure, nomem, nostack)) };
    result
}

pub(crate) fn sqrt_f32(x: f32) -> f32 {
    let result: f32;
    unsafe { core::arch::asm!("sqrtss {0}, {0}", inout(xmm_reg) x => result, options(pure, nomem, nostack)) };
    result
}
