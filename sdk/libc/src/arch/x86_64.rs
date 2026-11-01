//! x86-64: the entry, the string instructions, and SSE2's square roots.

/// Entry point for C programs. Stack layout at entry (set up by kernel):
///   [RSP]   = argc
///   [RSP+8] = argv[0], argv[1], ..., NULL
/// The image relocates itself first, from its own header's address.
#[cfg(not(feature = "std-runtime"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "lea rdi, [rip + __ehdr_start]",
        "call {relocate}",
        "mov rdi, [rsp]",      // argc
        "lea rsi, [rsp + 8]",  // argv
        "call {start_c}",
        "ud2",
        relocate = sym toyos::relocate::relocate_self,
        start_c = sym crate::runtime::start_c,
    );
}

/// Copy `n` bytes from `src` to `dest` as a forward copy would: correct for
/// any `dest` not above an overlapping `src`.
///
/// A destination less than [`SHORT_ALIAS`] above its source modulo 4 KiB makes
/// each load of a forward copy match an older, unrelated store on address bits
/// 11:0, which the load/store unit takes as a possible dependence: AMD's Family
/// 17h judges store-to-load forwarding by those bits alone (Software
/// Optimization Guide for AMD EPYC 7002, 56305 rev. 3.02, §2.12), and the
/// T14's Tiger Lake measures the same stall. Copied highest first, every store such a load
/// could match is 4 KiB minus the distance behind it and long retired.
pub(crate) unsafe fn copy_forward(dest: *mut u8, src: *const u8, n: usize) {
    let distance = (dest as usize).wrapping_sub(src as usize) % 4096;
    let disjoint = (dest as usize).abs_diff(src as usize) >= n;
    if n >= 64 && distance != 0 && distance < SHORT_ALIAS && disjoint {
        unsafe { copy_down_64(dest, src, n) };
        return;
    }
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

/// The distances below which [`copy_forward`] copies highest first: as far as
/// either bench was measured slower forward than down (the T14 to 256 bytes).
const SHORT_ALIAS: usize = 512;

/// Copy `n` bytes, at least 64, between disjoint ranges, 64 bytes a step by
/// SSE2 from the top; the lowest 64 are loaded first and stored last, so the
/// last step may overlap them.
unsafe fn copy_down_64(dest: *mut u8, src: *const u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "movups xmm4, [{s}]", "movups xmm5, [{s} + 16]", "movups xmm6, [{s} + 32]", "movups xmm7, [{s} + 48]",
            "2:",
            "cmp {p}, 64",
            "jbe 3f",
            "sub {p}, 64",
            "movups xmm0, [{s} + {p}]", "movups xmm1, [{s} + {p} + 16]",
            "movups xmm2, [{s} + {p} + 32]", "movups xmm3, [{s} + {p} + 48]",
            "movups [{d} + {p}], xmm0", "movups [{d} + {p} + 16], xmm1",
            "movups [{d} + {p} + 32], xmm2", "movups [{d} + {p} + 48], xmm3",
            "jmp 2b",
            "3:",
            "movups [{d}], xmm4", "movups [{d} + 16], xmm5", "movups [{d} + 32], xmm6", "movups [{d} + 48], xmm7",
            s = in(reg) src,
            d = in(reg) dest,
            p = inout(reg) n => _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
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

/// `wchar_t`, which is `int` here.
pub(crate) type WChar = i32;

// What the `long double` readers below widen, named as C names them, since
// this module is also compiled on its own (`toyos-libc-copies`).
unsafe extern "C" {
    fn strtod(s: *const u8, endptr: *mut *mut u8) -> f64;
    fn wcstod(s: *const WChar, endptr: *mut *mut WChar) -> f64;
}

/// `strtod`'s number as `long double`, x87 extended precision in `st0`: read
/// to `double`'s precision and widened, which is exact.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn strtold() {
    core::arch::naked_asm!(
        "sub rsp, 8",
        "call {strtod}",
        "movsd qword ptr [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 8",
        "ret",
        strtod = sym strtod,
    );
}

/// `strtold`, in the one locale there is.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn strtold_l() {
    core::arch::naked_asm!("jmp {strtold}", strtold = sym strtold);
}

/// `wcstod`'s number as `long double`, as [`strtold`] widens `strtod`'s.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn wcstold() {
    core::arch::naked_asm!(
        "sub rsp, 8",
        "call {wcstod}",
        "movsd qword ptr [rsp], xmm0",
        "fld qword ptr [rsp]",
        "add rsp, 8",
        "ret",
        wcstod = sym wcstod,
    );
}
