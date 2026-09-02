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

/// One 32-byte digest per [`BLOCK`], **to the end of the file rather than to a
/// declared length**: a device that grew was written to, and the last block is
/// digested short, so a size change either way moves the fingerprint.
pub fn whole_device(path: &Path) -> Vec<u8> {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|e| panic!("open {} to fingerprint: {e}", path.display()));
    let mut out = Vec::new();
    let mut buf = vec![0u8; BLOCK as usize];
    loop {
        let mut got = 0;
        while got < buf.len() {
            match file.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) => panic!("read {} to fingerprint: {e}", path.display()),
            }
        }
        if got == 0 {
            break;
        }
        out.extend_from_slice(&Sha256::digest(&buf[..got]));
        if got < buf.len() {
            break;
        }
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

    /// Every block is covered, and the failure says which one.
    #[test]
    fn a_write_anywhere_changes_the_fingerprint() {
        const LEN: u64 = 8 * BLOCK;
        let path = sparse("midpoint", LEN);
        let before = whole_device(&path);
        assert_eq!(
            before.len() as u64,
            32 * LEN / BLOCK,
            "one digest per block, and every block of the device"
        );
        assert_eq!(first_difference(&before, &whole_device(&path)), None);

        let mut file = std::fs::OpenOptions::new().write(true).open(&path).expect("open");
        file.seek(SeekFrom::Start(LEN / 2)).expect("seek");
        file.write_all(&[1]).expect("write");
        drop(file);

        let diff = first_difference(&before, &whole_device(&path))
            .expect("a byte at the midpoint is a byte the device did not have");
        assert!(diff.contains(&format!("offset {}", LEN / 2)), "{diff}");
        let _ = std::fs::remove_file(&path);
    }

    /// A device that came back a different size is a difference and not a
    /// panic, whichever way it moved.
    #[test]
    fn a_device_that_changed_size_is_a_difference_either_way() {
        const LEN: u64 = 3 * BLOCK;
        let path = sparse("resized", LEN);
        let before = whole_device(&path);

        std::fs::File::options().write(true).open(&path).unwrap().set_len(BLOCK).unwrap();
        let shorter = first_difference(&before, &whole_device(&path)).expect("shorter");
        assert!(shorter.contains("changed length"), "{shorter}");

        std::fs::File::options().write(true).open(&path).unwrap().set_len(4 * BLOCK).unwrap();
        let longer = first_difference(&before, &whole_device(&path)).expect("longer");
        assert!(longer.contains("changed length"), "{longer}");
        let _ = std::fs::remove_file(&path);
    }
}
