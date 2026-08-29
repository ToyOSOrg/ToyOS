//! Directories on the FAT `/log` volume are real: `mkdir` writes one the
//! volume keeps, a directory the mount grew for a file's path is visible and
//! removable once emptied, and every `rmdir` outcome is the real one.
//! `common::volumes::fs_dirs_durable` judges what this leaves off the raw
//! image — a directory the kernel only pretended to make is one `fatfs`
//! cannot see.

use std::fs::{self, File};
use std::io::Write;

use toyos_abi::syscall::{self, SyscallError};

/// Mirrored in `tests/common/volumes.rs`.
const KEEP: &str = "/log/fsdir-keep";
const GONE: &str = "/log/fsdir-gone";

fn readdir_needed(path: &str) -> Result<usize, SyscallError> {
    let mut buf = [0u8; 1];
    syscall::readdir(path.as_bytes(), &mut buf)
}

fn main() {
    // POSIX mkdir(2): a new directory answers, a repeat is EEXIST.
    fs::create_dir(KEEP).expect("mkdir on the FAT volume");
    let err = fs::create_dir(KEEP).expect_err("mkdir of an existing directory must refuse");
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists, "mkdir twice reported {err:?}");
    assert_eq!(readdir_needed(KEEP), Ok(0), "a fresh empty directory did not list as empty");

    // A directory the mount created for a file's path, then emptied by that
    // file's unlink: it must stay visible and become removable — on-disk
    // state `created_dirs` never saw.
    let file = format!("{GONE}/f.bin");
    let mut f = File::create(&file).expect("create under an implied directory");
    f.write_all(&[0x5C; 4096 + 33]).expect("write");
    f.sync_all().expect("fsync");
    drop(f);
    assert_eq!(
        syscall::rmdir(GONE.as_bytes()),
        Err(SyscallError::InvalidArgument),
        "rmdir of a non-empty directory must refuse"
    );
    assert_eq!(
        syscall::rmdir(file.as_bytes()),
        Err(SyscallError::InvalidArgument),
        "rmdir of a file must refuse"
    );
    fs::remove_file(&file).expect("unlink the file");
    assert_eq!(
        readdir_needed(GONE),
        Ok(0),
        "an emptied on-disk directory disappeared from list"
    );
    syscall::rmdir(GONE.as_bytes()).expect("rmdir of the emptied directory");
    assert_eq!(
        syscall::rmdir(GONE.as_bytes()),
        Err(SyscallError::NotFound),
        "rmdir of a removed directory must refuse"
    );
    assert_eq!(
        readdir_needed(GONE),
        Err(SyscallError::NotFound),
        "a removed directory still lists"
    );

    println!("staged /log directories for the host oracle");
}
