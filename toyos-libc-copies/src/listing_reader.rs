//! The readdir answer's reader against answers encoded as the kernel's
//! `sys_readdir` encodes them (`kernel/src/syscall/fs.rs`): a kind byte, the
//! name, a NUL and eight bytes of size, for each entry.

use crate::listing;

/// An answer for `entries`, each a name, whether it is a directory, and a size.
fn answer(entries: &[(&str, bool, u64)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, is_dir, size) in entries {
        out.push(if *is_dir { 2 } else { 1 });
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.extend_from_slice(&size.to_le_bytes());
    }
    out
}

fn read(bytes: &[u8]) -> Vec<(String, bool)> {
    let mut pos = 0;
    let mut out = Vec::new();
    while let Some(entry) = listing::next(bytes, &mut pos) {
        out.push((String::from_utf8(entry.name.to_vec()).unwrap(), entry.is_dir));
    }
    assert_eq!(pos, bytes.len(), "the reader stopped short of the answer's end");
    out
}

#[test]
fn every_entry_is_read_back_whatever_its_size_holds() {
    // Sizes whose bytes are NULs, kind bytes and letters: a reader that does
    // not step over all eight takes them for names.
    let entries = [
        ("a", false, 0),
        ("dir", true, 0x0201_0000_0000_0000),
        ("b.txt", false, 0x6162_6300_0102_0304),
        ("", false, u64::MAX),
        ("last", true, 17),
    ];
    let got = read(&answer(&entries));
    let want: Vec<(String, bool)> = entries.iter().map(|(n, d, _)| ((*n).to_string(), *d)).collect();
    assert_eq!(got, want);
    assert!(read(&[]).is_empty());
}

#[test]
#[should_panic(expected = "of kind 3")]
fn an_unknown_kind_is_a_broken_kernel() {
    read(&[3, b'x', 0, 0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
#[should_panic(expected = "no size")]
fn a_short_entry_is_a_broken_kernel() {
    read(&[1, b'x', 0, 0, 0]);
}
