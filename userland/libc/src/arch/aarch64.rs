//! AArch64: the entry, the copies and fills as pair loads and stores with a
//! byte tail, and the FP square roots. An unaligned `ldp`/`stp` is legal on the
//! Normal memory every user buffer is.

/// Entry point for C programs. Stack layout at entry (set up by kernel), with
/// the stack pointer 16-byte aligned:
///   [sp]   = argc
///   [sp+8] = argv[0], argv[1], ..., NULL
#[cfg(not(feature = "std-runtime"))]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "ldr x0, [sp]",        // argc
        "add x1, sp, #8",      // argv
        // The outermost frame record: a backtrace ends here.
        "mov x29, xzr",
        "mov x30, xzr",
        "bl {start_c}",
        "brk #0x1",
        start_c = sym crate::runtime::start_c,
    );
}

/// Copy `n` bytes from `src` to `dest`, lowest address first.
pub(crate) unsafe fn copy_forward(dest: *mut u8, src: *const u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "2:",
            "cmp {n}, #16",
            "b.lo 3f",
            "ldp {a}, {b}, [{src}], #16",
            "stp {a}, {b}, [{dest}], #16",
            "sub {n}, {n}, #16",
            "b 2b",
            "3:",
            "cbz {n}, 4f",
            "ldrb {a:w}, [{src}], #1",
            "strb {a:w}, [{dest}], #1",
            "sub {n}, {n}, #1",
            "b 3b",
            "4:",
            dest = inout(reg) dest => _,
            src = inout(reg) src => _,
            n = inout(reg) n => _,
            a = out(reg) _,
            b = out(reg) _,
            options(nostack),
        );
    }
}

/// Copy `n` bytes from `src` to `dest`, highest address first: the order a
/// `dest` overlapping `src` from above needs.
pub(crate) unsafe fn copy_backward(dest: *mut u8, src: *const u8, n: usize) {
    unsafe {
        core::arch::asm!(
            "add {src}, {src}, {n}",
            "add {dest}, {dest}, {n}",
            "2:",
            "cmp {n}, #16",
            "b.lo 3f",
            "ldp {a}, {b}, [{src}, #-16]!",
            "stp {a}, {b}, [{dest}, #-16]!",
            "sub {n}, {n}, #16",
            "b 2b",
            "3:",
            "cbz {n}, 4f",
            "ldrb {a:w}, [{src}, #-1]!",
            "strb {a:w}, [{dest}, #-1]!",
            "sub {n}, {n}, #1",
            "b 3b",
            "4:",
            dest = inout(reg) dest => _,
            src = inout(reg) src => _,
            n = inout(reg) n => _,
            a = out(reg) _,
            b = out(reg) _,
            options(nostack),
        );
    }
}

/// Set `n` bytes at `dest` to `byte`.
pub(crate) unsafe fn fill(dest: *mut u8, byte: u8, n: usize) {
    let word = u64::from(byte) * 0x0101_0101_0101_0101;
    unsafe {
        core::arch::asm!(
            "2:",
            "cmp {n}, #16",
            "b.lo 3f",
            "stp {w}, {w}, [{dest}], #16",
            "sub {n}, {n}, #16",
            "b 2b",
            "3:",
            "cbz {n}, 4f",
            "strb {w:w}, [{dest}], #1",
            "sub {n}, {n}, #1",
            "b 3b",
            "4:",
            dest = inout(reg) dest => _,
            n = inout(reg) n => _,
            w = in(reg) word,
            options(nostack),
        );
    }
}

pub(crate) fn sqrt_f64(x: f64) -> f64 {
    let result: f64;
    unsafe { core::arch::asm!("fsqrt {0:d}, {0:d}", inout(vreg) x => result, options(pure, nomem, nostack)) };
    result
}

pub(crate) fn sqrt_f32(x: f32) -> f32 {
    let result: f32;
    unsafe { core::arch::asm!("fsqrt {0:s}, {0:s}", inout(vreg) x => result, options(pure, nomem, nostack)) };
    result
}

/// `wchar_t`, which is `unsigned int` here.
pub(crate) type WChar = u32;

// What the `long double` readers below widen, named as C names them, since
// this module is also compiled on its own (`toyos-libc-copies`).
unsafe extern "C" {
    fn strtod(s: *const u8, endptr: *mut *mut u8) -> f64;
    fn wcstod(s: *const WChar, endptr: *mut *mut WChar) -> f64;
}

/// An IEEE binary128's bits, returned in `x0` and `x1`.
#[repr(C)]
struct Quad {
    lo: u64,
    hi: u64,
}

/// `x` as binary128, `long double` here: every `double` is exactly one.
extern "C" fn quad(x: f64) -> Quad {
    let bits = x.to_bits();
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = u128::from(bits & ((1 << 52) - 1));
    let (exponent, fraction) = match exponent {
        0 if fraction == 0 => (0, 0),
        // A subnormal double is a normal quad: its leading one at bit `top`.
        0 => {
            let top = 127 - u128::from(fraction.leading_zeros());
            (top + 16383 - 1074, (fraction << (112 - top)) & ((1 << 112) - 1))
        }
        0x7ff => (0x7fff, fraction << 60),
        _ => (u128::from(exponent) + 16383 - 1023, fraction << 60),
    };
    let q = u128::from(bits >> 63) << 127 | exponent << 112 | fraction;
    Quad { lo: q as u64, hi: (q >> 64) as u64 }
}

/// `strtod`'s number as `long double`, in `q0`: read to `double`'s precision
/// and widened, which is exact.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn strtold() {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "bl {strtod}",
        "bl {quad}",
        "fmov d0, x0",
        "mov v0.d[1], x1",
        "ldp x29, x30, [sp], #16",
        "ret",
        strtod = sym strtod,
        quad = sym quad,
    );
}

/// `strtold`, in the one locale there is.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn strtold_l() {
    core::arch::naked_asm!("b {strtold}", strtold = sym strtold);
}

/// `wcstod`'s number as `long double`, as [`strtold`] widens `strtod`'s.
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn wcstold() {
    core::arch::naked_asm!(
        "stp x29, x30, [sp, #-16]!",
        "mov x29, sp",
        "bl {wcstod}",
        "bl {quad}",
        "fmov d0, x0",
        "mov v0.d[1], x1",
        "ldp x29, x30, [sp], #16",
        "ret",
        wcstod = sym wcstod,
        quad = sym quad,
    );
}
