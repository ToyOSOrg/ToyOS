//! libc's one list of errno codes (`errno.rs`) against `include/errno.h`, the
//! numbers a C program compares them with: every code the library answers
//! with is the header's, by name and value.

use std::collections::HashMap;

#[test]
fn every_code_libc_answers_with_is_errno_h_s() {
    let header = include_str!("../../userland/libc/include/errno.h");
    let defined: HashMap<&str, i32> = header
        .lines()
        .filter_map(|line| line.strip_prefix("#define "))
        .filter_map(|rest| {
            let mut words = rest.split_whitespace();
            Some((words.next()?, words.next()?.parse().ok()?))
        })
        .collect();
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
        assert_eq!(defined.get(name), Some(&value), "{name}");
    }
}
