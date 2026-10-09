//! Which requests change what a connection's directory holds: each is refused
//! `PermissionDenied` before the volume sees it, on a connection whose grant is
//! read-only (`toyos::fs::Access`) or whose volume is.
//!
//! **Closed by default**: an operation this list does not name is one the
//! wire does not have, and its client is let go on every connection, so a
//! request added to the wire is served nowhere until it is named here.

use toyos::fs::*;

/// Whether request `op`, with an open's `flags`, may change what the directory
/// holds: `None` for an operation the wire does not have.
pub fn changes(op: u32, flags: u64) -> Option<bool> {
    match op {
        OPEN => Some(flags & (O_WRITE | O_APPEND | O_CREATE | O_TRUNCATE | O_CREATE_NEW) != 0),
        HELLO | CLOSE | READ | STAT | LSTAT | FSTAT | FSYNC | SYNC | READDIR | READLINK => Some(false),
        WRITE | TRUNCATE | MKDIR | RMDIR | UNLINK | RENAME | SYMLINK | STREAM => Some(true),
        _ => None,
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
            assert_eq!(changes(op, 0), Some(true), "request {op} writes");
        }
        for op in READS {
            assert_eq!(changes(op, 0), Some(false), "request {op} only reads");
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
        assert_eq!(changes(OPEN, O_READ), Some(false));
        assert_eq!(changes(OPEN, 0), Some(false));
        for flag in [O_WRITE, O_APPEND, O_CREATE, O_TRUNCATE, O_CREATE_NEW] {
            assert_eq!(changes(OPEN, flag), Some(true), "flag {flag}");
            assert_eq!(changes(OPEN, flag | O_READ), Some(true), "flag {flag} with a read");
        }
    }

    /// A request number the wire does not have is named neither, so its
    /// client is let go whatever its grant.
    #[test]
    fn a_request_the_wire_does_not_have_is_neither() {
        for op in [0, SYNC + 1, REPLY, LINK, u32::MAX] {
            assert_eq!(changes(op, 0), None, "request {op}");
        }
    }
}
