//! A libc header's integer `#define`s as a C program reads them: a decimal or
//! `0x` value after the name. A `#define` of anything else is not one.

pub(crate) const ERRNO_H: &str = include_str!("../../../userland/libc/include/errno.h");
pub(crate) const FCNTL_H: &str = include_str!("../../../userland/libc/include/fcntl.h");
pub(crate) const SIGNAL_H: &str = include_str!("../../../userland/libc/include/signal.h");

/// Each integer `#define` in `header`, in its order.
fn defines(header: &str) -> impl Iterator<Item = (&str, i64)> {
    header.lines().filter_map(|line| {
        let mut words = line.strip_prefix("#define ")?.split_whitespace();
        let (name, value) = (words.next()?, words.next()?);
        let value = match value.strip_prefix("0x") {
            Some(hex) => i64::from_str_radix(hex, 16),
            None => value.parse(),
        };
        Some((name, value.ok()?))
    })
}

fn as_int((name, value): (&str, i64)) -> (&str, i32) {
    (name, i32::try_from(value).unwrap_or_else(|_| panic!("{name} {value:#x} is no int")))
}

/// `name`'s value in `header`, as a C `int`.
pub(crate) fn int(header: &str, name: &str) -> i32 {
    let found = defines(header).find(|&(n, _)| n == name);
    as_int(found.unwrap_or_else(|| panic!("no integer #define {name}"))).1
}

/// Each signal `signal.h` numbers: `SIGHUP`, and not `SIG_BLOCK`.
pub(crate) fn signals() -> Vec<(&'static str, i32)> {
    defines(SIGNAL_H).filter(|(name, _)| name.starts_with("SIG") && !name.contains('_')).map(as_int).collect()
}
