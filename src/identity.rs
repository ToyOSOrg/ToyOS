//! What a source file is to a build: its token stream, not its text.
//!
//! **One definition, read by every question of the form "did this source
//! change what gets built"**: the version judge (`src/sdkversion.rs`) and the
//! key a sysroot is filed under (`src/toolchain.rs`). A comment — a doc comment
//! included — and the whitespace around tokens change no item, no layout and no
//! code, so a change made only of them is neither a version bump nor a new
//! sysroot.
//!
//! A `.rs` file is lexed just far enough to find its comments: every string,
//! raw string and character literal is kept byte for byte, every comment and
//! every run of whitespace becomes one space, and nothing else moves. Two files
//! with equal identities therefore lex to the same tokens. The converse is not
//! claimed: space appearing where there was none (`A;` to `A ;`) is a change,
//! because between two punctuation characters it can be one (`&&` against
//! `& &`), and telling those apart is a lexer this does not need to be. Any
//! other file is its bytes.
//!
//! What this gives up, deliberately: rustdoc output, and the line numbers a
//! panic location or debuginfo carries — a comment that adds a line moves those
//! and is still not a new sysroot. A `#![deny(missing_docs)]` crate is the one
//! place a doc comment decides whether a build succeeds; none of the crates
//! this is asked about carries it, and the crate's own build is what would
//! refuse.

use std::borrow::Cow;
use std::path::Path;

/// `bytes`, as what a build of the file at `path` can see of them.
pub fn of<'a>(path: &Path, bytes: &'a [u8]) -> Cow<'a, [u8]> {
    if path.extension().is_some_and(|e| e == "rs") {
        Cow::Owned(rust_tokens(bytes))
    } else {
        Cow::Borrowed(bytes)
    }
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// The length of the UTF-8 character whose first byte is `lead`.
fn char_len(lead: u8) -> usize {
    match lead {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

/// The source with every comment and every run of whitespace reduced to one
/// space between the tokens it separated.
fn rust_tokens(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut gap = false;
    let emit = |out: &mut Vec<u8>, gap: &mut bool, token: &[u8]| {
        if *gap && !out.is_empty() {
            out.push(b' ');
        }
        *gap = false;
        out.extend_from_slice(token);
    };
    let n = b.len();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if is_space(c) {
            gap = true;
            i += 1;
        } else if b[i..].starts_with(b"//") {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            gap = true;
        } else if b[i..].starts_with(b"/*") {
            // Nested, as Rust's block comments are.
            let mut depth = 0usize;
            while i < n {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            gap = true;
        } else if c == b'"' {
            let end = quoted_end(b, i);
            emit(&mut out, &mut gap, &b[i..end]);
            i = end;
        } else if c == b'\'' {
            let end = char_literal_end(b, i).unwrap_or(i + 1);
            emit(&mut out, &mut gap, &b[i..end]);
            i = end;
        } else if is_word(c) {
            let mut end = i;
            while end < n && is_word(b[end]) {
                end += 1;
            }
            let end = match &b[i..end] {
                b"r" | b"br" | b"cr" => raw_string_end(b, end).unwrap_or(end),
                _ => end,
            };
            emit(&mut out, &mut gap, &b[i..end]);
            i = end;
        } else {
            emit(&mut out, &mut gap, &b[i..=i]);
            i += 1;
        }
    }
    out
}

/// One past the `"` closing the string opened at `open`, escapes honoured.
fn quoted_end(b: &[u8], open: usize) -> usize {
    let mut j = open + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// One past a character literal opening at `open`, or `None` where the `'`
/// begins a lifetime or a label.
fn char_literal_end(b: &[u8], open: usize) -> Option<usize> {
    let first = *b.get(open + 1)?;
    if first == b'\\' {
        // The escaped character itself, then on to the close: `'\''`, `'\u{2764}'`.
        let mut j = open + 3;
        while j < b.len() && b[j] != b'\'' {
            j += 1;
        }
        return Some((j + 1).min(b.len()));
    }
    let close = open + 1 + char_len(first);
    (b.get(close) == Some(&b'\'')).then_some(close + 1)
}

/// One past a raw string whose prefix ends at `after_prefix`, or `None` where
/// the prefix is an identifier (`r#match`) or a lone `r`.
fn raw_string_end(b: &[u8], after_prefix: usize) -> Option<usize> {
    let mut j = after_prefix;
    while b.get(j) == Some(&b'#') {
        j += 1;
    }
    if b.get(j) != Some(&b'"') {
        return None;
    }
    let hashes = j - after_prefix;
    let mut k = j + 1;
    while k < b.len() {
        if b[k] == b'"' && b[k + 1..].iter().take(hashes).filter(|&&h| h == b'#').count() == hashes
        {
            return Some(k + 1 + hashes);
        }
        k += 1;
    }
    Some(b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rs(text: &str) -> String {
        String::from_utf8(of(Path::new("x.rs"), text.as_bytes()).into_owned()).unwrap()
    }

    fn same(a: &str, b: &str) -> bool {
        rs(a) == rs(b)
    }

    /// **A recorded real failure, verbatim**: the hunk of `toyos-abi/src/syscall.rs`
    /// that `c00056b9` (#499) changed, which cost a minor bump of three crates
    /// and a shared-sysroot rebuild in every worktree. Its identity did not move.
    #[test]
    fn the_doc_fix_that_cost_a_version_and_a_sysroot_is_neither() {
        let before = "    pub const CANARY_CHANGED: u64 = 11;
    /// Make the last CPU a shootdown waits for answer `arg` nanoseconds late,
    /// and take it away again.
    pub const TLB_ACK_DELAY_ARM: u64 = 12;
";
        let after = "    pub const CANARY_CHANGED: u64 = 11;
    /// Hold each other CPU's acknowledgement back for `arg` nanoseconds in
    /// turn, one at a time, and answer the smallest wait any of them cost the
    /// initiator — `0` on a machine with no other CPU to hold back. The
    /// arming is then left standing against every other CPU until
    /// `TLB_ACK_DELAY_DISARM` or the end of a fresh two-second window,
    /// whichever comes first.
    pub const TLB_ACK_DELAY_ARM: u64 = 12;
";
        assert!(same(before, after));
        // `439244cf`'s, from `toyos-abi/src/boot.rs`, the same shape.
        assert!(same(
            "/// The most windows the loader will carry: four memory windows on each of\n\
             /// sixteen root bridges.\npub const MAX_ROOT_BRIDGE_WINDOWS: usize = 64;\n",
            "/// The most windows the loader will carry.\npub const MAX_ROOT_BRIDGE_WINDOWS: usize = 64;\n",
        ));
    }

    /// And the other direction, which a function that ignored everything would
    /// pass the test above with: a value, a type, a name, a field.
    #[test]
    fn a_signature_or_a_value_is_a_change() {
        assert!(!same("pub const TLB_ACK_DELAY_ARM: u64 = 12;", "pub const TLB_ACK_DELAY_ARM: u64 = 13;"));
        assert!(!same("pub struct A;", "pub struct A(pub u64);"));
        assert!(!same("pub fn f(a: u32) {}", "pub fn f(a: u64) {}"));
        assert!(!same("pub fn f() {}", "pub fn g() {}"));
        assert!(!same("#[repr(C)] struct S { a: u8, b: u32 }", "#[repr(C)] struct S { b: u32, a: u8 }"));
        // Whitespace separates tokens, so its presence is a change and only its amount is not.
        assert!(!same("a b", "ab"));
        assert!(same("a  b", "a\n\tb"));
        assert!(same("a/**/b", "a b"));
        assert!(!same("a/**/b", "ab"));
    }

    /// Everything that only looks like a comment is kept, byte for byte.
    #[test]
    fn a_literal_is_never_read_as_a_comment() {
        assert!(!same(r#"const U: &str = "http://a";"#, r#"const U: &str = "http://b";"#));
        assert!(!same(r#"const U: &str = "a /* b */ c";"#, r#"const U: &str = "a  c";"#));
        assert!(!same("const S: &str = \"a  b\";", "const S: &str = \"a b\";"));
        assert!(!same(r#"const Q: &str = "\" // x";"#, r#"const Q: &str = "\" // y";"#));
        assert!(!same(r###"const R: &str = r#"a "// x" b"#;"###, r###"const R: &str = r#"a "// y" b"#;"###));
        assert!(!same(r#"const B: &[u8] = br"// x";"#, r#"const B: &[u8] = br"// y";"#));
        assert!(!same("const C: char = '\"'; // \"\nconst D: u8 = 1;", "const C: char = '\"'; // \"\nconst D: u8 = 2;"));
        assert!(!same("const C: char = '\\''; const D: &str = \"// x\";", "const C: char = '\\''; const D: &str = \"// y\";"));
        assert!(!same("const C: char = 'é'; const D: &str = \"// x\";", "const C: char = 'é'; const D: &str = \"// y\";"));
        // A lifetime is not a character literal, and a raw identifier is not a raw string.
        assert!(same("fn f<'a>(x: &'a str) -> &'a str { x } // one", "fn f<'a>(x: &'a str) -> &'a str { x }"));
        assert!(!same("fn f<'a>(x: &'a str) { \"// x\"; }", "fn f<'a>(x: &'a str) { \"// y\"; }"));
        assert!(same("let r#match = 1; // one", "let r#match = 1;"));
        // Block comments nest.
        assert!(same("a /* x /* y */ z */ b", "a b"));
        assert!(!same("a /* x /* y */ z */ b", "a z */ b"));
    }

    /// Anything but Rust is its bytes.
    #[test]
    fn a_file_that_is_not_rust_is_its_bytes() {
        let toml = b"[package] # a comment\n";
        assert_eq!(&*of(Path::new("Cargo.toml"), toml), toml);
        assert_eq!(&*of(Path::new("abi.h"), b"/* c */ int a;"), b"/* c */ int a;");
    }

    /// **The tree itself as the corpus**: every `.rs` file of every crate this
    /// is asked about lexes to a fixed point, and deleting every line of it that
    /// is a doc comment changes nothing.
    #[test]
    fn every_doc_comment_in_the_published_and_sysroot_crates_is_invisible() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        for dir in crate::sysroot::SYSROOT_SOURCES
            .iter()
            .copied()
            .chain(crate::sdkversion::PUBLISHED.iter().map(|k| k.dir))
        {
            walk(&root.join(dir), &mut files);
        }
        assert!(files.len() > 50, "the corpus walk found {} files", files.len());
        let mut docs = 0;
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            let id = rs(&text);
            assert_eq!(rs(&id), id, "{} does not lex to a fixed point", path.display());
            let undocumented: String = text
                .lines()
                .filter(|l| {
                    let doc = l.trim_start().starts_with("///") || l.trim_start().starts_with("//!");
                    docs += usize::from(doc);
                    !doc
                })
                .map(|l| format!("{l}\n"))
                .collect();
            assert_eq!(rs(&undocumented), id, "{}'s doc comments reach its identity", path.display());
        }
        assert!(docs > 1000, "the corpus holds {docs} doc lines, which is not the tree");
    }

    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target") {
                    walk(&path, out);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
}
