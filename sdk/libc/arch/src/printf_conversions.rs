//! printf's numeric conversions against what C17 7.21.6.1 says each prints:
//! the alternate form of every conversion that has one, and the flags, width
//! and precision it meets.

use crate::conversion::{self, Sink};
use Arg::{F, I, U};

impl Sink for Vec<u8> {
    fn put(&mut self, b: u8) {
        self.push(b);
    }
}

#[derive(Clone, Copy, Debug)]
enum Arg {
    I(i64),
    U(u64),
    F(f64),
}

fn printed(directive: &str, arg: Arg) -> String {
    let fmt = directive.strip_prefix('%').expect("a directive").as_bytes();
    let (spec, len) = conversion::spec(fmt, || panic!("{directive} reads no star")).expect("a whole directive");
    assert_eq!(len, fmt.len(), "{directive} is one directive");
    let mut out = Vec::new();
    match arg {
        I(v) => conversion::signed(&mut out, &spec, v),
        U(v) => conversion::unsigned(&mut out, &spec, v),
        F(v) => conversion::float(&mut out, &spec, v),
    }
    String::from_utf8(out).expect("ASCII")
}

fn check(cases: &[(&str, Arg, &str)]) {
    let wrong: Vec<_> = cases
        .iter()
        .map(|&(directive, arg, want)| (directive, arg, want, printed(directive, arg)))
        .filter(|(_, _, want, got)| want != got)
        .collect();
    assert!(wrong.is_empty(), "(directive, argument, C's, ours): {wrong:#?}");
}

/// `#`: `o`'s precision rises until its first digit is a 0, and a zero value
/// with a zero precision prints that one 0.
#[test]
fn alternate_octal_leads_with_a_zero() {
    check(&[
        ("%#o", U(8), "010"),
        ("%#o", U(0), "0"),
        ("%#.0o", U(0), "0"),
        ("%.0o", U(0), ""),
        ("%#.3o", U(8), "010"),
        ("%#.4o", U(8), "0010"),
        ("%#5o", U(8), "  010"),
        ("%#05o", U(8), "00010"),
        ("%#-5o", U(8), "010  "),
        ("%#lo", U(u64::MAX), "01777777777777777777777"),
    ]);
}

/// `#`: a nonzero `x` or `X` takes `0x` or `0X`, ahead of any zeros the
/// precision or the `0` flag adds.
#[test]
fn alternate_hexadecimal_takes_its_prefix_when_nonzero() {
    check(&[
        ("%#x", U(512), "0x200"),
        ("%#X", U(255), "0XFF"),
        ("%#x", U(0), "0"),
        ("%#.0x", U(0), ""),
        ("%#5.0x", U(0), "     "),
        ("%#08x", U(255), "0x0000ff"),
        ("%#.4x", U(255), "0x00ff"),
        ("%#10.4x", U(255), "    0x00ff"),
        ("%#08.4x", U(255), "  0x00ff"),
        ("%#-8x", U(255), "0xff    "),
        ("%#lx", U(u64::MAX), "0xffffffffffffffff"),
    ]);
}

/// `#`: `a`, `e` and `f` always have a decimal point; `g` keeps it and its
/// trailing zeros. An infinity is no number and has neither.
#[test]
fn alternate_floats_keep_the_decimal_point() {
    check(&[
        ("%#.0f", F(3.0), "3."),
        ("%#.0f", F(0.0), "0."),
        ("%#.0F", F(-2.5), "-2."),
        ("%#08.0f", F(3.0), "0000003."),
        ("%#.0e", F(3.0), "3.e+00"),
        ("%#.0E", F(12345.0), "1.E+04"),
        ("%#.0a", F(1.0), "0x1.p+0"),
        ("%#a", F(1.0), "0x1.p+0"),
        ("%#A", F(0.0), "0X0.P+0"),
        ("%#a", F(1.5), "0x1.8p+0"),
        ("%#g", F(1.0), "1.00000"),
        ("%#g", F(0.0), "0.00000"),
        ("%#.0g", F(3.0), "3."),
        ("%#.3g", F(1.5), "1.50"),
        ("%#g", F(100000.0), "100000."),
        ("%#g", F(1e6), "1.00000e+06"),
        ("%#g", F(0.0001), "0.000100000"),
        ("%#G", F(1e-5), "1.00000E-05"),
        ("%#.2g", F(0.5), "0.50"),
        ("%#f", F(f64::INFINITY), "inf"),
        ("%#08f", F(f64::NEG_INFINITY), "    -inf"),
    ]);
}

/// The same conversions without `#`, as the cases above contrast them.
#[test]
fn the_plain_forms_drop_what_the_alternate_keeps() {
    check(&[
        ("%o", U(8), "10"),
        ("%x", U(512), "200"),
        ("%.0f", F(3.0), "3"),
        ("%.0e", F(3.0), "3e+00"),
        ("%.0a", F(1.0), "0x1p+0"),
        ("%a", F(1.0), "0x1p+0"),
        ("%g", F(1.0), "1"),
        ("%g", F(0.0), "0"),
        ("%.3g", F(1.5), "1.5"),
        ("%g", F(100000.0), "100000"),
        ("%g", F(1e6), "1e+06"),
        ("%g", F(0.0001), "0.0001"),
        ("%G", F(1e-5), "1E-05"),
        ("%g", F(123456789.0), "1.23457e+08"),
    ]);
}

/// Zeros from the `0` flag follow the sign; a precision turns the flag off
/// for an integer, and an infinity or NaN pads with spaces.
#[test]
fn zero_padding_follows_the_sign() {
    check(&[
        ("%05d", I(-42), "-0042"),
        ("%+05d", I(42), "+0042"),
        ("% 05d", I(42), " 0042"),
        ("%-5d", I(-42), "-42  "),
        ("%05.3d", I(-7), " -007"),
        ("%.0d", I(0), ""),
        ("%d", I(i64::MIN), "-9223372036854775808"),
        ("%08.2f", F(-1.5), "-0001.50"),
        ("%+.1e", F(0.0), "+0.0e+00"),
        ("%05f", F(f64::NAN), "  nan"),
        ("%-6F", F(f64::INFINITY), "INF   "),
    ]);
}

/// `f` and `e` round the exact binary value to nearest, ties to even.
#[test]
fn decimal_floats_round_the_exact_value() {
    check(&[
        ("%.0f", F(0.5), "0"),
        ("%.0f", F(1.5), "2"),
        ("%.0f", F(2.5), "2"),
        ("%.1f", F(0.25), "0.2"),
        ("%.1f", F(0.35), "0.3"),
        ("%.2f", F(1.005), "1.00"),
        ("%f", F(1e20), "100000000000000000000.000000"),
        ("%.3e", F(1234.5), "1.234e+03"),
        ("%e", F(1e-300), "1.000000e-300"),
        ("%.1g", F(0.96), "1"),
        ("%.2g", F(99.5), "1e+02"),
    ]);
}

/// `a` is exact without a precision; with one it rounds to nearest, ties to
/// even, a carry making the leading digit 2. A subnormal's leading digit is 0.
#[test]
fn hexadecimal_floats_are_exact() {
    check(&[
        ("%a", F(0.5), "0x1p-1"),
        ("%a", F(-0.1), "-0x1.999999999999ap-4"),
        ("%A", F(255.0), "0X1.FEP+7"),
        ("%a", F(f64::MAX), "0x1.fffffffffffffp+1023"),
        ("%a", F(f64::MIN_POSITIVE), "0x1p-1022"),
        ("%a", F(f64::from_bits(1)), "0x0.0000000000001p-1022"),
        ("%.2a", F(1.0), "0x1.00p+0"),
        ("%.15a", F(1.0), "0x1.000000000000000p+0"),
        ("%.1a", F(1.03125), "0x1.0p+0"),
        ("%.1a", F(1.09375), "0x1.2p+0"),
        ("%.0a", F(1.5), "0x2p+0"),
        ("%.0a", F(2.5), "0x1p+1"),
        ("%.1a", F(1.96875), "0x2.0p+0"),
        ("%010a", F(1.0), "0x00001p+0"),
        ("%+a", F(1.0), "+0x1p+0"),
    ]);
}

/// Precision past every digit a double has is zeros, not a refusal.
#[test]
fn a_precision_past_the_exact_digits_is_zeros() {
    let fixed = printed("%.2000f", F(1.0));
    assert_eq!(fixed.len(), 2002);
    assert!(fixed.starts_with("1.") && fixed[2..].bytes().all(|b| b == b'0'));
    let tiny = printed("%.1100f", F(f64::from_bits(1)));
    assert!(tiny.ends_with(&format!("625{}", "0".repeat(1100 - 1074))), "{tiny}");
    let exponential = printed("%.1500e", F(1.0));
    assert_eq!(exponential, format!("1.{}e+00", "0".repeat(1500)));
    assert_eq!(printed("%.20a", F(1.0)), format!("0x1.{}p+0", "0".repeat(20)));
}

/// A `*` width or precision reads its argument; a negative width is the `-`
/// flag and a negative precision none at all.
#[test]
fn star_arguments_are_read_in_order() {
    let mut stars = [-6, -1].into_iter();
    let (spec, len) = conversion::spec(b"*.*x!", || stars.next().expect("two stars")).expect("a directive");
    assert_eq!((len, spec.width, spec.left, spec.precision, spec.conversion), (4, 6, true, None, b'x'));
    let (spec, len) = conversion::spec(b"#-+ 012.3llx", || panic!("no star")).expect("a directive");
    assert!(spec.alt && spec.left && spec.plus && spec.space && spec.zero && spec.wide);
    assert_eq!((len, spec.width, spec.precision), (12, 12, Some(3)));
    assert!(conversion::spec(b"#08.", || 0).is_none());
}
