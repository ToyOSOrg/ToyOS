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
/// any `dest` not above an overlapping `src`. Returns `dest`, so `memcpy`
/// can end in a jump to [`copy_long`] and a shorter copy runs one `rep movsb`
/// behind one compare, with nothing held across a call.
#[inline(always)]
pub(crate) unsafe fn copy_forward(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    if n >= ROUTED_LEAST {
        // SAFETY: as this function's caller promises.
        return unsafe { copy_long(dest, src, n) };
    }
    unsafe { movsb(dest, src, n) };
    dest
}

/// [`copy_forward`] from [`ROUTED_LEAST`] up, out of line.
///
/// A forward copy whose destination is a short way above its source modulo
/// 4 KiB loads each byte soon after storing to an unrelated address with the
/// same bits 11:0, and the load waits on that store: AMD's Family 17h judges
/// store-to-load forwarding eligibility by those bits alone (Software
/// Optimization Guide for AMD Family 17h Models 30h and Greater Processors,
/// 56305 rev. 3.02, §2.12), and Intel calls it 4K aliasing (Intel 64 and
/// IA-32 Architectures Optimization Reference Manual). Copied highest first,
/// every store such a load could match is 4 KiB minus the distance behind it
/// and long retired. Only disjoint ranges go down.
#[inline(never)]
unsafe fn copy_long(dest: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    let distance = (dest as usize).wrapping_sub(src as usize) % 4096;
    // `distance` in 1..SHORT_ALIAS, as one compare.
    let short = distance.wrapping_sub(1) < SHORT_ALIAS - 1;
    // SAFETY: `n` is at least `ROUTED_LEAST`, above the loops' 64; down only
    // between disjoint ranges.
    unsafe {
        if !short || n > ROUTED_MOST || (dest as usize).abs_diff(src as usize) < n {
            movsb(dest, src, n);
        } else if has_avx() {
            copy_down_avx(dest, src, n);
        } else {
            copy_down_sse2(dest, src, n);
        }
    }
    dest
}

#[inline(always)]
unsafe fn movsb(dest: *mut u8, src: *const u8, n: usize) {
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

/// The distance under which [`copy_long`] copies highest first.
const SHORT_ALIAS: usize = 512;

/// The shortest copy that asks where its ends sit, the shortest length a
/// stall has been measured at. Whether a shorter copy stalls is unmeasured.
const ROUTED_LEAST: usize = 4096;

/// The longest copy [`copy_long`] copies highest first. A longer one runs
/// from beyond the caches, where the loop's stores read each line before
/// writing it and measured slower than `rep movsb` at distances that do not
/// stall.
const ROUTED_MOST: usize = 1 << 20;

/// Whether this CPU runs AVX and its system saves the YMM state: CPUID.1:ECX
/// AVX (bit 28) and OSXSAVE (bit 27), and XCR0's SSE and AVX bits (1 and 2).
/// Asked once; a racing first call asks twice and stores the same answer.
pub(crate) fn has_avx() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering};
    static AVX: AtomicU8 = AtomicU8::new(0);
    match AVX.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let ecx = core::arch::x86_64::__cpuid(1).ecx;
            let avx = ecx & (1 << 28) != 0 && ecx & (1 << 27) != 0 && {
                let (lo, _hi): (u32, u32);
                // SAFETY: OSXSAVE says XGETBV exists; XCR0 is readable at every privilege level.
                unsafe { core::arch::asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") _hi, options(nomem, nostack, preserves_flags)) };
                lo & 0b110 == 0b110
            };
            AVX.store(if avx { 2 } else { 1 }, Ordering::Relaxed);
            avx
        }
    }
}

/// Where a descending loop of `width`-byte stores starts: the offset of the
/// last `width` boundary of the destination at or below its end. The bytes
/// above it go in one unaligned 64-byte store; every store below it is
/// aligned, since a store split across two lines costs both benches' cores
/// as much as the stall this loop avoids.
fn aligned_end(dest: *mut u8, n: usize, width: usize) -> usize {
    ((dest as usize + n) & !(width - 1)) - dest as usize
}

/// Copy `n` bytes, at least 64, between disjoint ranges by SSE2 from the
/// top, 64 bytes a step from [`aligned_end`]: the first and last 64 bytes are
/// loaded first and stored last, so the steps may overlap them.
pub(crate) unsafe fn copy_down_sse2(dest: *mut u8, src: *const u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "movups xmm4, [{s}]", "movups xmm5, [{s} + 16]", "movups xmm6, [{s} + 32]", "movups xmm7, [{s} + 48]",
            "movups xmm8, [{s} + {n} - 64]", "movups xmm9, [{s} + {n} - 48]",
            "movups xmm10, [{s} + {n} - 32]", "movups xmm11, [{s} + {n} - 16]",
            "2:",
            "cmp {p}, 64",
            "jbe 3f",
            "sub {p}, 64",
            "movups xmm0, [{s} + {p}]", "movups xmm1, [{s} + {p} + 16]",
            "movups xmm2, [{s} + {p} + 32]", "movups xmm3, [{s} + {p} + 48]",
            "movaps [{d} + {p}], xmm0", "movaps [{d} + {p} + 16], xmm1",
            "movaps [{d} + {p} + 32], xmm2", "movaps [{d} + {p} + 48], xmm3",
            "jmp 2b",
            "3:",
            "movups [{d} + {n} - 64], xmm8", "movups [{d} + {n} - 48], xmm9",
            "movups [{d} + {n} - 32], xmm10", "movups [{d} + {n} - 16], xmm11",
            "movups [{d}], xmm4", "movups [{d} + 16], xmm5", "movups [{d} + 32], xmm6", "movups [{d} + 48], xmm7",
            s = in(reg) src,
            d = in(reg) dest,
            n = in(reg) n,
            p = inout(reg) aligned_end(dest, n, 16) => _,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _,
            out("xmm4") _, out("xmm5") _, out("xmm6") _, out("xmm7") _,
            out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
            options(nostack),
        );
    }
}

/// [`copy_down_sse2`] 32 bytes a load and store, half the stores: a CPU
/// retiring one store a cycle copies at twice the rate. `vzeroupper` ends it,
/// so SSE code after it pays no transition (56305 rev. 3.02, §2.11.6).
///
/// # Safety
/// As [`copy_down_sse2`], on a CPU [`has_avx`] answers for.
#[target_feature(enable = "avx")]
pub(crate) unsafe fn copy_down_avx(dest: *mut u8, src: *const u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "vmovdqu ymm2, [{s}]", "vmovdqu ymm3, [{s} + 32]",
            "vmovdqu ymm4, [{s} + {n} - 64]", "vmovdqu ymm5, [{s} + {n} - 32]",
            "2:",
            "cmp {p}, 64",
            "jbe 3f",
            "sub {p}, 64",
            "vmovdqu ymm0, [{s} + {p}]", "vmovdqu ymm1, [{s} + {p} + 32]",
            "vmovdqa [{d} + {p}], ymm0", "vmovdqa [{d} + {p} + 32], ymm1",
            "jmp 2b",
            "3:",
            "vmovdqu [{d} + {n} - 64], ymm4", "vmovdqu [{d} + {n} - 32], ymm5",
            "vmovdqu [{d}], ymm2", "vmovdqu [{d} + 32], ymm3",
            "vzeroupper",
            s = in(reg) src,
            d = in(reg) dest,
            n = in(reg) n,
            p = inout(reg) aligned_end(dest, n, 32) => _,
            // `vzeroupper` clears every YMM register's upper half.
            out("ymm0") _, out("ymm1") _, out("ymm2") _, out("ymm3") _,
            out("ymm4") _, out("ymm5") _, out("ymm6") _, out("ymm7") _,
            out("ymm8") _, out("ymm9") _, out("ymm10") _, out("ymm11") _,
            out("ymm12") _, out("ymm13") _, out("ymm14") _, out("ymm15") _,
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
