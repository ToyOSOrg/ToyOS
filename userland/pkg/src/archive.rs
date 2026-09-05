//! The ustar half of a `.tar.gz`, decoded as far as unpacking a release
//! archive needs and no further.
//!
//! **Every field is the archive's claim about itself**, and this is the one
//! place a downloaded file decides where bytes are written. A name that leaves
//! the package directory, a header whose own checksum does not match, a size
//! that runs off the end and every type that is not a plain file or a
//! directory are refused by name rather than skipped.
//!
//! GNU long names, pax headers, links and device nodes are refused rather than
//! implemented: an archive that needs one is one this installer does not
//! install, said out loud.

const BLOCK: usize = 512;
const TYPE_FLAG: usize = 156;
const CHKSUM: usize = 148;
const CHKSUM_LEN: usize = 8;

/// What a tar entry may be here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    File,
    Dir,
}

/// One entry, borrowing the decompressed archive.
#[derive(Debug)]
pub struct Entry<'a> {
    /// Slash-separated, relative, and already checked to stay inside itself.
    pub path: String,
    pub kind: Kind,
    /// Empty for a directory.
    pub data: &'a [u8],
    /// The low nine permission bits; the rest of the mode is ignored.
    pub mode: u32,
}

impl Entry<'_> {
    /// Whether the archive marked this entry executable for its owner.
    pub fn executable(&self) -> bool {
        self.mode & 0o100 != 0
    }

    /// The entry's first path component, which is the directory a whole
    /// archive has to agree on.
    pub fn top(&self) -> &str {
        self.path.split('/').next().unwrap_or(&self.path)
    }
}

/// Decode every entry of an uncompressed tar.
pub fn entries(tar: &[u8]) -> Result<Vec<Entry<'_>>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + BLOCK <= tar.len() {
        let header = &tar[at..at + BLOCK];
        if header.iter().all(|&b| b == 0) {
            return Ok(out);
        }
        checksum_matches(header)?;
        let magic = &header[257..262];
        if magic != b"ustar" {
            return Err(format!("tar: block at {at} is not ustar"));
        }
        let size = octal(header, 124, 12, "size")?;
        let mode = octal(header, 100, 8, "mode")? as u32;
        let kind = match header[TYPE_FLAG] {
            b'0' | 0 => Kind::File,
            b'5' => Kind::Dir,
            other => {
                return Err(format!(
                    "tar: entry {:?} is type {:?}, and only a file or a directory is unpacked",
                    field(header, 0, 100),
                    other as char
                ))
            }
        };
        let path = path_of(header)?;
        at += BLOCK;
        let data = match kind {
            Kind::Dir => &tar[at..at],
            Kind::File => {
                let end = at
                    .checked_add(size as usize)
                    .filter(|end| *end <= tar.len())
                    .ok_or_else(|| format!("tar: {path} claims {size} bytes the archive lacks"))?;
                &tar[at..end]
            }
        };
        at += (size as usize).div_ceil(BLOCK) * BLOCK;
        out.push(Entry { path, kind, data, mode });
    }
    Err(String::from("tar: the archive ends inside a header"))
}

/// The header's own checksum, which is what makes a truncated or misaligned
/// block a refusal rather than a plausible entry.
fn checksum_matches(header: &[u8]) -> Result<(), String> {
    let want = octal(header, CHKSUM, CHKSUM_LEN, "checksum")?;
    let got: u64 = header
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (CHKSUM..CHKSUM + CHKSUM_LEN).contains(&i) { u64::from(b' ') } else { u64::from(b) }
        })
        .sum();
    if got != want {
        return Err(format!("tar: a header records checksum {want} and sums to {got}"));
    }
    Ok(())
}

/// `prefix/name`, refused unless it stays inside the archive's own tree: a
/// leading `/` or a `..` component is how an archive writes outside the
/// directory it was unpacked into.
fn path_of(header: &[u8]) -> Result<String, String> {
    let name = field(header, 0, 100);
    let prefix = field(header, 345, 155);
    let joined = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
    let path = joined.trim_end_matches('/');
    if path.is_empty() || path.starts_with('/') {
        return Err(format!("tar: {joined:?} is not a relative path"));
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("tar: {joined:?} has a component that leaves the archive"));
        }
    }
    Ok(path.to_string())
}

/// A NUL-terminated header field, as lossy UTF-8 so a refusal can quote it.
fn field(header: &[u8], at: usize, len: usize) -> String {
    let bytes = &header[at..at + len];
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(len);
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn octal(header: &[u8], at: usize, len: usize, what: &str) -> Result<u64, String> {
    let text = field(header, at, len);
    let digits = text.trim_matches(|c: char| c == ' ' || c == '\0');
    if digits.is_empty() || digits.bytes().any(|b| !(b'0'..=b'7').contains(&b)) {
        return Err(format!("tar: {what} field is {text:?}, which is not octal"));
    }
    u64::from_str_radix(digits, 8).map_err(|_| format!("tar: {what} field {text:?} does not fit"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One header as a writer would build it, so the refusals below are tested
    /// against archives that are otherwise valid.
    fn header(name: &str, kind: u8, size: u64, mode: u32) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[TYPE_FLAG] = kind;
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[CHKSUM..CHKSUM + CHKSUM_LEN].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
        h[CHKSUM..CHKSUM + CHKSUM_LEN].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }

    fn archive(parts: &[(&str, u8, &[u8], u32)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, kind, data, mode) in parts {
            out.extend(header(name, *kind, data.len() as u64, *mode));
            out.extend(*data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.extend(vec![0u8; 2 * BLOCK]);
        out
    }

    #[test]
    fn a_directory_and_two_files_come_back_whole() {
        let tar = archive(&[
            ("gbae/", b'5', b"", 0o755),
            ("gbae/gbae", b'0', b"ELF...", 0o755),
            ("gbae/LICENSE", b'0', b"MIT", 0o644),
        ]);
        let got = entries(&tar).expect("entries");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].path, "gbae");
        assert_eq!(got[0].kind, Kind::Dir);
        assert_eq!(got[1].path, "gbae/gbae");
        assert_eq!(got[1].data, b"ELF...");
        assert!(got[1].executable());
        assert_eq!(got[2].data, b"MIT");
        assert!(!got[2].executable());
        assert!(got.iter().all(|e| e.top() == "gbae"));
    }

    /// The refusal the installer's write path rests on.
    #[test]
    fn a_name_that_leaves_the_archive_is_refused() {
        for name in ["/etc/passwd", "gbae/../../system/bin/init", "../x", "./x", ""] {
            let tar = archive(&[(name, b'0', b"x", 0o644)]);
            assert!(entries(&tar).is_err(), "{name:?} was accepted");
        }
    }

    /// A type silently skipped is a package missing a file the archive carried.
    #[test]
    fn every_type_but_a_file_and_a_directory_is_refused() {
        for kind in [b'1', b'2', b'3', b'4', b'6', b'L', b'x', b'g'] {
            let tar = archive(&[("gbae/x", kind, b"", 0o644)]);
            assert!(entries(&tar).is_err(), "type {} was accepted", kind as char);
        }
    }

    #[test]
    fn a_header_that_does_not_sum_and_a_size_past_the_end_are_both_refused() {
        let mut tampered = archive(&[("gbae/gbae", b'0', b"ELF", 0o755)]);
        tampered[3] ^= 0x20;
        assert!(entries(&tampered).is_err());

        let mut short = archive(&[("gbae/gbae", b'0', b"ELF", 0o755)]);
        short.truncate(BLOCK + 1);
        assert!(entries(&short).is_err());

        assert!(entries(&[0u8; 7]).is_err());
        assert_eq!(entries(&[0u8; BLOCK]).expect("an end block ends it").len(), 0);
    }
}
