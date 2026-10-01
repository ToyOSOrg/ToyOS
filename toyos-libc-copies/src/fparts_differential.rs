//! `modf` and `logb` against the host C library's, bit for bit: every special
//! value, the edges of the subnormals and of the integers a double holds, and
//! a spread of bit patterns across every exponent.

use crate::fparts;

extern "C" {
    #[link_name = "modf"]
    fn host_modf(x: f64, int: *mut f64) -> f64;
    #[link_name = "logb"]
    fn host_logb(x: f64) -> f64;
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

fn inputs() -> Vec<f64> {
    let mut xs = vec![
        0.0,
        1.0,
        0.5,
        1.5,
        2.5,
        -2.5,
        123.456,
        1e300,
        f64::MAX,
        f64::MIN_POSITIVE,
        f64::MIN_POSITIVE / 2.0,
        f64::from_bits(1),
        f64::from_bits(0x000f_ffff_ffff_ffff),
        4_503_599_627_370_495.5,
        4_503_599_627_370_496.0,
        9_007_199_254_740_993.0,
        f64::INFINITY,
        f64::NAN,
    ];
    // Every exponent, three mantissas each.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for exponent in 0..=0x7ffu64 {
        for _ in 0..3 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            xs.push(f64::from_bits((exponent << 52) | (seed & 0x000f_ffff_ffff_ffff)));
        }
    }
    let negated: Vec<f64> = xs.iter().map(|x| -x).collect();
    xs.extend(negated);
    xs
}

#[test]
fn modf_is_the_host_libraries() {
    for x in inputs() {
        let (int, fraction) = fparts::modf(x);
        let mut host_int = 0.0;
        // SAFETY: `host_int` is a writable double.
        let host_fraction = unsafe { host_modf(x, &mut host_int) };
        assert!(same(int, host_int) && same(fraction, host_fraction), "modf({x:e} = {:#x}): ({int:e}, {fraction:e}), the host ({host_int:e}, {host_fraction:e})", x.to_bits());
    }
}

#[test]
fn logb_is_the_host_libraries() {
    for x in inputs() {
        // SAFETY: a pure function of its argument.
        let host = unsafe { host_logb(x) };
        assert!(same(fparts::logb(x), host), "logb({x:e} = {:#x}): {}, the host {host}", x.to_bits(), fparts::logb(x));
    }
}
