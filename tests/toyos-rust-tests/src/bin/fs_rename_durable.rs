//! Stage, on the FAT `/log` volume, the rename a failed source used to destroy,
//! and leave the destination behind for `common::volumes::fs_rename_durable` to
//! judge off the raw image — a source the kernel is not.

use std::fs::{self, File};
use std::io::Write;

/// Mirrored in `tests/common/volumes.rs`.
const VICTIM: &str = "/log/fstx-rename-victim.bin";
const SELFED: &str = "/log/fstx-rename-self.bin";
const ABSENT: &str = "/log/fstx-rename-absent.bin";
const LEN: usize = 5 * 4096 + 33;

fn payload() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(97) ^ 0x5A) as u8).collect()
}

fn stage(path: &str) {
    let mut f = File::create(path).unwrap_or_else(|e| panic!("create {path}: {e}"));
    f.write_all(&payload()).expect("write the payload");
    f.sync_all().expect("fsync the payload");
}

fn main() {
    let bytes = payload();
    let _ = fs::remove_file(ABSENT);

    stage(VICTIM);
    let err = fs::rename(ABSENT, VICTIM)
        .expect_err("rename of an absent source onto the victim must fail");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound, "rename(absent, victim) returned {err:?}");
    assert_eq!(fs::read(VICTIM).expect("victim gone after a failed rename"), bytes,
        "a failed rename changed the victim's bytes in the kernel's view");

    stage(SELFED);
    fs::rename(SELFED, SELFED).expect("rename(p, p) must succeed");
    assert_eq!(fs::read(SELFED).expect("self file gone after rename(p, p)"), bytes,
        "rename(p, p) changed the file in the kernel's view");

    println!("staged /log rename victims for the host oracle");
}
