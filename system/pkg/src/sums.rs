//! The release's own `SHA256SUMS`, which is the whole of stage 1's trust chain.
//!
//! **A third party's statement about the archive, read literally.** It covers
//! every asset of a release, so the one line this installer wants is found by
//! file name — and a name given twice with two digests is refused, because
//! taking the first would let an appended line decide what a name means.

use toyos_manifest::package::DIGEST_LEN;

/// The digest `SHA256SUMS` records for `file`.
///
/// The two spellings `sha256sum` writes — `<digest>  <name>` for text and
/// `<digest> *<name>` for binary — are the same statement, and a line in
/// neither shape is refused rather than skipped.
pub fn digest_for(text: &str, file: &str) -> Result<String, String> {
    let mut found: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let (digest, rest) = line
            .split_at_checked(DIGEST_LEN)
            .ok_or_else(|| format!("SHA256SUMS: {line:?} is shorter than a digest"))?;
        if !digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(format!("SHA256SUMS: {digest:?} is not a lowercase hex SHA-256"));
        }
        let name = rest
            .strip_prefix("  ")
            .or_else(|| rest.strip_prefix(" *"))
            .ok_or_else(|| format!("SHA256SUMS: {line:?} has no file name after its digest"))?;
        if name != file {
            continue;
        }
        if found.is_some_and(|first| first != digest) {
            return Err(format!("SHA256SUMS: {file} is given two different digests"));
        }
        found = Some(digest);
    }
    found
        .map(str::to_string)
        .ok_or_else(|| format!("SHA256SUMS: no line covers {file}"))
}

/// A digest as this file spells one.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gbae v0.2.0's own file, quoted whole: the outside statement.
    const RELEASE: &str = "\
40c73eb97c69c3002bb3b56d8d4cff620b84cef2760bf80ac40eb97565a0fa01  gbae-v0.2.0-linux-x86_64.tar.gz
191216f00c4c8d3cdcd58dd708c9a938ae4956b39eca62615fe95d11facf17d5  gbae-v0.2.0-macos-universal.tar.gz
99fcd8a7263b5c25cd90cead1baaa7200ef272100fc2226e008a4e8205ba2916  gbae-v0.2.0-toyos-x86_64.tar.gz
98bf5cf0036ddd20089a359957d8eadcd153d6e23d329b2c9b9c1bb62a2a9b3d  gbae-v0.2.0-windows-x86_64.zip
";

    #[test]
    fn the_one_line_for_this_asset_is_the_answer() {
        assert_eq!(
            digest_for(RELEASE, "gbae-v0.2.0-toyos-x86_64.tar.gz").unwrap(),
            "99fcd8a7263b5c25cd90cead1baaa7200ef272100fc2226e008a4e8205ba2916"
        );
        assert!(digest_for(RELEASE, "gbae-v0.2.0-toyos-aarch64.tar.gz").is_err());
        assert!(digest_for("", "gbae-v0.2.0-toyos-x86_64.tar.gz").is_err());
    }

    #[test]
    fn a_line_that_is_not_one_and_a_name_given_twice_are_both_refused() {
        assert!(digest_for("deadbeef  x\n", "x").is_err());
        assert!(digest_for(&format!("{}\tx\n", "a".repeat(DIGEST_LEN)), "x").is_err());
        assert!(digest_for(&format!("{}  x\n", "A".repeat(DIGEST_LEN)), "x").is_err());
        let twice = format!("{}  x\n{}  x\n", "a".repeat(DIGEST_LEN), "b".repeat(DIGEST_LEN));
        assert!(digest_for(&twice, "x").is_err());
        let same = format!("{}  x\n{}  x\n", "a".repeat(DIGEST_LEN), "a".repeat(DIGEST_LEN));
        assert!(digest_for(&same, "x").is_ok());
        assert_eq!(digest_for(&format!("{} *x\n", "a".repeat(DIGEST_LEN)), "x").unwrap().len(), 64);
    }

    #[test]
    fn hex_is_the_spelling_the_file_uses() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }
}
