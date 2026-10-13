//! `hexdump` against a recorded `xxd`, and its refusals by name.

use std::path::Path;

use crate::hexdump::*;
use toyos_tmpdir::TempDir;

/// The 25 bytes the host's `xxd` was run against.
const FIXTURE: &[u8] = b"Hello, world! ABCDEFGH\x00\x01\xff";

/// `xxd fixture`, verbatim, from `xxd 2025-08-24 by Juergen Weigert et al.`
/// (macOS's `/usr/bin/xxd`).
const XXD_ALL: &str = "\
00000000: 4865 6c6c 6f2c 2077 6f72 6c64 2120 4142  Hello, world! AB
00000010: 4344 4546 4748 0001 ff                   CDEFGH...
";

/// `xxd -s 4 -l 8 fixture`, verbatim, from the same `xxd`.
const XXD_WINDOW: &str = "\
00000004: 6f2c 2077 6f72 6c64                      o, world
";

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// What `hexdump` says for `words`, against a file holding [`FIXTURE`],
/// whose path stands in for `F`.
fn hexdump(words: &[&str]) -> Result<String, String> {
    let dir = TempDir::new("hexdump");
    let file = dir.join("fixture");
    fs_write(&file);
    let words: Vec<String> = words.iter().map(|w| if *w == "F" { file.display().to_string() } else { w.to_string() }).collect();
    let (path, skip, length) = request(&words)?;
    let mut out = Vec::new();
    dump(Path::new(&path.expect("a file named")), skip, length, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

fn fs_write(path: &Path) {
    std::fs::write(path, FIXTURE).unwrap();
}

#[test]
fn a_dump_is_xxd_s_byte_for_byte_whole_and_in_a_window() {
    assert_eq!(hexdump(&["F"]).unwrap(), XXD_ALL);
    assert_eq!(hexdump(&["-s", "4", "-l", "8", "F"]).unwrap(), XXD_WINDOW);
    assert_eq!(hexdump(&["-s", "0x4", "-l", "0x8", "F"]).unwrap(), XXD_WINDOW, "hex offsets");
    assert_eq!(hexdump(&["-l", "4096", "F"]).unwrap(), XXD_ALL, "a length past the end is the file's");
}

/// Every request that does not name one file and numbers is refused by
/// what is wrong with it, as is an offset past the end.
#[test]
fn a_bad_request_is_refused_by_name() {
    let past = (FIXTURE.len() + 1).to_string();
    for (words, why) in [
        (&["-s", past.as_str(), "F"][..], "past the end"),
        (&["-s", "twelve", "F"], "not a number"),
        (&["-l"], "needs a number"),
        (&["-q", "F"], "unknown option"),
        (&["F", "F"], "one file at a time"),
    ] {
        let refusal = hexdump(words).unwrap_err();
        assert!(refusal.contains(why), "{words:?}: {refusal}");
    }
    assert_eq!(request(&[]), Ok((None, 0, None)), "no file is main's usage line");
    assert_eq!(request(&args(&["/no/such/file"])), Ok((Some("/no/such/file".to_string()), 0, None)));
    assert!(dump(Path::new("/no/such/file"), 0, None, &mut Vec::new()).is_err_and(|why| why.contains("/no/such/file")));
}
