//! What `strerror` says (`errtext.rs`): a text for every code
//! `include/errno.h` defines, each NUL-terminated, and none for a number no
//! code has.

use crate::errtext::{text, UNKNOWN};
use crate::header::{self, ERRNO_H};

#[test]
fn every_code_errno_h_defines_has_its_own_text() {
    let header = include_str!("../../include/errno.h");
    let mut seen = Vec::new();
    for line in header.lines() {
        let Some(name) = line.strip_prefix("#define ").and_then(|rest| rest.split_whitespace().next()) else { continue };
        let Some(value) = line.split_whitespace().nth(2).and_then(|v| v.parse::<i32>().ok()) else { continue };
        assert_eq!(header::int(ERRNO_H, name), value, "{name}");
        let said = text(value).unwrap_or_else(|| panic!("{name} ({value}) has no text"));
        assert_eq!(said.last(), Some(&0), "{name}'s text is not terminated");
        assert!(!said[..said.len() - 1].contains(&0), "{name}'s text holds a NUL");
        seen.push(value);
    }
    assert!(seen.len() > 70, "errno.h defines {} codes", seen.len());
    for unknown in [-1, 15, 9999] {
        assert_eq!(text(unknown), None, "{unknown}");
    }
    assert_eq!(UNKNOWN, b"Unknown error\0", "what a number no code has reads");
}
