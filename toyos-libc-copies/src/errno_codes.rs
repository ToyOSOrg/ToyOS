//! libc's one list of errno codes (`errno.rs`) against `include/errno.h`, the
//! numbers a C program compares them with: every code the library answers
//! with is the header's, by name and value.

use crate::header::{self, ERRNO_H};

#[test]
fn every_code_libc_answers_with_is_errno_h_s() {
    let list = include_str!("../../userland/libc/src/errno.rs");
    let codes: Vec<(&str, i32)> = list
        .lines()
        .filter_map(|line| line.strip_prefix("pub(crate) const "))
        .map(|rest| {
            let (name, value) = rest.split_once(": i32 = ").unwrap_or_else(|| panic!("errno.rs: {rest}"));
            (name, value.trim_end_matches(';').parse().unwrap_or_else(|_| panic!("errno.rs: {rest}")))
        })
        .collect();
    assert!(codes.len() > 20, "errno.rs lists {} codes", codes.len());
    for (name, value) in codes {
        assert_eq!(header::int(ERRNO_H, name), value, "{name}");
    }
}
