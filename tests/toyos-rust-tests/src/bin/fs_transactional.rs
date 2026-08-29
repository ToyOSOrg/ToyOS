//! Namespace and size mutations are transactional: a rename that cannot find
//! its source destroys nothing, `rename(p, p)` is a no-op, and a shrink zeroes
//! the tail of the page it keeps so a regrow reads a hole rather than the
//! discarded bytes.
//!
//! The expected outcomes are POSIX's, not this kernel's — `rename(2)` returns an
//! error and leaves the destination when the source is absent and is a no-op
//! when the two names are the same file, and `ftruncate(2)` reads extended bytes
//! as zeros — so every assertion below is the standard judging the kernel.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

const PAGE: usize = 4096;

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + 7) as u8 | 1).collect()
}

/// A rename onto an existing destination that names no source must fail and
/// leave the destination byte-for-byte — the destination is disturbed only once
/// the source is known present.
fn rename_missing_source_keeps_destination(dir: &str) {
    let dst = format!("{dir}/fstx_keep_dst.bin");
    let missing = format!("{dir}/fstx_absent_src.bin");
    let bytes = pattern(3 * PAGE + 41);

    {
        let mut f = File::create(&dst).unwrap_or_else(|e| panic!("create {dst}: {e}"));
        f.write_all(&bytes).expect("write the destination");
        f.sync_all().expect("fsync the destination");
    }
    let _ = fs::remove_file(&missing);

    let err = fs::rename(&missing, &dst)
        .expect_err("rename of an absent source onto an existing destination must fail");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::NotFound,
        "{dir}: rename(absent, existing) returned {err:?}, not NotFound",
    );

    let back = fs::read(&dst)
        .unwrap_or_else(|e| panic!("{dir}: the destination is gone after a failed rename: {e}"));
    assert_eq!(back, bytes, "{dir}: a failed rename changed the destination's bytes");

    fs::remove_file(&dst).expect("cleanup");
}

/// The bcachefs destination loses its data only when it is dirty and unflushed
/// at rename time: the failed rename drops its pages and disarms its flush. This
/// arm carries an unflushed payload across the rename and reads it back.
fn rename_missing_source_keeps_dirty_destination(dir: &str) {
    let dst = format!("{dir}/fstx_dirty_dst.bin");
    let missing = format!("{dir}/fstx_dirty_src.bin");
    let old = pattern(2 * PAGE);
    let fresh: Vec<u8> = old.iter().map(|b| !b).collect();

    {
        let mut f = File::create(&dst).expect("create the destination");
        f.write_all(&old).expect("write the old contents");
        f.sync_all().expect("fsync the old contents");
    }
    let _ = fs::remove_file(&missing);

    // `fresh` is dirty and unflushed while the rename runs.
    let mut f = OpenOptions::new().write(true).open(&dst).expect("reopen to dirty the destination");
    f.write_all(&fresh).expect("write the unflushed payload");

    let err = fs::rename(&missing, &dst)
        .expect_err("rename of an absent source must fail even onto a dirty destination");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "{dir}: dirty-destination rename returned {err:?}");

    drop(f);

    let back = fs::read(&dst)
        .unwrap_or_else(|e| panic!("{dir}: the dirty destination is gone after a failed rename: {e}"));
    assert_eq!(back, fresh, "{dir}: a failed rename dropped the destination's unflushed payload");

    fs::remove_file(&dst).expect("cleanup");
}

/// `rename(p, p)` is the no-op success POSIX defines, not a path that destroys
/// the file and then reports it missing — `mv x .` reaches the kernel as exactly
/// this, its source and destination the same normalized path.
fn rename_self_is_a_noop(dir: &str) {
    let path = format!("{dir}/fstx_self.bin");
    let bytes = pattern(PAGE + 17);

    {
        let mut f = File::create(&path).expect("create the file");
        f.write_all(&bytes).expect("write the file");
        f.sync_all().expect("fsync the file");
    }

    fs::rename(&path, &path).unwrap_or_else(|e| panic!("{dir}: rename(p, p) must succeed: {e}"));

    let back = fs::read(&path)
        .unwrap_or_else(|e| panic!("{dir}: rename(p, p) lost the file: {e}"));
    assert_eq!(back, bytes, "{dir}: rename(p, p) changed the file");

    fs::remove_file(&path).expect("cleanup");
}

/// A case-only rename on a case-insensitive mount names one entry by two
/// strings and must keep the file; a byte-exact self-rename guard destroys it.
fn rename_case_only_keeps_the_file(dir: &str) {
    let upper = format!("{dir}/FSTX-Case.BIN");
    let lower = format!("{dir}/fstx-case.bin");
    let bytes = pattern(2 * PAGE + 7);

    {
        let mut f = File::create(&upper).unwrap_or_else(|e| panic!("create {upper}: {e}"));
        f.write_all(&bytes).expect("write the file");
        f.sync_all().expect("fsync the file");
    }

    fs::rename(&upper, &lower).unwrap_or_else(|e| panic!("{dir}: case-only rename failed: {e}"));

    let back = fs::read(&lower)
        .unwrap_or_else(|e| panic!("{dir}: a case-only rename lost the file: {e}"));
    assert_eq!(back, bytes, "{dir}: a case-only rename changed the file");

    fs::remove_file(&lower).expect("cleanup");
}

/// Shrink to a size inside a page while that page is resident, then regrow: the
/// bytes past the old end must read as zeros. The write, shrink and regrow share
/// one open so the straddled page is resident when the size is cut; `durable`
/// then fsyncs each step and reads back across a close, off the device.
fn shrink_then_regrow_reads_zeros(dir: &str, seed_len: usize, durable: bool) {
    let path = format!("{dir}/fstx_shrink.bin");
    let seed = pattern(seed_len);
    const CUT: u64 = 100;

    {
        let mut f = OpenOptions::new()
            .read(true).write(true).create(true).truncate(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("create {path}: {e}"));
        f.write_all(&seed).expect("write the seed");
        if durable { f.sync_all().expect("fsync the seed"); }
        f.set_len(CUT).expect("shrink into the first page");
        if durable { f.sync_all().expect("fsync the shrink"); }
        f.set_len(seed_len as u64).expect("regrow");
        if durable { f.sync_all().expect("fsync the regrow"); }

        if !durable {
            let mut got = vec![0u8; seed_len];
            f.seek(SeekFrom::Start(0)).expect("rewind");
            f.read_exact(&mut got).expect("read the whole file back");
            check_hole(dir, &got, &seed, CUT as usize);
            fs::remove_file(&path).expect("cleanup");
            return;
        }
    }

    // Durable: the size question is now the device's to answer.
    let mut f = File::open(&path).expect("reopen to read off the device");
    let mut got = vec![0u8; seed_len];
    f.read_exact(&mut got).expect("read the whole file back after reopen");
    check_hole(dir, &got, &seed, CUT as usize);
    fs::remove_file(&path).expect("cleanup");
}

fn check_hole(dir: &str, got: &[u8], seed: &[u8], cut: usize) {
    assert_eq!(&got[..cut], &seed[..cut], "{dir}: the surviving head changed across shrink and regrow");
    if let Some(at) = got[cut..].iter().position(|&b| b != 0) {
        panic!("{dir}: byte {} past the shrink is {:#04x}, not zero — a regrow served the discarded tail",
            cut + at, got[cut + at]);
    }
}

/// After a shrink and regrow, the regrown hole must read as zeros — including
/// between the old data and a byte written back into it. One open keeps the
/// straddled page resident throughout.
fn write_into_hole_reads_zeros(dir: &str) {
    let path = format!("{dir}/fstx_hole.bin");
    let seed = pattern(PAGE);
    const CUT: u64 = 100;
    const POKE: u64 = 200;

    let mut f = OpenOptions::new()
        .read(true).write(true).create(true).truncate(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("create {path}: {e}"));
    f.write_all(&seed).expect("write a page of pattern");
    f.set_len(CUT).expect("shrink into the page");
    f.set_len(PAGE as u64).expect("regrow to a full page");
    f.seek(SeekFrom::Start(POKE)).expect("seek into the regrown hole");
    f.write_all(&[0xEE]).expect("poke a byte into the hole");

    let mut got = vec![0u8; PAGE];
    f.seek(SeekFrom::Start(0)).expect("rewind");
    f.read_exact(&mut got).expect("read the whole page back");
    assert_eq!(&got[..CUT as usize], &seed[..CUT as usize], "{dir}: the surviving head changed");
    assert_eq!(got[POKE as usize], 0xEE, "{dir}: the poked byte did not land");
    for (i, &b) in got.iter().enumerate() {
        let expected_zero = i >= CUT as usize && i != POKE as usize;
        if expected_zero && b != 0 {
            panic!("{dir}: byte {i} past the shrink is {b:#04x}, not zero");
        }
    }

    drop(f);
    fs::remove_file(&path).expect("cleanup");
}

fn main() {
    // F8: a rename that cannot find its source keeps the destination, on every
    // writable mount — tmpfs, bcachefs and FAT.
    for dir in ["/tmp", "/home", "/log"] {
        rename_missing_source_keeps_destination(dir);
        rename_self_is_a_noop(dir);
    }
    rename_missing_source_keeps_dirty_destination("/home");
    rename_missing_source_keeps_dirty_destination("/tmp");
    // /log is FAT, the one case-insensitive mount.
    rename_case_only_keeps_the_file("/log");

    // F7: a shrink zeroes the tail of the page it keeps, so a regrow reads a
    // hole. /tmp is the in-memory control and also drops a whole page; /home
    // carries the straddled page to the device across a close.
    shrink_then_regrow_reads_zeros("/tmp", 2 * PAGE, false);
    shrink_then_regrow_reads_zeros("/home", PAGE, true);
    // Multi-page and durable: the pages a shrink drops must not come back off
    // the device after a close — the on-disk record has to give their blocks
    // up, not only the page cache.
    shrink_then_regrow_reads_zeros("/home", 3 * PAGE, true);
    shrink_then_regrow_reads_zeros("/log", 3 * PAGE, true);
    write_into_hole_reads_zeros("/tmp");
    write_into_hole_reads_zeros("/home");

    println!("fs transactional: rename keeps its destination, rename(p,p) is a no-op, a shrunk tail regrows as zeros");
}
