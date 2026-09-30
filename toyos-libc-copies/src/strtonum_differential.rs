//! libc's number reader against the host C library's `strtod`, `strtof`,
//! `strtol` and `strtoul`: the value, bit for bit, where it ends, and the
//! `ERANGE` and `EINVAL` C requires, over a corpus of the grammar's corners and
//! seeded random numbers, hexadecimal ones with a rounding tie in half of them.

use std::ffi::{c_int, CString};

use crate::strtonum::{self, Read, Refusal};

unsafe extern "C" {
    fn strtod(s: *const u8, end: *mut *mut u8) -> f64;
    fn strtof(s: *const u8, end: *mut *mut u8) -> f32;
    fn strtol(s: *const u8, end: *mut *mut u8, base: c_int) -> i64;
    fn strtoul(s: *const u8, end: *mut *mut u8, base: c_int) -> u64;
    #[cfg_attr(target_os = "macos", link_name = "__error")]
    #[cfg_attr(target_os = "linux", link_name = "__errno_location")]
    fn errno_location() -> *mut c_int;
}

/// The host's `ERANGE` and `EINVAL`, which macOS and Linux number alike.
const ERANGE: c_int = 34;
const EINVAL: c_int = 22;

/// What the host's reader answers for `s`: the value, where it ends, `errno`.
fn host<T>(s: &CString, read: impl Fn(*const u8, *mut *mut u8) -> T) -> (T, usize, c_int) {
    let mut end = std::ptr::null_mut();
    // SAFETY: the host's errno slot is this thread's.
    unsafe { *errno_location() = 0 };
    let value = read(s.as_ptr().cast(), &mut end);
    // SAFETY: as above.
    (value, end as usize - s.as_ptr() as usize, unsafe { *errno_location() })
}

/// What errno a refusal is, as the host numbers it.
fn errno_of(refused: &Option<Refusal>) -> c_int {
    match refused {
        None => 0,
        Some(Refusal::Base) => EINVAL,
        Some(Refusal::Range) => ERANGE,
    }
}

/// Both NaN of one sign, or the same bits.
fn same(a: f64, b: f64) -> bool {
    if a.is_nan() || b.is_nan() {
        a.is_nan() && b.is_nan() && a.is_sign_negative() == b.is_sign_negative()
    } else {
        a.to_bits() == b.to_bits()
    }
}

/// `s` as libc reads it through both its code units, which must agree.
fn ours<F: strtonum::Float + Copy + Into<f64>>(s: &CString) -> Read<F> {
    let wide: Vec<crate::arch::WChar> = s.as_bytes_with_nul().iter().map(|&b| b.into()).collect();
    // SAFETY: both strings are NUL-terminated.
    let (narrow, wide) = unsafe { (strtonum::float::<u8, F>(s.as_ptr().cast()), strtonum::float::<_, F>(wide.as_ptr())) };
    assert!(
        same(narrow.value.into(), wide.value.into()) && narrow.end == wide.end && errno_of(&narrow.refused) == errno_of(&wide.refused),
        "{s:?}: the wide reading differs"
    );
    narrow
}

/// `s` read as a `double` and a `float`, against the host. `ERANGE` is held
/// against the host's only for an overflow: C leaves an underflow's to each
/// library.
fn judge_float(s: &str) {
    let c = CString::new(s).unwrap();
    // SAFETY: `c` is NUL-terminated.
    let (value, end, errno) = host(&c, |s, e| unsafe { strtod(s, e) });
    let read = ours::<f64>(&c);
    assert!(same(read.value, value), "{s:?}: {:e} ({:#x}), the host's {value:e} ({:#x})", read.value, read.value.to_bits(), value.to_bits());
    assert_eq!(read.end, end, "{s:?}: where it ends");
    if value.is_infinite() && !s.to_ascii_lowercase().contains("inf") {
        assert_eq!((errno_of(&read.refused), errno), (ERANGE, ERANGE), "{s:?}: an overflow");
    }
    // SAFETY: as above.
    let (value, end, _) = host(&c, |s, e| unsafe { strtof(s, e) });
    let read = ours::<f32>(&c);
    assert!(same(read.value.into(), value.into()), "{s:?}: float {:e}, the host's {value:e}", read.value);
    assert_eq!(read.end, end, "{s:?}: where the float ends");
}

/// `s` read as `long` and `unsigned long` in `base`, against the host. Where a
/// base C defines finds no number, POSIX lets `errno` say `EINVAL` or nothing,
/// and it is not compared.
fn judge_int(s: &str, base: i32) {
    let c = CString::new(s).unwrap();
    let errno = |read_errno: c_int, end: usize| if end == 0 && (base == 0 || (2..=36).contains(&base)) { 0 } else { read_errno };
    // SAFETY: NUL-terminated.
    let read = unsafe { strtonum::signed::<u8>(c.as_ptr().cast(), base) };
    // SAFETY: as above.
    let (value, end, host_errno) = host(&c, |s, e| unsafe { strtol(s, e, base) });
    assert_eq!((read.value, read.end, errno(errno_of(&read.refused), read.end)), (value, end, errno(host_errno, end)), "strtol({s:?}, {base})");
    // SAFETY: as above.
    let read = unsafe { strtonum::unsigned::<u8>(c.as_ptr().cast(), base) };
    // SAFETY: as above.
    let (value, end, host_errno) = host(&c, |s, e| unsafe { strtoul(s, e, base) });
    assert_eq!((read.value, read.end, errno(errno_of(&read.refused), read.end)), (value, end, errno(host_errno, end)), "strtoul({s:?}, {base})");
}

/// A seeded xorshift, so a red names the case that reproduces it.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn hex(&mut self) -> char {
        char::from_digit(self.below(16) as u32, 16).unwrap()
    }
}

#[test]
fn the_grammar_s_corners_agree_with_the_host() {
    for s in [
        "", " ", "+", "-", ".", "e5", "0x", "0X", "0x.", "0x.p1", "0xp1", "0x1p", "0x1p+", "0x1p-x", "1e", "1e+",
        "1e-x", ".5", "5.", "-.5e-3", " \t\n\x0b\x0c\r+1.5", "1.5xyz", "inf", "-INFINITY", "infinit", "infx", "nan",
        "-nan", "NaN(12_ab)", "nan(", "nan()", "0x1.8", "0X1.8P1", "0x.8p1", "0x1P-2", "00x1p0", "0x0p0",
        "-0x0p0", "0.000", "-0", "1e309", "-1e309", "1e-400", "4.9e-324", "2.4703282292062328e-324",
        "2.4703282292062327e-324", "1.7976931348623157e308", "1.7976931348623158e308", "1.7976931348623159e308",
        "0x1.fffffffffffffp1023", "0x1.fffffffffffff7p1023", "0x1.fffffffffffff8p1023", "0x1p1024", "0x1p-1074",
        "0x1p-1075", "0x1.0000000000001p-1075", "0x1.8p-1075", "0x1p-1076", "0x0.0000000000001p-1022",
        "0x0.00000000000008p-1022", "0x0.00000000000018p-1022", "0x1.00000000000008p0", "0x1.00000000000018p0",
        "0x1.000000000000080000000000000000001p0", "0x1.0000000000000800000p0", "0x10000000000000080p0",
        "0x10000000000000180p-4", "0x.000000000000000000000000000000000000000001p200", "0x1p-99999999999999999999",
        "0x1p99999999999999999999", "0x1.000001p0", "0x1.0000008p0", "0x1.0000018p0", "0x1.fffffe8p127", "0x1p-149",
        "0x1p-150", "0x1.8p-150", "123456789012345678901234567890", "0.1", "0.30000000000000004", "9007199254740993",
        "1e23", "8.589973e9",
    ] {
        judge_float(s);
    }
    for (s, base) in [
        ("", 10), ("0x", 16), ("0x", 0), ("0xg", 16), ("0x1g", 0), ("  -0x10", 0), ("010", 0), ("08", 0), ("0", 0),
        ("+7", 8), ("-", 10), ("9223372036854775807", 10), ("9223372036854775808", 10), ("-9223372036854775808", 10),
        ("-9223372036854775809", 10), ("18446744073709551615", 10), ("18446744073709551616", 10), ("-1", 10),
        ("-18446744073709551615", 10), ("zz", 36), ("Zz", 36), ("z", 35), ("1", 1), ("1", 37), ("1", -1),
        ("7fffffffffffffff", 16), ("ffffffffffffffffff", 16), ("  \t12 34", 10),
    ] {
        judge_int(s, base);
    }
}

/// Hexadecimal numbers with up to 30 digits, exponents across both types'
/// ranges and past them, and in half of them a `double`'s 53 bits followed by
/// exactly half a unit, or half and a sticky bit, the ties to even.
#[test]
fn random_hexadecimal_numbers_round_as_the_host_s() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for _ in 0..200_000 {
        let mut s = String::new();
        if rng.below(4) == 0 {
            s.push('-');
        }
        s.push_str(if rng.below(2) == 0 { "0x" } else { "0X" });
        if rng.below(2) == 0 {
            s.push('1');
            s.push('.');
            (0..13).for_each(|_| s.push(rng.hex()));
            s.push('8');
            (0..rng.below(4)).for_each(|_| s.push('0'));
            if rng.below(2) == 0 {
                s.push('1');
            }
        } else {
            let digits = 1 + rng.below(30);
            let point = rng.below(digits + 1);
            for i in 0..digits {
                if i == point {
                    s.push('.');
                }
                s.push(rng.hex());
            }
        }
        let exponent = rng.below(2300) as i64 - 1150;
        s.push_str(&format!("{}{exponent}", if rng.below(2) == 0 { 'p' } else { 'P' }));
        if rng.below(8) == 0 {
            s.push('z');
        }
        judge_float(&s);
    }
}

/// Decimal numbers with up to 25 digits and exponents across `double`'s range.
#[test]
fn random_decimal_numbers_round_as_the_host_s() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for _ in 0..50_000 {
        let digits = 1 + rng.below(25);
        let point = rng.below(digits + 1);
        let mut s = String::new();
        for i in 0..digits {
            if i == point {
                s.push('.');
            }
            s.push(char::from_digit(rng.below(10) as u32, 10).unwrap());
        }
        s.push_str(&format!("e{}", rng.below(700) as i64 - 350));
        judge_float(&s);
    }
}

/// Integers in every base C defines, and in base 0 with each prefix.
#[test]
fn random_integers_read_as_the_host_s() {
    let mut rng = Rng(0xd1b5_4a32_d192_ed03);
    for _ in 0..50_000 {
        let base = [0, 2, 8, 10, 16, 36][rng.below(6) as usize];
        let mut s = String::from([" ", "", "-", "+"][rng.below(4) as usize]);
        if base == 0 || base == 16 {
            s.push_str(["", "0x", "0X", "0"][rng.below(4) as usize]);
        }
        for _ in 0..1 + rng.below(24) {
            s.push(char::from_digit(rng.below(36) as u32, 36).unwrap());
        }
        judge_int(&s, base);
    }
}
