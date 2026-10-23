//! A libc header's integer `#define`s as a C program reads them: a decimal or
//! `0x` value after the name. A `#define` of anything else is not one.

pub(crate) const ERRNO_H: &str = include_str!("../../include/errno.h");
pub(crate) const FCNTL_H: &str = include_str!("../../include/fcntl.h");
pub(crate) const LIMITS_H: &str = include_str!("../../include/limits.h");
pub(crate) const SIGNAL_H: &str = include_str!("../../include/signal.h");
pub(crate) const STAT_H: &str = include_str!("../../include/sys/stat.h");

/// Each integer `#define` in `header`, in its order.
fn defines(header: &str) -> impl Iterator<Item = (&str, i64)> {
    header.lines().filter_map(|line| {
        let mut words = line.strip_prefix("#define ")?.split_whitespace();
        let (name, value) = (words.next()?, words.next()?);
        let (negative, value) = match value.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let value = match value.strip_prefix("0x") {
            Some(hex) => i64::from_str_radix(hex, 16),
            None => value.parse(),
        };
        let value = value.ok()?;
        Some((name, if negative { -value } else { value }))
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

/// `name`'s value in `header`, as a flag word: `SA_RESETHAND` is no `int`.
pub(crate) fn flags(header: &str, name: &str) -> u64 {
    let found = defines(header).find(|&(n, _)| n == name);
    let (_, value) = found.unwrap_or_else(|| panic!("no integer #define {name}"));
    u64::try_from(value).unwrap_or_else(|_| panic!("{name} {value:#x} is no flag word"))
}

#[test]
fn pipe_buf_is_the_kernels_whole_pipe_write() {
    assert_eq!(toyos_abi::syscall::MAX_WHOLE_PIPE_WRITE, int(LIMITS_H, "PIPE_BUF") as usize);
}

/// Each signal `signal.h` numbers: `SIGHUP`, and not `SIG_BLOCK`.
pub(crate) fn signals() -> Vec<(&'static str, i32)> {
    defines(SIGNAL_H).filter(|(name, _)| name.starts_with("SIG") && !name.contains('_')).map(as_int).collect()
}
