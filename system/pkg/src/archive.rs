//! The ustar half of a `.tar.gz`, read as a stream: one header at a time, and
//! the entry's data after it, so no archive is ever held whole.
//!
//! **Every field is the archive's claim about itself**, and this is the one
//! place a downloaded file decides where bytes are written. A name that leaves
//! the package directory, a header whose own checksum does not match, an
//! archive that ends inside an entry and every type that is not a plain file,
//! a directory or a symlink are refused by name rather than skipped. Where a
//! symlink may point is the package's business (`crate::Planner`).
//!
//! GNU long names, pax headers, hard links and device nodes are refused rather
//! than implemented: an archive that needs one is one this installer does not
//! install, said out loud.

use std::io::Read;

const BLOCK: usize = 512;
const TYPE_FLAG: usize = 156;
const CHKSUM: usize = 148;
const CHKSUM_LEN: usize = 8;
const LINK_NAME: usize = 157;
const MAGIC: usize = 257;

/// What a tar entry may be here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    /// A symlink, holding its target as the archive wrote it.
    Symlink(String),
}

/// One entry's header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    /// Slash-separated, relative, and already checked to stay inside itself.
    pub path: String,
    pub kind: Kind,
    /// The data that follows the header: 0 but for a file.
    pub size: u64,
    /// The low nine permission bits; the rest of the mode is ignored.
    pub mode: u32,
}

impl Header {
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

/// An uncompressed tar, an entry at a time.
pub struct Reader<R> {
    inner: R,
    /// The current entry's data not yet read.
    left: u64,
    /// The zeros that pad it to a block.
    pad: u64,
    /// The path of the current entry, for a refusal that names it.
    current: String,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner, left: 0, pad: 0, current: String::new() }
    }

    /// The next entry, past whatever of the last one was not read, or `None` at
    /// the block of zeros that ends the archive.
    pub fn next_entry(&mut self) -> Result<Option<Header>, String> {
        self.skip(self.left + self.pad)?;
        self.left = 0;
        self.pad = 0;
        let mut header = [0u8; BLOCK];
        self.inner.read_exact(&mut header).map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => String::from("tar: the archive ends inside a header"),
            _ => format!("tar: {e}"),
        })?;
        if header.iter().all(|&b| b == 0) {
            return Ok(None);
        }
        let header = decode(&header)?;
        self.current = header.path.clone();
        self.left = header.size;
        self.pad = header.size.next_multiple_of(BLOCK as u64) - header.size;
        Ok(Some(header))
    }

    /// Up to `buf.len()` bytes of the current entry's data: 0 once it is all
    /// read, and a refusal where the archive ends before it does.
    pub fn read_data(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        if want == 0 {
            return Ok(0);
        }
        let n = self.inner.read(&mut buf[..want]).map_err(|e| format!("tar: {}: {e}", self.current))?;
        if n == 0 {
            return Err(format!("tar: the archive ends inside {}", self.current));
        }
        self.left -= n as u64;
        Ok(n)
    }

    /// Discard up to `bytes` of the stream. One that ends first is refused by
    /// the header read after it.
    fn skip(&mut self, bytes: u64) -> Result<(), String> {
        std::io::copy(&mut (&mut self.inner).take(bytes), &mut std::io::sink())
            .map(drop)
            .map_err(|e| format!("tar: {}: {e}", self.current))
    }
}

fn decode(header: &[u8; BLOCK]) -> Result<Header, String> {
    checksum_matches(header)?;
    // POSIX ustar is `ustar\0` then `00`; GNU tar's own format is `ustar  \0`,
    // and keeps other fields where ustar has the name's prefix.
    let posix = match &header[MAGIC..MAGIC + 8] {
        b"ustar\x0000" => true,
        b"ustar  \0" => false,
        _ => return Err(format!("tar: {:?} is not a ustar header", field(header, 0, 100))),
    };
    let mode = octal(header, 100, 8, "mode")? as u32 & 0o777;
    let size = octal(header, 124, 12, "size")?;
    let path = path_of(header, posix)?;
    let kind = match header[TYPE_FLAG] {
        b'0' | 0 => Kind::File,
        b'5' => Kind::Dir,
        b'2' => {
            let target = field(header, LINK_NAME, 100);
            if target.is_empty() {
                return Err(format!("tar: the symlink {path} holds no target"));
            }
            Kind::Symlink(target)
        }
        other => {
            return Err(format!(
                "tar: entry {path:?} is type {:?}, and only a file, a directory or a symlink is unpacked",
                other as char
            ))
        }
    };
    if kind != Kind::File && size != 0 {
        return Err(format!("tar: {path} claims {size} bytes of data, which only a file carries"));
    }
    Ok(Header { path, kind, size, mode })
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
fn path_of(header: &[u8], posix: bool) -> Result<String, String> {
    let name = field(header, 0, 100);
    let prefix = if posix { field(header, 345, 155) } else { String::new() };
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
pub(crate) mod tests {
    use super::*;

    /// One POSIX ustar header as a writer would build it, so the refusals below
    /// are tested against archives that are otherwise valid.
    pub(crate) fn header(name: &str, kind: u8, size: u64, mode: u32, link: &str) -> Vec<u8> {
        let mut h = vec![0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[TYPE_FLAG] = kind;
        h[LINK_NAME..LINK_NAME + link.len()].copy_from_slice(link.as_bytes());
        h[MAGIC..MAGIC + 8].copy_from_slice(b"ustar\x0000");
        seal(&mut h);
        h
    }

    /// Write the header's checksum over what it holds now.
    pub(crate) fn seal(h: &mut [u8]) {
        h[CHKSUM..CHKSUM + CHKSUM_LEN].copy_from_slice(b"        ");
        let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
        h[CHKSUM..CHKSUM + CHKSUM_LEN].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }

    /// `(name, type, data, mode, link target)` each, then the end.
    pub(crate) fn archive(parts: &[(&str, u8, &[u8], u32, &str)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, kind, data, mode, link) in parts {
            out.extend(header(name, *kind, data.len() as u64, *mode, link));
            out.extend(*data);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        }
        out.extend(vec![0u8; 2 * BLOCK]);
        out
    }

    /// Every entry with its data, read the way the installer reads them.
    fn all(tar: &[u8]) -> Result<Vec<(Header, Vec<u8>)>, String> {
        let mut reader = Reader::new(tar);
        let mut out = Vec::new();
        while let Some(h) = reader.next_entry()? {
            let mut data = Vec::new();
            let mut buf = [0u8; 3];
            loop {
                match reader.read_data(&mut buf)? {
                    0 => break,
                    n => data.extend_from_slice(&buf[..n]),
                }
            }
            out.push((h, data));
        }
        Ok(out)
    }

    #[test]
    fn a_directory_two_files_and_a_link_come_back_whole() {
        let tar = archive(&[
            ("gbae/", b'5', b"", 0o755, ""),
            ("gbae/gbae", b'0', b"ELF...", 0o755, ""),
            ("gbae/LICENSE", b'0', b"MIT", 0o644, ""),
            ("gbae/run", b'2', b"", 0o777, "gbae"),
        ]);
        let got = all(&tar).expect("entries");
        assert_eq!(got.len(), 4);
        assert_eq!((got[0].0.path.as_str(), &got[0].0.kind), ("gbae", &Kind::Dir));
        assert_eq!((got[1].0.path.as_str(), got[1].1.as_slice()), ("gbae/gbae", &b"ELF..."[..]));
        assert!(got[1].0.executable());
        assert_eq!(got[2].1, b"MIT");
        assert!(!got[2].0.executable());
        assert_eq!(got[3].0.kind, Kind::Symlink("gbae".into()));
        assert!(got.iter().all(|(h, _)| h.top() == "gbae"));
    }

    /// An entry whose data the reader never asked for is stepped over, padding
    /// and all, so the next header is where the archive put it.
    #[test]
    fn data_left_unread_is_skipped_to_the_next_header() {
        let tar = archive(&[("a/x", b'0', &[1u8; 700], 0o644, ""), ("a/y", b'0', b"y", 0o644, "")]);
        let mut reader = Reader::new(tar.as_slice());
        assert_eq!(reader.next_entry().unwrap().unwrap().path, "a/x");
        assert_eq!(reader.next_entry().unwrap().unwrap().path, "a/y");
        let mut buf = [0u8; 8];
        assert_eq!(reader.read_data(&mut buf).unwrap(), 1);
        assert_eq!(reader.next_entry().unwrap(), None);
    }

    /// The refusal the installer's write path rests on.
    #[test]
    fn a_name_that_leaves_the_archive_is_refused() {
        for name in ["/etc/passwd", "gbae/../../system/bin/supervisor", "../x", "./x", "a//b", ""] {
            let tar = archive(&[(name, b'0', b"x", 0o644, "")]);
            assert!(all(&tar).is_err(), "{name:?} was accepted");
        }
    }

    /// A type silently skipped is a package missing a file the archive carried.
    #[test]
    fn every_type_but_a_file_a_directory_and_a_symlink_is_refused() {
        for kind in *b"13467LKxg" {
            let tar = archive(&[("gbae/x", kind, b"", 0o644, "y")]);
            let why = all(&tar).expect_err(&format!("type {} was accepted", kind as char));
            assert!(why.contains("only a file, a directory or a symlink"), "{why}");
        }
        assert!(all(&archive(&[("gbae/x", b'2', b"", 0o777, "")])).is_err(), "a link to nothing");
        let link_with_data = archive(&[("gbae/x", b'2', b"data", 0o777, "y")]);
        assert!(all(&link_with_data).is_err(), "a link that carries data");
    }

    #[test]
    fn a_truncated_archive_is_refused_wherever_it_ends() {
        let whole = archive(&[("gbae/gbae", b'0', &[7u8; 1000], 0o755, "")]);
        // Inside the header, inside the data, inside the padding, and with no
        // end block at all.
        for cut in [1, BLOCK - 1, BLOCK + 1, BLOCK + 999, BLOCK + 1000, 3 * BLOCK] {
            let why = all(&whole[..cut]).expect_err(&format!("cut at {cut} was accepted"));
            assert!(why.contains("ends inside"), "cut at {cut}: {why}");
        }
        assert_eq!(all(&whole).unwrap().len(), 1);
        assert_eq!(all(&[0u8; BLOCK]).expect("an end block ends it").len(), 0);
    }

    #[test]
    fn a_header_that_does_not_sum_or_is_not_ustar_is_refused() {
        let mut tampered = archive(&[("gbae/gbae", b'0', b"ELF", 0o755, "")]);
        tampered[3] ^= 0x20;
        assert!(all(&tampered).is_err());

        let mut v7 = archive(&[("gbae/gbae", b'0', b"ELF", 0o755, "")]);
        v7[MAGIC..MAGIC + 8].fill(0);
        seal(&mut v7[..BLOCK]);
        assert!(all(&v7).unwrap_err().contains("not a ustar header"));
    }

    /// GNU tar's format keeps times where POSIX keeps the prefix, so its
    /// prefix field is never part of the name.
    #[test]
    fn a_gnu_header_s_prefix_field_is_not_its_name() {
        let mut tar = archive(&[("gbae/gbae", b'0', b"ELF", 0o755, "")]);
        tar[MAGIC..MAGIC + 8].copy_from_slice(b"ustar  \0");
        tar[345..352].copy_from_slice(b"1234567");
        seal(&mut tar[..BLOCK]);
        assert_eq!(all(&tar).unwrap()[0].0.path, "gbae/gbae");

        let mut posix = archive(&[("gbae", b'0', b"ELF", 0o755, "")]);
        posix[345..349].copy_from_slice(b"deep");
        seal(&mut posix[..BLOCK]);
        assert_eq!(all(&posix).unwrap()[0].0.path, "deep/gbae");
    }
}
