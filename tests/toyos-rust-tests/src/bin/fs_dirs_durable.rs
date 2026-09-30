//! Directories on the FAT `/log` volume are real: `mkdir` writes one the
//! volume keeps, a directory the mount grew for a file's path is visible and
//! removable once emptied, and every `rmdir` outcome is the real one.
//! `common::volumes::fs_dirs_durable` judges what this leaves off the raw
//! image — a directory the server only pretended to make is one `fatfs`
//! cannot see.

use std::fs::{self, File};
use std::io::{ErrorKind, Write};

/// Mirrored in `tests/common/volumes.rs`.
const KEEP: &str = "/log/fsdir-keep";
const GONE: &str = "/log/fsdir-gone";

/// How many entries `path` lists, or the kind of its refusal.
fn entries(path: &str) -> Result<usize, ErrorKind> {
    fs::read_dir(path).map(|d| d.count()).map_err(|e| e.kind())
}

fn rmdir(path: &str) -> Result<(), ErrorKind> {
    fs::remove_dir(path).map_err(|e| e.kind())
}

fn main() {
    // POSIX mkdir(2): a new directory answers, a repeat is EEXIST.
    fs::create_dir(KEEP).expect("mkdir on the FAT volume");
    let err = fs::create_dir(KEEP).expect_err("mkdir of an existing directory must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "mkdir twice reported {err:?}");
    assert_eq!(entries(KEEP), Ok(0), "a fresh empty directory did not list as empty");

    // A directory the mount created for a file's path, then emptied by that
    // file's unlink: it must stay visible and become removable — on-disk
    // state `created_dirs` never saw.
    let file = format!("{GONE}/f.bin");
    let mut f = File::create(&file).expect("create under an implied directory");
    f.write_all(&[0x5C; 4096 + 33]).expect("write");
    f.sync_all().expect("fsync");
    drop(f);
    assert_eq!(
        rmdir(GONE),
        Err(ErrorKind::InvalidInput),
        "rmdir of a non-empty directory must refuse"
    );
    assert_eq!(
        rmdir(&file),
        Err(ErrorKind::InvalidInput),
        "rmdir of a file must refuse"
    );
    fs::remove_file(&file).expect("unlink the file");
    assert_eq!(
        entries(GONE),
        Ok(0),
        "an emptied on-disk directory disappeared from list"
    );
    rmdir(GONE).expect("rmdir of the emptied directory");
    assert_eq!(
        rmdir(GONE),
        Err(ErrorKind::NotFound),
        "rmdir of a removed directory must refuse"
    );
    assert_eq!(
        entries(GONE),
        Err(ErrorKind::NotFound),
        "a removed directory still lists"
    );

    println!("staged /log directories for the host oracle");
}
