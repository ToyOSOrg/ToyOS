//! What a disk this system was not given is compared against, before and after
//! a boot.
//!
//! **The whole device, not the places a format is expected to write.** A write
//! is a write wherever it lands, and a fingerprint of the two ends is green
//! over every byte between them.
//!
//! One SHA-256 per [`BLOCK`] rather than one over the file, so a difference
//! still says where. The images are ~128 MiB and sparse, so the cost is one
//! sequential read of a file the test just wrote.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

/// The span one digest covers, and the resolution a difference is reported at.
pub const BLOCK: u64 = 1024 * 1024;

/// One 32-byte digest per [`BLOCK`] of the first `len` bytes of `path`.
///
/// A short read is a difference like any other: the last block's digest is over
/// the bytes that were there, so a truncation moves the fingerprint.
pub fn whole_device(path: &Path, len: u64) -> Vec<u8> {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("open {} to fingerprint: {e}", path.display()));
    let mut out = Vec::new();
    let mut buf = vec![0u8; BLOCK as usize];
    let mut left = len;
    while left > 0 {
        let want = usize::try_from(left.min(BLOCK)).expect("a block fits a usize");
        let mut got = 0;
        while got < want {
            match file.read(&mut buf[got..want]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => panic!("read {} to fingerprint: {e}", path.display()),
            }
        }
        out.extend_from_slice(&Sha256::digest(&buf[..got]));
        if got < want {
            break;
        }
        left -= want as u64;
    }
    out
}

/// Where two fingerprints first differ, rendered for a failure message.
pub fn first_difference(before: &[u8], after: &[u8]) -> Option<String> {
    if before == after {
        return None;
    }
    let at = before.iter().zip(after).position(|(a, b)| a != b);
    Some(match at {
        Some(at) => {
            let block = at as u64 / 32;
            format!(
                "the {BLOCK}-byte block at offset {} changed",
                block * BLOCK
            )
        }
        None => format!(
            "the device changed length: {} block(s) of digest against {}",
            before.len() / 32,
            after.len() / 32
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    fn sparse(name: &str, len: u64) -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("toyos-fingerprint-{}-{name}", std::process::id()));
        let file = std::fs::File::create(&path).expect("create");
        file.set_len(len).expect("size");
        path
    }

    /// The reading this replaced took the front and the back, so a write in
    /// between was invisible to a gate that says "untouched". Every block is
    /// covered, and the failure says which one.
    #[test]
    fn a_write_anywhere_changes_the_fingerprint() {
        const LEN: u64 = 8 * 1024 * 1024;
        let path = sparse("midpoint", LEN);
        let before = whole_device(&path, LEN);
        assert_eq!(
            before.len() as u64,
            32 * LEN / BLOCK,
            "one digest per block, and every block of the device"
        );
        assert_eq!(first_difference(&before, &whole_device(&path, LEN)), None);

        let mut file = std::fs::OpenOptions::new().write(true).open(&path).expect("open");
        file.seek(SeekFrom::Start(LEN / 2)).expect("seek");
        file.write_all(&[1]).expect("write");
        drop(file);

        let diff = first_difference(&before, &whole_device(&path, LEN))
            .expect("a byte at the midpoint is a byte the device did not have");
        assert!(diff.contains(&format!("offset {}", LEN / 2)), "{diff}");
        let _ = std::fs::remove_file(&path);
    }

    /// A device that came back shorter is a difference and not a panic: the
    /// block where the bytes stopped is the one that moved. A fingerprint that
    /// covers fewer blocks than the one it is held against is the other shape,
    /// and it is a difference too.
    #[test]
    fn a_shorter_device_is_a_difference_and_not_a_panic() {
        const LEN: u64 = 3 * BLOCK;
        let path = sparse("short", LEN);
        let before = whole_device(&path, LEN);
        std::fs::File::options().write(true).open(&path).unwrap().set_len(BLOCK).unwrap();
        let diff = first_difference(&before, &whole_device(&path, LEN)).expect("shorter");
        assert!(diff.contains(&format!("offset {BLOCK}")), "{diff}");
        let short = first_difference(&before, &whole_device(&path, BLOCK)).expect("fewer blocks");
        assert!(short.contains("changed length"), "{short}");
        let _ = std::fs::remove_file(&path);
    }
}
