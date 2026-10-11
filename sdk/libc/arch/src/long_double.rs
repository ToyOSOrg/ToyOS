//! AArch64's `strtold`, the arch module's own, against compiler-builtins'
//! `__extenddftf2`, the conversion a compiler emits for `(long double)x`: the
//! binary128 each leaves in `q0` for the `double` the host's `strtod` reads,
//! over the specials, every exponent's edges and seeded random bit patterns.

use std::ffi::CString;

unsafe extern "C" {
    /// The arch module's: this binary defines it, ahead of the host's.
    fn strtold();
    fn __extenddftf2();
}

/// `q0`'s bits after `f` is called with `x0` and `d0` as given.
fn q0(f: unsafe extern "C" fn(), x0: usize, d0: f64) -> u128 {
    let (lo, hi): (u64, u64);
    // SAFETY: both callees take at most `x0`, `x1` and `d0` under the C ABI,
    // and `x0` is a NUL-terminated string where `strtold` reads one.
    unsafe {
        core::arch::asm!(
            "blr {f}",
            "fmov x0, d0",
            "mov x1, v0.d[1]",
            f = in(reg) f,
            inout("x0") x0 => lo,
            inout("x1") 0usize => hi,
            inout("d0") d0 => _,
            clobber_abi("C"),
        );
    }
    u128::from(hi) << 64 | u128::from(lo)
}

fn judge(x: f64) {
    let text = CString::new(format!("{x:e}")).unwrap();
    let ours = q0(strtold, text.as_ptr() as usize, 0.0);
    let widened = q0(__extenddftf2, 0, x);
    assert_eq!(ours, widened, "{x:e} ({:#018x}): {ours:#034x}, compiler-builtins' {widened:#034x}", x.to_bits());
}

#[test]
fn strtold_widens_as_compiler_builtins_does() {
    for x in [0.0, -0.0, 1.0, -2.5, f64::MIN_POSITIVE, f64::MAX, f64::MIN, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        judge(x);
    }
    for exponent in 0..0x7ff_u64 {
        for fraction in [0, 1, 1 << 51, (1 << 52) - 1] {
            judge(f64::from_bits(exponent << 52 | fraction));
        }
    }
    let mut state = 0x853c_49e6_748f_ea9b_u64;
    for _ in 0..20_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let x = f64::from_bits(state);
        if !x.is_nan() {
            judge(x);
        }
    }
}
