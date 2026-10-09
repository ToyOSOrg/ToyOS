//! Which requests change what a connection's directory holds: each is refused
//! `PermissionDenied` before the volume sees it, on a connection whose grant is
//! read-only (`toyos::fs::Access`) or whose volume is.
//!
//! **Closed by default**: an operation this list does not name as one that
//! only reads is one that changes, so a request added to the wire is refused on
//! a read-only connection until it is named here.

use toyos::fs::*;

/// Whether request `op`, with an open's `flags`, may change what the directory
/// holds.
pub fn changes(op: u32, flags: u64) -> bool {
    match op {
        OPEN => flags & (O_WRITE | O_APPEND | O_CREATE | O_TRUNCATE | O_CREATE_NEW) != 0,
        HELLO | CLOSE | READ | STAT | LSTAT | FSTAT | FSYNC | SYNC | READDIR | READLINK => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every request the wire has, by what it does to the directory: the
    /// independent spelling `changes` is held to.
    const READS: [u32; 10] = [HELLO, CLOSE, READ, STAT, LSTAT, FSTAT, FSYNC, SYNC, READDIR, READLINK];
    const WRITES: [u32; 8] = [WRITE, TRUNCATE, MKDIR, RMDIR, UNLINK, RENAME, SYMLINK, STREAM];

    #[test]
    fn every_request_that_writes_changes_the_directory_and_none_that_reads_does() {
        for op in WRITES {
            assert!(changes(op, 0), "request {op} writes");
        }
        for op in READS {
            assert!(!changes(op, 0), "request {op} only reads");
        }
        // Every request number the wire has is one of the two, or `OPEN`.
        let mut named: Vec<u32> = READS.iter().chain(&WRITES).copied().chain([OPEN]).collect();
        named.sort_unstable();
        assert_eq!(named, (HELLO..=SYNC).collect::<Vec<_>>());
    }

    /// An open changes the directory by any one flag that writes, creates or
    /// truncates, alone or with a read.
    #[test]
    fn an_open_changes_the_directory_by_every_flag_but_read() {
        assert!(!changes(OPEN, O_READ));
        assert!(!changes(OPEN, 0));
        for flag in [O_WRITE, O_APPEND, O_CREATE, O_TRUNCATE, O_CREATE_NEW] {
            assert!(changes(OPEN, flag), "flag {flag}");
            assert!(changes(OPEN, flag | O_READ), "flag {flag} with a read");
        }
    }

    /// A request number the wire does not have is refused on a read-only
    /// connection, never served as a read.
    #[test]
    fn a_request_the_wire_does_not_have_changes_the_directory() {
        for op in [0, SYNC + 1, REPLY, LINK, u32::MAX] {
            assert!(changes(op, 0), "request {op}");
        }
    }
}
